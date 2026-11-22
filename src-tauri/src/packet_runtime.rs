//! The work packet store the app state holds, split out of `work_packets`
//! so that state compiles in app-store (#1924). The app's `work_packets`
//! module re-exports it.

pub struct PacketRuntime {
    pub store: std::sync::Mutex<::work_packets::PacketStore>,
}
impl PacketRuntime {
    pub fn open(path: &std::path::Path) -> ::work_packets::PacketResult<Self> {
        Ok(Self {
            store: std::sync::Mutex::new(::work_packets::PacketStore::open(path)?),
        })
    }
}
