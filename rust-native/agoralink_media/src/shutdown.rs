use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REASON_NONE: u8 = 0;
pub const WORKER_OWNERSHIP_FAILURE_TAG: &str = "worker-ownership-retained";

static CTRL_C_REQUESTED: AtomicBool = AtomicBool::new(false);
static CTRL_C_COUNT: AtomicUsize = AtomicUsize::new(0);
static CONSOLE_CTRL_USERS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum StopReason {
    Duration = 1,
    CtrlC = 2,
    WindowClosed = 3,
    ReceiverReadinessTimeout = 4,
    ProfileTransitionFailed = 5,
    QsvTimeout = 6,
    InternalError = 7,
    LocalStop = 8,
    PeerClosed = 9,
    PeerTimeout = 10,
    FatalError = 11,
    StartupFailure = 12,
}

impl StopReason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Duration => "duration",
            Self::CtrlC => "ctrl_c",
            Self::WindowClosed => "window_closed",
            Self::ReceiverReadinessTimeout => "receiver_readiness_timeout",
            Self::ProfileTransitionFailed => "profile_transition_failed",
            Self::QsvTimeout => "qsv_timeout",
            Self::InternalError => "internal_error",
            Self::LocalStop => "local_stop",
            Self::PeerClosed => "peer_closed",
            Self::PeerTimeout => "peer_timeout",
            Self::FatalError => "fatal_error",
            Self::StartupFailure => "startup_failure",
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Duration),
            2 => Some(Self::CtrlC),
            3 => Some(Self::WindowClosed),
            4 => Some(Self::ReceiverReadinessTimeout),
            5 => Some(Self::ProfileTransitionFailed),
            6 => Some(Self::QsvTimeout),
            7 => Some(Self::InternalError),
            8 => Some(Self::LocalStop),
            9 => Some(Self::PeerClosed),
            10 => Some(Self::PeerTimeout),
            11 => Some(Self::FatalError),
            12 => Some(Self::StartupFailure),
            _ => None,
        }
    }

    pub const fn should_notify_peer(self) -> bool {
        !matches!(
            self,
            Self::PeerClosed | Self::PeerTimeout | Self::StartupFailure
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LifecycleState {
    Starting = 0,
    Running = 1,
    Stopping = 2,
    Stopped = 3,
    Failed = 4,
    FailedCleanup = 5,
}

impl LifecycleState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::FailedCleanup => "failed_cleanup",
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Running,
            2 => Self::Stopping,
            3 => Self::Stopped,
            4 => Self::Failed,
            5 => Self::FailedCleanup,
            _ => Self::Starting,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ShutdownConfig {
    pub close_retry_initial: Duration,
    pub close_retry_max: Duration,
    pub close_handshake_timeout: Duration,
    pub peer_stale_after: Duration,
    pub peer_hard_timeout: Duration,
    pub worker_join_timeout: Duration,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            close_retry_initial: Duration::from_millis(150),
            close_retry_max: Duration::from_millis(600),
            close_handshake_timeout: Duration::from_millis(1_500),
            peer_stale_after: Duration::from_secs(2),
            peer_hard_timeout: Duration::from_secs(5),
            worker_join_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
    reason: Arc<AtomicU8>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            reason: Arc::new(AtomicU8::new(REASON_NONE)),
        }
    }

    pub fn cancel(&self, reason: StopReason) -> bool {
        let first = self
            .reason
            .compare_exchange(
                REASON_NONE,
                reason as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        self.cancelled.store(true, Ordering::Release);
        first
    }

    pub fn is_cancelled(&self) -> bool {
        if ctrl_c_requested() {
            self.cancel(StopReason::CtrlC);
        }
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn reason(&self) -> Option<StopReason> {
        if ctrl_c_requested() {
            self.cancel(StopReason::CtrlC);
        }
        StopReason::from_code(self.reason.load(Ordering::Acquire))
    }

    pub fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct ShutdownCoordinator {
    token: CancellationToken,
    lifecycle: Arc<AtomicU8>,
}

#[derive(Debug)]
pub struct RuntimeEventContext {
    run_id: u64,
    started_at: Instant,
    sequence: AtomicU64,
}

impl RuntimeEventContext {
    pub fn new(run_id: u64) -> Self {
        Self {
            run_id,
            started_at: Instant::now(),
            sequence: AtomicU64::new(0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn json_fragment(
        &self,
        _role: &str,
        stream_id: u8,
        video_session_id: Option<u64>,
        profile_id: u64,
        counter_scope: &str,
        state: &str,
        cancellation: &CancellationToken,
    ) -> String {
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let wall_timestamp_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        let elapsed_ms = self
            .started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let session = video_session_id.map_or_else(|| "null".to_string(), |id| id.to_string());
        let reason = cancellation.reason().map_or_else(
            || "null".to_string(),
            |reason| format!(r#""{}""#, reason.name()),
        );
        format!(
            r#""run_id":{},"stats_sequence":{},"wall_timestamp_us":{},"monotonic_elapsed_ms":{},"stream_id":{},"video_session_id":{},"profile_id":{},"counter_scope":"{}","state":"{}","stop_requested":{},"stop_reason":{}"#,
            self.run_id,
            sequence,
            wall_timestamp_us,
            elapsed_ms,
            stream_id,
            session,
            profile_id,
            counter_scope,
            state,
            cancellation.is_cancelled(),
            reason,
        )
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            lifecycle: Arc::new(AtomicU8::new(LifecycleState::Starting as u8)),
        }
    }

    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    pub fn request_stop(&self, reason: StopReason) -> bool {
        self.token.cancel(reason)
    }

    pub fn mark_running(&self) -> bool {
        self.lifecycle
            .compare_exchange(
                LifecycleState::Starting as u8,
                LifecycleState::Running as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn mark_failed(&self) {
        loop {
            let state = self.state();
            if matches!(
                state,
                LifecycleState::Stopping
                    | LifecycleState::Stopped
                    | LifecycleState::Failed
                    | LifecycleState::FailedCleanup
            ) {
                return;
            }
            if self
                .lifecycle
                .compare_exchange(
                    state as u8,
                    LifecycleState::Failed as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return;
            }
        }
    }

    pub fn begin_cleanup(&self) -> bool {
        loop {
            let state = self.state();
            if matches!(
                state,
                LifecycleState::Stopping | LifecycleState::Stopped | LifecycleState::FailedCleanup
            ) {
                return false;
            }
            if self
                .lifecycle
                .compare_exchange(
                    state as u8,
                    LifecycleState::Stopping as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    pub fn finish_cleanup(&self, all_workers_stopped: bool) {
        let final_state = if all_workers_stopped {
            LifecycleState::Stopped
        } else {
            LifecycleState::FailedCleanup
        };
        self.lifecycle.store(final_state as u8, Ordering::Release);
    }

    pub fn state(&self) -> LifecycleState {
        LifecycleState::from_code(self.lifecycle.load(Ordering::Acquire))
    }
}

pub fn ctrl_c_requested() -> bool {
    CTRL_C_REQUESTED.load(Ordering::Acquire)
}

pub fn classify_error(error: &str) -> StopReason {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("receiver-readiness-timeout")
        || normalized.contains("receiver readiness")
    {
        StopReason::ReceiverReadinessTimeout
    } else if normalized.contains("profile-control")
        || normalized.contains("profile-transition")
        || normalized.contains("receiver-transition")
        || normalized.contains("receiver-first-idr-timeout")
        || normalized.contains("receiver-first-render-timeout")
        || normalized.contains("receiver-settle")
    {
        StopReason::ProfileTransitionFailed
    } else if normalized.contains("async mft")
        || normalized.contains("qsv") && normalized.contains("timeout")
    {
        StopReason::QsvTimeout
    } else {
        StopReason::InternalError
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerJoinStatus {
    Joined,
    Panicked,
    TimedOut,
    NotStarted,
}

impl WorkerJoinStatus {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Panicked => "panicked",
            Self::TimedOut => "timed_out",
            Self::NotStarted => "not_started",
        }
    }

    pub const fn clean(self) -> bool {
        matches!(self, Self::Joined | Self::NotStarted)
    }
}

pub fn try_join_until(handle: &mut Option<JoinHandle<()>>, deadline: Instant) -> WorkerJoinStatus {
    let Some(worker) = handle.as_ref() else {
        return WorkerJoinStatus::NotStarted;
    };
    while !worker.is_finished() {
        let now = Instant::now();
        if now >= deadline {
            return WorkerJoinStatus::TimedOut;
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(5)),
        );
    }
    let worker = handle
        .take()
        .expect("finished worker handle remains owned until join");
    match worker.join() {
        Ok(()) => WorkerJoinStatus::Joined,
        Err(_) => WorkerJoinStatus::Panicked,
    }
}

#[derive(Debug)]
struct RetainedWorker {
    name: String,
    handle: JoinHandle<()>,
}

fn retained_workers() -> &'static Mutex<Vec<RetainedWorker>> {
    static WORKERS: OnceLock<Mutex<Vec<RetainedWorker>>> = OnceLock::new();
    WORKERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Transfers an unjoined worker to process-level ownership.
///
/// This is only an escalation path after cooperative cancellation and a bounded
/// join attempt have failed. It prevents `JoinHandle::drop` from silently
/// detaching a native-resource worker while the caller unwinds. Screen runtime
/// treats any retained worker as `failed_cleanup` and returns a non-zero error.
pub fn retain_unjoined_worker(
    name: impl Into<String>,
    handle: &mut Option<JoinHandle<()>>,
) -> bool {
    let Some(handle) = handle.take() else {
        return false;
    };
    let retained = RetainedWorker {
        name: name.into(),
        handle,
    };
    retained_workers()
        .lock()
        .expect("retained worker registry lock poisoned")
        .push(retained);
    true
}

pub fn retained_worker_count() -> usize {
    let _ = reap_finished_retained_workers();
    retained_workers()
        .lock()
        .map(|workers| workers.len())
        .unwrap_or(0)
}

pub fn retained_worker_names() -> Vec<String> {
    let _ = reap_finished_retained_workers();
    retained_workers()
        .lock()
        .map(|workers| workers.iter().map(|worker| worker.name.clone()).collect())
        .unwrap_or_default()
}

pub fn worker_ownership_failed(error: &str) -> bool {
    error.contains(WORKER_OWNERSHIP_FAILURE_TAG)
}

pub const fn terminal_event_type(all_workers_stopped: bool) -> &'static str {
    if all_workers_stopped {
        "NATIVE_SCREEN_STOPPED"
    } else {
        "NATIVE_SCREEN_SHUTDOWN_FAILED"
    }
}

pub const fn terminal_lifecycle_name(all_workers_stopped: bool) -> &'static str {
    if all_workers_stopped {
        "stopped"
    } else {
        "failed_cleanup"
    }
}

/// Joins retained workers that have since completed. Joining happens outside
/// the registry lock so no global lock is held across a blocking operation.
pub fn reap_finished_retained_workers() -> usize {
    let finished = {
        let Ok(mut workers) = retained_workers().lock() else {
            return 0;
        };
        let mut finished = Vec::new();
        let mut index = 0;
        while index < workers.len() {
            if workers[index].handle.is_finished() {
                finished.push(workers.swap_remove(index));
            } else {
                index += 1;
            }
        }
        finished
    };
    let count = finished.len();
    for worker in finished {
        let _ = worker.handle.join();
    }
    count
}

pub struct ConsoleCtrlGuard;

impl ConsoleCtrlGuard {
    pub fn install() -> Result<Self, String> {
        if CONSOLE_CTRL_USERS.fetch_add(1, Ordering::AcqRel) == 0 {
            CTRL_C_REQUESTED.store(false, Ordering::Release);
            CTRL_C_COUNT.store(0, Ordering::Release);
            if let Err(error) = install_console_handler() {
                CONSOLE_CTRL_USERS.fetch_sub(1, Ordering::AcqRel);
                return Err(error);
            }
        }
        Ok(Self)
    }
}

impl Drop for ConsoleCtrlGuard {
    fn drop(&mut self) {
        if CONSOLE_CTRL_USERS.fetch_sub(1, Ordering::AcqRel) == 1 {
            uninstall_console_handler();
        }
    }
}

#[cfg(windows)]
fn install_console_handler() -> Result<(), String> {
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), true) }
        .map_err(|error| format!("SetConsoleCtrlHandler failed: {error}"))
}

#[cfg(not(windows))]
fn install_console_handler() -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn uninstall_console_handler() {
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
}

#[cfg(not(windows))]
fn uninstall_console_handler() {}

#[cfg(windows)]
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
    use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT};

    if !matches!(
        ctrl_type,
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT
    ) {
        return false.into();
    }
    CTRL_C_REQUESTED.store(true, Ordering::Release);
    // Returning FALSE on the second signal delegates to the default handler, providing
    // an explicit force-exit escape hatch if bounded cleanup itself cannot complete.
    (CTRL_C_COUNT.fetch_add(1, Ordering::AcqRel) == 0).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::sync::mpsc;

    #[test]
    fn cancellation_reason_is_first_writer_wins() {
        let token = CancellationToken::new();
        token.cancel(StopReason::CtrlC);
        token.cancel(StopReason::InternalError);
        assert!(token.is_cancelled());
        assert_eq!(token.reason(), Some(StopReason::CtrlC));
    }

    #[test]
    fn shutdown_cleanup_is_single_entry_and_idempotent() {
        let coordinator = ShutdownCoordinator::new();
        assert_eq!(coordinator.state(), LifecycleState::Starting);
        assert!(coordinator.mark_running());
        assert_eq!(coordinator.state(), LifecycleState::Running);
        assert!(coordinator.request_stop(StopReason::LocalStop));
        assert!(!coordinator.request_stop(StopReason::FatalError));
        assert!(coordinator.begin_cleanup());
        assert!(!coordinator.begin_cleanup());
        assert_eq!(coordinator.state(), LifecycleState::Stopping);
        coordinator.finish_cleanup(true);
        coordinator.finish_cleanup(true);
        assert_eq!(coordinator.state(), LifecycleState::Stopped);
        assert_eq!(coordinator.token().reason(), Some(StopReason::LocalStop));
    }

    #[test]
    fn finite_join_reports_success_and_timeout() {
        let mut joined_handle = Some(thread::spawn(|| {}));
        let joined = try_join_until(
            &mut joined_handle,
            Instant::now() + Duration::from_millis(100),
        );
        assert_eq!(joined, WorkerJoinStatus::Joined);
        assert!(joined_handle.is_none());

        let mut timeout_handle = Some(thread::spawn(|| thread::sleep(Duration::from_millis(30))));
        let timeout = try_join_until(
            &mut timeout_handle,
            Instant::now() + Duration::from_millis(1),
        );
        assert_eq!(timeout, WorkerJoinStatus::TimedOut);
        assert!(
            timeout_handle.is_some(),
            "timeout must retain JoinHandle ownership"
        );
        assert_eq!(
            try_join_until(&mut timeout_handle, Instant::now() + Duration::from_secs(1)),
            WorkerJoinStatus::Joined
        );
        assert!(timeout_handle.is_none());
    }

    #[test]
    fn failed_worker_cleanup_never_reports_stopped() {
        let coordinator = ShutdownCoordinator::new();
        assert!(coordinator.mark_running());
        coordinator.request_stop(StopReason::LocalStop);
        assert!(coordinator.begin_cleanup());
        coordinator.finish_cleanup(false);
        assert_eq!(coordinator.state(), LifecycleState::FailedCleanup);
        assert_eq!(terminal_event_type(false), "NATIVE_SCREEN_SHUTDOWN_FAILED");
        assert_eq!(terminal_lifecycle_name(false), "failed_cleanup");
    }

    #[test]
    fn stop_reason_classification_is_stable() {
        assert_eq!(
            classify_error("receiver-readiness-timeout"),
            StopReason::ReceiverReadinessTimeout
        );
        assert_eq!(
            classify_error("async MFT need-input wait timed out"),
            StopReason::QsvTimeout
        );
        assert_eq!(
            classify_error("receiver-settle-recovery-limit"),
            StopReason::ProfileTransitionFailed
        );
        assert_eq!(classify_error("socket failed"), StopReason::InternalError);
    }

    #[test]
    fn cancelled_worker_exits_and_releases_udp_socket() {
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (address_sender, address_receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.set_nonblocking(true).unwrap();
            address_sender.send(socket.local_addr().unwrap()).unwrap();
            while !worker_token.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
        });
        let address = address_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        token.cancel(StopReason::CtrlC);
        let mut worker = Some(worker);
        assert_eq!(
            try_join_until(&mut worker, Instant::now() + Duration::from_secs(1)),
            WorkerJoinStatus::Joined
        );
        let rebound = UdpSocket::bind(address).expect("cancelled worker must release UDP socket");
        drop(rebound);
        assert_eq!(token.reason(), Some(StopReason::CtrlC));
    }

    #[test]
    fn shared_cancellation_stops_all_workers_within_one_deadline() {
        let token = CancellationToken::new();
        let mut workers = (0..6)
            .map(|_| {
                let worker_token = token.clone();
                Some(thread::spawn(move || {
                    while !worker_token.is_cancelled() {
                        thread::yield_now();
                    }
                }))
            })
            .collect::<Vec<_>>();
        token.cancel(StopReason::CtrlC);
        for worker in &mut workers {
            assert_eq!(
                try_join_until(worker, Instant::now() + Duration::from_secs(1)),
                WorkerJoinStatus::Joined
            );
        }
        assert_eq!(token.reason(), Some(StopReason::CtrlC));
    }

    #[test]
    fn one_slow_worker_does_not_consume_another_workers_join_budget() {
        let release_slow = Arc::new(AtomicBool::new(false));
        let slow_release = Arc::clone(&release_slow);
        let mut slow = Some(thread::spawn(move || {
            while !slow_release.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }));
        let mut fast = Some(thread::spawn(|| {}));

        assert_eq!(
            try_join_until(&mut slow, Instant::now() + Duration::from_millis(1)),
            WorkerJoinStatus::TimedOut
        );
        assert!(slow.is_some());
        assert_eq!(
            try_join_until(&mut fast, Instant::now() + Duration::from_secs(1)),
            WorkerJoinStatus::Joined
        );
        release_slow.store(true, Ordering::Release);
        assert_eq!(
            try_join_until(&mut slow, Instant::now() + Duration::from_secs(1)),
            WorkerJoinStatus::Joined
        );
    }

    #[test]
    fn timed_out_worker_can_be_retained_then_reaped_without_detach() {
        let baseline = retained_worker_count();
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let mut worker = Some(thread::spawn(move || {
            while !worker_release.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }));
        assert_eq!(
            try_join_until(&mut worker, Instant::now() + Duration::from_millis(1)),
            WorkerJoinStatus::TimedOut
        );
        assert!(retain_unjoined_worker("retention-test", &mut worker));
        assert!(worker.is_none());
        assert_eq!(retained_worker_count(), baseline + 1);

        release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while retained_worker_count() != baseline && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(retained_worker_count(), baseline);
    }

    #[test]
    fn ctrl_c_and_window_close_reasons_remain_distinct() {
        let ctrl_c = CancellationToken::new();
        ctrl_c.cancel(StopReason::CtrlC);
        let window = CancellationToken::new();
        window.cancel(StopReason::WindowClosed);
        assert_eq!(ctrl_c.reason().unwrap().name(), "ctrl_c");
        assert_eq!(window.reason().unwrap().name(), "window_closed");
    }

    #[test]
    fn runtime_event_context_uses_monotonic_sequence_and_first_stop_reason() {
        let token = CancellationToken::new();
        let context = RuntimeEventContext::new(77);
        let first = context.json_fragment("sender", 1, Some(9), 3, "interval", "running", &token);
        token.cancel(StopReason::Duration);
        let second = context.json_fragment("sender", 1, Some(9), 3, "run_total", "stopped", &token);
        assert!(first.contains(r#""stats_sequence":1"#));
        assert!(first.contains(r#""stop_requested":false"#));
        assert!(second.contains(r#""stats_sequence":2"#));
        assert!(second.contains(r#""stop_reason":"duration""#));
    }
}
