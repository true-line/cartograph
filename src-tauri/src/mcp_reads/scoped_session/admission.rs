use super::*;
use std::io::{self, Write};
pub(super) fn fits(value: &impl Serialize, limit: usize) -> Result<(), ReadError> {
    if limit == 0 || limit > context_hub::MAX_RESPONSE_BYTES {
        return Err(ReadError::Limit);
    }
    struct Counter {
        remaining: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("response limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Counter { remaining: limit }, value).map_err(|_| ReadError::Limit)
}
