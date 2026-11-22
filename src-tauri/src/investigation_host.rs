//! The investigation host's error and its bounded JSON encoding, split out
//! of `investigations` so the investigation runtime compiles in app-store
//! (#1924). The app's `investigations` module re-exports both.
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("Investigation context changed during acquisition.")]
    InputChanged,
    #[error("Investigation evidence is unavailable; no source was substituted.")]
    SourceUnavailable,
    #[error("Investigation input or source metadata is invalid.")]
    InvalidInput,
    #[error("Investigation limit exhausted.")]
    LimitExceeded,
    #[error("Investigation durable store capacity exhausted.")]
    StoreCapacity,
    #[error("Investigation storage or source ownership could not be verified.")]
    Operational,
}

impl From<agents::investigation::InvestigationError> for HostError {
    fn from(error: agents::investigation::InvestigationError) -> Self {
        match error {
            agents::investigation::InvestigationError::LimitExceeded => Self::LimitExceeded,
            _ => Self::InvalidInput,
        }
    }
}

pub fn bounded_json(value: &(impl Serialize + ?Sized), max: usize) -> Result<Vec<u8>, HostError> {
    struct Limited {
        bytes: Vec<u8>,
        max: usize,
    }
    impl std::io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("investigation byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Limited { bytes: vec![], max };
    serde_json::to_writer(&mut writer, value).map_err(|_| HostError::LimitExceeded)?;
    Ok(writer.bytes)
}
