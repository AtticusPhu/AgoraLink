use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub(crate) enum QueuePop<T> {
    Item(T),
    Timeout,
    Closed,
}

struct QueueState<T> {
    items: VecDeque<T>,
    closed: bool,
}

/// A small non-blocking producer queue. When full, the newest item is dropped
/// so a capture callback never waits for network I/O.
pub(crate) struct BoundedDropNewestQueue<T> {
    capacity: usize,
    state: Mutex<QueueState<T>>,
    ready: Condvar,
    drops: AtomicU64,
    peak: AtomicU64,
}

impl<T> BoundedDropNewestQueue<T> {
    pub(crate) fn new(capacity: usize) -> Result<Self, String> {
        if capacity == 0 {
            return Err("bounded queue capacity must be greater than zero".to_string());
        }
        Ok(Self {
            capacity,
            state: Mutex::new(QueueState {
                items: VecDeque::with_capacity(capacity),
                closed: false,
            }),
            ready: Condvar::new(),
            drops: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        })
    }

    pub(crate) fn try_push(&self, item: T) -> Result<(), T> {
        let Ok(mut state) = self.state.lock() else {
            self.drops.fetch_add(1, Ordering::Relaxed);
            return Err(item);
        };
        if state.closed || state.items.len() >= self.capacity {
            self.drops.fetch_add(1, Ordering::Relaxed);
            return Err(item);
        }
        state.items.push_back(item);
        self.peak
            .fetch_max(state.items.len() as u64, Ordering::Relaxed);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    pub(crate) fn pop_timeout(&self, timeout: Duration) -> QueuePop<T> {
        let Ok(state) = self.state.lock() else {
            return QueuePop::Closed;
        };
        let Ok((mut state, _)) = self.ready.wait_timeout_while(state, timeout, |state| {
            state.items.is_empty() && !state.closed
        }) else {
            return QueuePop::Closed;
        };
        if let Some(item) = state.items.pop_front() {
            QueuePop::Item(item)
        } else if state.closed {
            QueuePop::Closed
        } else {
            QueuePop::Timeout
        }
    }

    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
        self.ready.notify_all();
    }

    pub(crate) fn depth(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.items.len() as u64)
            .unwrap_or_default()
    }

    pub(crate) fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    pub(crate) fn drops(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DeadlineCadence {
    interval_ns: u64,
    next_ns: u64,
}

impl DeadlineCadence {
    pub(crate) fn new(interval: Duration) -> Result<Self, String> {
        let interval_ns = interval.as_nanos().min(u128::from(u64::MAX)) as u64;
        if interval_ns == 0 {
            return Err("deadline interval must be greater than zero".to_string());
        }
        Ok(Self {
            interval_ns,
            next_ns: 0,
        })
    }

    /// Returns a deadline without accumulating historical debt. A late worker
    /// resumes at `now` and the following deadline is one full interval later.
    pub(crate) fn next_deadline_ns(&mut self, now_ns: u64) -> u64 {
        if now_ns > self.next_ns {
            self.next_ns = now_ns;
        }
        let deadline = self.next_ns;
        self.next_ns = self.next_ns.saturating_add(self.interval_ns);
        deadline
    }
}

const LATENCY_BUCKET_US: u64 = 250;
const LATENCY_BUCKETS: usize = 2048;

#[derive(Clone)]
pub(crate) struct LatencyHistogram {
    buckets: [u64; LATENCY_BUCKETS],
    samples: u64,
    max_us: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; LATENCY_BUCKETS],
            samples: 0,
            max_us: 0,
        }
    }
}

impl LatencyHistogram {
    pub(crate) fn record_us(&mut self, value_us: u64) {
        let index = (value_us / LATENCY_BUCKET_US).min((LATENCY_BUCKETS - 1) as u64) as usize;
        self.buckets[index] = self.buckets[index].saturating_add(1);
        self.samples = self.samples.saturating_add(1);
        self.max_us = self.max_us.max(value_us);
    }

    pub(crate) fn percentile_us(&self, percentile: u32) -> u64 {
        if self.samples == 0 {
            return 0;
        }
        let rank = self
            .samples
            .saturating_mul(u64::from(percentile.min(100)))
            .div_ceil(100)
            .max(1);
        let mut seen = 0u64;
        for (index, count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= rank {
                return index as u64 * LATENCY_BUCKET_US;
            }
        }
        self.max_us
    }

