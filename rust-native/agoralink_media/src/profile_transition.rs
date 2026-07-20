use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::media_control::{ProfileAck, ProfileChange};

pub const CONTROL_ACK_DEADLINE: Duration = Duration::from_millis(2_500);
pub const CONTROL_RETRY_INTERVAL: Duration = Duration::from_millis(250);
pub const CONTROL_MAX_ATTEMPTS: u32 = 10;
pub const RECEIVER_PENDING_DEADLINE: Duration = Duration::from_secs(3);
pub const RECEIVER_FIRST_IDR_DEADLINE: Duration = Duration::from_millis(2_500);
pub const RECEIVER_FIRST_RENDER_DEADLINE: Duration = Duration::from_millis(1_500);
pub const RECEIVER_SETTLE_DEADLINE: Duration = Duration::from_millis(2_500);
pub const RECEIVER_READINESS_DEADLINE: Duration = Duration::from_secs(9);
pub const RECEIVER_TRANSITION_HARD_DEADLINE: Duration = Duration::from_secs(8);
pub const RECEIVER_MAX_SETTLE_RESTARTS: u32 = 1;
pub const SENDER_TRANSITION_TOTAL_DEADLINE: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TransitionFailureTelemetry {
    pub timeout_count: u64,
    pub failure_count: u64,
    pub last_failure_reason: Option<String>,
    pub last_failure_stage: Option<String>,
}

impl TransitionFailureTelemetry {
    pub fn clear_current(&mut self) {
        self.last_failure_reason = None;
        self.last_failure_stage = None;
    }

    pub fn record(&mut self, stage: &str, reason: &str, timed_out: bool) {
        if timed_out {
            self.timeout_count = self.timeout_count.saturating_add(1);
        }
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_failure_stage = Some(stage.to_string());
        self.last_failure_reason = Some(reason.to_string());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SenderTransitionPhase {
    Idle,
    ControlPending,
    AwaitControlAck,
    ActivateNewSession,
    AwaitReceiverReadiness,
    Committed,
    Rollback,
    Failed,
}

impl SenderTransitionPhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::ControlPending => "control-pending",
            Self::AwaitControlAck => "await-control-ack",
            Self::ActivateNewSession => "activate-new-session",
            Self::AwaitReceiverReadiness => "await-receiver-readiness",
            Self::Committed => "committed",
            Self::Rollback => "rollback",
            Self::Failed => "failed",
        }
    }
}

impl Default for SenderTransitionPhase {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiverTransitionPhase {
    Idle,
    AwaitNewSessionData,
    NewSessionActivated,
    AwaitFirstIdr,
    AwaitFirstRenderedFrame,
    Settling,
    Committed,
    Failed,
}

impl ReceiverTransitionPhase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::AwaitNewSessionData => "await-new-session-data",
            Self::NewSessionActivated => "new-session-activated",
            Self::AwaitFirstIdr => "await-first-idr",
            Self::AwaitFirstRenderedFrame => "await-first-rendered-frame",
            Self::Settling => "settling",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }
}

