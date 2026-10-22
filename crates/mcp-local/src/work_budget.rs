//! Activation-wide admission for synchronous read workers. Move each permit
//! into the actual worker: cancelling its async waiter must not release capacity
//! while that worker still owns graph copies or performs file validation.
//!
//! Reservation bytes are host-supplied conservative accounting units, not an RSS
//! measurement. The host must include all retained/transient copies in its model.
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

/// Explicit policy; there are no implicit application defaults.
#[derive(Clone, Copy)]
pub struct WorkLimits {
    /// Maximum simultaneous live synchronous workers.
    pub max_workers: NonZeroUsize,
    /// Aggregate conservative retained and transient allocation reservations.
    pub max_reserved_bytes: NonZeroUsize,
    /// Non-refundable total file-validation/read allowance for this activation.
    pub max_cumulative_read_bytes: u64,
}

#[derive(Default)]
struct Usage {
    revoked: bool,
    workers: usize,
    reserved_bytes: usize,
    read_bytes: u64,
}
struct Inner {
    limits: WorkLimits,
    usage: Mutex<Usage>,
}

/// Shared by every connection in one activation, including timed-out workers.
#[derive(Clone)]
pub struct WorkBudget(Arc<Inner>);

/// Fixed failure without source paths or resource contents.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("local read work unavailable or budget exhausted")]
pub struct WorkDenied;

/// Non-cloneable reservation. Dropping a caller's task handle does not release
/// this permit when it has already been moved into a synchronous worker.
pub struct WorkPermit {
    inner: Arc<Inner>,
    reserved_bytes: usize,
}
impl WorkBudget {
    /// Create a new activation ledger with explicit limits.
    pub fn new(limits: WorkLimits) -> Self {
        Self(Arc::new(Inner {
            limits,
            usage: Mutex::new(Usage::default()),
        }))
    }

    /// Immediate admission, with no unbounded queue of waiting requests.
    /// Charge the full conservative file-read allowance before the worker starts.
    /// Cumulative charges never refund, including failed or cancelled reads.
    pub fn try_acquire(
        &self,
        reserved_bytes: NonZeroUsize,
        read_bytes: u64,
    ) -> Result<WorkPermit, WorkDenied> {
        let mut usage = self.0.usage.lock().map_err(|_| WorkDenied)?;
        let bytes = usage
            .reserved_bytes
            .checked_add(reserved_bytes.get())
            .ok_or(WorkDenied)?;
        let reads = usage.read_bytes.checked_add(read_bytes).ok_or(WorkDenied)?;
        if usage.revoked
            || usage.workers >= self.0.limits.max_workers.get()
            || bytes > self.0.limits.max_reserved_bytes.get()
            || reads > self.0.limits.max_cumulative_read_bytes
        {
            return Err(WorkDenied);
        }
        usage.workers += 1;
        usage.reserved_bytes = bytes;
        usage.read_bytes = reads;
        Ok(WorkPermit {
            inner: Arc::clone(&self.0),
            reserved_bytes: reserved_bytes.get(),
        })
    }

    /// Whether all actual workers have released their reservations. Re-enable
    /// must wait for this after revocation rather than resetting a live ledger.
    pub fn is_idle(&self) -> Result<bool, WorkDenied> {
        self.0
            .usage
            .lock()
            .map(|usage| usage.workers == 0)
            .map_err(|_| WorkDenied)
    }

    /// Permanently stop new work. Existing synchronous work cannot be forcibly
    /// cancelled; it keeps its reservation until it exits. Host closes sockets.
    pub fn revoke(&self) {
        if let Ok(mut usage) = self.0.usage.lock() {
            usage.revoked = true;
        }
    }
}
impl WorkPermit {
    /// Charge further read work before each request on a retained worker.
    /// Charges are conservative allowances and do not refund after failure.
    pub fn charge_read(&self, bytes: u64) -> Result<(), WorkDenied> {
        let mut usage = self.inner.usage.lock().map_err(|_| WorkDenied)?;
        let total = usage.read_bytes.checked_add(bytes).ok_or(WorkDenied)?;
        if usage.revoked || total > self.inner.limits.max_cumulative_read_bytes {
            return Err(WorkDenied);
        }
        usage.read_bytes = total;
        Ok(())
    }

    /// Check immediately before an expensive phase. A concurrent revoke may
    /// occur after this check; this is cooperative admission, not preemption.
    pub fn check_active(&self) -> Result<(), WorkDenied> {
        let usage = self.inner.usage.lock().map_err(|_| WorkDenied)?;
        if usage.revoked {
            Err(WorkDenied)
        } else {
            Ok(())
        }
    }
}
impl Drop for WorkPermit {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.inner.usage.lock() {
            usage.workers -= 1;
            usage.reserved_bytes -= self.reserved_bytes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }
    fn budget(workers: usize, bytes: usize, reads: u64) -> WorkBudget {
        WorkBudget::new(WorkLimits {
            max_workers: nz(workers),
            max_reserved_bytes: nz(bytes),
            max_cumulative_read_bytes: reads,
        })
    }
    #[test]
    fn aggregates_connections_without_refunding_failed_read_work() {
        // AC-0289 foundation: the host must still supply/validate its cost model.
        let budget = budget(2, 10, 100);
        let other_connection = budget.clone();
        let first = budget.try_acquire(nz(6), 60).unwrap();
        assert!(other_connection.try_acquire(nz(5), 40).is_err());
        let second = other_connection.try_acquire(nz(4), 40).unwrap();
        assert!(budget.try_acquire(nz(1), 0).is_err());
        drop((first, second));
        assert!(budget.try_acquire(nz(1), 1).is_err());
        assert!(budget.try_acquire(nz(10), 0).is_ok());
    }
    #[test]
    fn dropped_waiter_does_not_release_running_worker_capacity() {
        let budget = budget(1, 10, 0);
        let permit = budget.try_acquire(nz(10), 0).unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _permit = permit;
            wait.recv().unwrap();
            drop(_permit);
            finished.send(()).unwrap();
        });
        drop(handle); // equivalent ownership property to detached blocking work
        assert!(budget.try_acquire(nz(1), 0).is_err());
        release.send(()).unwrap();
        done.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(budget.try_acquire(nz(10), 0).is_ok());
    }
    #[test]
    fn revoke_is_permanent_and_integer_overflow_fails_closed() {
        let budget = budget(2, usize::MAX, u64::MAX);
        let first = budget.try_acquire(nz(usize::MAX), u64::MAX).unwrap();
        assert!(budget.try_acquire(nz(1), 0).is_err());
        drop(first);
        assert!(budget.try_acquire(nz(1), 1).is_err());
        let active = budget.try_acquire(nz(1), 0).unwrap();
        assert!(active.charge_read(1).is_err());
        budget.revoke();
        assert!(active.charge_read(0).is_err());
        assert!(active.check_active().is_err());
        drop(active);
        assert!(budget.try_acquire(nz(1), 0).is_err());
    }
}
