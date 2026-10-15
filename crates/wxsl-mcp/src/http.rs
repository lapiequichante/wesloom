//! MCP's streamable HTTP transport, the subset this server needs, on
//! `std::net` (ADR 0083).
//!
//! `POST /mcp` carries one JSON-RPC message and gets its answer as
//! `application/json`, or `202` for a notification. `DELETE /mcp` ends the
//! session. There is no event stream: `GET` is `405`. Every request needs
//! the bearer token, and a request from a page that is not local is
//! refused, so a web page in the user's browser cannot reach the editor.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::protocol::Dispatcher;

/// The largest body accepted: a big graph edit or a long function.
const MAX_BODY: usize = 32 << 20;
/// The largest request head accepted.
const MAX_HEAD: usize = 64 << 10;
/// How long an idle keep-alive connection is kept.
const IDLE: Duration = Duration::from_secs(300);

/// One request.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    /// Names lowercased.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Whether the client wants the connection closed after this.
    fn closes(&self) -> bool {
        self.header("connection")
            .is_some_and(|value| value.eq_ignore_ascii_case("close"))
    }
}

/// One response.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let reason = match self.status {
            200 => "OK",
            202 => "Accepted",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            415 => "Unsupported Media Type",
            _ => "Error",
        };
        let mut head = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", self.body.len()));
        out.write_all(head.as_bytes())?;
        out.write_all(&self.body)?;
        out.flush()
    }
}

/// Accept connections until the listener fails, one thread each.
pub(crate) fn listen(listener: TcpListener, dispatcher: Dispatcher, token: String) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let dispatcher = dispatcher.clone();
        let token = token.clone();
        std::thread::spawn(move || {
            let _ = connection(stream, &dispatcher, &token);
        });
    }
}

