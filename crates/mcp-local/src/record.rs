//! Read explicit private endpoint records without following final symlinks.
//! This module performs no discovery, writes, application launch or handshake.

use crate::connection::Connection;
use mcp_wire::capability::LOCAL_PROTOCOL;
use rustix::fs::{Mode, OFlags};
use serde_json::{Value, json};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    protocol: String,
    socket: String,
    capability: String,
}
use std::{
    fs::{self, File, Metadata},
    io::Read,
    num::NonZeroUsize,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// Source-free validation or connection failure.
#[derive(Debug, thiserror::Error)]
#[error("local connection record unavailable or invalid")]
pub struct RecordError;

#[derive(Clone, Copy)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
    fn matches(self, metadata: &Metadata) -> bool {
        self.device == metadata.dev() && self.inode == metadata.ino()
    }
}
fn metadata(path: &Path) -> Result<Metadata, RecordError> {
    fs::symlink_metadata(path).map_err(|_| RecordError)
}
fn owned_mode(metadata: &Metadata, mode: u32) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o7777 == mode
}
fn private_record(metadata: &Metadata) -> bool {
    metadata.is_file()
        && owned_mode(metadata, 0o600)
        && metadata.nlink() == 1
        && metadata.len() <= 4096
}
fn private_socket(metadata: &Metadata) -> bool {
    metadata.file_type().is_socket() && owned_mode(metadata, 0o600)
}

/// Validated runtime credentials. Deliberately neither Debug nor Serialize.
pub struct ConnectionRecord {
    parent: PathBuf,
    record: PathBuf,
    socket: PathBuf,
    parent_identity: Identity,
    record_identity: Identity,
    socket_identity: Identity,
    capability: String,
}

impl ConnectionRecord {
    /// Read only an explicit absolute `connection.json` path. Final record and
    /// parent symlinks, special files, extra fields and escaped socket paths fail.
    pub fn read(path: &Path) -> Result<Self, RecordError> {
        if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new("connection.json"))
        {
            return Err(RecordError);
        }
        let parent_path = path.parent().ok_or(RecordError)?;
        let before_parent = metadata(parent_path)?;
        if !before_parent.is_dir() || !owned_mode(&before_parent, 0o700) {
            return Err(RecordError);
        }
        let parent = fs::canonicalize(parent_path).map_err(|_| RecordError)?;
        let parent_identity = Identity::of(&before_parent);
        let canonical_parent = metadata(&parent)?;
        if !parent_identity.matches(&canonical_parent) || !owned_mode(&canonical_parent, 0o700) {
            return Err(RecordError);
        }
        let record = parent.join("connection.json");
        let before = metadata(&record)?;
        if !private_record(&before) {
            return Err(RecordError);
        }
        let record_identity = Identity::of(&before);
        let fd = rustix::fs::open(
            &record,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| RecordError)?;
        let mut file = File::from(fd);
        let opened = file.metadata().map_err(|_| RecordError)?;
        if !private_record(&opened) || !record_identity.matches(&opened) {
            return Err(RecordError);
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| RecordError)?;
        if bytes.len() > 4096 {
            return Err(RecordError);
        }
        let after_open = file.metadata().map_err(|_| RecordError)?;
        let after_named = metadata(&record)?;
        if !private_record(&after_open)
            || !private_record(&after_named)
            || !record_identity.matches(&after_open)
            || !record_identity.matches(&after_named)
        {
            return Err(RecordError);
        }
        let value: WireRecord = serde_json::from_slice(&bytes).map_err(|_| RecordError)?;
        if value.protocol != LOCAL_PROTOCOL {
            return Err(RecordError);
        }
        let capability = &value.capability;
        if capability.len() != 64
            || !capability
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(RecordError);
        }
        let socket = parent.join("socket");
        if std::ffi::OsStr::new(&value.socket) != socket.as_os_str() {
            return Err(RecordError);
        }
        let socket_metadata = metadata(&socket)?;
        if !private_socket(&socket_metadata) {
            return Err(RecordError);
        }
        let result = Self {
            parent,
            record,
            socket,
            parent_identity,
            record_identity,
            socket_identity: Identity::of(&socket_metadata),
            capability: capability.to_owned(),
        };
        result.verify()?;
        // Also bracket the originally supplied parent spelling before returning.
        let final_parent = metadata(parent_path)?;
        if !final_parent.is_dir()
            || !owned_mode(&final_parent, 0o700)
            || !parent_identity.matches(&final_parent)
        {
            return Err(RecordError);
        }
        Ok(result)
    }

    fn verify(&self) -> Result<(), RecordError> {
        let parent = metadata(&self.parent)?;
        let record = metadata(&self.record)?;
        let socket = metadata(&self.socket)?;
        if !parent.is_dir()
            || !owned_mode(&parent, 0o700)
            || !self.parent_identity.matches(&parent)
            || !private_record(&record)
            || !self.record_identity.matches(&record)
            || !private_socket(&socket)
            || !self.socket_identity.matches(&socket)
        {
            return Err(RecordError);
        }
        Ok(())
    }

    /// Revalidate endpoint identities around the connection and native peer-UID
    /// check. A stale record never selects a replacement socket silently.
    pub async fn connect(
        &self,
        read_cap: NonZeroUsize,
        write_cap: NonZeroUsize,
        deadline: Duration,
    ) -> Result<Connection, RecordError> {
        self.verify()?;
        let connection = Connection::connect(&self.socket, read_cap, write_cap, deadline)
            .await
            .map_err(|_| RecordError)?;
        self.verify()?;
        Ok(connection)
    }

    /// Explicit credential exposure solely for the private authentication frame.
    /// Never log this value or place it into client configuration or arguments.
    pub fn authentication_preamble(&self) -> Value {
        json!({"protocol":LOCAL_PROTOCOL, "capability":self.capability})
    }
}

#[cfg(test)]
mod tests;
