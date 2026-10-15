//! `wxsl-mcp`: an MCP server over the editor's commands
//! ([ADR 0083](../../../docs/adr/0083-an-mcp-server-over-the-editors-commands.md)).
//!
//! An agent connected through the Model Context Protocol can write
//! shaders, inspect a scene and debug a frame in a running
//! [`Editor`], the one the user has open, or a
//! headless one. Every tool is a plan over the editor's
//! [`Command`](wxsl_editor::Command)s and [`Query`](wxsl_editor::Query)s
//! (ADR 0081), so whatever an agent does, the user can see and undo, and
//! the history marks it as the agent's.
//!
//! The transport runs on its own threads and the editor stays on the
//! render thread. The host calls [`Server::serve`] once per frame, which
//! answers the calls that arrived since the last one:
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use wxsl_mcp::{Frame, Root, Server};
//! # let (device, queue): (wgpu::Device, wgpu::Queue) = unimplemented!();
//! # let mut editor: wxsl_editor::Editor = unimplemented!();
//!
//! let mut server = Server::http(Root::new(".")?, 0, None)?;
//! eprintln!("MCP on {} (token {})", server.url().unwrap(), server.token().unwrap());
//! // …then, after drawing each frame:
//! server.serve(
//!     &mut editor,
//!     &Frame { device: &device, queue: &queue, width: 1600, height: 900, time: 0.0 },
//! );
//! # Ok(())
//! # }
//! ```
//!
//! A host with no window blocks in [`Server::wait`] instead of drawing,
//! and stops when [`Server::is_closed`] says the client has gone.
//!
//! # What is where
//!
//! | Module | Holds |
//! |---|---|
//! | `protocol` | JSON-RPC and the MCP methods, the same for both transports |
//! | `tools` | the tools and resources, each a plan over commands and queries |
//! | `http` | the streamable HTTP transport on `std::net`: token, origin, `/mcp` |
//! | `stdio` | the stdio transport: one message per line |
//! | `reference` | the node reference, generated from the registries |
//! | `sandbox` | the project root every path is confined to |
//! | `image` | screenshots as scaled-down PNG |
//!
//! # Security
//!
//! The HTTP listener binds `127.0.0.1` only, and every request needs the
//! bearer token. A request whose `Origin` is not local is refused. Every
//! path a tool takes, including the paths inside raw commands, must
//! resolve under the [`Root`]. While a client is connected the editor's
//! status bar names it.

#![warn(missing_docs)]
#![warn(unnameable_types)]

mod http;
mod image;
mod protocol;
mod reference;
mod sandbox;
mod stdio;
mod tools;

use std::io::{self, BufRead, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use wxsl_editor::Editor;

use crate::protocol::{Dispatcher, Job, Shared};

pub use reference::node_reference;
pub use sandbox::Root;

/// How long a transport waits for the render thread to answer a call.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// What a tool needs from the host's frame: the device, and the size and
/// clock the editor draws at.
pub struct Frame<'a> {
    /// The device the editor was created on.
    pub device: &'a wgpu::Device,
    /// Its queue.
    pub queue: &'a wgpu::Queue,
    /// The editor's width in physical pixels, as its last
    /// `UiEvent::Resized` gave it. A screenshot is this wide.
    pub width: u32,
    /// Its height.
    pub height: u32,
    /// The application's clock in seconds, as [`Editor::frame`] takes it.
    pub time: f64,
}

/// An MCP server, attached to one editor.
pub struct Server {
    jobs: mpsc::Receiver<Job>,
    /// A job [`Server::wait`] took off the channel, not yet run.
    pending: Option<Job>,
    shared: Arc<Shared>,
    root: Root,
    address: Option<SocketAddr>,
    token: Option<String>,
}

impl Server {
    fn new(root: Root) -> (Self, Dispatcher) {
        let (sender, jobs) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let dispatcher = Dispatcher::new(sender, shared.clone(), root.clone(), ANSWER_TIMEOUT);
        let server = Server {
            jobs,
            pending: None,
            shared,
            root,
            address: None,
            token: None,
        };
        (server, dispatcher)
    }

    /// Serve over HTTP on `127.0.0.1:port`, or a free port with 0. With no
    /// `token`, a random one is made; clients send it as
    /// `Authorization: Bearer <token>`.
    pub fn http(root: Root, port: u16, token: Option<String>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let (mut server, dispatcher) = Server::new(root);
        let token = token.unwrap_or_else(random_token);
        server.address = Some(listener.local_addr()?);
        server.token = Some(token.clone());
        std::thread::Builder::new()
            .name("wxsl-mcp http".into())
            .spawn(move || http::listen(listener, dispatcher, token))?;
        Ok(server)
    }

    /// Serve over this process's standard input and output. Nothing else
    /// may write to standard output.
    pub fn stdio(root: Root) -> io::Result<Self> {
        Server::lines(root, io::BufReader::new(io::stdin()), io::stdout())
    }

    /// Serve one message per line over any pair of streams: stdio, a
    /// pipe, or a test's channel.
    pub fn lines(
        root: Root,
        input: impl BufRead + Send + 'static,
        output: impl Write + Send + 'static,
    ) -> io::Result<Self> {
        let (server, dispatcher) = Server::new(root);
        std::thread::Builder::new()
            .name("wxsl-mcp stdio".into())
            .spawn(move || stdio::serve(dispatcher, input, output))?;
        Ok(server)
    }

    /// Where the HTTP transport listens.
    pub fn address(&self) -> Option<SocketAddr> {
        self.address
    }

    /// The endpoint a client connects to, for the HTTP transport.
    pub fn url(&self) -> Option<String> {
        self.address.map(|address| format!("http://{address}/mcp"))
    }

    /// The bearer token the HTTP transport requires.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The root paths are confined to.
    pub fn root(&self) -> &Root {
        &self.root
    }

    /// Answer every call that arrived since the last time, against
    /// `editor`, on this thread. Also shows or clears the agent in the
    /// editor's status bar. Returns how many calls ran.
    pub fn serve(&mut self, editor: &mut Editor, frame: &Frame<'_>) -> usize {
        let agent = self.shared.agent.lock().expect("never poisoned").clone();
        if editor.agent() != agent.as_deref() {
            editor.set_agent(agent);
        }
        let mut count = 0;
        while let Some(job) = self.pending.take().or_else(|| self.jobs.try_recv().ok()) {
            let reply = tools::run(job.action, editor, frame, &self.root);
            // The transport may have timed out and gone; nothing to tell.
            let _ = job.reply.send(reply);
            count += 1;
        }
        count
    }

    /// Block until a call arrives or `timeout` passes. Returns whether one
    /// is waiting for [`Server::serve`].
    pub fn wait(&mut self, timeout: Duration) -> bool {
        if self.pending.is_none() {
            self.pending = self.jobs.recv_timeout(timeout).ok();
        }
        self.pending.is_some()
    }

    /// Whether the transport has ended, the stdio client having closed
    /// its input. HTTP never ends.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::SeqCst) && self.pending.is_none()
    }
}

/// A fresh random token: 128 bits from the standard library's per-process
/// random hash keys, mixed with the clock.
pub(crate) fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    (0..2u64)
        .map(|round| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(round);
            hasher.write_u128(nanos);
            hasher.write_u32(std::process::id());
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_differ() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
