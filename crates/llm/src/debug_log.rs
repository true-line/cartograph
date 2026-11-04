//! Local model-exchange debug log (#1624).
//!
//! When the host installs a sink, every model completion this crate sends is
//! recorded twice: once as the request leaves (`Sent`) and once when the
//! response or error arrives (`Received`), so someone watching the log can
//! tell a call that is still in flight from one that finished. The module
//! never writes files, sends data or records headers (API keys travel only in
//! headers, never in the recorded bodies).
//!
//! Recording never blocks a provider call, and every byte it owns comes from
//! one process pool of [`QUEUE_BYTES`]. Before anything is copied, a call
//! reserves the entry's worst-case charge: [`ENTRY_OVERHEAD`] plus the target
//! plus at most [`MAX_BODY_BYTES`] + [`MARKER_BYTES`] of text per body. If the
//! pool cannot cover it, nothing is copied; the entry is dropped and counted,
//! and the next admitted entry reports the count in `dropped_before`. The
//! reservation is released only after a dedicated writer thread has handed
//! the entry to the sink, so queued and in-delivery entries are both charged.
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// Most bytes of decoded text kept from one request or response body.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Most bytes of the truncation note appended to a clipped body.
pub const MARKER_BYTES: usize = 96;
/// Fixed charge per entry for its struct and queue slot, so even empty
/// entries consume pool budget.
pub const ENTRY_OVERHEAD: usize = std::mem::size_of::<ModelExchange>() + 64;
/// The process pool every recorded byte is charged to.
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
    /// decoded as UTF-8 and clipped to [`MAX_BODY_BYTES`] with a stated note.
    pub request: Option<String>,
    /// The response body or completion text, bounded like `request`.
    pub response: Option<String>,
    /// A short error description when the call did not succeed.
    pub error: Option<String>,
    /// Entries dropped (pool full) between the previous admitted entry and
    /// this one.
    pub dropped_before: u64,
}

type Sink = dyn Fn(&ModelExchange) + Send + Sync;

#[derive(Default)]
struct Queue {
    entries: VecDeque<(ModelExchange, usize)>,
    /// Bytes reserved by producers, queued, or being delivered.
    bytes: usize,
    dropped: u64,
    delivering: bool,
}

/// A byte-bounded pool and queue drained by one writer thread into the sink.
pub(crate) struct Recorder {
    capacity: usize,
    enabled: AtomicBool,
    sink: RwLock<Option<Arc<Sink>>>,
    queue: Mutex<Queue>,
    ready: Condvar,
    idle: Condvar,
    writer: std::sync::Once,
}

/// Pool bytes reserved for one entry before anything is copied.
pub(crate) struct Reservation {
    charge: usize,
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

    /// Reserve `charge` pool bytes, or count a drop and return `None`.
    pub(crate) fn reserve(&self, charge: usize) -> Option<Reservation> {
        if !self.enabled() {
            return None;
        }
        let mut queue = self.queue.lock().ok()?;
        match queue.bytes.checked_add(charge) {
            Some(total) if total <= self.capacity => {
                queue.bytes = total;
                Some(Reservation { charge })
            }
            _ => {
                queue.dropped += 1;
                None
            }
        }
    }

