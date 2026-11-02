//! Local model-exchange debug log (#1624).
//!
//! When the host installs a sink, every model completion this crate sends is
//! handed to it twice: once as the request leaves (`Sent`) and once when the
//! response or error arrives (`Received`), so someone watching the log can
//! tell a call that is still in flight from one that finished. The sink is
//! process-local; this module never writes files, sends data or records
//! headers (API keys travel only in headers, never in the recorded bodies).
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// Which half of a call an entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The request is about to be sent; `request` is set.
    Sent,
    /// The call finished; `response` or `error` is set and `duration` is real.
    Received,
}

/// One half of a request/response exchange with a model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelExchange {
    /// Process-wide call number pairing a `Sent` entry with its `Received`.
    pub call: u64,
    pub phase: Phase,
    /// When this entry was produced.
    pub at: SystemTime,
    /// Time since `Sent`; zero on the `Sent` entry itself.
    pub duration: Duration,
    /// `bounded` for the exact wire bodies of an investigation call,
    /// `firewall` for a single-shot completion's redacted payload and text.
    pub path: &'static str,
    /// Endpoint URL (bounded) or provider id (firewall).
    pub target: String,
    /// The request as sent (bounded) or the redacted messages (firewall).
    pub request: Option<String>,
    /// The response body or completion text, when one was received.
    pub response: Option<String>,
    /// A short error description when the call did not succeed.
    pub error: Option<String>,
}

type Sink = dyn Fn(&ModelExchange) + Send + Sync;

static SINK: RwLock<Option<Arc<Sink>>> = RwLock::new(None);
static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

/// Install (or replace) the process sink. Pass `None` to stop recording.
pub fn set_sink(sink: Option<Arc<Sink>>) {
    if let Ok(mut slot) = SINK.write() {
        *slot = sink;
    }
}

/// Whether a sink is installed, so callers can skip copying request bodies.
pub(crate) fn enabled() -> bool {
    SINK.read().is_ok_and(|slot| slot.is_some())
}

fn emit(exchange: &ModelExchange) {
    let sink = SINK.read().ok().and_then(|slot| slot.clone());
    if let Some(sink) = sink {
        sink(exchange);
    }
}

/// An in-flight call whose `Sent` entry has been recorded.
pub(crate) struct Call {
    call: u64,
    path: &'static str,
    target: String,
    clock: Instant,
}

/// Record the `Sent` entry now. Never fails the call.
pub(crate) fn begin(path: &'static str, target: String, request: String) -> Call {
    let call = NEXT_CALL.fetch_add(1, Ordering::Relaxed);
    emit(&ModelExchange {
        call,
        phase: Phase::Sent,
        at: SystemTime::now(),
        duration: Duration::ZERO,
        path,
        target: target.clone(),
        request: Some(request),
        response: None,
        error: None,
    });
    Call {
        call,
        path,
        target,
        clock: Instant::now(),
    }
}

impl Call {
    /// Record the matching `Received` entry.
    pub(crate) fn finish(self, response: Option<String>, error: Option<String>) {
        emit(&ModelExchange {
            call: self.call,
            phase: Phase::Received,
            at: SystemTime::now(),
            duration: self.clock.elapsed(),
            path: self.path,
            target: self.target,
            request: None,
            response,
            error,
        });
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Mutex;

    static RECORDED: Mutex<Vec<ModelExchange>> = Mutex::new(Vec::new());
    static INSTALL: std::sync::Once = std::sync::Once::new();

    /// Install one capturing sink for the whole test binary.
    pub(crate) fn capture() {
        INSTALL.call_once(|| {
            set_sink(Some(Arc::new(|exchange: &ModelExchange| {
                RECORDED.lock().unwrap().push(exchange.clone());
            })));
        });
    }

    /// Both entries of every call whose `Sent` request contains `marker`.
    pub(crate) fn calls_with(marker: &str) -> Vec<(ModelExchange, Option<ModelExchange>)> {
        let recorded = RECORDED.lock().unwrap().clone();
        recorded
            .iter()
            .filter(|entry| {
                entry.phase == Phase::Sent
                    && entry.request.as_deref().is_some_and(|r| r.contains(marker))
            })
            .map(|sent| {
                let received = recorded
                    .iter()
                    .find(|entry| entry.call == sent.call && entry.phase == Phase::Received)
                    .cloned();
                (sent.clone(), received)
            })
            .collect()
    }
}
