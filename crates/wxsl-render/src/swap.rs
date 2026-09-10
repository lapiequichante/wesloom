//! Swapping pipeline without dropping a frame.
//!
//! Swapping a pipeline re-derives everything declarative at once — the
//! stage set, the target descriptors, the pass order — because all of that
//! is only data. What is not free is compiling the stages the new pipeline
//! wants and the old one did not, and `wgpu` exposes no asynchronous
//! pipeline creation to hide it behind.
//!
//! So the compile happens on a worker thread while the *previous* pipeline
//! keeps presenting, and the swap lands in a single frame once every
//! variant is ready ([ADR 0022](../../../docs/adr/0022-material-stages-replace-the-render-path-enum.md)).
//! [`SwapProgress`] is what a `compiling 3/7` indicator reads.
//!
//! ```text
//!   request ──> worker thread: WXSL -> WGSL, no device
//!      │                              │
//!   old pipeline keeps drawing        │ results, one by one
//!      │                              v
//!      └──────────── poll ──> shader modules ──> swap, in one frame
//! ```
//!
//! # What is on which thread
//!
//! Only the WXSL-to-WGSL half runs on the worker: it is a pure function
//! over text and needs no device, so it cannot be affected by anything the
//! render thread is doing. Turning the WGSL into a `wgpu` module happens
//! on the render thread as each result arrives, one per poll rather than
//! all at the end, so the cost is spread over the frames the swap takes
//! anyway.
//!
//! On a single-threaded target there is no worker: the requests are
//! compiled where they are made and the swap lands on the next poll. That
//! is a blocking swap, which is exactly what the indicator is for.

use std::sync::mpsc::{self, Receiver, TryRecvError};

use crate::error::RenderError;
use crate::graph::{RenderGraph, Schedule};
use crate::library::ShaderLibrary;
use crate::pipeline::StockPipeline;
use crate::variants::{LightingRequest, MaterialRequest, VariantKey};

/// One shader a swap is waiting on.
#[derive(Clone, Debug)]
pub enum Request {
    /// A material compiled for one stage.
    Material(MaterialRequest),
    /// The deferred lighting pass.
    Lighting(LightingRequest),
}

impl Request {
    /// The cache key the result belongs under.
    pub fn key(&self) -> VariantKey {
        match self {
            Request::Material(request) => request.key,
            Request::Lighting(request) => request.key,
        }
    }

    fn compile(&self, library: &ShaderLibrary) -> Compiled {
        let (label, wgsl) = match self {
            Request::Material(request) => request.compile(library),
            Request::Lighting(request) => request.compile(library),
        };
        Compiled {
            key: self.key(),
            label,
            wgsl,
        }
    }
}

/// One finished compile, on its way back to the render thread.
pub struct Compiled {
    /// Where it belongs in the variant cache.
    pub key: VariantKey,
    /// Label for the `wgpu` module.
    pub label: String,
    /// The WGSL, or why there is none.
    pub wgsl: Result<String, RenderError>,
}

/// How far along a swap is, for an indicator to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapProgress {
    /// Variants compiled so far.
    pub done: usize,
    /// Variants the swap is waiting on in total.
    pub total: usize,
}

impl SwapProgress {
    /// Whether every variant is ready.
    pub fn is_complete(&self) -> bool {
        self.done >= self.total
    }

    /// Progress in `0.0..=1.0`. A swap needing nothing is complete, not
    /// a division by zero.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        self.done as f32 / self.total as f32
    }
}

impl core::fmt::Display for SwapProgress {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "compiling {}/{}", self.done, self.total)
    }
}

/// A pipeline swap in flight.
pub struct PipelineSwap {
    /// The pass list to adopt once everything is ready.
    pub graph: RenderGraph,
    /// Its schedule, computed when the swap was requested — the free half.
    pub schedule: Schedule,
    /// Which stock pipeline this is, if it is one.
    pub pipeline: Option<StockPipeline>,
    results: Receiver<Compiled>,
    done: usize,
    total: usize,
    /// Whether the worker has dropped its end of the channel. Recorded
    /// when `drain` sees it rather than probed on demand: `try_recv` takes
    /// a message when there is one, so a method that asks "are you
    /// disconnected?" would eat a compiled variant to find out.
    disconnected: bool,
}

impl PipelineSwap {
    /// Start compiling `requests` for `graph`, off the render thread where
    /// the platform has one.
    pub fn start(
        graph: RenderGraph,
        schedule: Schedule,
        pipeline: Option<StockPipeline>,
        library: &ShaderLibrary,
        requests: Vec<Request>,
    ) -> Self {
        let total = requests.len();
        let results = spawn(library.clone(), requests);
        PipelineSwap {
            graph,
            schedule,
            pipeline,
            results,
            done: 0,
            total,
            disconnected: false,
        }
    }

    /// How far along it is.
    pub fn progress(&self) -> SwapProgress {
        SwapProgress {
            done: self.done,
            total: self.total,
        }
    }

    /// Take whatever the worker has finished since the last call.
    ///
    /// Never blocks: a swap that is not ready yet simply returns fewer
    /// results, and the caller draws another frame of the old pipeline.
    pub fn drain(&mut self) -> Vec<Compiled> {
        let mut ready = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(compiled) => {
                    self.done += 1;
                    ready.push(compiled);
                }
                Err(TryRecvError::Empty) => break,
                // The worker finished and dropped its end. Everything it
                // sent has just been drained, so this is ordinarily the
                // last poll of a complete swap; if the thread died
                // mid-way, `done < total` keeps the swap pending rather
                // than adopting a half-compiled pipeline, and
                // `is_stalled` is how a caller notices.
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    break;
                }
            }
        }
        ready
    }

    /// Whether every variant has arrived.
    pub fn is_complete(&self) -> bool {
        self.done >= self.total
    }

    /// Whether the worker is gone and the swap will never complete.
    ///
    /// Only reachable if the worker thread panicked, which means a bug in
    /// the compiler rather than in the shader — but a swap that silently
    /// hangs forever behind a progress indicator is worse than one that
    /// says so.
    pub fn is_stalled(&self) -> bool {
        self.disconnected && !self.is_complete()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn(library: ShaderLibrary, requests: Vec<Request>) -> Receiver<Compiled> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for request in requests {
            // A closed channel means the swap was abandoned — a second
            // swap requested before this one landed, or the renderer
            // dropped. Stop rather than compile shaders nobody wants.
            if sender.send(request.compile(&library)).is_err() {
                return;
            }
        }
    });
    receiver
}

/// The single-threaded fallback: compile now, report progress once.
#[cfg(target_arch = "wasm32")]
fn spawn(library: ShaderLibrary, requests: Vec<Request>) -> Receiver<Compiled> {
    let (sender, receiver) = mpsc::channel();
    for request in requests {
        if sender.send(request.compile(&library)).is_err() {
            break;
        }
    }
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_of_a_swap_that_needs_nothing_is_complete() {
        // Swapping to a pipeline whose every stage is already cached must
        // land immediately, not show `compiling 0/0` forever.
        let progress = SwapProgress { done: 0, total: 0 };
        assert!(progress.is_complete());
        assert_eq!(progress.fraction(), 1.0);
        assert_eq!(progress.to_string(), "compiling 0/0");
    }

    #[test]
    fn progress_reads_the_way_an_indicator_wants_it() {
        let progress = SwapProgress { done: 3, total: 7 };
        assert!(!progress.is_complete());
        assert_eq!(progress.to_string(), "compiling 3/7");
        assert!((progress.fraction() - 3.0 / 7.0).abs() < 1e-6);
    }
}
