//! Detached task ownership within one connection's admitted reservation.
//! The byte model must include MAX_LIVE_TASKS overlapping executions in addition
//! to connection work. This module enforces lifetime/count, not that byte model.
use mcp_local::work_budget::{WorkDenied, WorkPermit};
use std::sync::{Arc, Mutex};

struct Inner {
    permit: Arc<WorkPermit>,
    live: Mutex<usize>,
}

pub(crate) struct TaskReservations(Arc<Inner>);
pub(crate) struct DetachedReservation(Arc<Inner>);

impl From<Arc<WorkPermit>> for TaskReservations {
    fn from(permit: Arc<WorkPermit>) -> Self {
        Self(Arc::new(Inner {
            permit,
            live: Mutex::new(0),
        }))
    }
}

impl TaskReservations {
    /// Called after durable nonce recovery but before new-task preparation or
    /// publication. A rejected attempt has no queue and owns no execution slot.
    pub(crate) fn try_acquire(&self) -> Result<DetachedReservation, WorkDenied> {
        let mut live = self.0.live.lock().map_err(|_| WorkDenied)?;
        self.0.permit.check_active()?;
        if *live >= crate::jobs::investigations::MAX_LIVE_TASKS {
            return Err(WorkDenied);
        }
        *live += 1;
        Ok(DetachedReservation(self.0.clone()))
    }
}

impl Drop for DetachedReservation {
    fn drop(&mut self) {
        if let Ok(mut live) = self.0.live.lock() {
            *live -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_local::work_budget::{WorkBudget, WorkLimits};
    use std::num::NonZeroUsize;

    #[test]
    fn detached_tasks_keep_reservation_after_connection_drop_and_revocation() {
        // AC-0325: real thread lifetime, no provider or transport qualification.
        let one = NonZeroUsize::new(1).unwrap();
        let budget = WorkBudget::new(WorkLimits {
            max_workers: one,
            max_reserved_bytes: one,
            max_cumulative_read_bytes: 0,
        });
        let permit = Arc::new(budget.try_acquire(one, 0).unwrap());
        let tasks = TaskReservations::from(permit.clone());
        let first = tasks.try_acquire().unwrap();
        let second = tasks.try_acquire().unwrap();
        assert!(tasks.try_acquire().is_err());
        drop(second);
        let second = tasks.try_acquire().unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _reservation = first;
            wait.recv().unwrap();
        });
        budget.revoke();
        assert!(tasks.try_acquire().is_err());
        drop((permit, tasks, second)); // disconnected connection and other job
        assert!(!budget.is_idle().unwrap());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(budget.is_idle().unwrap());
        assert!(budget.try_acquire(one, 0).is_err()); // revocation never resets
    }
}