    /// Queue an entry built inside `reservation`; never blocks on the sink.
    pub(crate) fn push(&self, reservation: Reservation, mut exchange: ModelExchange) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        exchange.dropped_before = std::mem::take(&mut queue.dropped);
        queue.entries.push_back((exchange, reservation.charge));
        self.ready.notify_one();
    }

    fn deliver(&self) {
        loop {
            let (exchange, charge) = {
                let Ok(mut queue) = self.queue.lock() else {
                    return;
                };
                while queue.entries.is_empty() {
                    queue = match self.ready.wait(queue) {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                }
                queue.delivering = true;
                queue.entries.pop_front().expect("non-empty")
            };
            let sink = self.sink.read().ok().and_then(|slot| slot.clone());
            if let Some(sink) = sink {
                sink(&exchange);
            }
            drop(exchange);
            if let Ok(mut queue) = self.queue.lock() {
                queue.bytes -= charge;
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

    #[cfg(test)]
    fn reserved_bytes(&self) -> usize {
        self.queue.lock().unwrap().bytes
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

/// Whether a sink is installed, so callers can skip recording entirely.
pub(crate) fn enabled() -> bool {
    #[cfg(test)]
    let recorder = test_support::recorder();
    #[cfg(not(test))]
    let recorder = &RECORDER;
    recorder.enabled()
}

/// Worst-case pool charge of one entry.
fn charge(target: &str, bodies: usize) -> usize {
    ENTRY_OVERHEAD + target.len() + bodies * (MAX_BODY_BYTES + MARKER_BYTES)
}

/// Decode `parts` (one logical body) as UTF-8, replacing invalid sequences,
/// keeping at most [`MAX_BODY_BYTES`] of output and stating any omission in
/// at most [`MARKER_BYTES`]. The output never exceeds the sum of both bounds,
/// whatever the input, and no larger temporary is made.
pub(crate) fn excerpt(parts: &[&[u8]]) -> String {
    let total: usize = parts.iter().map(|part| part.len()).sum();
    // Exactly the worst-case output, so pushing never reallocates past the
    // charge reserved for it (a replacement char is 3 bytes per input byte).
    let mut out = String::with_capacity(total.saturating_mul(3).min(MAX_BODY_BYTES) + MARKER_BYTES);
    let mut consumed = 0_usize;
    'parts: for part in parts {
        for chunk in part.utf8_chunks() {
            let valid = chunk.valid();
            let room = MAX_BODY_BYTES - out.len();
            let mut end = valid.len().min(room);
            while !valid.is_char_boundary(end) {
                end -= 1;
            }
            out.push_str(&valid[..end]);
            consumed += end;
            if end < valid.len() {
                break 'parts;
            }
            let invalid = chunk.invalid();
            if !invalid.is_empty() {
                if MAX_BODY_BYTES - out.len() < char::REPLACEMENT_CHARACTER.len_utf8() {
                    break 'parts;
                }
                out.push(char::REPLACEMENT_CHARACTER);
                consumed += invalid.len();
            }
        }
    }
    if consumed < total {
        let _ = write!(
            out,
            "\n[debug log: truncated, first {consumed} of {total} bytes shown]"
        );
    }
    out
}

/// An in-flight call whose `Sent` entry has been queued.
pub(crate) struct Call {
    #[cfg(test)]
    recorder: Arc<Recorder>,
    call: u64,
    path: &'static str,
    target: String,
    clock: Instant,
}

/// Queue the `Sent` entry for a request made of `parts`. Copies nothing
/// unless the pool covers the entry's worst case; never blocks on the sink.
pub(crate) fn begin(path: &'static str, target: &str, parts: &[&[u8]]) -> Call {
    let call = NEXT_CALL.fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    test_support::BEGUN_ON_THREAD.with(|begun| begun.set(begun.get() + 1));
    #[cfg(test)]
    let recorder = test_support::recorder();
    #[cfg(not(test))]
    let recorder = &RECORDER;
    if let Some(reservation) = recorder.reserve(charge(target, 1)) {
        recorder.push(
            reservation,
            ModelExchange {
                call,
                phase: Phase::Sent,
                at: SystemTime::now(),
                duration: Duration::ZERO,
                path,
                target: target.to_owned(),
                request: Some(excerpt(parts)),
                response: None,
                error: None,
                dropped_before: 0,
            },
        );
    }
    Call {
        #[cfg(test)]
        recorder,
        call,
        path,
        target: target.to_owned(),
        clock: Instant::now(),
    }
}

impl Call {
    /// Queue the matching `Received` entry, under the same pool rule.
    pub(crate) fn finish(self, response: Option<&[u8]>, error: Option<String>) {
        #[cfg(test)]
        let recorder = &self.recorder;
        #[cfg(not(test))]
        let recorder = &RECORDER;
        let Some(reservation) = recorder.reserve(charge(&self.target, 1)) else {
            return;
        };
        recorder.push(
            reservation,
            ModelExchange {
                call: self.call,
                phase: Phase::Received,
                at: SystemTime::now(),
                duration: self.clock.elapsed(),
                path: self.path,
                target: self.target,
                request: None,
                response: response.map(|body| excerpt(&[body])),
                error,
                dropped_before: 0,
            },
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    thread_local! {
        static RECORDER: Arc<Recorder> = Recorder::new(QUEUE_BYTES);
        static RECORDED: Arc<Mutex<Vec<ModelExchange>>> = Arc::default();
        /// `Sent` entries begun by this thread, so a test's control check
        /// can act at the instant after the request was logged.
        pub(crate) static BEGUN_ON_THREAD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
        /// Only this capture's writer is held by the slow-sink test.
        pub(crate) static HOLD_SLOW_SINK: Arc<AtomicBool> = Arc::default();
    }

    pub(super) fn recorder() -> Arc<Recorder> {
        RECORDER.with(Arc::clone)
    }

    /// Install a capturing sink owned by this test thread. Unrelated
    /// completion tests cannot reserve from its pool or append its entries.
    pub(crate) fn capture() {
        let recorded = RECORDED.with(Arc::clone);
        let hold = HOLD_SLOW_SINK.with(Arc::clone);
        recorder().set_sink(Some(Arc::new(move |exchange: &ModelExchange| {
            let slow = exchange
                .request
                .as_deref()
                .is_some_and(|r| r.contains("debug-1624-slow-sink"));
            while slow && hold.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            recorded.lock().unwrap().push(exchange.clone());
        })));
    }

    /// Both entries of every call whose `Sent` request contains `marker`,
    /// after the writer has delivered everything queued so far.
    pub(crate) fn calls_with(marker: &str) -> Vec<(ModelExchange, Option<ModelExchange>)> {
        assert!(
            recorder().flush(Duration::from_secs(10)),
            "debug log did not drain"
        );
        let recorded = RECORDED.with(|entries| entries.lock().unwrap().clone());
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

    fn entry(call: u64) -> ModelExchange {
        ModelExchange {
            call,
            phase: Phase::Sent,
            at: SystemTime::UNIX_EPOCH,
            duration: Duration::ZERO,
            path: "bounded",
            target: String::new(),
            request: None,
            response: None,
            error: None,
            dropped_before: 0,
        }
    }

    // AC-0560 (#1674): capture pools remain independent under parallel
    // pressure, and a call moved to another thread keeps its original sink.
    #[test]
    fn debug_capture_isolates_parallel_pressure_and_cross_thread_finish() {
        use std::sync::mpsc;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let pressure = std::thread::spawn(move || {
            test_support::capture();
            test_support::HOLD_SLOW_SINK.with(|hold| hold.store(true, Ordering::Release));
            for _ in 0..QUEUE_BYTES / MAX_BODY_BYTES {
                let _call = begin("test", "synthetic", &[b"debug-1624-slow-sink"]);
            }
            assert!(
                test_support::recorder()
                    .reserve(charge("synthetic", 1))
                    .is_none()
            );
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            test_support::HOLD_SLOW_SINK.with(|hold| hold.store(false, Ordering::Release));
            assert!(test_support::calls_with("isolated-target").is_empty());
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        test_support::capture();
        let call = begin("test", "synthetic", &[b"isolated-target"]);
        std::thread::spawn(move || call.finish(None, Some("Cancelled".into())))
            .join()
            .unwrap();
        // Release pressure before assertions so a failed assertion cannot
        // leave the other capture's writer held indefinitely.
        release_tx.send(()).unwrap();
        pressure.join().unwrap();
        let calls = test_support::calls_with("isolated-target");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].1.as_ref().unwrap().error.as_deref(),
            Some("Cancelled")
        );
        assert!(test_support::calls_with("debug-1624-slow-sink").is_empty());
    }

    // AC-0560 (#1624): a slow sink never blocks the caller. Every entry,
    // even an empty one, is charged at least ENTRY_OVERHEAD before it is
    // built; past the pool entries are dropped without copying and counted
    // on the next admitted entry; delivery releases exactly the charge.
    #[test]
    fn debug_pool_charges_every_entry_and_never_blocks_on_the_sink() {
        let gate = Arc::new(AtomicBool::new(true));
        let seen = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
        let recorder = Recorder::new(2 * ENTRY_OVERHEAD + 10);
        assert!(
            recorder.reserve(1).is_none(),
            "disabled pool admits nothing"
        );
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
            // Empty entries: the overhead alone bounds how many are held.
            if let Some(reservation) = recorder.reserve(ENTRY_OVERHEAD) {
                recorder.push(reservation, entry(call));
            }
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(recorder.reserved_bytes(), 2 * ENTRY_OVERHEAD);
        assert!(!recorder.flush(Duration::from_millis(50)));
        gate.store(false, Ordering::Release);
        assert!(recorder.flush(Duration::from_secs(10)));
        assert_eq!(recorder.reserved_bytes(), 0);
        let reservation = recorder.reserve(ENTRY_OVERHEAD).unwrap();
        recorder.push(reservation, entry(6));
        assert!(recorder.flush(Duration::from_secs(10)));
        assert_eq!(*seen.lock().unwrap(), vec![(1, 0), (2, 0), (6, 3)]);
        assert!(recorder.reserve(usize::MAX).is_none());
    }

    // AC-0560: decoded output never exceeds MAX_BODY_BYTES + MARKER_BYTES,
    // even when every input byte is invalid UTF-8 (each would otherwise
    // become a 3-byte replacement), and it never splits a character.
    #[test]
    fn debug_excerpt_bounds_decoded_output_for_any_input() {
        assert_eq!(excerpt(&[b"short"]), "short");
        assert_eq!(excerpt(&[b"a", b"\xff", b"b"]), "a\u{FFFD}b");
        let note = |shown: usize, total: usize| {
            format!("\n[debug log: truncated, first {shown} of {total} bytes shown]")
        };
        let invalid = vec![0xff_u8; MAX_BODY_BYTES];
        let text = excerpt(&[&invalid]);
        let kept = MAX_BODY_BYTES / 3;
        assert!(text.ends_with(&note(kept, MAX_BODY_BYTES)));
        assert_eq!(text.len(), kept * 3 + note(kept, MAX_BODY_BYTES).len());

        let mut multibyte = vec![b'a'; MAX_BODY_BYTES - 1];
        multibyte.extend_from_slice("é".as_bytes());
        let text = excerpt(&[&multibyte]);
        assert!(text.ends_with(&note(MAX_BODY_BYTES - 1, MAX_BODY_BYTES + 1)));

        let max_note = note(usize::MAX, usize::MAX);
        assert!(max_note.len() <= MARKER_BYTES);
        for input in [
            &invalid,
            &multibyte,
            &vec![b'x'; 3 * MAX_BODY_BYTES],
            &vec![0xff; 7],
        ] {
            let text = excerpt(&[input]);
            assert!(text.capacity() <= MAX_BODY_BYTES + MARKER_BYTES);
            assert!(text.len() <= text.capacity());
        }
    }
}
