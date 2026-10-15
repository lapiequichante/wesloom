//! MCP's stdio transport: one JSON-RPC message per line each way.

use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;

use crate::protocol::Dispatcher;

/// Answer every line of `input` on `output`, in order, until `input`
/// ends; then the session is over.
pub(crate) fn serve(dispatcher: Dispatcher, input: impl BufRead, mut output: impl Write) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(answer) = dispatcher.handle_text(&line) {
            if writeln!(output, "{answer}")
                .and_then(|()| output.flush())
                .is_err()
            {
                break;
            }
        }
    }
    dispatcher.disconnect();
    dispatcher.shared().closed.store(true, Ordering::SeqCst);
}