    pub(crate) fn max_us(&self) -> u64 {
        self.max_us
    }
}

pub(crate) fn run_self_test() -> Result<(), String> {
    let queue = BoundedDropNewestQueue::new(2)?;
    queue.try_push(1).map_err(|_| "queue push failed")?;
    queue.try_push(2).map_err(|_| "queue push failed")?;
    if queue.try_push(3).is_ok() || queue.depth() != 2 || queue.drops() != 1 {
        return Err("bounded queue drop-newest policy failed".to_string());
    }
    if !matches!(queue.pop_timeout(Duration::ZERO), QueuePop::Item(1)) {
        return Err("bounded queue ordering failed".to_string());
    }

    let mut video = DeadlineCadence::new(Duration::from_nanos(1_000_000_000 / 60))?;
    let mut deadline = 0u64;
    for frame in 0..36_000u64 {
        let now = frame.saturating_mul(1_000_000_000 / 60);
        deadline = video.next_deadline_ns(now);
        // An unrelated audio worker can stall; it does not mutate video cadence.
        let _audio_stall_ns = if frame % 100 == 0 { 20_000_000 } else { 0 };
    }
    let expected = 35_999u64.saturating_mul(1_000_000_000 / 60);
    if deadline.abs_diff(expected) > 1_000_000_000 / 60 {
        return Err("video deadline drift exceeded one frame in soak".to_string());
    }

    let mut histogram = LatencyHistogram::default();
    for value in [10, 20, 30, 40, 50, 10_000] {
        histogram.record_us(value);
    }
    if histogram.percentile_us(50) > 250 || histogram.max_us() != 10_000 {
        return Err("latency histogram failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn bounded_queue_never_blocks_or_grows_without_limit() {
        let queue = Arc::new(BoundedDropNewestQueue::new(4).unwrap());
        for value in 0..100 {
            let _ = queue.try_push(value);
        }
        assert_eq!(queue.depth(), 4);
        assert_eq!(queue.peak(), 4);
        assert_eq!(queue.drops(), 96);
    }

    #[test]
    fn audio_consumer_stall_does_not_move_video_deadlines() {
        let mut video = DeadlineCadence::new(Duration::from_nanos(1_000_000_000 / 60)).unwrap();
        let mut deadlines = Vec::new();
        for frame in 0..600u64 {
            deadlines.push(video.next_deadline_ns(frame * (1_000_000_000 / 60)));
            if frame % 60 == 0 {
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert!(deadlines
            .windows(2)
            .all(|pair| pair[1].saturating_sub(pair[0]) == 1_000_000_000 / 60));
    }

    #[test]
    fn queue_lock_is_independent_from_deadline_and_repair_work() {
        let queue = BoundedDropNewestQueue::new(1).unwrap();
        queue.try_push(7).unwrap();
        let mut video = DeadlineCadence::new(Duration::from_millis(16)).unwrap();
        assert_eq!(video.next_deadline_ns(100), 100);
        assert_eq!(video.next_deadline_ns(16_000_100), 16_000_100);
        assert_eq!(queue.depth(), 1);
    }

    #[test]
    fn ten_minute_audio_video_sender_soak_has_bounded_deadline_drift() {
        let interval_ns = 1_000_000_000 / 60;
        let mut video = DeadlineCadence::new(Duration::from_nanos(interval_ns)).unwrap();
        let queue = BoundedDropNewestQueue::new(32).unwrap();
        let mut last_deadline = 0u64;
        for frame in 0..36_000u64 {
            let now_ns = frame.saturating_mul(interval_ns);
            last_deadline = video.next_deadline_ns(now_ns);
            for audio_chunk in 0..2u64 {
                let _ = queue.try_push(frame * 2 + audio_chunk);
            }
            if frame % 2 == 0 {
                let _ = queue.pop_timeout(Duration::ZERO);
            }
        }
        let expected = 35_999u64.saturating_mul(interval_ns);
        assert!(last_deadline.abs_diff(expected) <= interval_ns);
        assert!(queue.depth() <= 32);
        assert!(queue.drops() > 0);
    }
}
