use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug)]
struct CallbackState {
    accepting: bool,
    in_flight: usize,
}

/// Serializes callback admission against native resource teardown.
///
/// A callback obtains a lease before touching its WinRT/D3D context. Shutdown
/// first closes admission, unregisters the native event, waits for all leases
/// to drain, drains callback-produced objects, and only then closes the native
/// producer before dropping callback state.
#[derive(Debug)]
pub struct CallbackBarrier {
    state: Mutex<CallbackState>,
    drained: Condvar,
}

impl CallbackBarrier {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(CallbackState {
                accepting: true,
                in_flight: 0,
            }),
            drained: Condvar::new(),
        })
    }

    pub fn try_enter(self: &Arc<Self>) -> Option<CallbackLease> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return None;
        }
        state.in_flight = state.in_flight.saturating_add(1);
        drop(state);
        Some(CallbackLease {
            barrier: Arc::clone(self),
        })
    }

    pub fn stop_accepting(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        if state.in_flight == 0 {
            self.drained.notify_all();
        }
    }

    pub fn wait_until_idle(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.in_flight != 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> (bool, usize) {
        self.state
            .lock()
            .map(|state| (state.accepting, state.in_flight))
            .unwrap_or((false, 0))
    }
}

pub struct CallbackLease {
    barrier: Arc<CallbackBarrier>,
}

impl Drop for CallbackLease {
    fn drop(&mut self) {
        let mut state = self
            .barrier
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            self.barrier.drained.notify_all();
        }
    }
}

