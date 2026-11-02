//! Local model-exchange debug log (#1624).
//!
//! When the host installs a sink, every model completion this crate sends is
//! recorded twice: once as the request leaves (`Sent`) and once when the
//! response or error arrives (`Received`), so someone watching the log can
//! tell a call that is still in flight from one that finished. The module
//! never writes files, sends data or records headers (API keys travel only in
//! headers, never in the recorded bodies).
//!
//! Recording never blocks a provider call and its memory is a fixed constant
//! outside every bounded workspace: `begin`/`finish` copy at most
//! [`MAX_BODY_BYTES`] of each body and push it onto an in-memory queue capped
//! at [`QUEUE_BYTES`]; a dedicated writer thread hands queued entries to the
//! sink. An entry that would exceed the cap is dropped and counted, and the
//! next admitted entry reports the count in `dropped_before`.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// Most bytes of one request or response body copied into an entry.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Most bytes of entries waiting for (or being handed to) the sink.
pub const QUEUE_BYTES: usize = 8 * 1024 * 1024;

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
    /// The request as sent (bounded) or the redacted messages (firewall),
    /// truncated to [`MAX_BODY_BYTES`] with a stated omission.
    pub request: Option<String>,
    /// The response body or completion text, truncated like `request`.
    pub response: Option<String>,
    /// A short error description when the call did not succeed.
    pub error: Option<String>,
    /// Entries dropped (queue full) between the previous admitted entry and this one.
    pub dropped_before: u64,
}

impl ModelExchange {
    fn size(&self) -> usize {
        self.target.len()
            + self.request.as_ref().map_or(0, String::len)
            + self.response.as_ref().map_or(0, String::len)
            + self.error.as_ref().map_or(0, String::len)
    }
}

type Sink = dyn Fn(&ModelExchange) + Send + Sync;

#[derive(Default)]
struct Queue {
    entries: VecDeque<ModelExchange>,
    /// Bytes of queued entries plus the one being delivered.
    bytes: usize,
    dropped: u64,
    delivering: bool,
}

/// A bounded queue drained by one writer thread into the installed sink.
pub(crate) struct Recorder {
    capacity: usize,
    enabled: AtomicBool,
    sink: RwLock<Option<Arc<Sink>>>,
    queue: Mutex<Queue>,
    ready: Condvar,
    idle: Condvar,
    writer: std::sync::Once,
}

