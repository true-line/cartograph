//! #718 private-record foundation only, not packaged-client acceptance.
use super::*;
use crate::endpoint::Endpoint;
use mcp_wire::capability::Capability;
use std::os::unix::fs::{PermissionsExt, symlink};

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(test);
}
fn fixture() -> (tempfile::TempDir, Endpoint, PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let mut endpoint = Endpoint::bind(parent.path()).unwrap();
    let path = endpoint.publish(&Capability::generate().unwrap()).unwrap();
    (parent, endpoint, path)
}
fn replace(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn reads_real_private_publication_and_connects() {
    run(async {
        let (_parent, endpoint, path) = fixture();
        let record = ConnectionRecord::read(&path).unwrap();
        let preamble = record.authentication_preamble();
        assert_eq!(preamble["protocol"], LOCAL_PROTOCOL);
        assert_eq!(preamble["capability"].as_str().unwrap().len(), 64);
        let cap = NonZeroUsize::new(512).unwrap();
        let mut connection = record
            .connect(cap, cap, Duration::from_secs(1))
            .await
            .unwrap();
        let server = endpoint.accept().await.unwrap();
        let mut server = Connection::from_stream(server, cap, cap, Duration::from_secs(1)).unwrap();
        connection.write(&preamble).await.unwrap();
        assert_eq!(server.read().await.unwrap(), Some(preamble));
    });
}

#[test]
fn rejects_symlink_record_and_parent() {
    run(async {
        let (_parent, _endpoint, path) = fixture();
        let original = path.with_extension("original");
        fs::rename(&path, &original).unwrap();
        symlink(&original, &path).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&original, &path).unwrap();
        let alias = tempfile::tempdir().unwrap();
        symlink(path.parent().unwrap(), alias.path().join("alias")).unwrap();
        assert!(ConnectionRecord::read(&alias.path().join("alias/connection.json")).is_err());
        assert!(ConnectionRecord::read(Path::new("connection.json")).is_err());
    });
}

#[test]
fn rejects_public_modes_hardlinks_and_oversize() {
    run(async {
        let (_parent, _endpoint, path) = fixture();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
        fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        let link = path.with_extension("link");
        fs::hard_link(&path, &link).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
        fs::remove_file(link).unwrap();
        fs::write(&path, vec![b'x'; 4097]).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
    });
}

#[test]
fn rejects_nonregular_fifo_without_opening_or_blocking() {
    run(async {
        let (_parent, _endpoint, path) = fixture();
        fs::remove_file(&path).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .args(["-m", "600"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(ConnectionRecord::read(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
    });
}

#[test]
fn rejects_malformed_protocol_field_set_token_and_path() {
    run(async {
        let (_parent, _endpoint, path) = fixture();
        let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mut cases = vec![json!({}), json!([])];
        for (key, value) in [
            ("protocol", json!("other")),
            ("extra", json!(true)),
            ("socket", json!("/tmp/socket")),
            (
                "socket",
                json!(format!("{}/../socket", path.parent().unwrap().display())),
            ),
            ("capability", json!("A".repeat(64))),
            ("capability", json!("a".repeat(63))),
            ("capability", json!(false)),
        ] {
            let mut value_case = original.clone();
            value_case[key] = value;
            cases.push(value_case);
        }
        for value in cases {
            replace(&path, &value);
            assert!(ConnectionRecord::read(&path).is_err());
        }
        fs::write(&path, b"{not-json").unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
        let raw = String::from_utf8(serde_json::to_vec(&original).unwrap()).unwrap();
        let duplicate = format!("{{\"protocol\":\"{LOCAL_PROTOCOL}\",{}", &raw[1..]);
        fs::write(&path, duplicate).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
    });
}

#[test]
fn rejects_changed_socket_record_and_directory_before_connect() {
    run(async {
        let (_parent, endpoint, path) = fixture();
        let record = ConnectionRecord::read(&path).unwrap();
        let socket = endpoint.socket_path();
        fs::rename(&socket, socket.with_extension("old")).unwrap();
        let _replacement = tokio::net::UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let cap = NonZeroUsize::new(512).unwrap();
        assert!(
            record
                .connect(cap, cap, Duration::from_secs(1))
                .await
                .is_err()
        );
        assert!(ConnectionRecord::read(&path).is_ok());
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(ConnectionRecord::read(&path).is_err());
    });
}