/// Serve one connection's requests, kept alive until the client closes.
fn connection(stream: TcpStream, dispatcher: &Dispatcher, token: &str) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    loop {
        let request = match read_request(&mut reader) {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => {
                let status = if error.kind() == io::ErrorKind::InvalidData {
                    if error.to_string().contains("too large") {
                        413
                    } else {
                        400
                    }
                } else {
                    return Err(error);
                };
                Response::new(status, error.to_string()).write(&mut writer)?;
                return Ok(());
            }
        };
        respond(&request, dispatcher, token).write(&mut writer)?;
        if request.closes() {
            return Ok(());
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Read one request, or `None` at a clean end of stream.
pub(crate) fn read_request(reader: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut head = 0;
    let mut line = String::new();
    let mut next_line = |reader: &mut dyn BufRead, line: &mut String| -> io::Result<usize> {
        line.clear();
        let read = reader.take(MAX_HEAD as u64).read_line(line)?;
        head += read;
        if head > MAX_HEAD {
            return Err(invalid("request head too large"));
        }
        Ok(read)
    };
    // Tolerate blank lines between requests.
    loop {
        if next_line(reader, &mut line)? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(invalid("malformed request line"));
    };
    let mut request = Request {
        method: method.to_string(),
        path: path.to_string(),
        ..Request::default()
    };
    loop {
        if next_line(reader, &mut line)? == 0 {
            return Err(invalid("the request head ended early"));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| invalid("malformed header"))?;
        request
            .headers
            .push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    if request
        .header("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        request.body = read_chunked(reader)?;
    } else if let Some(length) = request.header("content-length") {
        let length: usize = length
            .parse()
            .map_err(|_| invalid("malformed content-length"))?;
        if length > MAX_BODY {
            return Err(invalid("request body too large"));
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        request.body = body;
    }
    Ok(Some(request))
}

fn read_chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader.take(1024).read_line(&mut line)?;
        let size = line.trim().split(';').next().unwrap_or_default();
        let size = usize::from_str_radix(size, 16).map_err(|_| invalid("malformed chunk size"))?;
        if body.len() + size > MAX_BODY {
            return Err(invalid("request body too large"));
        }
        if size == 0 {
            // Trailers, up to the blank line.
            loop {
                line.clear();
                if reader.take(MAX_HEAD as u64).read_line(&mut line)? == 0 || line.trim().is_empty()
                {
                    return Ok(body);
                }
            }
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        line.clear();
        reader.take(2).read_line(&mut line)?;
    }
}

/// Whether an `Origin` names this machine.
fn local_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Compare without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |diff, (x, y)| diff | (x ^ y))
            == 0
}

/// Answer one request: the transport's whole policy, testable without a
/// socket.
pub(crate) fn respond(request: &Request, dispatcher: &Dispatcher, token: &str) -> Response {
    if request.path.split('?').next() != Some("/mcp") {
        return Response::new(404, "the MCP endpoint is /mcp");
    }
    if let Some(origin) = request.header("origin") {
        if !local_origin(origin) {
            return Response::new(403, "requests from other origins are refused");
        }
    }
    let authorized = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|given| same(given.trim(), token));
    if !authorized {
        let mut response = Response::new(401, "a bearer token is required");
        response
            .headers
            .push(("WWW-Authenticate", "Bearer".to_string()));
        return response;
    }
    match request.method.as_str() {
        "POST" => {
            if request
                .header("content-type")
                .is_some_and(|value| !value.starts_with("application/json"))
            {
                return Response::new(415, "the body is application/json");
            }
            let Ok(text) = std::str::from_utf8(&request.body) else {
                return Response::new(400, "the body is not UTF-8");
            };
            match dispatcher.handle_text(text) {
                Some(answer) => {
                    let mut response = Response::new(200, answer.to_string());
                    response
                        .headers
                        .push(("Content-Type", "application/json".to_string()));
                    if answer["result"]["serverInfo"].is_object() {
                        response
                            .headers
                            .push(("Mcp-Session-Id", crate::random_token()));
                    }
                    response
                }
                None => Response::new(202, Vec::new()),
            }
        }
        "DELETE" => {
            dispatcher.disconnect();
            Response::new(200, Vec::new())
        }
        _ => {
            let mut response = Response::new(405, "POST a JSON-RPC message, or DELETE the session");
            response.headers.push(("Allow", "POST, DELETE".to_string()));
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};

    use super::*;
    use crate::protocol::Shared;
    use crate::sandbox::Root;

    fn dispatcher() -> Dispatcher {
        let (jobs, receiver) = mpsc::channel();
        std::mem::forget(receiver);
        Dispatcher::new(
            jobs,
            Arc::new(Shared::default()),
            Root::new(std::env::temp_dir()).unwrap(),
            Duration::from_secs(1),
        )
    }

    fn post(body: &str, headers: &[(&str, &str)]) -> Request {
        Request {
            method: "POST".into(),
            path: "/mcp".into(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    const PING: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

    #[test]
    fn requests_parse_with_a_length_or_in_chunks_and_keep_alive() {
        let text = "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello\
                    POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x\r\nde\r\n0\r\n\r\n";
        let mut reader = BufReader::new(text.as_bytes());
        let first = read_request(&mut reader).unwrap().unwrap();
        assert_eq!(
            (first.method.as_str(), first.path.as_str()),
            ("POST", "/mcp")
        );
        assert_eq!(first.header("host"), Some("x"));
        assert_eq!(first.body, b"hello");
        let second = read_request(&mut reader).unwrap().unwrap();
        assert_eq!(second.body, b"abcde");
        assert_eq!(read_request(&mut reader).unwrap(), None);
    }

    #[test]
    fn oversized_and_malformed_requests_are_refused() {
        let huge = format!(
            "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(read_request(&mut BufReader::new(huge.as_bytes())).is_err());
        assert!(read_request(&mut BufReader::new("GARBAGE\r\n\r\n".as_bytes())).is_err());
    }

    #[test]
    fn the_token_and_the_origin_are_checked_before_anything_runs() {
        let dispatcher = dispatcher();
        let auth = ("authorization", "Bearer secret");
        assert_eq!(respond(&post(PING, &[]), &dispatcher, "secret").status, 401);
        assert_eq!(
            respond(
                &post(PING, &[("authorization", "Bearer wrong!")]),
                &dispatcher,
                "secret"
            )
            .status,
            401
        );
        assert_eq!(
            respond(
                &post(PING, &[auth, ("origin", "https://evil.example")]),
                &dispatcher,
                "secret"
            )
            .status,
            403
        );
        for origin in [
            "http://localhost:5173",
            "http://127.0.0.1",
            "http://[::1]:80",
        ] {
            assert_eq!(
                respond(
                    &post(PING, &[auth, ("origin", origin)]),
                    &dispatcher,
                    "secret"
                )
                .status,
                200,
                "{origin}"
            );
        }
        let answer = respond(&post(PING, &[auth]), &dispatcher, "secret");
        assert_eq!(answer.status, 200);
        assert!(String::from_utf8(answer.body)
            .unwrap()
            .contains("\"result\""));
    }

    #[test]
    fn notifications_are_accepted_and_other_methods_and_paths_refused() {
        let dispatcher = dispatcher();
        let auth = ("authorization", "Bearer t");
        let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(
            respond(&post(notification, &[auth]), &dispatcher, "t").status,
            202
        );
        let mut get = post("", &[auth]);
        get.method = "GET".into();
        assert_eq!(respond(&get, &dispatcher, "t").status, 405);
        let mut elsewhere = post(PING, &[auth]);
        elsewhere.path = "/".into();
        assert_eq!(respond(&elsewhere, &dispatcher, "t").status, 404);
        assert_eq!(
            respond(
                &post(PING, &[auth, ("content-type", "text/plain")]),
                &dispatcher,
                "t"
            )
            .status,
            415
        );
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let answer = respond(&post(initialize, &[auth]), &dispatcher, "t");
        assert!(answer
            .headers
            .iter()
            .any(|(name, _)| *name == "Mcp-Session-Id"));
    }
}
