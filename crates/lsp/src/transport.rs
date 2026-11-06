//! The stdio serve loop (issues 041/042): framed reads, dispatch,
//! framed writes, exit-code propagation. All policy lives in the
//! [`Handler`] implementations ([`crate::server::Server`] for grammar
//! authoring, [`crate::pack::server::PackServer`] for pack languages);
//! this module only moves bytes.

use std::io::{BufRead, Write};

use serde_json::Value;

use crate::rpc::{self, FrameError};
use crate::server::{Flow, Server};

/// A message handler behind the serve loop: one decoded frame in, the
/// optional outgoing frame and a flow directive out.
pub trait Handler {
    /// Handle one decoded message body.
    fn handle_message(&mut self, body: &[u8]) -> (Option<Value>, Flow);

    /// The process exit code for `exit`.
    fn exit_code(&self) -> i32;
}

impl Handler for Server {
    fn handle_message(&mut self, body: &[u8]) -> (Option<Value>, Flow) {
        Server::handle_message(self, body)
    }

    fn exit_code(&self) -> i32 {
        Server::exit_code(self)
    }
}

/// The default inbound message bound ([`rpc::MAX_MESSAGE`]).
pub const DEFAULT_MAX_MESSAGE: usize = rpc::MAX_MESSAGE;

/// Serve the grammar LSP session over `reader`/`writer` until `exit`
/// or EOF (the issue-041 entry point).
///
/// `max_message` bounds one framed message (see [`rpc::MAX_MESSAGE`]).
///
/// Returns the process exit code: the server's code on `exit`, `1` on
/// a broken stream.
pub fn serve(reader: &mut impl BufRead, writer: &mut impl Write, max_message: usize) -> i32 {
    let mut server = Server::new();
    serve_with(&mut server, reader, writer, max_message)
}

/// Serve any [`Handler`] over `reader`/`writer` (issue 042: the pack
/// server rides the same loop).
pub fn serve_with(
    handler: &mut dyn Handler,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    max_message: usize,
) -> i32 {
    loop {
        match rpc::read_frame(reader, max_message) {
            Ok(body) => {
                let (outgoing, flow) = handler.handle_message(&body);
                if let Some(value) = outgoing {
                    let Ok(bytes) = serde_json::to_vec(&value) else {
                        return 1;
                    };
                    if rpc::write_frame(writer, &bytes).is_err() {
                        return 1;
                    }
                }
                if flow == Flow::Exit {
                    return handler.exit_code();
                }
            }
            // The client hung up gracefully: a clean shutdown.
            Err(FrameError::Eof) => return 0,
            // Corrupt stream or oversized frame: answer once, give up.
            Err(_) => {
                let error = rpc::response_err(
                    &rpc::Id::Num(0),
                    rpc::PARSE_ERROR,
                    "malformed framed message",
                );
                if let Ok(bytes) = serde_json::to_vec(&error) {
                    let _ = rpc::write_frame(writer, &bytes);
                }
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufWriter;

    #[test]
    fn serve_exits_zero_on_clean_eof_without_exit_notification() {
        let mut output = Vec::new();
        {
            let mut writer = BufWriter::new(&mut output);
            let mut reader = &b""[..];
            assert_eq!(serve(&mut reader, &mut writer, 1024), 0);
        }
        assert_eq!(output.len(), 0);
    }

    #[test]
    fn serve_answers_parse_error_on_garbage_and_exits_one() {
        let mut output = Vec::new();
        {
            let mut writer = BufWriter::new(&mut output);
            let mut reader = &b"not a frame at all\n"[..];
            assert_eq!(serve(&mut reader, &mut writer, 1024), 1);
        }
        assert!(
            std::str::from_utf8(&output)
                .expect("utf8")
                .contains("malformed framed message")
        );
    }
}
