//! Same-account Unix streams with bounded frame-wide deadlines.
//! Peer verification precedes any caller authentication exchange. The caller
//! must still authenticate capabilities before admitting protocol operations.

use std::{num::NonZeroUsize, path::Path, time::Duration};

use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

/// Source-free transport errors. Any operation failure closes the connection.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConnectionError {
    /// A zero deadline was supplied.
    #[error("deadline must be nonzero")]
    InvalidDeadline,
    /// Connected peer is not verifiably the current account.
    #[error("peer account verification failed")]
    Peer,
    /// A frame or connection operation exceeded its total deadline.
    #[error("local transport deadline exceeded")]
    Deadline,
    /// Transport I/O failed or the connection was closed.
    #[error("local transport I/O failed")]
    Io,
    /// Frame content or size was invalid.
    #[error("{0}")]
    Frame(mcp_wire::FrameError),
}

/// A verified native connection. Explicit shutdown, EOF and failures drop its
/// socket. Cancelling a polled read/write also drops its socket rather than
/// allowing reuse after partial input consumption or output emission.
pub struct Connection {
    stream: Option<BufReader<UnixStream>>,
    read_cap: NonZeroUsize,
    write_cap: NonZeroUsize,
    deadline: Duration,
}

impl Connection {
    /// Verify an accepted or already connected stream before any application I/O.
    pub fn from_stream(
        stream: UnixStream,
        read_cap: NonZeroUsize,
        write_cap: NonZeroUsize,
        deadline: Duration,
    ) -> Result<Self, ConnectionError> {
        if deadline.is_zero() {
            return Err(ConnectionError::InvalidDeadline);
        }
        let peer = stream.peer_cred().map_err(|_| ConnectionError::Peer)?;
        if peer.uid() != rustix::process::geteuid().as_raw() {
            return Err(ConnectionError::Peer);
        }
        Ok(Self {
            stream: Some(BufReader::with_capacity(
                read_cap.get().saturating_add(1).min(8192),
                stream,
            )),
            read_cap,
            write_cap,
            deadline,
        })
    }

    /// Connect within the deadline and verify the server account. Never launches
    /// an application or authenticates implicitly.
    pub async fn connect(
        path: &Path,
        read_cap: NonZeroUsize,
        write_cap: NonZeroUsize,
        deadline: Duration,
    ) -> Result<Self, ConnectionError> {
        if deadline.is_zero() {
            return Err(ConnectionError::InvalidDeadline);
        }
        let stream = timeout(deadline, UnixStream::connect(path))
            .await
            .map_err(|_| ConnectionError::Deadline)?
            .map_err(|_| ConnectionError::Io)?;
        Self::from_stream(stream, read_cap, write_cap, deadline)
    }

    /// Read one bounded line; deadline spans all fragments rather than each read.
    /// Uses the framing contract from `mcp-wire`, including CR accounting and EOF.
    pub async fn read(&mut self) -> Result<Option<Value>, ConnectionError> {
        let mut stream = self.stream.take().ok_or(ConnectionError::Io)?;
        let result = timeout(self.deadline, read_line(&mut stream, self.read_cap))
            .await
            .unwrap_or(Err(ConnectionError::Deadline));
        if matches!(result, Ok(Some(_))) {
            self.stream = Some(stream);
        }
        result
    }

    /// Serialize within the cap before output, then write the entire frame under
    /// one deadline. A partial I/O failure invalidates the connection.
    pub async fn write(&mut self, value: &Value) -> Result<(), ConnectionError> {
        let mut stream = self.stream.take().ok_or(ConnectionError::Io)?;
        let mut bytes = Vec::new();
        if let Err(error) = mcp_wire::write_frame(&mut bytes, value, self.write_cap) {
            return Err(ConnectionError::Frame(error));
        }
        let result = timeout(self.deadline, stream.get_mut().write_all(&bytes))
            .await
            .map_err(|_| ConnectionError::Deadline)
            .and_then(|result| result.map_err(|_| ConnectionError::Io));
        if result.is_ok() {
            self.stream = Some(stream);
        }
        result
    }

    /// Consume and immediately drop the socket; no drain or application launch.
    pub fn shutdown(self) {
        drop(self);
    }

    /// Transfer the verified connection to a full-duplex bridge without losing
    /// any bytes already buffered after authentication. Caller owns shutdown.
    pub fn into_stream(mut self) -> Result<BufReader<UnixStream>, ConnectionError> {
        self.stream.take().ok_or(ConnectionError::Io)
    }
}

/// Read a bounded JSON frame under one total deadline. On cancellation/error,
/// discard the reader; some frame bytes may already have been consumed.
pub async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    cap: NonZeroUsize,
    deadline: Duration,
) -> Result<Option<Value>, ConnectionError> {
    if deadline.is_zero() {
        return Err(ConnectionError::InvalidDeadline);
    }
    timeout(deadline, read_line(reader, cap))
        .await
        .unwrap_or(Err(ConnectionError::Deadline))
}

/// Bound serialization before output, then write and flush under one deadline.
/// On cancellation/error discard the writer, since output may be partial.
pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &Value,
    cap: NonZeroUsize,
    deadline: Duration,
) -> Result<(), ConnectionError> {
    if deadline.is_zero() {
        return Err(ConnectionError::InvalidDeadline);
    }
    let mut bytes = Vec::new();
    mcp_wire::write_frame(&mut bytes, value, cap).map_err(ConnectionError::Frame)?;
    timeout(deadline, async {
        writer
            .write_all(&bytes)
            .await
            .map_err(|_| ConnectionError::Io)?;
        writer.flush().await.map_err(|_| ConnectionError::Io)
    })
    .await
    .unwrap_or(Err(ConnectionError::Deadline))
}

async fn read_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    cap: NonZeroUsize,
) -> Result<Option<Value>, ConnectionError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|_| ConnectionError::Io)?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(ConnectionError::Frame(mcp_wire::FrameError::Unterminated))
            };
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let count = newline.unwrap_or(available.len());
        if count > cap.get() - bytes.len() {
            return Err(ConnectionError::Frame(mcp_wire::FrameError::Limit));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| ConnectionError::Frame(mcp_wire::FrameError::InvalidJson));
        }
    }
}

#[cfg(test)]
mod tests;