impl Default for ReceiverTransitionPhase {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReceiverReadinessObservation {
    pub first_idr_decoded: bool,
    pub render_initialized: bool,
    pub frames_rendered_total: u64,
    pub progressing: bool,
    pub damaged_gop_total: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiverReadinessOutcome {
    Pending,
    Committed,
    Failed(&'static str),
}

#[derive(Clone, Debug)]
pub struct ReceiverReadinessTransition {
    pub phase: ReceiverTransitionPhase,
    pub started_at: Instant,
    pub overall_deadline: Instant,
    pub phase_deadline: Instant,
    pub first_idr_at: Option<Instant>,
    pub first_rendered_at: Option<Instant>,
    pub settle_started_at: Option<Instant>,
    pub settle_windows: u32,
    pub recovery_count: u32,
    pub settle_restart_count: u32,
    pub failure_stage: Option<&'static str>,
    pub failure_reason: Option<&'static str>,
    pub first_idr_wait_ms: Option<f64>,
    rendered_baseline: u64,
    last_damaged_gop_total: u64,
    recovering_after_damage: bool,
}

impl ReceiverReadinessTransition {
    pub fn begin(now: Instant, rendered_baseline: u64, damaged_gop_total: u64) -> Self {
        Self {
            phase: ReceiverTransitionPhase::AwaitFirstIdr,
            started_at: now,
            overall_deadline: now + RECEIVER_TRANSITION_HARD_DEADLINE,
            phase_deadline: now + RECEIVER_FIRST_IDR_DEADLINE,
            first_idr_at: None,
            first_rendered_at: None,
            settle_started_at: None,
            settle_windows: 0,
            recovery_count: 0,
            settle_restart_count: 0,
            failure_stage: None,
            failure_reason: None,
            first_idr_wait_ms: None,
            rendered_baseline,
            last_damaged_gop_total: damaged_gop_total,
            recovering_after_damage: false,
        }
    }

    pub fn observe(
        &mut self,
        now: Instant,
        observation: ReceiverReadinessObservation,
    ) -> ReceiverReadinessOutcome {
        if matches!(
            self.phase,
            ReceiverTransitionPhase::Committed | ReceiverTransitionPhase::Failed
        ) {
            return if self.phase == ReceiverTransitionPhase::Committed {
                ReceiverReadinessOutcome::Committed
            } else {
                ReceiverReadinessOutcome::Failed(
                    self.failure_reason.unwrap_or("receiver-readiness-timeout"),
                )
            };
        }

        if observation.damaged_gop_total > self.last_damaged_gop_total {
            self.last_damaged_gop_total = observation.damaged_gop_total;
            if matches!(
                self.phase,
                ReceiverTransitionPhase::AwaitFirstRenderedFrame
                    | ReceiverTransitionPhase::Settling
            ) {
                self.recovery_count = self.recovery_count.saturating_add(1);
                self.recovering_after_damage = true;
                self.phase = ReceiverTransitionPhase::AwaitFirstIdr;
                self.phase_deadline =
                    (now + RECEIVER_FIRST_IDR_DEADLINE).min(self.overall_deadline);
                self.first_rendered_at = None;
                self.settle_started_at = None;
                self.settle_windows = 0;
                self.rendered_baseline = observation.frames_rendered_total;
            }
        }

        if now >= self.overall_deadline {
            return self.fail("receiver-transition-overall-timeout");
        }

        if self.phase == ReceiverTransitionPhase::AwaitFirstIdr && observation.first_idr_decoded {
            if self.recovering_after_damage {
                if self.settle_restart_count >= RECEIVER_MAX_SETTLE_RESTARTS {
                    return self.fail("receiver-settle-recovery-limit");
                }
                self.settle_restart_count = self.settle_restart_count.saturating_add(1);
                self.recovering_after_damage = false;
            }
            if self.first_idr_at.is_none() {
                self.first_idr_at = Some(now);
                self.first_idr_wait_ms =
                    Some(now.saturating_duration_since(self.started_at).as_secs_f64() * 1_000.0);
            }
            self.phase = ReceiverTransitionPhase::AwaitFirstRenderedFrame;
            self.rendered_baseline = observation.frames_rendered_total;
            self.phase_deadline = (now + RECEIVER_FIRST_RENDER_DEADLINE).min(self.overall_deadline);
        }

        if self.phase == ReceiverTransitionPhase::AwaitFirstRenderedFrame
            && observation.render_initialized
            && observation.frames_rendered_total > self.rendered_baseline
        {
            self.first_rendered_at = Some(now);
            self.settle_started_at = Some(now);
            self.phase = ReceiverTransitionPhase::Settling;
            self.phase_deadline = (now + RECEIVER_SETTLE_DEADLINE).min(self.overall_deadline);
        }

        if self.phase == ReceiverTransitionPhase::Settling {
            if observation.progressing {
                self.settle_windows = self.settle_windows.saturating_add(1);
            } else {
                self.settle_windows = 0;
            }
            if self.settle_windows >= 3 {
                self.phase = ReceiverTransitionPhase::Committed;
                return ReceiverReadinessOutcome::Committed;
            }
        }

        if now >= self.phase_deadline {
            let reason = match self.phase {
                ReceiverTransitionPhase::AwaitFirstIdr => "receiver-first-idr-timeout",
                ReceiverTransitionPhase::AwaitFirstRenderedFrame => "receiver-first-render-timeout",
                ReceiverTransitionPhase::Settling => "receiver-settle-timeout",
                _ => "receiver-readiness-timeout",
            };
            return self.fail(reason);
        }

        ReceiverReadinessOutcome::Pending
    }

    pub fn settle_deadline_remaining(&self, now: Instant) -> Duration {
        if self.phase == ReceiverTransitionPhase::Settling {
            self.phase_deadline.saturating_duration_since(now)
        } else {
            Duration::ZERO
        }
    }

    pub fn overall_deadline_remaining(&self, now: Instant) -> Duration {
        self.overall_deadline.saturating_duration_since(now)
    }

    fn fail(&mut self, reason: &'static str) -> ReceiverReadinessOutcome {
        self.failure_stage = Some(self.phase.name());
        self.failure_reason = Some(reason);
        self.phase = ReceiverTransitionPhase::Failed;
        ReceiverReadinessOutcome::Failed(reason)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileControlKey {
    pub old_session_id: u64,
    pub new_session_id: u64,
    pub change_sequence: u64,
    pub profile_generation: u64,
}

impl ProfileControlKey {
    pub const fn from_change(change: ProfileChange) -> Self {
        Self {
            old_session_id: change.old_session_id,
            new_session_id: change.new_session_id,
            change_sequence: change.change_sequence,
            profile_generation: change.profile_generation,
        }
    }

    pub const fn matches_ack(self, ack: ProfileAck) -> bool {
        self.old_session_id == ack.old_session_id
            && self.new_session_id == ack.new_session_id
            && self.change_sequence == ack.change_sequence
            && self.profile_generation == ack.profile_generation
    }
}

#[derive(Clone, Debug)]
pub struct SenderProfileTransition {
    pub change: ProfileChange,
    pub phase: SenderTransitionPhase,
    pub started_at: Instant,
    pub ack_deadline: Instant,
    pub total_deadline: Instant,
    pub readiness_deadline: Option<Instant>,
    pub next_retry_at: Instant,
    pub attempts: u32,
    pub last_control_sent_at: Option<Instant>,
    pub ack_received_at: Option<Instant>,
    pub failure_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SenderAckDecision {
    Accepted,
    Rejected(u8),
    Unmatched,
}

impl SenderProfileTransition {
    pub fn prepared(change: ProfileChange, now: Instant) -> Self {
        Self {
            change,
            phase: SenderTransitionPhase::ControlPending,
            started_at: now,
            ack_deadline: now + CONTROL_ACK_DEADLINE,
            total_deadline: now + SENDER_TRANSITION_TOTAL_DEADLINE,
            readiness_deadline: None,
            next_retry_at: now,
            attempts: 0,
            last_control_sent_at: None,
            ack_received_at: None,
            failure_reason: None,
        }
    }

    pub const fn key(&self) -> ProfileControlKey {
        ProfileControlKey::from_change(self.change)
    }

    pub fn should_send_control(&self, now: Instant) -> bool {
        matches!(
            self.phase,
            SenderTransitionPhase::ControlPending | SenderTransitionPhase::AwaitControlAck
        ) && self.attempts < CONTROL_MAX_ATTEMPTS
            && now <= self.ack_deadline
            && now >= self.next_retry_at
    }

    pub fn record_control_sent(&mut self, now: Instant) {
        self.attempts = self.attempts.saturating_add(1);
        self.last_control_sent_at = Some(now);
        self.phase = SenderTransitionPhase::AwaitControlAck;
        self.next_retry_at = now + CONTROL_RETRY_INTERVAL;
    }

    pub fn observe_ack(&mut self, ack: ProfileAck, now: Instant) -> SenderAckDecision {
        if !self.key().matches_ack(ack) {
            return SenderAckDecision::Unmatched;
        }
        if !matches!(
            self.phase,
            SenderTransitionPhase::ControlPending | SenderTransitionPhase::AwaitControlAck
        ) {
            return SenderAckDecision::Unmatched;
        }
        if !ack.is_accepted() {
            self.cancel("profile-control-rejected");
            return SenderAckDecision::Rejected(ack.reason_code);
        }
        self.ack_received_at = Some(now);
        self.phase = SenderTransitionPhase::ActivateNewSession;
        SenderAckDecision::Accepted
    }

    #[cfg(test)]
    pub fn accept_ack(&mut self, ack: ProfileAck, now: Instant) -> bool {
        self.observe_ack(ack, now) == SenderAckDecision::Accepted
    }

    pub fn activate(&mut self, now: Instant) -> Result<(), String> {
        if self.phase != SenderTransitionPhase::ActivateNewSession {
            return Err("profile transition cannot activate before a matching ACK".to_string());
        }
        self.phase = SenderTransitionPhase::AwaitReceiverReadiness;
        self.readiness_deadline = Some(now + RECEIVER_READINESS_DEADLINE);
        Ok(())
    }

    pub fn commit(&mut self) -> Result<(), String> {
        if self.phase != SenderTransitionPhase::AwaitReceiverReadiness {
            return Err("profile transition cannot commit before readiness".to_string());
        }
        self.phase = SenderTransitionPhase::Committed;
        Ok(())
    }

    pub fn check_deadline(&mut self, now: Instant) -> Option<&'static str> {
        let reason = if matches!(
            self.phase,
            SenderTransitionPhase::ControlPending | SenderTransitionPhase::AwaitControlAck
        ) && now >= self.ack_deadline
        {
            Some("profile-control-ack-timeout")
        } else if self.phase == SenderTransitionPhase::AwaitReceiverReadiness
            && self
                .readiness_deadline
                .is_some_and(|deadline| now >= deadline)
        {
            Some("receiver-readiness-timeout")
        } else if !matches!(
            self.phase,
            SenderTransitionPhase::Committed
                | SenderTransitionPhase::Rollback
                | SenderTransitionPhase::Failed
        ) && now >= self.total_deadline
        {
            Some("profile-transition-total-timeout")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.phase = if self.ack_received_at.is_none() {
                SenderTransitionPhase::Rollback
            } else {
                SenderTransitionPhase::Failed
            };
            self.failure_reason = Some(reason.to_string());
        }
        reason
    }

    pub fn cancel(&mut self, reason: &str) {
        self.phase = if self.ack_received_at.is_none() {
            SenderTransitionPhase::Rollback
        } else {
            SenderTransitionPhase::Failed
        };
        self.failure_reason = Some(reason.to_string());
    }
}

#[derive(Clone, Debug)]
pub struct PendingReceiverProfile {
    pub change: ProfileChange,
    pub source: SocketAddr,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub ack_count: u32,
    pub phase: ReceiverTransitionPhase,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReceiverProfileControlStats {
    pub mprf_packets_received: u64,
    pub mprf_ack_packets_sent: u64,
    pub mprf_duplicate_reacked: u64,
    pub mprf_pending_expired: u64,
    pub mprf_rejected_foreign_peer: u64,
    pub mprf_rejected_old_session: u64,
    pub mprf_rejected_sequence: u64,
    pub mprf_rejected_generation: u64,
    pub mprf_rejected_invalid_fields: u64,
    pub new_session_activation_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiverControlDecision {
    Ack(ProfileAck),
    Reack(ProfileAck),
    RejectForeignPeer,
    RejectOldSession,
    RejectSequence,
    RejectGeneration,
    RejectInvalidFields,
    RejectPendingConflict,
}

#[derive(Clone, Debug, Default)]
pub struct ReceiverProfileController {
    active_session_id: Option<u64>,
    active_generation: u64,
    highest_accepted_sequence: u64,
    highest_accepted_generation: u64,
    last_committed_key: Option<ProfileControlKey>,
    pending: Option<PendingReceiverProfile>,
    last_expired_new_session_id: Option<u64>,
    stats: ReceiverProfileControlStats,
}

impl ReceiverProfileController {
    pub fn sync_active(&mut self, session_id: u64, generation: u64) {
        if self.active_session_id.is_none() {
            self.active_session_id = Some(session_id);
            self.active_generation = generation;
            self.highest_accepted_generation = generation;
        }
    }

    pub fn observe(
        &mut self,
        change: ProfileChange,
        source: SocketAddr,
        pinned_peer: Option<SocketAddr>,
        now: Instant,
    ) -> ReceiverControlDecision {
        self.stats.mprf_packets_received = self.stats.mprf_packets_received.saturating_add(1);
        if change.validate().is_err() {
            self.stats.mprf_rejected_invalid_fields =
                self.stats.mprf_rejected_invalid_fields.saturating_add(1);
            return ReceiverControlDecision::RejectInvalidFields;
        }
        if pinned_peer != Some(source) {
            self.stats.mprf_rejected_foreign_peer =
                self.stats.mprf_rejected_foreign_peer.saturating_add(1);
            return ReceiverControlDecision::RejectForeignPeer;
        }
        let key = ProfileControlKey::from_change(change);
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| ProfileControlKey::from_change(pending.change) == key)
        {
            let pending = self.pending.as_mut().expect("pending was checked");
            pending.ack_count = pending.ack_count.saturating_add(1);
            self.stats.mprf_duplicate_reacked = self.stats.mprf_duplicate_reacked.saturating_add(1);
            return ReceiverControlDecision::Reack(ProfileAck::accepted(change));
        }
        if self.last_committed_key == Some(key) {
            self.stats.mprf_duplicate_reacked = self.stats.mprf_duplicate_reacked.saturating_add(1);
            return ReceiverControlDecision::Reack(ProfileAck::accepted(change));
        }
        if self.active_session_id != Some(change.old_session_id) {
            self.stats.mprf_rejected_old_session =
                self.stats.mprf_rejected_old_session.saturating_add(1);
            return ReceiverControlDecision::RejectOldSession;
        }
        if change.change_sequence == 0 || change.change_sequence <= self.highest_accepted_sequence {
            self.stats.mprf_rejected_sequence = self.stats.mprf_rejected_sequence.saturating_add(1);
            return ReceiverControlDecision::RejectSequence;
        }
        if change.profile_generation == 0
            || change.profile_generation <= self.highest_accepted_generation
        {
            self.stats.mprf_rejected_generation =
                self.stats.mprf_rejected_generation.saturating_add(1);
            return ReceiverControlDecision::RejectGeneration;
        }
        if self.pending.is_some() {
            self.stats.mprf_rejected_sequence = self.stats.mprf_rejected_sequence.saturating_add(1);
            return ReceiverControlDecision::RejectPendingConflict;
        }
        if self.last_expired_new_session_id == Some(change.new_session_id) {
            self.stats.mprf_rejected_invalid_fields =
                self.stats.mprf_rejected_invalid_fields.saturating_add(1);
            return ReceiverControlDecision::RejectInvalidFields;
        }
        self.highest_accepted_sequence = change.change_sequence;
        self.highest_accepted_generation = change.profile_generation;
        self.pending = Some(PendingReceiverProfile {
            change,
            source,
            created_at: now,
            expires_at: now + RECEIVER_PENDING_DEADLINE,
            ack_count: 1,
            phase: ReceiverTransitionPhase::AwaitNewSessionData,
        });
        ReceiverControlDecision::Ack(ProfileAck::accepted(change))
    }

    pub fn record_ack_sent(&mut self) {
        self.stats.mprf_ack_packets_sent = self.stats.mprf_ack_packets_sent.saturating_add(1);
    }

    pub fn record_invalid_fields(&mut self) {
        self.stats.mprf_packets_received = self.stats.mprf_packets_received.saturating_add(1);
        self.stats.mprf_rejected_invalid_fields =
            self.stats.mprf_rejected_invalid_fields.saturating_add(1);
    }

    pub fn activate_if_pending(
        &mut self,
        session_id: u64,
        source: SocketAddr,
        now: Instant,
    ) -> Option<PendingReceiverProfile> {
        self.expire(now);
        let pending = self.pending.as_ref()?;
        if pending.change.new_session_id != session_id || pending.source != source {
            return None;
        }
        let mut pending = self.pending.take()?;
        pending.phase = ReceiverTransitionPhase::NewSessionActivated;
        self.active_session_id = Some(pending.change.new_session_id);
        self.active_generation = pending.change.profile_generation;
        self.last_committed_key = Some(ProfileControlKey::from_change(pending.change));
        self.stats.new_session_activation_count =
            self.stats.new_session_activation_count.saturating_add(1);
        Some(pending)
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| now >= pending.expires_at)
        {
            return false;
        }
        if let Some(pending) = self.pending.take() {
            self.last_expired_new_session_id = Some(pending.change.new_session_id);
        }
        self.stats.mprf_pending_expired = self.stats.mprf_pending_expired.saturating_add(1);
        true
    }

    #[cfg(test)]
    pub fn active_session_id(&self) -> Option<u64> {
        self.active_session_id
    }

    #[cfg(test)]
    pub fn active_generation(&self) -> u64 {
        self.active_generation
    }

    pub fn pending(&self) -> Option<&PendingReceiverProfile> {
        self.pending.as_ref()
    }

    pub fn stats(&self) -> ReceiverProfileControlStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_control::MEDIA_CONTROL_VERSION;

    fn peer(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    fn change(sequence: u64, generation: u64, old: u64, new: u64) -> ProfileChange {
        ProfileChange {
            version: MEDIA_CONTROL_VERSION,
            old_session_id: old,
            new_session_id: new,
            change_sequence: sequence,
            profile_generation: generation,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_mbps: 4.0,
            timestamp_us: 1,
            reason_code: 2,
        }
    }

    #[test]
    fn all_control_packets_lost_never_activates_sender_session() {
        let start = Instant::now();
        let mut sender = SenderProfileTransition::prepared(change(1, 1, 10, 20), start);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);
        for attempt in 0..CONTROL_MAX_ATTEMPTS {
            let now = start + CONTROL_RETRY_INTERVAL * attempt;
            assert!(sender.should_send_control(now));
            sender.record_control_sent(now);
        }
        assert_eq!(
            sender.check_deadline(start + CONTROL_ACK_DEADLINE),
            Some("profile-control-ack-timeout")
        );
        assert_eq!(sender.phase, SenderTransitionPhase::Rollback);
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(receiver.pending().is_none());
    }

    #[test]
    fn duplicate_control_is_reacked_without_activation() {
        let now = Instant::now();
        let source = peer(5000);
        let packet = change(1, 1, 10, 20);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);
        assert!(matches!(
            receiver.observe(packet, source, Some(source), now),
            ReceiverControlDecision::Ack(_)
        ));
        for _ in 0..9 {
            assert!(matches!(
                receiver.observe(packet, source, Some(source), now),
                ReceiverControlDecision::Reack(_)
            ));
        }
        assert_eq!(receiver.active_session_id(), Some(10));
        assert_eq!(receiver.pending().unwrap().ack_count, 10);
        assert_eq!(receiver.stats().mprf_duplicate_reacked, 9);
    }

    #[test]
    fn higher_sequence_cannot_roll_generation_back() {
        let now = Instant::now();
        let source = peer(5000);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 8);
        let first = change(10, 10, 10, 20);
        assert!(matches!(
            receiver.observe(first, source, Some(source), now),
            ReceiverControlDecision::Ack(_)
        ));
        let activated = receiver
            .activate_if_pending(20, source, now + Duration::from_millis(1))
            .unwrap();
        assert_eq!(activated.change.profile_generation, 10);
        assert_eq!(
            receiver.observe(
                change(11, 9, 20, 30),
                source,
                Some(source),
                now + Duration::from_millis(2)
            ),
            ReceiverControlDecision::RejectGeneration
        );
        assert_eq!(receiver.active_generation(), 10);
    }

    #[test]
    fn forged_peer_cannot_create_pending_transition() {
        let now = Instant::now();
        let source = peer(5000);
        let attacker = peer(5001);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);
        assert_eq!(
            receiver.observe(change(1, 1, 10, 20), attacker, Some(source), now),
            ReceiverControlDecision::RejectForeignPeer
        );
        assert!(receiver.pending().is_none());
        assert_eq!(receiver.active_session_id(), Some(10));
    }

    #[test]
    fn malformed_profile_datagrams_cannot_redirect_or_mutate_receiver_state() {
        let packet = change(1, 1, 10, 20);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);

        let mut malformed_reserved = packet.encode().unwrap();
        malformed_reserved[5] = 1;
        let mut malformed_generation = packet.encode().unwrap();
        malformed_generation[32..40].copy_from_slice(&0u64.to_be_bytes());
        for datagram in [
            malformed_reserved,
            malformed_generation,
            packet.encode().unwrap()[..20].to_vec(),
        ] {
            assert!(ProfileChange::decode(&datagram).is_err());
            // The live receiver invokes observe only after decode succeeds.
            assert_eq!(receiver.active_session_id(), Some(10));
            assert!(receiver.pending().is_none());
            assert_eq!(receiver.stats().mprf_packets_received, 0);
        }
    }