pub fn shutdown_callback_source<Remove, DrainPending, CloseSession, ClosePool>(
    barrier: &CallbackBarrier,
    mut remove_handler: Remove,
    mut drain_pending: DrainPending,
    mut close_session: CloseSession,
    mut close_pool: ClosePool,
) -> Result<(), String>
where
    Remove: FnMut() -> Result<(), String>,
    DrainPending: FnMut() -> Result<(), String>,
    CloseSession: FnMut() -> Result<(), String>,
    ClosePool: FnMut() -> Result<(), String>,
{
    barrier.stop_accepting();
    let mut errors = Vec::new();
    if let Err(error) = remove_handler() {
        errors.push(error);
    }
    barrier.wait_until_idle();
    if let Err(error) = drain_pending() {
        errors.push(error);
    }
    if let Err(error) = close_session() {
        errors.push(error);
    }
    if let Err(error) = close_pool() {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    struct FakeFrame {
        id: usize,
        close_count: Arc<AtomicUsize>,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl FakeFrame {
        fn close(self) {
            self.close_count.fetch_add(1, Ordering::SeqCst);
            self.events
                .lock()
                .unwrap()
                .push(format!("close-frame-{}", self.id));
        }
    }

    struct TestGuard {
        barrier: Arc<CallbackBarrier>,
        pending: Arc<Mutex<VecDeque<FakeFrame>>>,
        events: Arc<Mutex<Vec<String>>>,
        closed: bool,
    }

    impl TestGuard {
        fn close(&mut self) -> Result<(), String> {
            if self.closed {
                return Ok(());
            }
            self.closed = true;
            let remove_events = Arc::clone(&self.events);
            let drain_events = Arc::clone(&self.events);
            let pending = Arc::clone(&self.pending);
            let session_events = Arc::clone(&self.events);
            let pool_events = Arc::clone(&self.events);
            shutdown_callback_source(
                &self.barrier,
                move || {
                    remove_events
                        .lock()
                        .unwrap()
                        .push("remove-handler".to_string());
                    Ok(())
                },
                move || {
                    let mut pending = pending.lock().unwrap();
                    while let Some(frame) = pending.pop_front() {
                        frame.close();
                    }
                    drain_events.lock().unwrap().push("drain-done".to_string());
                    Ok(())
                },
                move || {
                    session_events
                        .lock()
                        .unwrap()
                        .push("close-session".to_string());
                    Ok(())
                },
                move || {
                    pool_events.lock().unwrap().push("close-pool".to_string());
                    Ok(())
                },
            )
        }
    }

    impl Drop for TestGuard {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }

    fn test_guard(frame_count: usize) -> (TestGuard, Vec<Arc<AtomicUsize>>) {
        let barrier = CallbackBarrier::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut frames = VecDeque::new();
        let mut close_counts = Vec::new();
        for id in 1..=frame_count {
            let close_count = Arc::new(AtomicUsize::new(0));
            frames.push_back(FakeFrame {
                id,
                close_count: Arc::clone(&close_count),
                events: Arc::clone(&events),
            });
            close_counts.push(close_count);
        }
        (
            TestGuard {
                barrier,
                pending: Arc::new(Mutex::new(frames)),
                events,
                closed: false,
            },
            close_counts,
        )
    }

    #[test]
    fn shutdown_rejects_new_callbacks_and_waits_for_in_flight_callback() {
        let barrier = CallbackBarrier::new();
        let lease = barrier.try_enter().expect("initial callback is accepted");
        let waiter_barrier = Arc::clone(&barrier);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            waiter_barrier.stop_accepting();
            started_tx.send(()).unwrap();
            waiter_barrier.wait_until_idle();
            done_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(barrier.try_enter().is_none());
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(lease);
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
        assert_eq!(barrier.snapshot(), (false, 0));
    }

    #[test]
    fn in_flight_callback_finishes_before_pending_frames_and_native_sources_close() {
        let (mut guard, close_counts) = test_guard(2);
        let lease = guard
            .barrier
            .try_enter()
            .expect("callback lease is accepted before shutdown");
        let events = Arc::clone(&guard.events);
        let barrier = Arc::clone(&guard.barrier);
        let cleanup = thread::spawn(move || guard.close().unwrap());
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if events
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "remove-handler")
            {
                break;
            }
            assert!(Instant::now() < deadline, "remove handler was not reached");
            thread::yield_now();
        }
        assert!(barrier.try_enter().is_none());
        assert_eq!(events.lock().unwrap().as_slice(), ["remove-handler"]);
        events.lock().unwrap().push("callback-exit".to_string());
        drop(lease);
        cleanup.join().unwrap();
        assert_eq!(
            events.lock().unwrap().as_slice(),
            [
                "remove-handler",
                "callback-exit",
                "close-frame-1",
                "close-frame-2",
                "drain-done",
                "close-session",
                "close-pool",
            ]
        );
        assert!(close_counts
            .iter()
            .all(|count| count.load(Ordering::SeqCst) == 1));
        assert_eq!(barrier.snapshot(), (false, 0));
    }

    #[test]
    fn ready_receiver_disconnect_runs_remove_wait_drain_and_close_once() {
        fn fail_after_registration(guard: TestGuard) -> Result<(), &'static str> {
            let _guard = guard;
            Err("ready receiver disconnected")
        }

        let (guard, close_counts) = test_guard(2);
        let events = Arc::clone(&guard.events);
        let barrier = Arc::clone(&guard.barrier);
        let pending = Arc::clone(&guard.pending);
        assert!(fail_after_registration(guard).is_err());
        assert_eq!(
            events.lock().unwrap().as_slice(),
            [
                "remove-handler",
                "close-frame-1",
                "close-frame-2",
                "drain-done",
                "close-session",
                "close-pool",
            ]
        );
        assert!(close_counts
            .iter()
            .all(|count| count.load(Ordering::SeqCst) == 1));
        assert!(pending.lock().unwrap().is_empty());
        assert_eq!(barrier.snapshot(), (false, 0));
    }

    #[test]
    fn repeated_cleanup_is_idempotent_without_double_close() {
        let (mut guard, close_counts) = test_guard(2);
        let events = Arc::clone(&guard.events);
        guard.close().unwrap();
        guard.close().unwrap();
        drop(guard);
        assert!(close_counts
            .iter()
            .all(|count| count.load(Ordering::SeqCst) == 1));
        assert_eq!(
            events.lock().unwrap().as_slice(),
            [
                "remove-handler",
                "close-frame-1",
                "close-frame-2",
                "drain-done",
                "close-session",
                "close-pool",
            ]
        );
    }

    #[test]
    fn callback_start_stop_soak_has_no_in_flight_leases_or_pending_frames() {
        for _ in 0..100 {
            let (mut guard, close_counts) = test_guard(2);
            let barrier = Arc::clone(&guard.barrier);
            let pending = Arc::clone(&guard.pending);
            let lease = barrier.try_enter().unwrap();
            drop(lease);
            guard.close().unwrap();
            guard.close().unwrap();
            assert_eq!(barrier.snapshot(), (false, 0));
            assert!(pending.lock().unwrap().is_empty());
            assert!(close_counts
                .iter()
                .all(|count| count.load(Ordering::SeqCst) == 1));
        }
    }
}
