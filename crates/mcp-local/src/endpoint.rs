//! Fresh owner-private Unix endpoints. No app startup or store access.
use mcp_wire::capability::{Capability, LOCAL_PROTOCOL};
use std::fs::{self, Metadata, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

/// Source-free endpoint failure; paths and capabilities never enter diagnostics.
#[derive(Debug, thiserror::Error)]
#[error("local endpoint unavailable or invalid")]
pub struct EndpointError;

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
fn metadata(path: &Path) -> Result<Metadata, EndpointError> {
    fs::symlink_metadata(path).map_err(|_| EndpointError)
}
fn owned(metadata: &Metadata) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o077 == 0
}
fn secure_parent(path: &Path) -> Result<PathBuf, EndpointError> {
    let before = metadata(path)?;
    if !before.is_dir()
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.mode() & 0o022 != 0
    {
        return Err(EndpointError);
    }
    let canonical = fs::canonicalize(path).map_err(|_| EndpointError)?;
    if !Identity::of(&before).matches(&metadata(&canonical)?) {
        return Err(EndpointError);
    }
    Ok(canonical)
}

/// One activation's listener. Host owns cancellation of pending accept/reads.
/// Cleanup never replaces an existing endpoint or recursively removes a path.
pub struct Endpoint {
    listener: UnixListener,
    directory: PathBuf,
    directory_identity: Identity,
    socket_identity: Identity,
    record_identity: Option<Identity>,
}
impl Endpoint {
    /// Bind under a trusted existing app runtime parent. Requires a Tokio runtime.
    pub fn bind(parent: &Path) -> Result<Self, EndpointError> {
        let parent = secure_parent(parent)?;
        let directory = tempfile::Builder::new()
            .prefix("mcp-")
            .tempdir_in(parent)
            .map_err(|_| EndpointError)?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| EndpointError)?;
        let directory_metadata = metadata(directory.path())?;
        if !directory_metadata.is_dir() || !owned(&directory_metadata) {
            return Err(EndpointError);
        }
        let path = directory.path().join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).map_err(|_| EndpointError)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|_| EndpointError)?;
        let socket_metadata = metadata(&path)?;
        if !socket_metadata.file_type().is_socket() || !owned(&socket_metadata) {
            return Err(EndpointError);
        }
        listener.set_nonblocking(true).map_err(|_| EndpointError)?;
        let listener = UnixListener::from_std(listener).map_err(|_| EndpointError)?;
        Ok(Self {
            listener,
            directory: directory.keep(),
            directory_identity: Identity::of(&directory_metadata),
            socket_identity: Identity::of(&socket_metadata),
            record_identity: None,
        })
    }
    /// Socket path for explicit client configuration; this is not a credential.
    pub fn socket_path(&self) -> PathBuf {
        self.directory.join("socket")
    }

    fn verify(&self) -> Result<(), EndpointError> {
        let directory = metadata(&self.directory)?;
        let socket = metadata(&self.socket_path())?;
        if !directory.is_dir()
            || !owned(&directory)
            || !self.directory_identity.matches(&directory)
            || !socket.file_type().is_socket()
            || !owned(&socket)
            || !self.socket_identity.matches(&socket)
        {
            return Err(EndpointError);
        }
        Ok(())
    }
    /// Publish one private record after listening. Repeated publication fails.
    /// No stable-profile discovery or client configuration is created here.
    pub fn publish(&mut self, capability: &Capability) -> Result<PathBuf, EndpointError> {
        self.verify()?;
        let record = self.directory.join("connection.json");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&record)
            .map_err(|_| EndpointError)?;
        let identity = Identity::of(&file.metadata().map_err(|_| EndpointError)?);
        // Record ownership immediately so failed writes can clean up our file.
        self.record_identity = Some(identity);
        let value = serde_json::json!({"protocol": LOCAL_PROTOCOL, "socket":self.socket_path(), "capability":capability.expose_for_local_transport()});
        serde_json::to_writer(&mut file, &value).map_err(|_| EndpointError)?;
        file.flush()
            .and_then(|()| file.sync_all())
            .map_err(|_| EndpointError)?;
        let current = metadata(&record)?;
        if !current.is_file()
            || !owned(&current)
            || current.nlink() != 1
            || !identity.matches(&current)
        {
            return Err(EndpointError);
        }
        self.verify()?;
        Ok(record)
    }
    /// Accept only peers with this process's effective UID. The capability
    /// handshake is still mandatory after this native identity check.
    pub async fn accept(&self) -> Result<UnixStream, EndpointError> {
        self.verify()?;
        let (stream, _) = self.listener.accept().await.map_err(|_| EndpointError)?;
        self.verify()?;
        if stream.peer_cred().map_err(|_| EndpointError)?.uid()
            != rustix::process::geteuid().as_raw()
        {
            return Err(EndpointError);
        }
        Ok(stream)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        // Only our original directory and named entries can be removed. Unknown
        // or replaced entries are left for explicit diagnosis, never followed.
        let Ok(directory) = metadata(&self.directory) else {
            return;
        };
        if !directory.is_dir() || !self.directory_identity.matches(&directory) {
            return;
        }
        for (name, identity) in [
            ("connection.json", self.record_identity),
            ("socket", Some(self.socket_identity)),
        ] {
            let path = self.directory.join(name);
            if let (Some(identity), Ok(current)) = (identity, metadata(&path))
                && identity.matches(&current)
            {
                let _ = fs::remove_file(path);
            }
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    #[test]
    fn private_endpoint_publishes_once_and_cleans_owned_entries() {
        // #718 / AC-0289 native endpoint foundation; not full client acceptance.
        runtime().block_on(async {
            let parent = tempfile::tempdir().unwrap();
            let mut endpoint = Endpoint::bind(parent.path()).unwrap();
            let socket = endpoint.socket_path();
            assert_eq!(metadata(&socket).unwrap().mode() & 0o777, 0o600);
            let secret = Capability::generate().unwrap();
            let record = endpoint.publish(&secret).unwrap();
            assert_eq!(metadata(&record).unwrap().mode() & 0o777, 0o600);
            assert!(endpoint.publish(&secret).is_err());
            let client = UnixStream::connect(&socket).await.unwrap();
            let server = endpoint.accept().await.unwrap();
            drop((client, server, endpoint));
            assert!(!socket.exists());
            assert!(!record.exists());
        });
    }
    #[test]
    fn refuses_writable_parent_and_replaced_socket_without_deleting_replacement() {
        runtime().block_on(async {
            let parent = tempfile::tempdir().unwrap();
            fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o777)).unwrap();
            assert!(Endpoint::bind(parent.path()).is_err());
            fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let mut endpoint = Endpoint::bind(parent.path()).unwrap();
            let socket = endpoint.socket_path();
            // Keep original inode allocated to make replacement distinct.
            fs::rename(&socket, socket.with_extension("original")).unwrap();
            fs::write(&socket, b"replacement").unwrap();
            assert!(endpoint.publish(&Capability::generate().unwrap()).is_err());
            drop(endpoint);
            assert_eq!(fs::read(socket).unwrap(), b"replacement");
        });
    }
}