    #[test]
    fn pending_expiry_keeps_old_session_active() {
        let now = Instant::now();
        let source = peer(5000);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);
        receiver.observe(change(1, 1, 10, 20), source, Some(source), now);
        assert!(receiver.expire(now + RECEIVER_PENDING_DEADLINE));
        assert!(receiver.pending().is_none());
        assert_eq!(receiver.active_session_id(), Some(10));
    }

    #[test]
    fn expired_controls_cannot_replay_sequence_or_generation() {
        let start = Instant::now();
        let source = peer(5000);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 1);

        let first = change(10, 10, 10, 20);
        assert!(matches!(
            receiver.observe(first, source, Some(source), start),
            ReceiverControlDecision::Ack(_)
        ));
        assert!(receiver.expire(start + RECEIVER_PENDING_DEADLINE));

        let second_at = start + RECEIVER_PENDING_DEADLINE + Duration::from_millis(1);
        let second = change(11, 11, 10, 30);
        assert!(matches!(
            receiver.observe(second, source, Some(source), second_at),
            ReceiverControlDecision::Ack(_)
        ));
        assert!(receiver.expire(second_at + RECEIVER_PENDING_DEADLINE));

        assert_eq!(
            receiver.observe(
                first,
                source,
                Some(source),
                second_at + RECEIVER_PENDING_DEADLINE + Duration::from_millis(1)
            ),
            ReceiverControlDecision::RejectSequence
        );
        assert_eq!(
            receiver.observe(
                change(12, 10, 10, 40),
                source,
                Some(source),
                second_at + RECEIVER_PENDING_DEADLINE + Duration::from_millis(2)
            ),
            ReceiverControlDecision::RejectGeneration
        );
        assert!(matches!(
            receiver.observe(
                change(12, 12, 10, 40),
                source,
                Some(source),
                second_at + RECEIVER_PENDING_DEADLINE + Duration::from_millis(3)
            ),
            ReceiverControlDecision::Ack(_)
        ));
        assert_eq!(receiver.active_session_id(), Some(10));
    }

    #[test]
    fn matching_ack_is_required_before_sender_activation() {
        let now = Instant::now();
        let packet = change(1, 1, 10, 20);
        let mut sender = SenderProfileTransition::prepared(packet, now);
        assert!(sender.activate(now).is_err());
        let wrong = ProfileAck::accepted(change(2, 2, 10, 30));
        assert!(!sender.accept_ack(wrong, now));
        assert!(sender.accept_ack(ProfileAck::accepted(packet), now));
        sender.activate(now).unwrap();
        assert_eq!(sender.phase, SenderTransitionPhase::AwaitReceiverReadiness);
    }

    #[test]
    fn all_acks_lost_leave_receiver_on_old_session_until_pending_expires() {
        let start = Instant::now();
        let source = peer(5000);
        let packet = change(1, 1, 10, 20);
        let mut sender = SenderProfileTransition::prepared(packet, start);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);

        for attempt in 0..CONTROL_MAX_ATTEMPTS {
            let now = start + CONTROL_RETRY_INTERVAL * attempt;
            assert!(sender.should_send_control(now));
            sender.record_control_sent(now);
            let decision = receiver.observe(packet, source, Some(source), now);
            assert!(matches!(
                decision,
                ReceiverControlDecision::Ack(_) | ReceiverControlDecision::Reack(_)
            ));
            // Fault injection: every ACK is dropped.
        }

        assert_eq!(
            sender.check_deadline(start + CONTROL_ACK_DEADLINE),
            Some("profile-control-ack-timeout")
        );
        assert_eq!(sender.phase, SenderTransitionPhase::Rollback);
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(receiver.pending().is_some());
        assert!(receiver.expire(start + RECEIVER_PENDING_DEADLINE));
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(receiver.pending().is_none());
        assert_eq!(receiver.stats().new_session_activation_count, 0);
    }

    #[test]
    fn successful_transition_commits_each_side_once() {
        let start = Instant::now();
        let source = peer(5000);
        let packet = change(1, 1, 10, 20);
        let mut sender = SenderProfileTransition::prepared(packet, start);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);

        sender.record_control_sent(start);
        let ReceiverControlDecision::Ack(ack) =
            receiver.observe(packet, source, Some(source), start)
        else {
            panic!("receiver did not accept valid MPRF");
        };
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(sender.accept_ack(ack, start + Duration::from_millis(1)));
        sender.activate(start + Duration::from_millis(1)).unwrap();

        let activated = receiver
            .activate_if_pending(20, source, start + Duration::from_millis(50))
            .expect("matching new-session DATA must activate pending profile");
        assert_eq!(activated.change, packet);
        assert_eq!(receiver.active_session_id(), Some(20));
        assert_eq!(receiver.active_generation(), 1);
        assert!(receiver
            .activate_if_pending(20, source, start + Duration::from_millis(51))
            .is_none());
        assert_eq!(receiver.stats().new_session_activation_count, 1);

        sender.commit().unwrap();
        assert_eq!(sender.phase, SenderTransitionPhase::Committed);
    }

    #[test]
    fn delayed_new_session_activates_before_pending_deadline() {
        let start = Instant::now();
        let source = peer(5000);
        let packet = change(1, 1, 10, 20);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);
        assert!(matches!(
            receiver.observe(packet, source, Some(source), start),
            ReceiverControlDecision::Ack(_)
        ));
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(receiver
            .activate_if_pending(10, source, start + Duration::from_secs(2))
            .is_none());
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(receiver
            .activate_if_pending(
                20,
                source,
                start + RECEIVER_PENDING_DEADLINE - Duration::from_millis(1)
            )
            .is_some());
        assert_eq!(receiver.active_session_id(), Some(20));
    }

    #[test]
    fn readiness_timeout_is_finite_and_fails_after_ack() {
        let start = Instant::now();
        let packet = change(1, 1, 10, 20);
        let mut sender = SenderProfileTransition::prepared(packet, start);
        sender.record_control_sent(start);
        assert!(sender.accept_ack(ProfileAck::accepted(packet), start));
        sender.activate(start).unwrap();
        assert_eq!(
            sender.check_deadline(start + RECEIVER_READINESS_DEADLINE),
            Some("receiver-readiness-timeout")
        );
        assert_eq!(sender.phase, SenderTransitionPhase::Failed);
    }

    #[test]
    fn old_data_during_pending_and_stale_data_after_commit_do_not_switch_sessions() {
        let start = Instant::now();
        let source = peer(5000);
        let packet = change(1, 1, 10, 20);
        let mut receiver = ReceiverProfileController::default();
        receiver.sync_active(10, 0);
        receiver.observe(packet, source, Some(source), start);

        assert!(receiver
            .activate_if_pending(10, source, start + Duration::from_millis(1))
            .is_none());
        assert_eq!(receiver.active_session_id(), Some(10));
        assert!(receiver
            .activate_if_pending(20, source, start + Duration::from_millis(2))
            .is_some());
        assert!(receiver
            .activate_if_pending(10, source, start + Duration::from_millis(3))
            .is_none());
        assert_eq!(receiver.active_session_id(), Some(20));
        assert_eq!(receiver.stats().new_session_activation_count, 1);
    }

    #[test]
    fn duplicate_ack_and_cancellation_are_idempotent_and_finite() {
        let start = Instant::now();
        let packet = change(1, 1, 10, 20);
        let mut before_ack = SenderProfileTransition::prepared(packet, start);
        before_ack.cancel("duration-expired-before-ack");
        assert_eq!(before_ack.phase, SenderTransitionPhase::Rollback);

        let ack = ProfileAck::accepted(packet);
        let mut sender = SenderProfileTransition::prepared(packet, start);
        sender.record_control_sent(start);
        assert!(sender.accept_ack(ack, start));
        assert!(!sender.accept_ack(ack, start));
        sender.cancel("duration-expired");
        assert_eq!(sender.phase, SenderTransitionPhase::Failed);
        assert_eq!(sender.failure_reason.as_deref(), Some("duration-expired"));
    }

    #[test]
    fn cancellation_during_control_ack_and_readiness_is_finite() {
        let start = Instant::now();
        let packet = change(1, 1, 10, 20);
        let token = crate::shutdown::CancellationToken::new();

        let mut awaiting_ack = SenderProfileTransition::prepared(packet, start);
        awaiting_ack.record_control_sent(start);
        token.cancel(crate::shutdown::StopReason::CtrlC);
        if token.is_cancelled() {
            awaiting_ack.cancel("profile-control-cancelled");
        }
        assert_eq!(awaiting_ack.phase, SenderTransitionPhase::Rollback);

        let mut awaiting_readiness = SenderProfileTransition::prepared(packet, start);
        awaiting_readiness.record_control_sent(start);
        assert!(awaiting_readiness.accept_ack(ProfileAck::accepted(packet), start));
        awaiting_readiness.activate(start).unwrap();
        if token.is_cancelled() {
            awaiting_readiness.cancel("profile-readiness-cancelled");
        }
        assert_eq!(awaiting_readiness.phase, SenderTransitionPhase::Failed);
    }

    #[test]
    fn damaged_settle_recovers_once_and_commits_within_hard_deadline() {
        let start = Instant::now();
        let mut transition = ReceiverReadinessTransition::begin(start, 0, 0);
        let observation = |first_idr, rendered, damaged| ReceiverReadinessObservation {
            first_idr_decoded: first_idr,
            render_initialized: true,
            frames_rendered_total: rendered,
            progressing: true,
            damaged_gop_total: damaged,
        };

        assert_eq!(
            transition.observe(start + Duration::from_millis(100), observation(true, 0, 0)),
            ReceiverReadinessOutcome::Pending
        );
        transition.observe(start + Duration::from_millis(150), observation(true, 1, 0));
        transition.observe(start + Duration::from_millis(250), observation(true, 2, 0));
        assert_eq!(transition.phase, ReceiverTransitionPhase::Settling);

        transition.observe(start + Duration::from_millis(300), observation(false, 2, 1));
        assert_eq!(transition.phase, ReceiverTransitionPhase::AwaitFirstIdr);
        assert_eq!(transition.recovery_count, 1);
        assert!(
            transition.overall_deadline_remaining(start + Duration::from_millis(300))
                < RECEIVER_TRANSITION_HARD_DEADLINE
        );

        transition.observe(start + Duration::from_millis(400), observation(true, 2, 1));
        assert_eq!(transition.settle_restart_count, 1);
        transition.observe(start + Duration::from_millis(450), observation(true, 3, 1));
        transition.observe(start + Duration::from_millis(550), observation(true, 4, 1));
        assert_eq!(
            transition.observe(start + Duration::from_millis(650), observation(true, 5, 1)),
            ReceiverReadinessOutcome::Committed
        );
        assert_eq!(transition.phase, ReceiverTransitionPhase::Committed);
        assert_eq!(transition.first_idr_wait_ms, Some(100.0));
    }

    #[test]
    fn repeated_damaged_settle_fails_after_one_restart() {
        let start = Instant::now();
        let mut transition = ReceiverReadinessTransition::begin(start, 0, 0);
        let observation = |first_idr, rendered, damaged| ReceiverReadinessObservation {
            first_idr_decoded: first_idr,
            render_initialized: true,
            frames_rendered_total: rendered,
            progressing: true,
            damaged_gop_total: damaged,
        };
        transition.observe(start + Duration::from_millis(10), observation(true, 0, 0));
        transition.observe(start + Duration::from_millis(20), observation(true, 1, 0));
        transition.observe(start + Duration::from_millis(30), observation(false, 1, 1));
        transition.observe(start + Duration::from_millis(40), observation(true, 1, 1));
        transition.observe(start + Duration::from_millis(50), observation(true, 2, 1));
        transition.observe(start + Duration::from_millis(60), observation(false, 2, 2));
        assert_eq!(
            transition.observe(start + Duration::from_millis(70), observation(true, 2, 2)),
            ReceiverReadinessOutcome::Failed("receiver-settle-recovery-limit")
        );
        assert_eq!(transition.phase, ReceiverTransitionPhase::Failed);
        assert_eq!(transition.failure_stage, Some("await-first-idr"));
    }

    #[test]
    fn cancellation_soak_leaves_no_sender_transition_active() {
        let start = Instant::now();
        let packet = change(1, 1, 10, 20);
        for phase in [
            SenderTransitionPhase::ControlPending,
            SenderTransitionPhase::AwaitControlAck,
            SenderTransitionPhase::ActivateNewSession,
            SenderTransitionPhase::AwaitReceiverReadiness,
        ] {
            for _ in 0..100 {
                let mut transition = SenderProfileTransition::prepared(packet, start);
                if phase != SenderTransitionPhase::ControlPending {
                    transition.record_control_sent(start);
                }
                if matches!(
                    phase,
                    SenderTransitionPhase::ActivateNewSession
                        | SenderTransitionPhase::AwaitReceiverReadiness
                ) {
                    assert!(transition.accept_ack(ProfileAck::accepted(packet), start));
                }
                if phase == SenderTransitionPhase::AwaitReceiverReadiness {
                    transition.activate(start).unwrap();
                }
                assert_eq!(transition.phase, phase);
                transition.cancel("ctrl-c-soak");
                assert!(matches!(
                    transition.phase,
                    SenderTransitionPhase::Rollback | SenderTransitionPhase::Failed
                ));
            }
        }
    }

    #[test]
    fn readiness_failure_telemetry_is_committed_before_snapshot() {
        let mut telemetry = TransitionFailureTelemetry::default();
        telemetry.record(
            SenderTransitionPhase::AwaitReceiverReadiness.name(),
            "receiver-readiness-timeout",
            true,
        );
        let snapshot = telemetry.clone();
        assert_eq!(snapshot.timeout_count, 1);
        assert_eq!(snapshot.failure_count, 1);
        assert_eq!(
            snapshot.last_failure_reason.as_deref(),
            Some("receiver-readiness-timeout")
        );
        assert_eq!(
            snapshot.last_failure_stage.as_deref(),
            Some("await-receiver-readiness")
        );
    }

    #[test]
    fn matching_rejected_ack_terminates_control_wait_without_activation() {
        let start = Instant::now();
        let packet = change(1, 1, 10, 20);
        let mut sender = SenderProfileTransition::prepared(packet, start);
        sender.record_control_sent(start);
        let rejected = ProfileAck {
            version: crate::media_control::MEDIA_CONTROL_VERSION,
            status: crate::media_control::PROFILE_ACK_STATUS_REJECTED,
            reason_code: 7,
            old_session_id: packet.old_session_id,
            new_session_id: packet.new_session_id,
            change_sequence: packet.change_sequence,
            profile_generation: packet.profile_generation,
        };
        assert_eq!(
            sender.observe_ack(rejected, start + Duration::from_millis(1)),
            SenderAckDecision::Rejected(7)
        );
        assert_eq!(sender.phase, SenderTransitionPhase::Rollback);
        assert!(sender.activate(start).is_err());
    }

    #[test]
    fn virtual_thirty_minute_fault_soak_has_no_permanent_transition_or_rollback() {
        let start = Instant::now();
        let source = peer(5000);
        let mut receiver = ReceiverProfileController::default();
        let mut active_session = 100u64;
        receiver.sync_active(active_session, 0);
        let mut successful_transitions = 0u64;

        // 120 transition opportunities at 15-second intervals model 30 minutes.
        for sequence in 1..=120u64 {
            let now = start + Duration::from_secs(sequence * 15);
            let new_session = 1_000 + sequence;
            let packet = change(sequence, sequence, active_session, new_session);
            let mut sender = SenderProfileTransition::prepared(packet, now);

            if sequence % 17 == 0 {
                // All MPRF packets are lost.
                for attempt in 0..CONTROL_MAX_ATTEMPTS {
                    let send_at = now + CONTROL_RETRY_INTERVAL * attempt;
                    sender.record_control_sent(send_at);
                }
                assert!(sender.check_deadline(now + CONTROL_ACK_DEADLINE).is_some());
                assert_eq!(sender.phase, SenderTransitionPhase::Rollback);
                assert_eq!(receiver.active_session_id(), Some(active_session));
                continue;
            }

            let mut last_ack = None;
            for attempt in 0..CONTROL_MAX_ATTEMPTS {
                let send_at = now + CONTROL_RETRY_INTERVAL * attempt;
                sender.record_control_sent(send_at);
                let decision = receiver.observe(packet, source, Some(source), send_at);
                last_ack = match decision {
                    ReceiverControlDecision::Ack(ack) | ReceiverControlDecision::Reack(ack) => {
                        Some(ack)
                    }
                    other => panic!("unexpected control decision during soak: {other:?}"),
                };
                if sequence % 13 != 0 {
                    break;
                }
            }

            if sequence % 13 == 0 {
                // MPRF arrives, but all ACKs are lost.
                assert!(sender.check_deadline(now + CONTROL_ACK_DEADLINE).is_some());
                assert_eq!(sender.phase, SenderTransitionPhase::Rollback);
                assert!(receiver.expire(now + RECEIVER_PENDING_DEADLINE));
                assert_eq!(receiver.active_session_id(), Some(active_session));
                continue;
            }

            let ack = last_ack.expect("successful soak transition must have an ACK");
            assert!(sender.accept_ack(ack, now + Duration::from_millis(1)));
            sender.activate(now + Duration::from_millis(1)).unwrap();
            assert!(receiver
                .activate_if_pending(new_session, source, now + Duration::from_secs(2))
                .is_some());
            sender.commit().unwrap();
            active_session = new_session;
            successful_transitions = successful_transitions.saturating_add(1);
            assert_eq!(receiver.active_generation(), sequence);
            assert!(receiver.pending().is_none());
            assert_eq!(sender.phase, SenderTransitionPhase::Committed);
        }

        assert!(successful_transitions > 90);
        assert_eq!(receiver.active_session_id(), Some(active_session));
        assert!(receiver.pending().is_none());
        assert_eq!(
            receiver.stats().new_session_activation_count,
            successful_transitions
        );
    }
}