impl Recorder {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            enabled: AtomicBool::new(false),
            sink: RwLock::new(None),
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            idle: Condvar::new(),
            writer: std::sync::Once::new(),
        })
    }

    pub(crate) fn set_sink(self: &Arc<Self>, sink: Option<Arc<Sink>>) {
        let on = sink.is_some();
        if let Ok(mut slot) = self.sink.write() {
            *slot = sink;
        }
        self.enabled.store(on, Ordering::Release);
        if on {
            self.writer.call_once(|| {
                let recorder = Arc::clone(self);
                // Detached: it waits on the queue for the life of the process.
                let _ = std::thread::Builder::new()
                    .name("model-debug-log".into())
                    .spawn(move || recorder.deliver());
            });
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Queue an entry without blocking on the sink; drop it when full.
    pub(crate) fn push(&self, mut exchange: ModelExchange) {
        if !self.enabled() {
            return;
        }
        let size = exchange.size();
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        if queue.bytes + size > self.capacity {
            queue.dropped += 1;
            return;
        }
        queue.bytes += size;
        exchange.dropped_before = std::mem::take(&mut queue.dropped);
        queue.entries.push_back(exchange);
        self.ready.notify_one();
    }

    fn deliver(&self) {
        loop {
            let (mut exchange, size) = {
                let Ok(mut queue) = self.queue.lock() else {
                    return;
                };
                while queue.entries.is_empty() {
                    queue = match self.ready.wait(queue) {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                }
                let exchange = queue.entries.pop_front().expect("non-empty");
                queue.delivering = true;
                let size = exchange.size();
                (exchange, size)
            };
            let sink = self.sink.read().ok().and_then(|slot| slot.clone());
            if let Some(sink) = sink {
                sink(&exchange);
            }
            exchange.request = None;
            exchange.response = None;
            if let Ok(mut queue) = self.queue.lock() {
                queue.bytes -= size;
                queue.delivering = false;
                if queue.entries.is_empty() {
                    self.idle.notify_all();
                }
            }
        }
    }

    /// Wait until every queued entry has been handed to the sink.
    pub(crate) fn flush(&self, timeout: Duration) -> bool {
        let Ok(queue) = self.queue.lock() else {
            return false;
        };
        self.idle
            .wait_timeout_while(queue, timeout, |queue| {
                !queue.entries.is_empty() || queue.delivering
            })
            .map(|(_, result)| !result.timed_out())
            .unwrap_or(false)
    }
}

static RECORDER: LazyLock<Arc<Recorder>> = LazyLock::new(|| Recorder::new(QUEUE_BYTES));
static NEXT_CALL: AtomicU64 = AtomicU64::new(1);

/// Install (or replace) the process sink. Pass `None` to stop recording.
/// The sink runs on a dedicated thread, never on a provider call.
pub fn set_sink(sink: Option<Arc<Sink>>) {
    RECORDER.set_sink(sink);
}

/// Wait up to `timeout` for queued entries to reach the sink.
pub fn flush(timeout: Duration) -> bool {
    RECORDER.flush(timeout)
}

/// Whether a sink is installed, so callers can skip copying request bodies.
pub(crate) fn enabled() -> bool {
    RECORDER.enabled()
}

/// Copy at most [`MAX_BODY_BYTES`] of `bytes` as text, stating any omission.
pub(crate) fn excerpt(bytes: &[u8]) -> String {
    if bytes.len() <= MAX_BODY_BYTES {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut text = String::from_utf8_lossy(&bytes[..MAX_BODY_BYTES]).into_owned();
    text.push_str(&format!(
        "\n[debug log: truncated, first {MAX_BODY_BYTES} of {} bytes shown]",
        bytes.len()
    ));
    text
}

/// An in-flight call whose `Sent` entry has been queued.
pub(crate) struct Call {
    call: u64,
    path: &'static str,
    target: String,
    clock: Instant,
}

/// Queue the `Sent` entry now. Never blocks on the sink or fails the call.
pub(crate) fn begin(path: &'static str, target: String, request: String) -> Call {
    let call = NEXT_CALL.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    test_support::BEGUN_ON_THREAD.with(|begun| begun.set(begun.get() + 1));
    RECORDER.push(ModelExchange {
        call,
        phase: Phase::Sent,
        at: SystemTime::now(),
        duration: Duration::ZERO,
        path,
        target: target.clone(),
        request: Some(request),
        response: None,
        error: None,
        dropped_before: 0,
    });
    Call {
        call,
        path,
        target,
        clock: Instant::now(),
    }
}

impl Call {
    /// Queue the matching `Received` entry.
    pub(crate) fn finish(self, response: Option<String>, error: Option<String>) {
        RECORDER.push(ModelExchange {
            call: self.call,
            phase: Phase::Received,
            at: SystemTime::now(),
            duration: self.clock.elapsed(),
            path: self.path,
            target: self.target,
            request: None,
            response,
            error,
            dropped_before: 0,
        });
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    static RECORDED: Mutex<Vec<ModelExchange>> = Mutex::new(Vec::new());
    thread_local! {
        /// `Sent` entries queued by this thread, so a test's control check
        /// can act at the instant after the request was logged.
        pub(crate) static BEGUN_ON_THREAD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    static INSTALL: std::sync::Once = std::sync::Once::new();
    /// While set, the capturing sink blocks on entries containing
    /// `debug-1624-slow-sink`, standing in for a slow disk.
    pub(crate) static HOLD_SLOW_SINK: AtomicBool = AtomicBool::new(false);

    /// Install one capturing sink for the whole test binary.
    pub(crate) fn capture() {
        INSTALL.call_once(|| {
            set_sink(Some(Arc::new(|exchange: &ModelExchange| {
                let slow = exchange
                    .request
                    .as_deref()
                    .is_some_and(|r| r.contains("debug-1624-slow-sink"));
                while slow && HOLD_SLOW_SINK.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                RECORDED.lock().unwrap().push(exchange.clone());
            })));
        });
    }

    /// Both entries of every call whose `Sent` request contains `marker`,
    /// after the writer has delivered everything queued so far.
    pub(crate) fn calls_with(marker: &str) -> Vec<(ModelExchange, Option<ModelExchange>)> {
        assert!(flush(Duration::from_secs(10)), "debug log did not drain");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(call: u64, body: usize) -> ModelExchange {
        ModelExchange {
            call,
            phase: Phase::Sent,
            at: SystemTime::UNIX_EPOCH,
            duration: Duration::ZERO,
            path: "bounded",
            target: String::new(),
            request: Some("x".repeat(body)),
            response: None,
            error: None,
            dropped_before: 0,
        }
    }

    // AC-0560 (#1624): a slow sink never blocks the caller; past the byte
    // cap entries are dropped and counted, and the count is reported on the
    // next admitted entry, so debug memory stays a fixed constant.
    #[test]
    fn debug_queue_is_byte_bounded_and_never_blocks_on_the_sink() {
        let gate = Arc::new(AtomicBool::new(true));
        let seen = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
        let recorder = Recorder::new(100);
        let (hold, log) = (Arc::clone(&gate), Arc::clone(&seen));
        recorder.set_sink(Some(Arc::new(move |exchange: &ModelExchange| {
            while hold.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            log.lock()
                .unwrap()
                .push((exchange.call, exchange.dropped_before));
        })));
        let started = Instant::now();
        for call in 1..=5 {
            recorder.push(entry(call, 40));
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!recorder.flush(Duration::from_millis(50)));
        gate.store(false, Ordering::Release);
        assert!(recorder.flush(Duration::from_secs(10)));
        recorder.push(entry(6, 40));
        assert!(recorder.flush(Duration::from_secs(10)));
        // 100 bytes admit two 40-byte entries; calls 3..=5 are dropped and
        // reported on the next entry admitted after them.
        assert_eq!(*seen.lock().unwrap(), vec![(1, 0), (2, 0), (6, 3)]);
    }

    #[test]
    fn debug_excerpt_states_truncation() {
        assert_eq!(excerpt(b"short"), "short");
        let long = vec![b'a'; MAX_BODY_BYTES + 7];
        let text = excerpt(&long);
        assert!(text.ends_with(&format!(
            "[debug log: truncated, first {MAX_BODY_BYTES} of {} bytes shown]",
            MAX_BODY_BYTES + 7
        )));
        assert!(text.len() < MAX_BODY_BYTES + 100);
    }
}
