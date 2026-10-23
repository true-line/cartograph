//! Bounded local protocol framing, admission and capability handshake primitives.
//! No listener, filesystem access, app startup or context implementation.
//!
//! Caps count bytes before LF, including the optional CR in CRLF. A clean EOF
//! returns `None`; an empty line or unterminated final frame is an error. Treat
//! a read error as terminal: the reader need not consume the offending frame.
//! Callers bound their own buffered reader and choose all policy limits.

use std::io::{self, BufRead, Write};
use std::num::NonZeroUsize;

use serde_json::Value;

pub mod admission;
pub mod capability;
pub mod tools;

/// Source-free framing failures. Underlying errors and payloads are not exposed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    /// Input or output failed.
    #[error("frame I/O failed")]
    Io,
    /// The configured byte cap was exceeded.
    #[error("frame exceeds byte limit")]
    Limit,
    /// A nonempty final frame had no LF delimiter.
    #[error("unterminated frame")]
    Unterminated,
    /// The frame was not exactly one valid UTF-8 JSON value.
    #[error("invalid JSON frame")]
    InvalidJson,
}

/// Read exactly one LF-delimited JSON value, preserving serde's recursion cap.
/// JSON-RPC envelope validation belongs to the caller.
pub fn read_frame(
    reader: &mut impl BufRead,
    cap: NonZeroUsize,
) -> Result<Option<Value>, FrameError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| FrameError::Io)?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(FrameError::Unterminated)
            };
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(available.len());
        if count > cap.get() - bytes.len() {
            return Err(FrameError::Limit);
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| FrameError::InvalidJson);
        }
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    cap: usize,
    limited: bool,
}

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.cap - self.bytes.len() {
            self.limited = true;
            return Err(io::Error::other("frame limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serialize within the cap before writing anything, then append exactly one LF.
/// Output I/O failures can leave partial output; limit failures cannot.
pub fn write_frame(
    writer: &mut impl Write,
    value: &Value,
    cap: NonZeroUsize,
) -> Result<(), FrameError> {
    let mut bounded = BoundedBytes {
        bytes: Vec::new(),
        cap: cap.get(),
        limited: false,
    };
    if serde_json::to_writer(&mut bounded, value).is_err() {
        return Err(if bounded.limited {
            FrameError::Limit
        } else {
            FrameError::InvalidJson
        });
    }
    writer
        .write_all(&bounded.bytes)
        .and_then(|()| writer.write_all(b"\n"))
        .map_err(|_| FrameError::Io)
}

#[cfg(test)]
mod tests;
