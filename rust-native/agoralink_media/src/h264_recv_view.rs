#[derive(Debug)]
pub struct H264RecvViewConfig {
    pub bind: String,
    pub port: u16,
    pub decoder_fps: u32,
    pub duration_sec: Option<u64>,
    pub frame_timeout_ms: u64,
    pub reorder_wait_ms: Option<u64>,
    pub playout_delay_ms: u64,
    pub max_inflight_frames: usize,
    pub max_decode_queue: usize,
    pub strict_decode_order: bool,
    pub drop_damaged_gop: bool,
    pub debug_dump_frames: Option<String>,
    pub debug_dump_limit: usize,
    pub json_interval_ms: u64,
    pub title: String,
    pub render_scale: crate::win32_gdi_viewer::RenderScaleMode,
    pub window_mode: crate::win32_gdi_viewer::WindowMode,
    pub render_backend: crate::video_renderer::RenderBackend,
    pub repair_mode: crate::repair::RepairMode,
    pub nack_delay_ms: u64,
    pub nack_repeat_ms: u64,
    pub nack_max_rounds: u8,
    pub audio_mode: AudioRecvMode,
    pub audio_jitter_buffer_ms: u32,
    pub av_sync_mode: crate::av_sync::AvSyncMode,
    pub display_refresh_detect: crate::display_capability::DisplayRefreshDetect,
    pub capability_feedback_ms: u64,
    pub mode: H264RecvViewMode,
    pub verbose: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum H264RecvViewMode {
    Probe,
    Screen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioRecvMode {
    Off,
    On,
}

impl AudioRecvMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "off" => Ok(Self::Off),
            "on" => Ok(Self::On),
            other => Err(format!("invalid audio mode: {other}")),
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::io::{self, Write};
    use std::net::{SocketAddr, UdpSocket};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{AudioRecvMode, H264RecvViewConfig, H264RecvViewMode};
    use crate::av_sync::{
        AvSyncBypassReason, AvSyncDecision, AvSyncInput, AvSyncMode, AvSyncScheduler, AvSyncState,
        MediaClockJumpDetector,
    };
    use crate::color_spec::{ColorSpec, MediaColorMetadata};
    use crate::decoded_frame_renderer::OwnedBgraFrame;
    use crate::h264_annex_b::{dimensions_from_sps, VideoDimensions};
    use crate::h264_reassembly::{
        DamagedGopStats, DamagedGopTracker, EncodedFrame, H264Reassembler, ReassemblyConfig,
        ReassemblyStats, ReorderWait, TimingMetric,
    };
    use crate::media_clock::{MediaClock, MediaTimestampUs, ReceiverMediaClockAnchor};
    use crate::playout_buffer::{PlayoutBuffer, PlayoutStats};
    use crate::video_renderer::{VideoRenderStats, VideoRenderer};
    use crate::wmf_h264_decoder::{DecodedFrame, WmfH264Decoder, DECODER_NAME};

    const MAX_DATAGRAM_SIZE: usize = 2048;
    const MAX_DATAGRAMS_PER_TICK: usize = 1024;
    enum DecodeQueueItem {
        Reset,
        Frame(EncodedFrame),
    }

    struct DecodeQueue {
        items: VecDeque<DecodeQueueItem>,
        waiting_for_keyframe: bool,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        complete_frame_queue_drops: u64,
        decode_queue_peak: usize,
        last_keyframe_id: Option<u64>,
        damaged_gop: DamagedGopTracker,
    }

    impl DecodeQueue {
        fn new(capacity: usize, drop_damaged_gop: bool) -> Self {
            Self {
                items: VecDeque::with_capacity(capacity + 1),
                waiting_for_keyframe: false,
                frames_predecode_dropped: 0,
                frames_waiting_keyframe_dropped: 0,
                keyframe_recovery_count: 0,
                complete_frame_queue_drops: 0,
                decode_queue_peak: 0,
                last_keyframe_id: None,
                damaged_gop: DamagedGopTracker::new(drop_damaged_gop),
            }
        }

        fn frame_len(&self) -> usize {
            self.items
                .iter()
                .filter(|item| matches!(item, DecodeQueueItem::Frame(_)))
                .count()
        }

        fn begin_keyframe_recovery(&mut self) {
            let dropped = self.frame_len() as u64;
            self.complete_frame_queue_drops += dropped;
            self.frames_predecode_dropped += dropped;
            self.items.clear();
            self.items.push_back(DecodeQueueItem::Reset);
            self.waiting_for_keyframe = true;
            self.keyframe_recovery_count += 1;
        }

        fn begin_damaged_gop_recovery(&mut self, now: Instant, damaged_frame_id: Option<u64>) {
            if !self.damaged_gop.mark_damaged(now, damaged_frame_id) {
                return;
            }
            let dropped = self.frame_len() as u64;
            self.frames_predecode_dropped += dropped;
            self.frames_waiting_keyframe_dropped += dropped;
            self.damaged_gop.discard_queued_frames(dropped);
            self.items.clear();
            self.items.push_back(DecodeQueueItem::Reset);
            self.waiting_for_keyframe = true;
            self.keyframe_recovery_count += 1;
        }

        fn begin_profile_transition(&mut self) {
            let dropped = self.frame_len() as u64;
            self.complete_frame_queue_drops =
                self.complete_frame_queue_drops.saturating_add(dropped);
            self.frames_predecode_dropped = self.frames_predecode_dropped.saturating_add(dropped);
            self.items.clear();
            self.items.push_back(DecodeQueueItem::Reset);
            self.waiting_for_keyframe = true;
            self.damaged_gop.reset_for_profile_change();
        }

        fn enqueue_frame(&mut self, frame: EncodedFrame, max_decode_queue: usize) {
            let was_damaged = self.damaged_gop.waiting_keyframe();
            let Some(frame) = self.damaged_gop.prepare_frame(frame, Instant::now()) else {
                self.frames_predecode_dropped += 1;
                self.frames_waiting_keyframe_dropped += 1;
                return;
            };
            if was_damaged && !self.damaged_gop.waiting_keyframe() {
                self.waiting_for_keyframe = false;
            }
            if self.waiting_for_keyframe {
                if !frame.is_idr() {
                    self.frames_predecode_dropped += 1;
                    self.frames_waiting_keyframe_dropped += 1;
                    return;
                }
                self.last_keyframe_id = Some(frame.frame_id);
                self.waiting_for_keyframe = false;
                self.items.push_back(DecodeQueueItem::Frame(frame));
                self.decode_queue_peak = self.decode_queue_peak.max(self.frame_len());
                return;
            }

            if self.frame_len() >= max_decode_queue {
                self.begin_keyframe_recovery();
                if !frame.is_idr() {
                    self.frames_predecode_dropped += 1;
                    self.frames_waiting_keyframe_dropped += 1;
                    self.complete_frame_queue_drops += 1;
                    return;
                }
            }
            if frame.is_idr() {
                self.last_keyframe_id = Some(frame.frame_id);
                self.waiting_for_keyframe = false;
            }
            self.items.push_back(DecodeQueueItem::Frame(frame));
            self.decode_queue_peak = self.decode_queue_peak.max(self.frame_len());
        }

        fn damaged_gop_stats(&self) -> DamagedGopStats {
            self.damaged_gop.stats()
        }
    }

    #[derive(Clone, Copy, Default)]
    struct NetworkSnapshot {
        reassembly: ReassemblyStats,
        session_id: Option<u64>,
        inflight_frames: usize,
        completed_waiting: usize,
        decode_queue: usize,
        decode_queue_peak: usize,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        last_keyframe_id: Option<u64>,
        waiting_keyframe: bool,
        damaged_gop: DamagedGopStats,
        drop_damaged_gop: bool,
        udp_recv_buffer_bytes: i32,
        udp_recv_buffer_bytes_requested: i32,
        udp_recv_loop_overruns: u64,
        complete_frame_queue_drops: u64,
        playout: PlayoutStats,
        playout_buffer_frames: usize,
        playout_delay_ms: u64,
        repair_mode: crate::repair::RepairMode,
        nack_packets_sent: u64,
        nack_items_sent: u64,
        nack_frames_requested: u64,
        nack_rounds_sent: u64,
        repair_packets_received: u64,
        repair_packets_inserted: u64,
        repair_duplicate_packets: u64,
        repair_frames_completed: u64,
        repair_send_errors: u64,
        repair_deadline_missed: u64,
        repair_wait_ms_total: f64,
        repair_wait_ms_max: f64,
        nack_candidate_frames: u64,
        nack_suppressed_progressing_frames: u64,
        nack_suppressed_too_early: u64,
        nack_suppressed_already_requested: u64,
        nack_suppressed_item_limit: u64,
        nack_items_deduped: u64,
        nack_items_per_requested_frame_total: u64,
        nack_items_per_requested_frame_max: u64,
        video_data_packets_received: u64,
        video_fec_packets_received: u64,
        audio_packets_received: u64,
        unknown_packets_received: u64,
        nack_control_packets_received: u64,
        repair_packets_dropped_wrong_type: u64,
        repair_packets_dropped_late: u64,
        repair_packets_dropped_no_frame: u64,
        repair_deadline_ms_total: f64,
        repair_deadline_ms_max: f64,
        repair_deadline_samples: u64,
        repair_unique_packets_received: u64,
        repair_duplicate_packets_received: u64,
        repair_packets_received_after_frame_complete: u64,
        repair_cancelled_frame_complete: u64,
        nack_candidates_first_round: u64,
        nack_candidates_late_discovery: u64,
        missing_first_detected_age: TimingMetric,
        missing_first_detected_to_deadline: TimingMetric,
        first_nack_to_deadline: TimingMetric,
        first_nack_age: TimingMetric,
        first_round_budget: TimingMetric,
        second_round_budget: TimingMetric,
        repair_arrival_to_deadline: TimingMetric,
        video_packet_dispatch_ns_total: u64,
        video_packet_dispatch_count: u64,
        video_packet_dispatch_ns_max: u64,
        media_anchor: ReceiverMediaClockAnchor,
        capability_feedback_sent: u64,
        capability_feedback_send_errors: u64,
        profile_changes_received: u64,
        profile_changes_invalid: u64,
        profile_change_sequence: u64,
        profile_generation: u64,
        profile_target_width: u32,
        profile_target_height: u32,
        profile_target_fps: u32,
        profile_target_bitrate_mbps: f32,
        profile_decoder_resets: u64,
        stale_profile_packets_dropped: u64,
        feedback_sample_eligible: bool,
        receiver_valid_feedback_windows: u32,
        receiver_render_ready: bool,
        receiver_profile_settled: bool,
        receiver_profile_acknowledged: bool,
        receiver_first_idr_decoded: bool,
        receiver_first_frame_rendered: bool,
        profile_transition_active: bool,
        profile_transition_started_us: u64,
        profile_transition_deadline_us: u64,
        profile_transition_phase: crate::profile_transition::ReceiverTransitionPhase,
        transition_timeout_count: u64,
        transition_failure_reason: Option<&'static str>,
        transition_settle_windows: u32,
        transition_settle_duration_ms: u32,
        pending_profile_present: bool,
        pending_profile_age_ms: u64,
        pending_profile_deadline_remaining_ms: u64,
        pending_profile_change_sequence: u64,
        pending_profile_generation: u64,
        pending_profile_old_session_id: u64,
        pending_profile_new_session_id: u64,
        mprf_packets_received: u64,
        mprf_ack_packets_sent: u64,
        mprf_duplicate_reacked: u64,
        mprf_pending_expired: u64,
        mprf_rejected_foreign_peer: u64,
        mprf_rejected_old_session: u64,
        mprf_rejected_sequence: u64,
        mprf_rejected_generation: u64,
        mprf_rejected_invalid_fields: u64,
        new_session_activation_count: u64,
        new_session_first_packet_wait_ms_total: f64,
        new_session_first_packet_wait_ms_max: f64,
        new_session_first_idr_wait_ms: Option<f64>,
        transition_recovery_count: u32,
        transition_settle_restart_count: u32,
        transition_settle_deadline_remaining_ms: u64,
        transition_overall_deadline_remaining_ms: u64,
        transition_failure_stage: Option<&'static str>,
        stream_close_received: u64,
        stream_close_ack_sent: u64,
        stream_close_rejected_pre_session: u64,
        stream_close_rejected_peer: u64,
        stream_close_rejected_session: u64,
        stream_close_rejected_invalid: u64,
        stream_close_duplicate: u64,
        stream_close_sent: u64,
        stream_close_retry_count: u64,
        stream_close_ack_received: bool,
        stream_close_handshake_timeout: bool,
        peer_timeout_triggered: bool,
        peer_last_valid_age_ms: u64,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct ReceiverProfileStats {
        changes_received: u64,
        changes_invalid: u64,
        change_sequence: u64,
        generation: u64,
        target_width: u32,
        target_height: u32,
        target_fps: u32,
        target_bitrate_mbps: f32,
        decoder_resets: u64,
        stale_packets_dropped: u64,
        pending_profile_present: bool,
        pending_profile_age_ms: u64,
        pending_profile_deadline_remaining_ms: u64,
        pending_profile_change_sequence: u64,
        pending_profile_generation: u64,
        pending_profile_old_session_id: u64,
        pending_profile_new_session_id: u64,
        mprf_packets_received: u64,
        mprf_ack_packets_sent: u64,
        mprf_duplicate_reacked: u64,
        mprf_pending_expired: u64,
        mprf_rejected_foreign_peer: u64,
        mprf_rejected_old_session: u64,
        mprf_rejected_sequence: u64,
        mprf_rejected_generation: u64,
        mprf_rejected_invalid_fields: u64,
        new_session_activation_count: u64,
        new_session_first_packet_wait_ms_total: f64,
        new_session_first_packet_wait_ms_max: f64,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct ReceiverRepairStats {
        nack_packets_sent: u64,
        nack_items_sent: u64,
        nack_frames_requested: u64,
        nack_rounds_sent: u64,
        repair_packets_received: u64,
        repair_packets_inserted: u64,
        repair_duplicate_packets: u64,
        repair_frames_completed: u64,
        repair_send_errors: u64,
        repair_deadline_missed: u64,
        repair_wait_ms_total: f64,
        repair_wait_ms_max: f64,
        nack_candidate_frames: u64,
        nack_suppressed_progressing_frames: u64,
        nack_suppressed_too_early: u64,
        nack_suppressed_already_requested: u64,
        nack_suppressed_item_limit: u64,
        nack_items_deduped: u64,
        nack_items_per_requested_frame_total: u64,
        nack_items_per_requested_frame_max: u64,
        repair_deadline_ms_total: f64,
        repair_deadline_ms_max: f64,
        repair_deadline_samples: u64,
        repair_unique_packets_received: u64,
        repair_duplicate_packets_received: u64,
        repair_packets_received_after_frame_complete: u64,
        repair_cancelled_frame_complete: u64,
        nack_candidates_first_round: u64,
        nack_candidates_late_discovery: u64,
        missing_first_detected_age: TimingMetric,
        missing_first_detected_to_deadline: TimingMetric,
        first_nack_to_deadline: TimingMetric,
        first_nack_age: TimingMetric,
        first_round_budget: TimingMetric,
        second_round_budget: TimingMetric,
        repair_arrival_to_deadline: TimingMetric,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct PacketDispatchStats {
        video_data_packets_received: u64,
        video_fec_packets_received: u64,
        audio_packets_received: u64,
        unknown_packets_received: u64,
        nack_control_packets_received: u64,
        repair_packets_dropped_wrong_type: u64,
        repair_packets_dropped_late: u64,
        repair_packets_dropped_no_frame: u64,
        stream_close_received: u64,
        stream_close_ack_sent: u64,
        stream_close_rejected_pre_session: u64,
        stream_close_rejected_peer: u64,
        stream_close_rejected_session: u64,
        stream_close_rejected_invalid: u64,
        stream_close_duplicate: u64,
        stream_close_sent: u64,
        stream_close_retry_count: u64,
        stream_close_ack_received: bool,
        stream_close_handshake_timeout: bool,
        peer_timeout_triggered: bool,
        peer_last_valid_age_ms: u64,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum IncomingStreamCloseDecision {
        Accept(crate::media_control::StreamClose),
        Duplicate(crate::media_control::StreamClose),
        RejectPreSession,
        RejectPeer,
        RejectSession,
        RejectInvalid,
    }

    fn classify_incoming_stream_close(
        datagram: &[u8],
        source: SocketAddr,
        pinned_peer: Option<SocketAddr>,
        active_session_id: Option<u64>,
        accepted_close: Option<(SocketAddr, crate::media_control::StreamClose)>,
    ) -> IncomingStreamCloseDecision {
        let Ok(close) = crate::media_control::StreamClose::decode(datagram) else {
            return IncomingStreamCloseDecision::RejectInvalid;
        };
        if pinned_peer.is_none() || active_session_id.is_none() || active_session_id == Some(0) {
            return IncomingStreamCloseDecision::RejectPreSession;
        }
        if pinned_peer != Some(source) {
            return IncomingStreamCloseDecision::RejectPeer;
        }
        if close.stream_id != crate::STREAM_VIDEO
            || close.video_session_id == 0
            || active_session_id != Some(close.video_session_id)
        {
            return IncomingStreamCloseDecision::RejectSession;
        }
        if let Some((accepted_source, accepted)) = accepted_close {
            if accepted_source == source
                && accepted.video_session_id == close.video_session_id
                && accepted.close_id == close.close_id
            {
                return IncomingStreamCloseDecision::Duplicate(close);
            }
            return IncomingStreamCloseDecision::RejectInvalid;
        }
        IncomingStreamCloseDecision::Accept(close)
    }

    fn classify_short_or_nack(datagram: &[u8]) -> (bool, bool) {
        let nack = datagram.len() >= 4 && &datagram[..4] == b"NACK";
        (datagram.len() < 4 || nack, nack)
    }

    fn send_stream_close_ack(
        socket: &UdpSocket,
        source: SocketAddr,
        close: crate::media_control::StreamClose,
    ) -> bool {
        close
            .ack()
            .encode()
            .ok()
            .and_then(|bytes| {
                socket
                    .send_to(&bytes, source)
                    .ok()
                    .map(|sent| sent == bytes.len())
            })
            .unwrap_or(false)
    }

    #[derive(Default)]
    struct SharedNetworkState {
        snapshot: NetworkSnapshot,
        error: Option<String>,
    }

    #[derive(Clone, Default)]
    struct CapabilityFeedbackSource {
        render: VideoRenderStats,
        active_render_fps: f64,
        decoder_input_fps: f64,
        decode_queue_drops_total: u64,
        render_replacements_total: u64,
        repair_deadline_missed_total: u64,
        damaged_gop_total: u64,
        packets_lost_total: u64,
        session_id: Option<u64>,
        profile_generation: u64,
        frames_rendered_total: u64,
        rendered_baseline: u64,
        feedback_sample_eligible: bool,
        valid_feedback_windows: u32,
        render_ready: bool,
        profile_settled: bool,
        profile_acknowledged: bool,
        first_idr_decoded: bool,
        first_frame_rendered: bool,
        profile_transition_active: bool,
        profile_transition_started_us: u64,
        profile_transition_deadline_us: u64,
        profile_transition_phase: crate::profile_transition::ReceiverTransitionPhase,
        transition_timeout_count: u64,
        transition_failure_reason: Option<&'static str>,
        transition_settle_windows: u32,
        transition_settle_duration_ms: u32,
        new_session_first_idr_wait_ms: Option<f64>,
        transition_recovery_count: u32,
        transition_settle_restart_count: u32,
        transition_settle_deadline_remaining_ms: u64,
        transition_overall_deadline_remaining_ms: u64,
        transition_failure_stage: Option<&'static str>,
        readiness_transition: Option<crate::profile_transition::ReceiverReadinessTransition>,
    }

    impl CapabilityFeedbackSource {
        fn cancel_profile_transition(&mut self) {
            if !self.profile_transition_active {
                return;
            }
            let failure_stage = self
                .readiness_transition
                .as_ref()
                .map(|transition| transition.phase.name())
                .unwrap_or_else(|| self.profile_transition_phase.name());
            self.profile_transition_active = false;
            self.profile_transition_phase =
                crate::profile_transition::ReceiverTransitionPhase::Failed;
            self.transition_failure_stage = Some(failure_stage);
            self.transition_failure_reason = Some("receiver-transition-cancelled");
            self.transition_settle_deadline_remaining_ms = 0;
            self.transition_overall_deadline_remaining_ms = 0;
            self.feedback_sample_eligible = false;
            self.profile_settled = false;
            self.readiness_transition = None;
        }

        fn write_transition_snapshot(&self, snapshot: &mut NetworkSnapshot) {
            snapshot.feedback_sample_eligible = self.feedback_sample_eligible;
            snapshot.receiver_valid_feedback_windows = self.valid_feedback_windows;
            snapshot.receiver_render_ready = self.render_ready;
            snapshot.receiver_profile_settled = self.profile_settled;
            snapshot.receiver_profile_acknowledged = self.profile_acknowledged;
            snapshot.receiver_first_idr_decoded = self.first_idr_decoded;
            snapshot.receiver_first_frame_rendered = self.first_frame_rendered;
            snapshot.profile_transition_active = self.profile_transition_active;
            snapshot.profile_transition_started_us = self.profile_transition_started_us;
            snapshot.profile_transition_deadline_us = self.profile_transition_deadline_us;
            snapshot.profile_transition_phase = self.profile_transition_phase;
            snapshot.transition_timeout_count = self.transition_timeout_count;
            snapshot.transition_failure_reason = self.transition_failure_reason;
            snapshot.transition_settle_windows = self.transition_settle_windows;
            snapshot.transition_settle_duration_ms = self.transition_settle_duration_ms;
            snapshot.new_session_first_idr_wait_ms = self.new_session_first_idr_wait_ms;
            snapshot.transition_recovery_count = self.transition_recovery_count;
            snapshot.transition_settle_restart_count = self.transition_settle_restart_count;
            snapshot.transition_settle_deadline_remaining_ms =
                self.transition_settle_deadline_remaining_ms;
            snapshot.transition_overall_deadline_remaining_ms =
                self.transition_overall_deadline_remaining_ms;
            snapshot.transition_failure_stage = self.transition_failure_stage;
        }

        fn begin_profile_transition(
            &mut self,
            session_id: u64,
            profile_generation: u64,
            now: Instant,
            now_us: u64,
        ) {
            self.session_id = Some(session_id);
            self.profile_generation = profile_generation;
            self.rendered_baseline = self.frames_rendered_total;
            self.feedback_sample_eligible = false;
            self.valid_feedback_windows = 0;
            self.render_ready = false;
            self.profile_settled = false;
            self.profile_acknowledged = true;
            self.first_idr_decoded = false;
            self.first_frame_rendered = false;
            self.profile_transition_active = true;
            self.profile_transition_started_us = now_us;
            self.profile_transition_deadline_us = now_us.saturating_add(
                crate::profile_transition::RECEIVER_TRANSITION_HARD_DEADLINE.as_micros() as u64,
            );
            self.profile_transition_phase =
                crate::profile_transition::ReceiverTransitionPhase::AwaitFirstIdr;
            self.transition_failure_reason = None;
            self.transition_settle_windows = 0;
            self.transition_settle_duration_ms = 0;
            self.new_session_first_idr_wait_ms = None;
            self.transition_recovery_count = 0;
            self.transition_settle_restart_count = 0;
            self.transition_settle_deadline_remaining_ms = 0;
            self.transition_overall_deadline_remaining_ms =
                crate::profile_transition::RECEIVER_TRANSITION_HARD_DEADLINE
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
            self.transition_failure_stage = None;
            self.readiness_transition = Some(
                crate::profile_transition::ReceiverReadinessTransition::begin(
                    now,
                    self.frames_rendered_total,
                    self.damaged_gop_total,
                ),
            );
        }

        #[allow(clippy::too_many_arguments)]
        fn update_readiness(
            &mut self,
            session_id: Option<u64>,
            profile_generation: u64,
            render_initialized: bool,
            frames_rendered_total: u64,
            first_idr_decoded: bool,
            now: Instant,
        ) {
            let Some(session_id) = session_id else {
                self.feedback_sample_eligible = false;
                self.valid_feedback_windows = 0;
                return;
            };
            if self.profile_transition_phase
                == crate::profile_transition::ReceiverTransitionPhase::Failed
            {
                self.feedback_sample_eligible = false;
                self.profile_settled = false;
                return;
            }
            if self.session_id != Some(session_id) || self.profile_generation != profile_generation
            {
                if self.session_id.is_some() {
                    self.begin_profile_transition(
                        session_id,
                        profile_generation,
                        now,
                        crate::now_millis().saturating_mul(1_000),
                    );
                } else {
                    self.session_id = Some(session_id);
                    self.profile_generation = profile_generation;
                    self.rendered_baseline = 0;
                    self.profile_acknowledged = true;
                }
            }
            self.frames_rendered_total = frames_rendered_total;
            self.render_ready = render_initialized && frames_rendered_total > 0;
            self.first_frame_rendered = frames_rendered_total > self.rendered_baseline;
            self.first_idr_decoded = first_idr_decoded;
            let current_window_progressing =
                self.active_render_fps > 0.0 && self.decoder_input_fps > 0.0;
            let ready = self.profile_acknowledged
                && self.render_ready
                && self.first_idr_decoded
                && self.first_frame_rendered
                && current_window_progressing;
            if self.profile_transition_active {
                let previous_phase = self.profile_transition_phase;
                let outcome = self
                    .readiness_transition
                    .as_mut()
                    .map(|transition| {
                        transition.observe(
                            now,
                            crate::profile_transition::ReceiverReadinessObservation {
                                first_idr_decoded,
                                render_initialized,
                                frames_rendered_total,
                                progressing: current_window_progressing,
                                damaged_gop_total: self.damaged_gop_total,
                            },
                        )
                    })
                    .unwrap_or(crate::profile_transition::ReceiverReadinessOutcome::Failed(
                        "receiver-readiness-state-missing",
                    ));
                if let Some(transition) = self.readiness_transition.as_ref() {
                    self.profile_transition_phase = transition.phase;
                    self.transition_settle_windows = transition.settle_windows;
                    self.transition_settle_duration_ms =
                        now.saturating_duration_since(transition.started_at)
                            .as_millis()
                            .min(u128::from(u32::MAX)) as u32;
                    self.new_session_first_idr_wait_ms = transition.first_idr_wait_ms;
                    self.transition_recovery_count = transition.recovery_count;
                    self.transition_settle_restart_count = transition.settle_restart_count;
                    self.transition_settle_deadline_remaining_ms = transition
                        .settle_deadline_remaining(now)
                        .as_millis()
                        .min(u128::from(u64::MAX))
                        as u64;
                    self.transition_overall_deadline_remaining_ms = transition
                        .overall_deadline_remaining(now)
                        .as_millis()
                        .min(u128::from(u64::MAX))
                        as u64;
                    self.transition_failure_stage = transition.failure_stage;
                    self.first_idr_decoded = transition.first_idr_at.is_some()
                        && transition.phase
                            != crate::profile_transition::ReceiverTransitionPhase::AwaitFirstIdr;
                    self.first_frame_rendered = transition.first_rendered_at.is_some();
                    self.valid_feedback_windows = transition.settle_windows;
                }
                match outcome {
                    crate::profile_transition::ReceiverReadinessOutcome::Pending => {}
                    crate::profile_transition::ReceiverReadinessOutcome::Committed => {
                        self.profile_transition_active = false;
                    }
                    crate::profile_transition::ReceiverReadinessOutcome::Failed(reason) => {
                        self.profile_transition_active = false;
                        if previous_phase
                            != crate::profile_transition::ReceiverTransitionPhase::Failed
                        {
                            self.transition_timeout_count =
                                self.transition_timeout_count.saturating_add(1);
                        }
                        self.transition_failure_reason = Some(reason);
                    }
                }
            } else if ready {
                self.valid_feedback_windows = self.valid_feedback_windows.saturating_add(1);
            } else {
                self.valid_feedback_windows = 0;
            }
            self.profile_settled =
                ready && self.valid_feedback_windows >= 3 && !self.profile_transition_active;
            self.feedback_sample_eligible = self.profile_settled;
        }

        fn state_flags(&self) -> u32 {
            let mut flags = 0u32;
            if self.feedback_sample_eligible {
                flags |= crate::media_control::FEEDBACK_FLAG_SAMPLE_ELIGIBLE;
            }
            if self.render_ready {
                flags |= crate::media_control::FEEDBACK_FLAG_RENDER_READY;
            }
            if self.profile_settled {
                flags |= crate::media_control::FEEDBACK_FLAG_PROFILE_SETTLED;
            }
            if self.first_idr_decoded {
                flags |= crate::media_control::FEEDBACK_FLAG_FIRST_IDR_DECODED;
            }
            if self.first_frame_rendered {
                flags |= crate::media_control::FEEDBACK_FLAG_FIRST_FRAME_RENDERED;
            }
            if self.profile_transition_active {
                flags |= crate::media_control::FEEDBACK_FLAG_PROFILE_TRANSITION_ACTIVE;
            }
            if self.profile_acknowledged {
                flags |= crate::media_control::FEEDBACK_FLAG_PROFILE_ACKNOWLEDGED;
            }
            flags
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct CapabilityFeedbackCounters {
        sent: u64,
        send_errors: u64,
        sequence: u64,
        last_display_generation: Option<u64>,
        previous_decode_queue_drops: u64,
        previous_render_replacements: u64,
        previous_repair_deadline_missed: u64,
        previous_damaged_gop: u64,
        previous_packets_lost: u64,
    }

    #[derive(Default)]
    struct ViewerStats {
        frames_decoded: u64,
        frames_decoder_input: u64,
        frames_rendered: u64,
        frames_render_skipped: u64,
        frames_decoded_not_rendered: u64,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        decoder_errors: u64,
        decoder_resets: u64,
        decoder_configured_fps: u32,
        render_queue_peak: usize,
        decoded_frame_queue_len: usize,
        decoded_frame_queue_drops: u64,
        render_frame_copies: u64,
        render_buffer_reused: u64,
        render_buffer_generation: u64,
        nv12_y_stride: usize,
        nv12_uv_stride: usize,
        nv12_uv_offset: usize,
        nv12_allocated_height: usize,
        nv12_buffer_len: usize,
        expected_tight_len: usize,
        decoder_used_2d_buffer: bool,
        color_spec: ColorSpec,
        decoder_color_metadata: MediaColorMetadata,
        decode_ms_total: f64,
        render_ms_total: f64,
        video_frames_dropped_for_av_sync: u64,
        video_frames_held_for_av_sync: u64,
        av_sync_offset_ms_total: f64,
        av_sync_offset_ms_max: f64,
        av_sync_offset_samples: u64,
        video_sync_gating_enabled: bool,
        av_sync_mode: AvSyncMode,
        av_sync_state: AvSyncState,
        av_sync_bypass_reason: Option<AvSyncBypassReason>,
        av_sync_state_transitions: u64,
        av_sync_forced_release_count: u64,
        av_sync_hold_epoch_ms: u64,
        audio_stats: crate::audio_udp::IntegratedAudioRecvStats,
        render_state: VideoRenderStats,
    }

    struct PendingRender {
        frame_id: u64,
        timestamp_us: u64,
        frame: DecodedFrame,
        width: u32,
        height: u32,
    }

    #[derive(Default)]
    struct RenderWorkerState {
        pending: Option<PendingRender>,
        render_state: VideoRenderStats,
        frames_rendered: u64,
        frames_replaced: u64,
        queue_peak: usize,
        render_ms_total: f64,
        video_frames_dropped_for_av_sync: u64,
        video_frames_held_for_av_sync: u64,
        av_sync_offset_ms_total: f64,
        av_sync_offset_ms_max: f64,
        av_sync_offset_samples: u64,
        video_sync_gating_enabled: bool,
        av_sync_mode: AvSyncMode,
        av_sync_state: AvSyncState,
        av_sync_bypass_reason: Option<AvSyncBypassReason>,
        av_sync_state_transitions: u64,
        av_sync_forced_release_count: u64,
        av_sync_hold_epoch_ms: u64,
        closed_by_user: bool,
        error: Option<String>,
    }

    struct DebugFrameDumper {
        directory: Option<PathBuf>,
        limit: usize,
        dumped: usize,
    }

    impl DebugFrameDumper {
        fn new(directory: Option<String>, limit: usize) -> Self {
            Self {
                directory: directory.map(PathBuf::from),
                limit,
                dumped: 0,
            }
        }

        fn maybe_dump(
            &mut self,
            frame: &DecodedFrame,
            width: u32,
            height: u32,
            generation: u64,
        ) -> Result<(), String> {
            let Some(directory) = self.directory.as_deref() else {
                return Ok(());
            };
            if self.dumped >= self.limit {
                return Ok(());
            }
            self.dumped += 1;
            OwnedBgraFrame::from_decoded(frame, width, height, generation)?
                .dump_raw(directory, self.dumped as u64)
        }
    }

    struct DecodeState {
        decoder: Option<WmfH264Decoder>,
        dimensions: Option<VideoDimensions>,
        waiting_for_keyframe: bool,
        input_index: u64,
        last_keyframe_id: Option<u64>,
        next_render_generation: u64,
    }

    impl DecodeState {
        fn new() -> Self {
            Self {
                decoder: None,
                dimensions: None,
                waiting_for_keyframe: true,
                input_index: 0,
                last_keyframe_id: None,
                next_render_generation: 0,
            }
        }

        fn mark_discontinuity(&mut self, stats: &mut ViewerStats, count_recovery: bool) {
            if self.decoder.take().is_some() {
                stats.decoder_resets += 1;
            }
            if count_recovery {
                stats.keyframe_recovery_count += 1;
            }
            self.waiting_for_keyframe = true;
            self.input_index = 0;
            self.last_keyframe_id = None;
        }
    }

    #[derive(Debug)]
    struct ReceiverWorkerShutdownSummary {
        render: crate::shutdown::WorkerJoinStatus,
        network: crate::shutdown::WorkerJoinStatus,
        audio: &'static str,
        join_error: Option<String>,
        runtime_error: Option<String>,
        cleanup_ms: f64,
        cleanup_deadline_ms: u64,
        lifecycle_state: &'static str,
        retained_workers: Vec<String>,
    }

    impl ReceiverWorkerShutdownSummary {
        fn join_clean(&self) -> bool {
            self.join_error.is_none()
                && self.audio != "incomplete"
                && self.render.clean()
                && self.network.clean()
                && self.retained_workers.is_empty()
        }

        fn runtime_completed_clean(&self) -> bool {
            self.runtime_error.is_none()
        }

        fn clean(&self) -> bool {
            self.join_clean() && self.runtime_completed_clean()
        }

        fn json_fragment(&self) -> String {
            format!(
                r#""worker_join_render":"{}","worker_join_network":"{}","worker_join_audio":"{}","worker_join_all_clean":{},"worker_join_error":{},"runtime_completed_clean":{},"runtime_error":{},"terminal_success":{},"retained_worker_count":{},"retained_workers":{},"cleanup_duration_ms":{:.3},"cleanup_deadline_ms":{},"qsv_async_wait_timeouts":0,"qsv_async_wait_cancelled":0,"qsv_drain_timeouts":0,"cleanup_lifecycle_state":"{}","cleanup_stop_requested":true"#,
                self.render.name(),
                self.network.name(),
                self.audio,
                self.join_clean(),
                optional_json_string(self.join_error.as_deref()),
                self.runtime_completed_clean(),
                optional_json_string(self.runtime_error.as_deref()),
                self.clean(),
                self.retained_workers.len(),
                json_string_array(&self.retained_workers),
                self.cleanup_ms,
                self.cleanup_deadline_ms,
                self.lifecycle_state,
            )
        }
    }

    pub fn run(config: H264RecvViewConfig) -> Result<(), String> {
        if let Err(error) = validate_config(&config) {
            print_startup_failure(&config, &error);
            return Err(error);
        }
        let _console_ctrl = match crate::shutdown::ConsoleCtrlGuard::install() {
            Ok(guard) => guard,
            Err(error) => {
                print_startup_failure(&config, &error);
                return Err(error);
            }
        };
        let shutdown_coordinator = crate::shutdown::ShutdownCoordinator::new();
        let cancellation = shutdown_coordinator.token();
        let event_context = crate::shutdown::RuntimeEventContext::new(crate::make_session_id());
        let socket = match UdpSocket::bind(format!("{}:{}", config.bind, config.port)) {
            Ok(socket) => socket,
            Err(error) => {
                let error = format!("UDP bind failed: {error}");
                print_startup_failure(&config, &error);
                return Err(error);
            }
        };
        let udp_recv_buffer_bytes = match crate::udp_socket::configure_receive_buffer(
            &socket,
            crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                print_startup_failure(&config, &error);
                return Err(error);
            }
        };
        if let Err(error) = socket.set_nonblocking(true) {
            let error = format!("set UDP nonblocking failed: {error}");
            print_startup_failure(&config, &error);
            return Err(error);
        }
        let queue = Arc::new(Mutex::new(DecodeQueue::new(
            config.max_decode_queue,
            config.drop_damaged_gop,
        )));
        let network_state = Arc::new(Mutex::new(SharedNetworkState::default()));
        if let Ok(mut state) = network_state.lock() {
            state.snapshot.udp_recv_buffer_bytes = udp_recv_buffer_bytes;
            state.snapshot.udp_recv_buffer_bytes_requested =
                crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES;
        }
        let stop = cancellation.flag();
        let receiver_clock = (config.audio_mode == AudioRecvMode::On).then(MediaClock::new);
        let mut audio_receiver = if config.audio_mode == AudioRecvMode::On {
            Some(crate::audio_udp::spawn_integrated_audio_receiver(
                config.audio_jitter_buffer_ms,
                receiver_clock
                    .as_ref()
                    .expect("audio mode creates a media clock")
                    .clone(),
            ))
        } else {
            None
        };
        let audio_ingest = audio_receiver.as_ref().map(|audio| audio.ingest());
        let audio_master = audio_receiver.as_ref().map(|audio| audio.master_clock());
        let render_state = Arc::new(Mutex::new(RenderWorkerState::default()));
        let capability_source = Arc::new(Mutex::new(CapabilityFeedbackSource::default()));
        let active_decoder_fps = Arc::new(AtomicU64::new(u64::from(config.decoder_fps)));
        let (render_thread, initial_render_stats) = match spawn_render_thread(
            Arc::clone(&render_state),
            Arc::clone(&stop),
            cancellation.clone(),
            config.title.clone(),
            config.render_scale,
            config.window_mode,
            config.render_backend,
            config.debug_dump_frames.clone(),
            config.debug_dump_limit,
            audio_master,
            config.playout_delay_ms,
            config.audio_jitter_buffer_ms,
            config.av_sync_mode,
            config.display_refresh_detect,
            Arc::clone(&capability_source),
        ) {
            Ok(render) => render,
            Err(error) => {
                shutdown_coordinator.mark_failed();
                let audio_error = audio_receiver
                    .as_mut()
                    .and_then(|audio| audio.stop_and_join().err());
                let detail = if let Some(audio_error) = audio_error {
                    format!("{error}; audio cleanup: {audio_error}")
                } else {
                    error
                };
                print_startup_failure(&config, &detail);
                return Err(detail);
            }
        };
        let network_thread = match spawn_network_thread(
            socket,
            Arc::clone(&queue),
            Arc::clone(&network_state),
            Arc::clone(&stop),
            cancellation.clone(),
            receiver_clock,
            audio_ingest,
            config.frame_timeout_ms,
            config.reorder_wait_ms,
            config.playout_delay_ms,
            config.max_inflight_frames,
            config.max_decode_queue,
            config.drop_damaged_gop,
            config.repair_mode,
            config.nack_delay_ms,
            config.nack_repeat_ms,
            config.nack_max_rounds,
            config.capability_feedback_ms,
            Arc::clone(&capability_source),
            Arc::clone(&active_decoder_fps),
        ) {
            Ok(worker) => worker,
            Err(error) => {
                shutdown_coordinator.mark_failed();
                stop.store(true, Ordering::SeqCst);
                let mut render_thread = Some(render_thread);
                let render_status = crate::shutdown::try_join_until(
                    &mut render_thread,
                    Instant::now() + Duration::from_secs(2),
                );
                if render_status == crate::shutdown::WorkerJoinStatus::TimedOut {
                    crate::shutdown::retain_unjoined_worker(
                        "receiver-render-startup",
                        &mut render_thread,
                    );
                }
                let audio_error = audio_receiver
                    .as_mut()
                    .and_then(|audio| audio.stop_and_join().err());
                let mut detail = format!("{error}; render worker cleanup={}", render_status.name());
                if let Some(audio_error) = audio_error {
                    detail.push_str(&format!("; audio cleanup: {audio_error}"));
                }
                if render_status == crate::shutdown::WorkerJoinStatus::TimedOut {
                    detail = format!(
                        "{}: {detail}",
                        crate::shutdown::WORKER_OWNERSHIP_FAILURE_TAG
                    );
                }
                print_startup_failure(&config, &detail);
                return Err(detail);
            }
        };

        shutdown_coordinator.mark_running();
        if config.verbose {
            eprintln!(
                "h264-recv-view bind={}:{} frame_timeout_ms={} reorder_wait_ms={} playout_delay_ms={} max_inflight_frames={} max_decode_queue={} strict_decode_order={} drop_damaged_gop={} debug_dump_frames={:?} debug_dump_limit={} udp_receive_buffer={} decoder=\"{}\" output=NV12 render_backend={} render_scale={} window_mode={} title=\"{}\" duration_sec={}",
                config.bind,
                config.port,
                config.frame_timeout_ms,
                config
                    .reorder_wait_ms
                    .map_or_else(|| "auto".to_string(), |value| value.to_string()),
                config.playout_delay_ms,
                config.max_inflight_frames,
                config.max_decode_queue,
                config.strict_decode_order,
                config.drop_damaged_gop,
                config.debug_dump_frames,
                config.debug_dump_limit,
                udp_recv_buffer_bytes,
                DECODER_NAME,
                initial_render_stats.selected.name(),
                config.render_scale.name(),
                config.window_mode.name(),
                config.title,
                optional_duration_text(config.duration_sec)
            );
        }
        print_started(
            &config,
            initial_render_stats.clone(),
            &event_context,
            &cancellation,
            shutdown_coordinator.state().name(),
        );

        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_network = ReassemblyStats::default();
        let mut previous_decoded = 0u64;
        let mut previous_decoder_input = 0u64;
        let mut previous_rendered = 0u64;
        let mut decode_state = DecodeState::new();
        let mut stats = ViewerStats::default();
        stats.decoder_configured_fps = config.decoder_fps;
        stats.render_state = initial_render_stats;
        stats.audio_stats = audio_receiver
            .as_ref()
            .map(|audio| audio.stats())
            .unwrap_or_default();
        let mut closed_by_user = false;

        loop {
            if cancellation.is_cancelled() || stop.load(Ordering::SeqCst) {
                break;
            }
            if duration_elapsed(started_at, config.duration_sec) {
                cancellation.cancel(crate::shutdown::StopReason::Duration);
                break;
            }
            sync_render_worker_stats(&render_state, &mut stats);
            if let Some(audio) = audio_receiver.as_ref() {
                stats.audio_stats = audio.stats();
            }
            if render_state
                .lock()
                .map(|state| state.closed_by_user)
                .unwrap_or(false)
            {
                closed_by_user = true;
                cancellation.cancel(crate::shutdown::StopReason::WindowClosed);
                break;
            }

            let items = queue.lock().map_or_else(
                |_| Vec::new(),
                |mut queue| queue.items.drain(..).collect::<Vec<_>>(),
            );
            if items.is_empty() {
                thread::sleep(Duration::from_millis(1));
            } else {
                for item in items {
                    match item {
                        DecodeQueueItem::Reset => {
                            decode_state.mark_discontinuity(&mut stats, false);
                        }
                        DecodeQueueItem::Frame(frame) => {
                            let decoder_fps = active_decoder_fps
                                .load(Ordering::Acquire)
                                .clamp(1, u64::from(u32::MAX))
                                as u32;
                            stats.decoder_configured_fps = decoder_fps;
                            process_encoded_frame(
                                frame,
                                &mut decode_state,
                                &render_state,
                                &mut stats,
                                decoder_fps,
                            );
                        }
                    }
                }
            }

            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_millis(config.json_interval_ms) {
                let snapshot = network_snapshot(&network_state);
                let elapsed = now.duration_since(report_at).as_secs_f64().max(0.001);
                if let Ok(mut capability) = capability_source.lock() {
                    capability.render = stats.render_state.clone();
                    capability.active_render_fps =
                        stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed;
                    capability.decoder_input_fps = stats
                        .frames_decoder_input
                        .saturating_sub(previous_decoder_input)
                        as f64
                        / elapsed;
                    capability.decode_queue_drops_total = snapshot.complete_frame_queue_drops;
                    capability.render_replacements_total = stats.frames_decoded_not_rendered;
                    capability.repair_deadline_missed_total = snapshot.repair_deadline_missed;
                    capability.damaged_gop_total = snapshot.damaged_gop.damaged_gop_count;
                    capability.packets_lost_total = snapshot.reassembly.packets_lost_estimate;
                    capability.update_readiness(
                        snapshot.session_id,
                        snapshot.profile_generation,
                        stats.render_state.window.initialized,
                        stats.frames_rendered,
                        decode_state.last_keyframe_id.is_some()
                            && !decode_state.waiting_for_keyframe,
                        now,
                    );
                    if let Ok(mut shared) = network_state.lock() {
                        capability.write_transition_snapshot(&mut shared.snapshot);
                    }
                }
                print_stats(
                    snapshot,
                    &stats,
                    decode_state.dimensions,
                    decode_state.waiting_for_keyframe,
                    decode_state.last_keyframe_id,
                    config.strict_decode_order,
                    previous_network,
                    previous_decoded,
                    previous_decoder_input,
                    previous_rendered,
                    now.duration_since(report_at),
                    config.mode,
                    &event_context,
                    &cancellation,
                    shutdown_coordinator.state().name(),
                );
                previous_network = snapshot.reassembly;
                previous_decoded = stats.frames_decoded;
                previous_decoder_input = stats.frames_decoder_input;
                previous_rendered = stats.frames_rendered;
                report_at = now;
            }
        }

        if cancellation.reason().is_none() {
            cancellation.cancel(crate::shutdown::StopReason::LocalStop);
        }
        let cleanup_started = Instant::now();
        shutdown_coordinator.request_stop(
            cancellation
                .reason()
                .unwrap_or(crate::shutdown::StopReason::LocalStop),
        );
        let _cleanup_owner = shutdown_coordinator.begin_cleanup();
        stop.store(true, Ordering::SeqCst);
        let shutdown_config = crate::shutdown::ShutdownConfig::default();
        let mut render_thread = Some(render_thread);
        let mut network_thread = Some(network_thread);
        let render_join = crate::shutdown::try_join_until(
            &mut render_thread,
            Instant::now() + shutdown_config.worker_join_timeout,
        );
        if render_join == crate::shutdown::WorkerJoinStatus::TimedOut {
            crate::shutdown::retain_unjoined_worker("receiver-render", &mut render_thread);
        }
        sync_render_worker_stats(&render_state, &mut stats);
        closed_by_user |= render_state
            .lock()
            .map(|state| state.closed_by_user)
            .unwrap_or(false);
        let network_join = crate::shutdown::try_join_until(
            &mut network_thread,
            Instant::now() + shutdown_config.worker_join_timeout,
        );
        if network_join == crate::shutdown::WorkerJoinStatus::TimedOut {
            crate::shutdown::retain_unjoined_worker("receiver-network", &mut network_thread);
        }
        if let Ok(mut capability) = capability_source.lock() {
            capability.cancel_profile_transition();
            if let Ok(mut shared) = network_state.lock() {
                capability.write_transition_snapshot(&mut shared.snapshot);
            }
        }
        let mut join_errors = Vec::new();
        if !render_join.clean() {
            join_errors.push(format!("render worker shutdown: {}", render_join.name()));
        }
        if !network_join.clean() {
            join_errors.push(format!("network worker shutdown: {}", network_join.name()));
        }
        let audio_join = if let Some(audio) = audio_receiver.as_mut() {
            match audio.stop_and_join() {
                Ok(()) => "joined",
                Err(error) => {
                    join_errors.push(error);
                    "incomplete"
                }
            }
        } else {
            "not_started"
        };
        if let Some(audio) = audio_receiver.as_ref() {
            stats.audio_stats = audio.stats();
        }
        let (snapshot, mut network_error) = network_state.lock().map_or_else(
            |_| {
                (
                    NetworkSnapshot::default(),
                    Some("network state lock was poisoned".to_string()),
                )
            },
            |state| (state.snapshot, state.error.clone()),
        );
        if network_error.is_none() {
            network_error = render_state
                .lock()
                .ok()
                .and_then(|state| state.error.clone());
        }
        let retained_workers = crate::shutdown::retained_worker_names();
        if !retained_workers.is_empty() {
            join_errors.push(format!(
                "{}: retained workers: {}",
                crate::shutdown::WORKER_OWNERSHIP_FAILURE_TAG,
                retained_workers.join(",")
            ));
        }
        let joins_clean = join_errors.is_empty()
            && render_join.clean()
            && network_join.clean()
            && audio_join != "incomplete"
            && retained_workers.is_empty();
        shutdown_coordinator.finish_cleanup(joins_clean);
        let shutdown = ReceiverWorkerShutdownSummary {
            render: render_join,
            network: network_join,
            audio: audio_join,
            join_error: (!join_errors.is_empty()).then(|| join_errors.join("; ")),
            runtime_error: network_error.clone(),
            cleanup_ms: cleanup_started.elapsed().as_secs_f64() * 1_000.0,
            cleanup_deadline_ms: shutdown_config
                .worker_join_timeout
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            lifecycle_state: shutdown_coordinator.state().name(),
            retained_workers,
        };
        let reason = cancellation.reason().unwrap_or_else(|| {
            if closed_by_user {
                crate::shutdown::StopReason::WindowClosed
            } else if let Some(error) = network_error.as_deref().or(shutdown.join_error.as_deref())
            {
                crate::shutdown::classify_error(error)
            } else {
                crate::shutdown::StopReason::Duration
            }
        });
        let dimensions = decode_state.dimensions.unwrap_or(VideoDimensions {
            width: 0,
            height: 0,
        });
        if let Some(error) = network_error.as_deref().or(shutdown.join_error.as_deref()) {
            if config.mode == H264RecvViewMode::Screen {
                let error_context = event_context.json_fragment(
                    "receiver",
                    crate::STREAM_VIDEO,
                    snapshot.session_id,
                    snapshot.profile_generation,
                    "run_total",
                    "stopping",
                    &cancellation,
                );
                println!(
                    r#"{{"type":"NATIVE_SCREEN_ERROR","role":"receiver","mode":"screen-recv","error":"{}",{}}}"#,
                    json_escape(error),
                    error_context,
                );
            }
        }
        print_done(
            &config,
            snapshot,
            &stats,
            dimensions,
            decode_state.last_keyframe_id,
            decode_state.waiting_for_keyframe,
            closed_by_user,
            reason,
            started_at.elapsed().as_secs_f64(),
            &shutdown,
            &event_context,
            &cancellation,
        );
        io::stdout().flush().ok();
        if config.verbose {
            eprintln!("h264-recv-view stopped reason={}", reason.name());
        }
        if let Some(error) = network_error.or(shutdown.join_error) {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn spawn_network_thread(
        socket: UdpSocket,
        queue: Arc<Mutex<DecodeQueue>>,
        state: Arc<Mutex<SharedNetworkState>>,
        stop: Arc<AtomicBool>,
        cancellation: crate::shutdown::CancellationToken,
        receiver_clock: Option<MediaClock>,
        audio_ingest: Option<crate::audio_udp::IntegratedAudioIngest>,
        frame_timeout_ms: u64,
        reorder_wait_ms: Option<u64>,
        playout_delay_ms: u64,
        max_inflight_frames: usize,
        max_decode_queue: usize,
        drop_damaged_gop: bool,
        repair_mode: crate::repair::RepairMode,
        nack_delay_ms: u64,
        nack_repeat_ms: u64,
        nack_max_rounds: u8,
        capability_feedback_ms: u64,
        capability_source: Arc<Mutex<CapabilityFeedbackSource>>,
        active_decoder_fps: Arc<AtomicU64>,
    ) -> Result<thread::JoinHandle<()>, String> {
        thread::Builder::new()
            .name("agoralink-h264-recv".to_string())
            .spawn(move || {
                if let Err(err) = receive_loop(
                    socket,
                    &queue,
                    &state,
                    &stop,
                    &cancellation,
                    receiver_clock,
                    audio_ingest,
                    frame_timeout_ms,
                    reorder_wait_ms,
                    playout_delay_ms,
                    max_inflight_frames,
                    max_decode_queue,
                    drop_damaged_gop,
                    repair_mode,
                    nack_delay_ms,
                    nack_repeat_ms,
                    nack_max_rounds,
                    capability_feedback_ms,
                    capability_source,
                    active_decoder_fps,
                ) {
                    cancellation.cancel(crate::shutdown::classify_error(&err));
                    if let Ok(mut shared) = state.lock() {
                        shared.error = Some(err);
                    }
                    stop.store(true, Ordering::SeqCst);
                }
            })
            .map_err(|err| format!("spawn H.264 receive thread failed: {err}"))
    }

    fn receive_loop(
        socket: UdpSocket,
        queue: &Arc<Mutex<DecodeQueue>>,
        state: &Arc<Mutex<SharedNetworkState>>,
        stop: &Arc<AtomicBool>,
        cancellation: &crate::shutdown::CancellationToken,
        receiver_clock: Option<MediaClock>,
        audio_ingest: Option<crate::audio_udp::IntegratedAudioIngest>,
        frame_timeout_ms: u64,
        reorder_wait_ms: Option<u64>,
        playout_delay_ms: u64,
        max_inflight_frames: usize,
        max_decode_queue: usize,
        drop_damaged_gop: bool,
        repair_mode: crate::repair::RepairMode,
        nack_delay_ms: u64,
        nack_repeat_ms: u64,
        nack_max_rounds: u8,
        capability_feedback_ms: u64,
        capability_source: Arc<Mutex<CapabilityFeedbackSource>>,
        active_decoder_fps: Arc<AtomicU64>,
    ) -> Result<(), String> {
        let mut reassembler = H264Reassembler::new(ReassemblyConfig {
            frame_timeout: Duration::from_millis(frame_timeout_ms),
            reorder_wait: reorder_wait_ms.map_or(ReorderWait::Auto, |milliseconds| {
                ReorderWait::Fixed(Duration::from_millis(milliseconds))
            }),
            max_inflight_frames,
        })?;
        if repair_mode == crate::repair::RepairMode::Nack {
            reassembler.set_repair_window(Some(Duration::from_millis(playout_delay_ms.max(20))));
        }
        let mut datagram = [0u8; MAX_DATAGRAM_SIZE];
        let mut previous_expired = 0u64;
        let mut playout = PlayoutBuffer::new(playout_delay_ms)?;
        let mut udp_recv_loop_overruns = 0u64;
        let mut peer = None;
        let mut requested_packets: HashSet<crate::repair::PacketKey> = HashSet::new();
        let mut completed_repair_packets: HashMap<crate::repair::PacketKey, Instant> =
            HashMap::new();
        let mut requested_frames: HashSet<u64> = HashSet::new();
        let mut repair_started: HashMap<u64, Instant> = HashMap::new();
        let mut repair_stats = ReceiverRepairStats::default();
        let mut media_anchor = ReceiverMediaClockAnchor::new(playout_delay_ms);
        let mut video_packet_dispatch_ns_total = 0u64;
        let mut video_packet_dispatch_count = 0u64;
        let mut video_packet_dispatch_ns_max = 0u64;
        let mut dispatch_stats = PacketDispatchStats::default();
        let mut audio_session_id = None;
        let mut feedback = CapabilityFeedbackCounters::default();
        let mut profile_stats = ReceiverProfileStats::default();
        let mut profile_controller =
            crate::profile_transition::ReceiverProfileController::default();
        let mut next_feedback_at = Instant::now() + Duration::from_millis(capability_feedback_ms);
        let mut last_valid_peer_activity: Option<Instant> = None;
        let mut peer_close_linger_until: Option<Instant> = None;
        let mut accepted_peer_close: Option<(SocketAddr, crate::media_control::StreamClose)> = None;

        while !stop.load(Ordering::SeqCst)
            || peer_close_linger_until.is_some_and(|deadline| Instant::now() < deadline)
        {
            let mut did_work = false;
            let mut datagrams_this_tick = 0usize;
            for _ in 0..MAX_DATAGRAMS_PER_TICK {
                match socket.recv_from(&mut datagram) {
                    Ok((length, source_peer)) => {
                        did_work = true;
                        datagrams_this_tick += 1;
                        let received_at = Instant::now();
                        if crate::media_control::is_stream_close(&datagram[..length]) {
                            match classify_incoming_stream_close(
                                &datagram[..length],
                                source_peer,
                                peer,
                                reassembler.session_id(),
                                accepted_peer_close,
                            ) {
                                IncomingStreamCloseDecision::Accept(close) => {
                                    last_valid_peer_activity = Some(received_at);
                                    accepted_peer_close = Some((source_peer, close));
                                    dispatch_stats.stream_close_received =
                                        dispatch_stats.stream_close_received.saturating_add(1);
                                    if send_stream_close_ack(&socket, source_peer, close) {
                                        dispatch_stats.stream_close_ack_sent =
                                            dispatch_stats.stream_close_ack_sent.saturating_add(1);
                                    }
                                    peer_close_linger_until.get_or_insert_with(|| {
                                        received_at
                                            + crate::shutdown::ShutdownConfig::default()
                                                .close_retry_max
                                    });
                                    cancellation.cancel(crate::shutdown::StopReason::PeerClosed);
                                    continue;
                                }
                                IncomingStreamCloseDecision::Duplicate(close) => {
                                    dispatch_stats.stream_close_duplicate =
                                        dispatch_stats.stream_close_duplicate.saturating_add(1);
                                    if send_stream_close_ack(&socket, source_peer, close) {
                                        dispatch_stats.stream_close_ack_sent =
                                            dispatch_stats.stream_close_ack_sent.saturating_add(1);
                                    }
                                    continue;
                                }
                                IncomingStreamCloseDecision::RejectPreSession => {
                                    dispatch_stats.stream_close_rejected_pre_session =
                                        dispatch_stats
                                            .stream_close_rejected_pre_session
                                            .saturating_add(1);
                                    continue;
                                }
                                IncomingStreamCloseDecision::RejectPeer => {
                                    dispatch_stats.stream_close_rejected_peer =
                                        dispatch_stats.stream_close_rejected_peer.saturating_add(1);
                                    continue;
                                }
                                IncomingStreamCloseDecision::RejectSession => {
                                    dispatch_stats.stream_close_rejected_session = dispatch_stats
                                        .stream_close_rejected_session
                                        .saturating_add(1);
                                    continue;
                                }
                                IncomingStreamCloseDecision::RejectInvalid => {
                                    dispatch_stats.stream_close_rejected_invalid = dispatch_stats
                                        .stream_close_rejected_invalid
                                        .saturating_add(1);
                                    continue;
                                }
                            }
                        }
                        if stop.load(Ordering::SeqCst) {
                            continue;
                        }
                        if crate::media_control::is_profile_change(&datagram[..length]) {
                            let change = match crate::media_control::ProfileChange::decode(
                                &datagram[..length],
                            ) {
                                Ok(change) => change,
                                Err(_) => {
                                    profile_controller.record_invalid_fields();
                                    profile_stats.changes_invalid =
                                        profile_stats.changes_invalid.saturating_add(1);
                                    continue;
                                }
                            };
                            let decision =
                                profile_controller.observe(change, source_peer, peer, received_at);
                            let ack = match decision {
                                crate::profile_transition::ReceiverControlDecision::Ack(ack) => {
                                    profile_stats.changes_received =
                                        profile_stats.changes_received.saturating_add(1);
                                    Some(ack)
                                }
                                crate::profile_transition::ReceiverControlDecision::Reack(ack) => {
                                    Some(ack)
                                }
                                _ => {
                                    profile_stats.changes_invalid =
                                        profile_stats.changes_invalid.saturating_add(1);
                                    None
                                }
                            };
                            if let Some(ack) = ack {
                                last_valid_peer_activity = Some(received_at);
                                if let Ok(bytes) = ack.encode() {
                                    if socket
                                        .send_to(&bytes, source_peer)
                                        .is_ok_and(|sent| sent == bytes.len())
                                    {
                                        profile_controller.record_ack_sent();
                                    }
                                }
                            }
                            continue;
                        }
                        if let Some(audio) = audio_ingest.as_ref() {
                            match audio.accept_datagram(&datagram[..length]) {
                                crate::audio_udp::AudioIngressOutcome::NotAudio => {}
                                crate::audio_udp::AudioIngressOutcome::Accepted => {
                                    dispatch_stats.audio_packets_received += 1;
                                    continue;
                                }
                                crate::audio_udp::AudioIngressOutcome::Invalid
                                | crate::audio_udp::AudioIngressOutcome::DroppedSessionMismatch => {
                                    // Audio packets are isolated from video reassembly even
                                    // when malformed or from a foreign session.
                                    continue;
                                }
                            }
                        }
                        let (reject_non_media, is_nack) =
                            classify_short_or_nack(&datagram[..length]);
                        if reject_non_media {
                            dispatch_stats.unknown_packets_received += 1;
                            if is_nack {
                                dispatch_stats.nack_control_packets_received += 1;
                                dispatch_stats.repair_packets_dropped_wrong_type += 1;
                            }
                            continue;
                        }
                        let Some((packet_session, key, flags)) =
                            crate::repair::media_packet_key(&datagram[..length])
                        else {
                            dispatch_stats.unknown_packets_received += 1;
                            continue;
                        };
                        if peer.is_some_and(|pinned| pinned != source_peer) {
                            profile_stats.stale_packets_dropped =
                                profile_stats.stale_packets_dropped.saturating_add(1);
                            continue;
                        }
                        if reassembler
                            .session_id()
                            .is_some_and(|session_id| session_id != packet_session)
                        {
                            let packet = match crate::MediaPacket::decode(&datagram[..length]) {
                                Ok(packet)
                                    if packet.stream_id == crate::STREAM_VIDEO
                                        && packet.flags & crate::FLAG_FEC == 0 =>
                                {
                                    packet
                                }
                                _ => {
                                    profile_stats.stale_packets_dropped =
                                        profile_stats.stale_packets_dropped.saturating_add(1);
                                    continue;
                                }
                            };
                            let Some(pending) = profile_controller.activate_if_pending(
                                packet.session_id,
                                source_peer,
                                received_at,
                            ) else {
                                profile_stats.stale_packets_dropped =
                                    profile_stats.stale_packets_dropped.saturating_add(1);
                                continue;
                            };
                            let change = pending.change;
                            let first_packet_wait_ms = received_at
                                .saturating_duration_since(pending.created_at)
                                .as_secs_f64()
                                * 1_000.0;
                            profile_stats.new_session_first_packet_wait_ms_total +=
                                first_packet_wait_ms;
                            profile_stats.new_session_first_packet_wait_ms_max = profile_stats
                                .new_session_first_packet_wait_ms_max
                                .max(first_packet_wait_ms);
                            reassembler.switch_session(change.new_session_id)?;
                            previous_expired = reassembler.stats().frames_incomplete_expired;
                            playout.clear_for_discontinuity();
                            requested_packets.clear();
                            completed_repair_packets.clear();
                            requested_frames.clear();
                            repair_started.clear();
                            queue
                                .lock()
                                .map_err(|_| "decode queue lock was poisoned".to_string())?
                                .begin_profile_transition();
                            if let Some(audio) = audio_ingest.as_ref() {
                                audio.set_expected_session_id(change.new_session_id);
                                audio_session_id = Some(change.new_session_id);
                            }
                            media_anchor = ReceiverMediaClockAnchor::new(playout_delay_ms);
                            profile_stats.change_sequence = change.change_sequence;
                            profile_stats.generation = change.profile_generation;
                            profile_stats.target_width = change.width;
                            profile_stats.target_height = change.height;
                            profile_stats.target_fps = change.fps;
                            profile_stats.target_bitrate_mbps = change.bitrate_mbps;
                            if let Ok(mut source) = capability_source.lock() {
                                source.begin_profile_transition(
                                    change.new_session_id,
                                    change.profile_generation,
                                    received_at,
                                    crate::now_millis().saturating_mul(1_000),
                                );
                            }
                            active_decoder_fps.store(u64::from(change.fps), Ordering::Release);
                            profile_stats.decoder_resets =
                                profile_stats.decoder_resets.saturating_add(1);
                            next_feedback_at = received_at;
                        }
                        if flags & crate::FLAG_FEC == 0 {
                            dispatch_stats.video_data_packets_received += 1;
                        } else {
                            dispatch_stats.video_fec_packets_received += 1;
                        }
                        // NACKs must target the video source socket. In A/V mode the
                        // audio sender has a different ephemeral source port.
                        let video_dispatch_started = Instant::now();
                        if repair_mode == crate::repair::RepairMode::Nack {
                            if flags & crate::FLAG_FEC == 0 {
                                if requested_packets.remove(&key) {
                                    if let Some(remaining) = reassembler
                                        .repair_deadline_remaining(key.frame_id, received_at)
                                    {
                                        repair_stats.repair_arrival_to_deadline.observe(remaining);
                                    }
                                    if reassembler.has_inflight_frame(key.frame_id) {
                                        repair_stats.repair_packets_received += 1;
                                        repair_stats.repair_packets_inserted += 1;
                                        repair_stats.repair_unique_packets_received += 1;
                                    } else {
                                        repair_stats.repair_packets_received += 1;
                                        repair_stats.repair_unique_packets_received += 1;
                                        repair_stats
                                            .repair_packets_received_after_frame_complete += 1;
                                        dispatch_stats.repair_packets_dropped_no_frame += 1;
                                    }
                                    completed_repair_packets.insert(key, received_at);
                                } else if completed_repair_packets.contains_key(&key) {
                                    repair_stats.repair_packets_received += 1;
                                    repair_stats.repair_duplicate_packets_received += 1;
                                    repair_stats.repair_duplicate_packets += 1;
                                    if !reassembler.has_inflight_frame(key.frame_id) {
                                        repair_stats
                                            .repair_packets_received_after_frame_complete += 1;
                                        dispatch_stats.repair_packets_dropped_late += 1;
                                    }
                                }
                            }
                        }
                        match reassembler.accept_datagram(&datagram[..length], received_at) {
                            Ok(frames) => {
                                last_valid_peer_activity = Some(received_at);
                                if peer.is_none() {
                                    peer = Some(source_peer);
                                }
                                if let Some(session_id) = reassembler.session_id() {
                                    profile_controller
                                        .sync_active(session_id, profile_stats.generation);
                                }
                                if let (Some(audio), Some(session_id)) =
                                    (audio_ingest.as_ref(), reassembler.session_id())
                                {
                                    if audio_session_id != Some(session_id) {
                                        audio.set_expected_session_id(session_id);
                                        audio_session_id = Some(session_id);
                                    }
                                }
                                for frame in &frames {
                                    if requested_frames.remove(&frame.frame_id) {
                                        repair_stats.repair_frames_completed += 1;
                                        if let Some(started) =
                                            repair_started.remove(&frame.frame_id)
                                        {
                                            let wait_ms = received_at
                                                .saturating_duration_since(started)
                                                .as_secs_f64()
                                                * 1000.0;
                                            repair_stats.repair_wait_ms_total += wait_ms;
                                            repair_stats.repair_wait_ms_max =
                                                repair_stats.repair_wait_ms_max.max(wait_ms);
                                        }
                                        let before = requested_packets.len();
                                        requested_packets.retain(|key| {
                                            if key.frame_id != frame.frame_id {
                                                return true;
                                            }
                                            completed_repair_packets.insert(*key, received_at);
                                            false
                                        });
                                        repair_stats.repair_cancelled_frame_complete = repair_stats
                                            .repair_cancelled_frame_complete
                                            .saturating_add(
                                                before.saturating_sub(requested_packets.len())
                                                    as u64,
                                            );
                                    }
                                }
                                let reassembly_stats = reassembler.stats();
                                let current_expired = reassembly_stats.frames_incomplete_expired;
                                if current_expired > previous_expired {
                                    repair_stats.repair_deadline_missed +=
                                        current_expired - previous_expired;
                                    playout.clear_for_discontinuity();
                                    begin_queue_recovery(
                                        queue,
                                        drop_damaged_gop,
                                        Instant::now(),
                                        reassembly_stats.last_damaged_frame_id,
                                    )?;
                                    previous_expired = current_expired;
                                    record_repair_deadline_misses(
                                        reassembly_stats.last_damaged_frame_id,
                                        received_at,
                                        &mut requested_frames,
                                        &mut repair_started,
                                        &mut repair_stats,
                                    );
                                }
                                observe_video_frames(
                                    &frames,
                                    receiver_clock.as_ref(),
                                    &mut media_anchor,
                                );
                                playout.push_frames(frames, received_at);
                            }
                            Err(_) => {}
                        }
                        let video_dispatch_ns = video_dispatch_started
                            .elapsed()
                            .as_nanos()
                            .min(u128::from(u64::MAX))
                            as u64;
                        video_packet_dispatch_ns_total =
                            video_packet_dispatch_ns_total.saturating_add(video_dispatch_ns);
                        video_packet_dispatch_count = video_packet_dispatch_count.saturating_add(1);
                        video_packet_dispatch_ns_max =
                            video_packet_dispatch_ns_max.max(video_dispatch_ns);
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err)
                        if is_expected_udp_peer_reset(
                            err.kind(),
                            peer_close_linger_until.is_some() || last_valid_peer_activity.is_some(),
                        ) =>
                    {
                        break;
                    }
                    Err(err) => return Err(format!("UDP receive failed: {err}")),
                }
            }
            if datagrams_this_tick == MAX_DATAGRAMS_PER_TICK {
                udp_recv_loop_overruns += 1;
            }

            let now = Instant::now();
            if last_valid_peer_activity.is_some_and(|last| {
                now.saturating_duration_since(last)
                    >= crate::shutdown::ShutdownConfig::default().peer_hard_timeout
            }) {
                dispatch_stats.peer_timeout_triggered = true;
                cancellation.cancel(crate::shutdown::StopReason::PeerTimeout);
                break;
            }
            if profile_controller.expire(now) {
                next_feedback_at = now;
            }
            let control_stats = profile_controller.stats();
            profile_stats.mprf_packets_received = control_stats.mprf_packets_received;
            profile_stats.mprf_ack_packets_sent = control_stats.mprf_ack_packets_sent;
            profile_stats.mprf_duplicate_reacked = control_stats.mprf_duplicate_reacked;
            profile_stats.mprf_pending_expired = control_stats.mprf_pending_expired;
            profile_stats.mprf_rejected_foreign_peer = control_stats.mprf_rejected_foreign_peer;
            profile_stats.mprf_rejected_old_session = control_stats.mprf_rejected_old_session;
            profile_stats.mprf_rejected_sequence = control_stats.mprf_rejected_sequence;
            profile_stats.mprf_rejected_generation = control_stats.mprf_rejected_generation;
            profile_stats.mprf_rejected_invalid_fields = control_stats.mprf_rejected_invalid_fields;
            profile_stats.new_session_activation_count = control_stats.new_session_activation_count;
            if let Some(pending) = profile_controller.pending() {
                profile_stats.pending_profile_present = true;
                profile_stats.pending_profile_age_ms =
                    now.saturating_duration_since(pending.created_at)
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64;
                profile_stats.pending_profile_deadline_remaining_ms = pending
                    .expires_at
                    .saturating_duration_since(now)
                    .as_millis()
                    .min(u128::from(u64::MAX))
                    as u64;
                profile_stats.pending_profile_change_sequence = pending.change.change_sequence;
                profile_stats.pending_profile_generation = pending.change.profile_generation;
                profile_stats.pending_profile_old_session_id = pending.change.old_session_id;
                profile_stats.pending_profile_new_session_id = pending.change.new_session_id;
            } else {
                profile_stats.pending_profile_present = false;
                profile_stats.pending_profile_age_ms = 0;
                profile_stats.pending_profile_deadline_remaining_ms = 0;
                profile_stats.pending_profile_change_sequence = 0;
                profile_stats.pending_profile_generation = 0;
                profile_stats.pending_profile_old_session_id = 0;
                profile_stats.pending_profile_new_session_id = 0;
            }
            if let (Some(peer), Some(session_id)) = (peer, reassembler.session_id()) {
                let source = capability_source
                    .lock()
                    .map(|source| source.clone())
                    .unwrap_or_default();
                let display_generation = source.render.window.display.display_generation;
                let display_changed = feedback.last_display_generation != Some(display_generation);
                if now >= next_feedback_at || display_changed {
                    feedback.sequence = feedback.sequence.saturating_add(1);
                    let display = &source.render.window.display;
                    let packet = crate::media_control::CapabilityFeedback {
                        version: crate::media_control::CAPABILITY_FEEDBACK_VERSION,
                        session_id,
                        feedback_sequence: feedback.sequence,
                        display_generation,
                        display_refresh_numerator: display.refresh.numerator,
                        display_refresh_denominator: display.refresh.denominator.max(1),
                        display_width: display.display_width,
                        display_height: display.display_height,
                        present_fps_measured: source.render.d3d11.present_fps_measured as f32,
                        present_interval_p95_ms: source.render.d3d11.present_interval_ms_p95 as f32,
                        active_render_fps: source.active_render_fps as f32,
                        decoder_input_fps: source.decoder_input_fps as f32,
                        decode_queue_drops_delta: source
                            .decode_queue_drops_total
                            .saturating_sub(feedback.previous_decode_queue_drops),
                        render_replacements_delta: source
                            .render_replacements_total
                            .saturating_sub(feedback.previous_render_replacements),
                        repair_deadline_missed_delta: source
                            .repair_deadline_missed_total
                            .saturating_sub(feedback.previous_repair_deadline_missed),
                        damaged_gop_delta: source
                            .damaged_gop_total
                            .saturating_sub(feedback.previous_damaged_gop),
                        packets_lost_delta: source
                            .packets_lost_total
                            .saturating_sub(feedback.previous_packets_lost),
                        timestamp_us: receiver_clock.as_ref().map_or_else(
                            || crate::now_millis().saturating_mul(1000),
                            MediaClock::now_us,
                        ),
                        profile_generation: source.profile_generation,
                        state_flags: source.state_flags(),
                        valid_feedback_windows: source.valid_feedback_windows,
                        transition_settle_windows: source.transition_settle_windows,
                        transition_settle_duration_ms: source.transition_settle_duration_ms,
                        profile_transition_started_us: source.profile_transition_started_us,
                    };
                    match packet.encode().and_then(|bytes| {
                        socket
                            .send_to(&bytes, peer)
                            .map_err(|err| format!("send capability feedback failed: {err}"))
                            .and_then(|sent| {
                                (sent == bytes.len())
                                    .then_some(())
                                    .ok_or_else(|| "capability feedback UDP short send".to_string())
                            })
                    }) {
                        Ok(()) => {
                            feedback.sent = feedback.sent.saturating_add(1);
                            feedback.last_display_generation = Some(display_generation);
                            feedback.previous_decode_queue_drops = source.decode_queue_drops_total;
                            feedback.previous_render_replacements =
                                source.render_replacements_total;
                            feedback.previous_repair_deadline_missed =
                                source.repair_deadline_missed_total;
                            feedback.previous_damaged_gop = source.damaged_gop_total;
                            feedback.previous_packets_lost = source.packets_lost_total;
                        }
                        Err(_) => {
                            feedback.send_errors = feedback.send_errors.saturating_add(1);
                        }
                    }
                    next_feedback_at = now + Duration::from_millis(capability_feedback_ms);
                }
            }
            completed_repair_packets.retain(|_, completed_at| {
                now.duration_since(*completed_at) <= Duration::from_secs(1)
            });
            if repair_mode == crate::repair::RepairMode::Nack {
                if let (Some(peer), Some(session_id)) = (peer, reassembler.session_id()) {
                    let collection = reassembler.collect_nack_items(
                        now,
                        Duration::from_millis(nack_delay_ms),
                        Duration::from_millis(nack_repeat_ms),
                        nack_max_rounds,
                        Duration::from_millis(playout_delay_ms.max(20)),
                        crate::h264_reassembly::DEFAULT_NACK_ITEMS_PER_FRAME,
                    );
                    repair_stats.nack_candidate_frames += collection.stats.candidate_frames;
                    repair_stats.nack_suppressed_progressing_frames +=
                        collection.stats.suppressed_progressing_frames;
                    repair_stats.nack_suppressed_too_early += collection.stats.suppressed_too_early;
                    repair_stats.nack_suppressed_already_requested +=
                        collection.stats.suppressed_already_requested;
                    repair_stats.nack_suppressed_item_limit +=
                        collection.stats.suppressed_item_limit;
                    repair_stats.nack_items_deduped += collection.stats.items_deduped;
                    repair_stats.nack_items_per_requested_frame_total +=
                        collection.stats.items_per_requested_frame_total;
                    repair_stats.nack_items_per_requested_frame_max = repair_stats
                        .nack_items_per_requested_frame_max
                        .max(collection.stats.items_per_requested_frame_max);
                    repair_stats.nack_candidates_first_round +=
                        collection.stats.candidates_first_round;
                    repair_stats.nack_candidates_late_discovery +=
                        collection.stats.candidates_late_discovery;
                    repair_stats
                        .missing_first_detected_age
                        .merge(collection.stats.missing_first_detected_age);
                    repair_stats
                        .missing_first_detected_to_deadline
                        .merge(collection.stats.missing_first_detected_to_deadline);
                    repair_stats
                        .first_nack_to_deadline
                        .merge(collection.stats.first_nack_to_deadline);
                    repair_stats
                        .first_nack_age
                        .merge(collection.stats.first_nack_age);
                    repair_stats
                        .first_round_budget
                        .merge(collection.stats.first_round_budget);
                    repair_stats
                        .second_round_budget
                        .merge(collection.stats.second_round_budget);
                    let items = collection.items;
                    if !items.is_empty() {
                        let unique_frames = items
                            .iter()
                            .map(|item| item.frame_id)
                            .collect::<HashSet<_>>();
                        repair_stats.nack_frames_requested += collection.stats.requested_frames;
                        repair_stats.nack_rounds_sent += 1;
                        requested_frames.extend(unique_frames);
                        for frame_id in &requested_frames {
                            repair_started.entry(*frame_id).or_insert(now);
                        }
                        requested_packets.extend(items.iter().copied());
                        for chunk in items.chunks(crate::repair::MAX_NACK_ITEMS) {
                            let nack = crate::repair::NackPacket {
                                session_id,
                                items: chunk.to_vec(),
                            }
                            .encode()?;
                            match socket.send_to(&nack, peer) {
                                Ok(sent) if sent == nack.len() => {
                                    repair_stats.nack_packets_sent += 1;
                                    repair_stats.nack_items_sent += chunk.len() as u64;
                                }
                                _ => repair_stats.repair_send_errors += 1,
                            }
                        }
                    }
                }
            }
            if stop.load(Ordering::SeqCst) && peer_close_linger_until.is_some() {
                if !did_work {
                    thread::sleep(Duration::from_millis(1));
                }
                continue;
            }
            let frames = reassembler.expire(now);
            let reassembly_stats = reassembler.stats();
            let current_expired = reassembly_stats.frames_incomplete_expired;
            if current_expired > previous_expired {
                repair_stats.repair_deadline_missed += current_expired - previous_expired;
                playout.clear_for_discontinuity();
                begin_queue_recovery(
                    queue,
                    drop_damaged_gop,
                    Instant::now(),
                    reassembly_stats.last_damaged_frame_id,
                )?;
                previous_expired = current_expired;
                record_repair_deadline_misses(
                    reassembly_stats.last_damaged_frame_id,
                    now,
                    &mut requested_frames,
                    &mut repair_started,
                    &mut repair_stats,
                );
            }
            observe_video_frames(&frames, receiver_clock.as_ref(), &mut media_anchor);
            playout.push_frames(frames, now);
            enqueue_network_frames(playout.pop_due(now), queue, max_decode_queue)?;
            update_network_snapshot(
                state,
                &reassembler,
                queue,
                &playout,
                udp_recv_loop_overruns,
                repair_mode,
                repair_stats,
                dispatch_stats,
                video_packet_dispatch_ns_total,
                video_packet_dispatch_count,
                video_packet_dispatch_ns_max,
                media_anchor,
                feedback,
                profile_stats,
            )?;
            if !did_work {
                thread::sleep(Duration::from_millis(1));
            }
        }
        dispatch_stats.peer_last_valid_age_ms = last_valid_peer_activity
            .map(|last| {
                Instant::now()
                    .saturating_duration_since(last)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            })
            .unwrap_or(0);
        if let (Some(peer), Some(reason)) = (peer, cancellation.reason()) {
            if reason.should_notify_peer() {
                let close = crate::media_control::StreamClose {
                    version: crate::media_control::MEDIA_CONTROL_VERSION,
                    stream_id: crate::STREAM_VIDEO,
                    reason_code: reason as u8,
                    video_session_id: reassembler.session_id().unwrap_or(0),
                    close_id: crate::make_session_id(),
                    timestamp_us: receiver_clock.as_ref().map_or_else(
                        || crate::now_millis().saturating_mul(1_000),
                        MediaClock::now_us,
                    ),
                    last_frame_id: reassembler.stats().last_frame_id.unwrap_or(0),
                };
                let close_result = perform_receiver_close_handshake(&socket, peer, close);
                dispatch_stats.stream_close_sent = close_result.0;
                dispatch_stats.stream_close_retry_count = close_result.1;
                dispatch_stats.stream_close_ack_received = close_result.2;
                dispatch_stats.stream_close_handshake_timeout = close_result.3;
            }
        }
        update_network_snapshot(
            state,
            &reassembler,
            queue,
            &playout,
            udp_recv_loop_overruns,
            repair_mode,
            repair_stats,
            dispatch_stats,
            video_packet_dispatch_ns_total,
            video_packet_dispatch_count,
            video_packet_dispatch_ns_max,
            media_anchor,
            feedback,
            profile_stats,
        )
    }

    fn perform_receiver_close_handshake(
        socket: &UdpSocket,
        peer: std::net::SocketAddr,
        close: crate::media_control::StreamClose,
    ) -> (u64, u64, bool, bool) {
        perform_receiver_close_handshake_with_config(
            socket,
            peer,
            close,
            crate::shutdown::ShutdownConfig::default(),
        )
    }

    fn is_expected_udp_peer_reset(kind: io::ErrorKind, peer_known: bool) -> bool {
        // Windows reports an ICMP port-unreachable from a vanished UDP peer as
        // WSAECONNRESET. Once a valid peer is known, liveness timeout owns that
        // failure path; treating it as a runtime fault would bypass peer_timeout.
        peer_known && kind == io::ErrorKind::ConnectionReset
    }

    fn perform_receiver_close_handshake_with_config(
        socket: &UdpSocket,
        peer: std::net::SocketAddr,
        close: crate::media_control::StreamClose,
        config: crate::shutdown::ShutdownConfig,
    ) -> (u64, u64, bool, bool) {
        let Ok(bytes) = close.encode() else {
            return (0, 0, false, true);
        };
        let deadline = Instant::now() + config.close_handshake_timeout;
        let mut retry = config.close_retry_initial;
        let mut sent = 0u64;
        // A final media datagram can already be queued ahead of the close ACK.
        // Use the normal receive size so Windows does not turn that datagram into
        // WSAEMSGSIZE and abort an otherwise healthy close handshake.
        let mut buffer = [0u8; MAX_DATAGRAM_SIZE];
        while Instant::now() < deadline {
            if socket
                .send_to(&bytes, peer)
                .is_ok_and(|written| written == bytes.len())
            {
                sent = sent.saturating_add(1);
            }
            let attempt_deadline = (Instant::now() + retry).min(deadline);
            while Instant::now() < attempt_deadline {
                match socket.recv_from(&mut buffer) {
                    Ok((length, source)) if source == peer => {
                        if crate::media_control::StreamCloseAck::decode(&buffer[..length])
                            .is_ok_and(|ack| ack.matches(close))
                        {
                            return (sent, sent.saturating_sub(1), true, false);
                        }
                        if let Ok(remote_close) =
                            crate::media_control::StreamClose::decode(&buffer[..length])
                        {
                            if let Ok(ack) = remote_close.ack().encode() {
                                let _ = socket.send_to(&ack, peer);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => return (sent, sent.saturating_sub(1), false, true),
                }
            }
            retry = retry.saturating_mul(2).min(config.close_retry_max);
        }
        (sent, sent.saturating_sub(1), false, sent > 0)
    }

    fn observe_video_frames(
        frames: &[EncodedFrame],
        receiver_clock: Option<&MediaClock>,
        media_anchor: &mut ReceiverMediaClockAnchor,
    ) {
        let Some(receiver_clock) = receiver_clock else {
            return;
        };
        for frame in frames {
            media_anchor.observe_video(
                MediaTimestampUs(frame.timestamp_ms.saturating_mul(1000)),
                receiver_clock,
            );
        }
    }

    fn enqueue_network_frames(
        frames: Vec<EncodedFrame>,
        queue: &Arc<Mutex<DecodeQueue>>,
        max_decode_queue: usize,
    ) -> Result<(), String> {
        if frames.is_empty() {
            return Ok(());
        }
        let mut queue = queue
            .lock()
            .map_err(|_| "decode queue lock was poisoned".to_string())?;
        for frame in frames {
            queue.enqueue_frame(frame, max_decode_queue);
        }
        Ok(())
    }

    fn begin_queue_recovery(
        queue: &Arc<Mutex<DecodeQueue>>,
        drop_damaged_gop: bool,
        now: Instant,
        damaged_frame_id: Option<u64>,
    ) -> Result<(), String> {
        if drop_damaged_gop {
            queue
                .lock()
                .map_err(|_| "decode queue lock was poisoned".to_string())?
                .begin_damaged_gop_recovery(now, damaged_frame_id);
        }
        Ok(())
    }

    fn record_repair_deadline_misses(
        last_damaged_frame_id: Option<u64>,
        now: Instant,
        requested_frames: &mut HashSet<u64>,
        repair_started: &mut HashMap<u64, Instant>,
        repair_stats: &mut ReceiverRepairStats,
    ) {
        let Some(last_damaged_frame_id) = last_damaged_frame_id else {
            return;
        };
        let expired_frames: Vec<u64> = repair_started
            .keys()
            .copied()
            .filter(|frame_id| *frame_id <= last_damaged_frame_id)
            .collect();
        for frame_id in expired_frames {
            if let Some(started_at) = repair_started.remove(&frame_id) {
                let elapsed_ms = now.saturating_duration_since(started_at).as_secs_f64() * 1000.0;
                repair_stats.repair_deadline_ms_total += elapsed_ms;
                repair_stats.repair_deadline_ms_max =
                    repair_stats.repair_deadline_ms_max.max(elapsed_ms);
                repair_stats.repair_deadline_samples += 1;
            }
            requested_frames.remove(&frame_id);
        }
    }

    fn update_network_snapshot(
        state: &Arc<Mutex<SharedNetworkState>>,
        reassembler: &H264Reassembler,
        queue: &Arc<Mutex<DecodeQueue>>,
        playout: &PlayoutBuffer,
        udp_recv_loop_overruns: u64,
        repair_mode: crate::repair::RepairMode,
        repair_stats: ReceiverRepairStats,
        dispatch_stats: PacketDispatchStats,
        video_packet_dispatch_ns_total: u64,
        video_packet_dispatch_count: u64,
        video_packet_dispatch_ns_max: u64,
        media_anchor: ReceiverMediaClockAnchor,
        feedback: CapabilityFeedbackCounters,
        profile: ReceiverProfileStats,
    ) -> Result<(), String> {
        let queue = queue
            .lock()
            .map_err(|_| "decode queue lock was poisoned".to_string())?;
        let mut state = state
            .lock()
            .map_err(|_| "network state lock was poisoned".to_string())?;
        let udp_recv_buffer_bytes = state.snapshot.udp_recv_buffer_bytes;
        let udp_recv_buffer_bytes_requested = state.snapshot.udp_recv_buffer_bytes_requested;
        let feedback_sample_eligible = state.snapshot.feedback_sample_eligible;
        let receiver_valid_feedback_windows = state.snapshot.receiver_valid_feedback_windows;
        let receiver_render_ready = state.snapshot.receiver_render_ready;
        let receiver_profile_settled = state.snapshot.receiver_profile_settled;
        let receiver_profile_acknowledged = state.snapshot.receiver_profile_acknowledged;
        let receiver_first_idr_decoded = state.snapshot.receiver_first_idr_decoded;
        let receiver_first_frame_rendered = state.snapshot.receiver_first_frame_rendered;
        let profile_transition_active = state.snapshot.profile_transition_active;
        let profile_transition_started_us = state.snapshot.profile_transition_started_us;
        let profile_transition_deadline_us = state.snapshot.profile_transition_deadline_us;
        let profile_transition_phase = state.snapshot.profile_transition_phase;
        let transition_timeout_count = state.snapshot.transition_timeout_count;
        let transition_failure_reason = state.snapshot.transition_failure_reason;
        let transition_settle_windows = state.snapshot.transition_settle_windows;
        let transition_settle_duration_ms = state.snapshot.transition_settle_duration_ms;
        let new_session_first_idr_wait_ms = state.snapshot.new_session_first_idr_wait_ms;
        let transition_recovery_count = state.snapshot.transition_recovery_count;
        let transition_settle_restart_count = state.snapshot.transition_settle_restart_count;
        let transition_settle_deadline_remaining_ms =
            state.snapshot.transition_settle_deadline_remaining_ms;
        let transition_overall_deadline_remaining_ms =
            state.snapshot.transition_overall_deadline_remaining_ms;
        let transition_failure_stage = state.snapshot.transition_failure_stage;
        state.snapshot = NetworkSnapshot {
            reassembly: reassembler.stats(),
            session_id: reassembler.session_id(),
            inflight_frames: reassembler.inflight_len(),
            completed_waiting: reassembler.completed_waiting_len(),
            decode_queue: queue.frame_len(),
            decode_queue_peak: queue.decode_queue_peak,
            frames_predecode_dropped: queue.frames_predecode_dropped,
            frames_waiting_keyframe_dropped: queue.frames_waiting_keyframe_dropped,
            keyframe_recovery_count: queue.keyframe_recovery_count,
            last_keyframe_id: queue.last_keyframe_id,
            waiting_keyframe: queue.waiting_for_keyframe || queue.damaged_gop.waiting_keyframe(),
            damaged_gop: queue.damaged_gop_stats(),
            drop_damaged_gop: queue.damaged_gop.enabled(),
            udp_recv_buffer_bytes,
            udp_recv_buffer_bytes_requested,
            udp_recv_loop_overruns,
            complete_frame_queue_drops: queue.complete_frame_queue_drops,
            playout: playout.stats(),
            playout_buffer_frames: playout.len(),
            playout_delay_ms: playout.delay_ms(),
            repair_mode,
            nack_packets_sent: repair_stats.nack_packets_sent,
            nack_items_sent: repair_stats.nack_items_sent,
            nack_frames_requested: repair_stats.nack_frames_requested,
            nack_rounds_sent: repair_stats.nack_rounds_sent,
            repair_packets_received: repair_stats.repair_packets_received,
            repair_packets_inserted: repair_stats.repair_packets_inserted,
            repair_duplicate_packets: repair_stats.repair_duplicate_packets,
            repair_frames_completed: repair_stats.repair_frames_completed,
            repair_send_errors: repair_stats.repair_send_errors,
            repair_deadline_missed: repair_stats.repair_deadline_missed,
            repair_wait_ms_total: repair_stats.repair_wait_ms_total,
            repair_wait_ms_max: repair_stats.repair_wait_ms_max,
            nack_candidate_frames: repair_stats.nack_candidate_frames,
            nack_suppressed_progressing_frames: repair_stats.nack_suppressed_progressing_frames,
            nack_suppressed_too_early: repair_stats.nack_suppressed_too_early,
            nack_suppressed_already_requested: repair_stats.nack_suppressed_already_requested,
            nack_suppressed_item_limit: repair_stats.nack_suppressed_item_limit,
            nack_items_deduped: repair_stats.nack_items_deduped,
            nack_items_per_requested_frame_total: repair_stats.nack_items_per_requested_frame_total,
            nack_items_per_requested_frame_max: repair_stats.nack_items_per_requested_frame_max,
            video_data_packets_received: dispatch_stats.video_data_packets_received,
            video_fec_packets_received: dispatch_stats.video_fec_packets_received,
            audio_packets_received: dispatch_stats.audio_packets_received,
            unknown_packets_received: dispatch_stats.unknown_packets_received,
            nack_control_packets_received: dispatch_stats.nack_control_packets_received,
            repair_packets_dropped_wrong_type: dispatch_stats.repair_packets_dropped_wrong_type,
            repair_packets_dropped_late: dispatch_stats.repair_packets_dropped_late,
            repair_packets_dropped_no_frame: dispatch_stats.repair_packets_dropped_no_frame,
            repair_deadline_ms_total: repair_stats.repair_deadline_ms_total,
            repair_deadline_ms_max: repair_stats.repair_deadline_ms_max,
            repair_deadline_samples: repair_stats.repair_deadline_samples,
            repair_unique_packets_received: repair_stats.repair_unique_packets_received,
            repair_duplicate_packets_received: repair_stats.repair_duplicate_packets_received,
            repair_packets_received_after_frame_complete: repair_stats
                .repair_packets_received_after_frame_complete,
            repair_cancelled_frame_complete: repair_stats.repair_cancelled_frame_complete,
            nack_candidates_first_round: repair_stats.nack_candidates_first_round,
            nack_candidates_late_discovery: repair_stats.nack_candidates_late_discovery,
            missing_first_detected_age: repair_stats.missing_first_detected_age,
            missing_first_detected_to_deadline: repair_stats.missing_first_detected_to_deadline,
            first_nack_to_deadline: repair_stats.first_nack_to_deadline,
            first_nack_age: repair_stats.first_nack_age,
            first_round_budget: repair_stats.first_round_budget,
            second_round_budget: repair_stats.second_round_budget,
            repair_arrival_to_deadline: repair_stats.repair_arrival_to_deadline,
            video_packet_dispatch_ns_total,
            video_packet_dispatch_count,
            video_packet_dispatch_ns_max,
            media_anchor,
            capability_feedback_sent: feedback.sent,
            capability_feedback_send_errors: feedback.send_errors,
            profile_changes_received: profile.changes_received,
            profile_changes_invalid: profile.changes_invalid,
            profile_change_sequence: profile.change_sequence,
            profile_generation: profile.generation,
            profile_target_width: profile.target_width,
            profile_target_height: profile.target_height,
            profile_target_fps: profile.target_fps,
            profile_target_bitrate_mbps: profile.target_bitrate_mbps,
            profile_decoder_resets: profile.decoder_resets,
            stale_profile_packets_dropped: profile.stale_packets_dropped,
            feedback_sample_eligible,
            receiver_valid_feedback_windows,
            receiver_render_ready,
            receiver_profile_settled,
            receiver_profile_acknowledged,
            receiver_first_idr_decoded,
            receiver_first_frame_rendered,
            profile_transition_active: profile.pending_profile_present || profile_transition_active,
            profile_transition_started_us,
            profile_transition_deadline_us,
            profile_transition_phase: if profile.pending_profile_present {
                crate::profile_transition::ReceiverTransitionPhase::AwaitNewSessionData
            } else {
                profile_transition_phase
            },
            transition_timeout_count,
            transition_failure_reason,
            transition_settle_windows,
            transition_settle_duration_ms,
            new_session_first_idr_wait_ms,
            transition_recovery_count,
            transition_settle_restart_count,
            transition_settle_deadline_remaining_ms,
            transition_overall_deadline_remaining_ms,
            transition_failure_stage,
            pending_profile_present: profile.pending_profile_present,
            pending_profile_age_ms: profile.pending_profile_age_ms,
            pending_profile_deadline_remaining_ms: profile.pending_profile_deadline_remaining_ms,
            pending_profile_change_sequence: profile.pending_profile_change_sequence,
            pending_profile_generation: profile.pending_profile_generation,
            pending_profile_old_session_id: profile.pending_profile_old_session_id,
            pending_profile_new_session_id: profile.pending_profile_new_session_id,
            mprf_packets_received: profile.mprf_packets_received,
            mprf_ack_packets_sent: profile.mprf_ack_packets_sent,
            mprf_duplicate_reacked: profile.mprf_duplicate_reacked,
            mprf_pending_expired: profile.mprf_pending_expired,
            mprf_rejected_foreign_peer: profile.mprf_rejected_foreign_peer,
            mprf_rejected_old_session: profile.mprf_rejected_old_session,
            mprf_rejected_sequence: profile.mprf_rejected_sequence,
            mprf_rejected_generation: profile.mprf_rejected_generation,
            mprf_rejected_invalid_fields: profile.mprf_rejected_invalid_fields,
            new_session_activation_count: profile.new_session_activation_count,
            new_session_first_packet_wait_ms_total: profile.new_session_first_packet_wait_ms_total,
            new_session_first_packet_wait_ms_max: profile.new_session_first_packet_wait_ms_max,
            stream_close_received: dispatch_stats.stream_close_received,
            stream_close_ack_sent: dispatch_stats.stream_close_ack_sent,
            stream_close_rejected_pre_session: dispatch_stats.stream_close_rejected_pre_session,
            stream_close_rejected_peer: dispatch_stats.stream_close_rejected_peer,
            stream_close_rejected_session: dispatch_stats.stream_close_rejected_session,
            stream_close_rejected_invalid: dispatch_stats.stream_close_rejected_invalid,
            stream_close_duplicate: dispatch_stats.stream_close_duplicate,
            stream_close_sent: dispatch_stats.stream_close_sent,
            stream_close_retry_count: dispatch_stats.stream_close_retry_count,
            stream_close_ack_received: dispatch_stats.stream_close_ack_received,
            stream_close_handshake_timeout: dispatch_stats.stream_close_handshake_timeout,
            peer_timeout_triggered: dispatch_stats.peer_timeout_triggered,
            peer_last_valid_age_ms: dispatch_stats.peer_last_valid_age_ms,
        };
        Ok(())
    }

    fn process_encoded_frame(
        frame: EncodedFrame,
        decode_state: &mut DecodeState,
        render_state: &Arc<Mutex<RenderWorkerState>>,
        stats: &mut ViewerStats,
        decoder_fps: u32,
    ) {
        if frame.is_idr() {
            decode_state.last_keyframe_id = Some(frame.frame_id);
        }
        if decode_state.waiting_for_keyframe {
            if !frame.is_idr() {
                stats.frames_waiting_keyframe_dropped += 1;
                return;
            }
            let dimensions = match dimensions_from_sps(&frame.bytes) {
                Ok(dimensions) => dimensions,
                Err(err) => {
                    stats.frames_waiting_keyframe_dropped += 1;
                    eprintln!(
                        "keyframe {} has no usable SPS; waiting for next keyframe: {err}",
                        frame.frame_id
                    );
                    return;
                }
            };
            match WmfH264Decoder::new(dimensions.width, dimensions.height, decoder_fps) {
                Ok(decoder) => {
                    decode_state.decoder = Some(decoder);
                    decode_state.dimensions = Some(dimensions);
                    decode_state.waiting_for_keyframe = false;
                    decode_state.input_index = 0;
                    decode_state.last_keyframe_id = Some(frame.frame_id);
                }
                Err(err) => {
                    stats.decoder_errors += 1;
                    eprintln!(
                        "decoder initialization failed at frame {}: {err}",
                        frame.frame_id
                    );
                    return;
                }
            }
        }

        let Some(decoder) = decode_state.decoder.as_mut() else {
            stats.frames_waiting_keyframe_dropped += 1;
            decode_state.waiting_for_keyframe = true;
            return;
        };
        stats.frames_decoder_input += 1;
        let decode_started = Instant::now();
        let decoded = match decoder.decode_access_unit(&frame.bytes, decode_state.input_index) {
            Ok(decoded) => decoded,
            Err(err) => {
                stats.decoder_errors += 1;
                eprintln!(
                    "decoder rejected frame {} timestamp_ms={}: {err}; waiting for next keyframe",
                    frame.frame_id, frame.timestamp_ms
                );
                decode_state.mark_discontinuity(stats, true);
                return;
            }
        };
        decode_state.input_index += 1;
        if !decoded.is_empty() {
            stats.decode_ms_total += decode_started.elapsed().as_secs_f64() * 1000.0;
            stats.frames_decoded += decoded.len() as u64;
        }

        let Some(dimensions) = decode_state.dimensions else {
            return;
        };
        for decoded_frame in decoded {
            decode_state.next_render_generation += 1;
            stats.render_buffer_generation = decode_state.next_render_generation;
            stats.nv12_y_stride = decoded_frame.y_stride;
            stats.nv12_uv_stride = decoded_frame.uv_stride;
            stats.nv12_uv_offset = decoded_frame.uv_offset;
            stats.nv12_allocated_height = decoded_frame.allocated_height;
            stats.nv12_buffer_len = decoded_frame.source_buffer_len;
            stats.expected_tight_len = decoded_frame.expected_tight_len;
            stats.decoder_used_2d_buffer = decoded_frame.used_2d_buffer;
            stats.color_spec = decoded_frame.color_spec;
            stats.decoder_color_metadata = decoded_frame.color_metadata;
            submit_render_frame(
                render_state,
                PendingRender {
                    frame_id: frame.frame_id,
                    timestamp_us: frame.timestamp_ms.saturating_mul(1000),
                    frame: decoded_frame,
                    width: dimensions.width,
                    height: dimensions.height,
                },
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_render_thread(
        shared: Arc<Mutex<RenderWorkerState>>,
        stop: Arc<AtomicBool>,
        cancellation: crate::shutdown::CancellationToken,
        title: String,
        scale_mode: crate::win32_gdi_viewer::RenderScaleMode,
        window_mode: crate::win32_gdi_viewer::WindowMode,
        backend: crate::video_renderer::RenderBackend,
        debug_directory: Option<String>,
        debug_limit: usize,
        audio_master: Option<Arc<Mutex<crate::audio_udp::AudioMasterClockState>>>,
        video_playout_delay_ms: u64,
        audio_jitter_buffer_ms: u32,
        av_sync_mode: AvSyncMode,
        display_refresh_detect: crate::display_capability::DisplayRefreshDetect,
        capability_source: Arc<Mutex<CapabilityFeedbackSource>>,
    ) -> Result<(thread::JoinHandle<()>, VideoRenderStats), String> {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("agoralink-h264-render".to_string())
            .spawn(move || {
                let mut renderer = match VideoRenderer::create_with_display_detection(
                    &title,
                    scale_mode,
                    window_mode,
                    backend,
                    display_refresh_detect,
                ) {
                    Ok(renderer) => renderer,
                    Err(err) => {
                        let _ = started_tx.send(Err(err.clone()));
                        if let Ok(mut state) = shared.lock() {
                            state.error = Some(err);
                        }
                        return;
                    }
                };
                let initial_stats = renderer.stats();
                let _ = started_tx.send(Ok(initial_stats.clone()));
                if let Ok(mut state) = shared.lock() {
                    state.render_state = initial_stats.clone();
                    state.av_sync_mode = av_sync_mode;
                    state.av_sync_state =
                        if av_sync_mode == AvSyncMode::Conservative && audio_master.is_some() {
                            AvSyncState::WaitingForRenderer
                        } else {
                            AvSyncState::Disabled
                        };
                }
                if let Ok(mut capability) = capability_source.lock() {
                    capability.render = initial_stats.clone();
                }
                let mut dumper = DebugFrameDumper::new(debug_directory, debug_limit);
                let mut generation = 0u64;
                let render_clock = Instant::now();
                // Do not construct a scheduler for video-only or explicit AV-sync-off
                // paths. This preserves the structural audio-off isolation contract.
                let mut av_scheduler = (av_sync_mode == AvSyncMode::Conservative
                    && audio_master.is_some())
                .then(|| AvSyncScheduler::new(AvSyncMode::Conservative));
                let mut audio_clock_jump_detector = av_scheduler
                    .as_ref()
                    .map(|_| MediaClockJumpDetector::default());
                while !stop.load(Ordering::SeqCst) {
                    if !renderer.pump_messages() {
                        cancellation.cancel(crate::shutdown::StopReason::WindowClosed);
                        if let Ok(mut state) = shared.lock() {
                            state.closed_by_user = true;
                        }
                        stop.store(true, Ordering::SeqCst);
                        break;
                    }
                    if let Ok(mut capability) = capability_source.lock() {
                        capability.render = renderer.stats();
                    }
                    let pending = shared
                        .lock()
                        .ok()
                        .and_then(|mut state| state.pending.take());
                    let Some(pending) = pending else {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    };
                    let now_us =
                        render_clock.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                    let renderer_initialized = renderer.stats().window.initialized;
                    if let Some(scheduler) = av_scheduler.as_mut() {
                        let master_status = audio_master
                            .as_ref()
                            .map(crate::audio_udp::audio_master_sync_status)
                            .unwrap_or_default();
                        let video_offset_us = video_playout_delay_ms
                            .saturating_sub(u64::from(audio_jitter_buffer_ms))
                            .saturating_mul(1000);
                        let clock_jump = match (
                            audio_clock_jump_detector.as_mut(),
                            master_status.playhead_us,
                        ) {
                            (Some(detector), Some(playhead_us)) => {
                                detector.observe(now_us, playhead_us)
                            }
                            (Some(detector), None) => {
                                detector.reset();
                                false
                            }
                            _ => false,
                        };
                        let decision = scheduler.decide(AvSyncInput {
                            now_us,
                            video_timestamp_us: pending.timestamp_us,
                            renderer_initialized,
                            audio_enabled: true,
                            audio_playhead_us: master_status.playhead_us,
                            audio_master_stable: master_status.stable,
                            audio_master_stale: master_status.stale,
                            timeline_discontinuity: master_status.timeline_discontinuity,
                            device_padding_valid: master_status.device_padding_valid,
                            clock_jump,
                            session_matched: master_status.session_matched,
                            video_offset_us,
                        });
                        let telemetry = scheduler.telemetry(now_us);
                        if let Ok(mut state) = shared.lock() {
                            apply_av_sync_telemetry(&mut state, av_sync_mode, telemetry);
                            if let Some(playhead_ts_us) = master_status.playhead_us {
                                let target = playhead_ts_us.saturating_add(video_offset_us);
                                let offset_ms = (pending.timestamp_us as i128 - target as i128)
                                    .unsigned_abs()
                                    as f64
                                    / 1000.0;
                                state.av_sync_offset_ms_total += offset_ms;
                                state.av_sync_offset_ms_max =
                                    state.av_sync_offset_ms_max.max(offset_ms);
                                state.av_sync_offset_samples += 1;
                            }
                        }
                        match decision {
                            AvSyncDecision::Hold { .. } => {
                                if let Ok(mut state) = shared.lock() {
                                    if state.pending.is_some() {
                                        state.frames_replaced += 1;
                                    }
                                    state.pending = Some(pending);
                                }
                                thread::sleep(Duration::from_millis(2));
                                continue;
                            }
                            AvSyncDecision::DropLateFrame => continue,
                            AvSyncDecision::RenderNow | AvSyncDecision::BypassAndRender(_) => {}
                        }
                    } else if let Ok(mut state) = shared.lock() {
                        state.av_sync_mode = av_sync_mode;
                        state.av_sync_state = AvSyncState::Disabled;
                        state.av_sync_bypass_reason = None;
                        state.video_sync_gating_enabled = false;
                    }
                    generation += 1;
                    let started = Instant::now();
                    let result = dumper
                        .maybe_dump(&pending.frame, pending.width, pending.height, generation)
                        .and_then(|_| {
                            renderer.render_decoded(
                                &pending.frame,
                                pending.width,
                                pending.height,
                                pending.frame_id,
                            )
                        });
                    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                    if let Ok(mut state) = shared.lock() {
                        state.render_state = renderer.stats();
                        if let Err(err) = result {
                            cancellation.cancel(crate::shutdown::StopReason::InternalError);
                            state.error = Some(err);
                            stop.store(true, Ordering::SeqCst);
                        } else {
                            state.frames_rendered += 1;
                            state.render_ms_total += elapsed_ms;
                        }
                    }
                }
            })
            .map_err(|err| format!("spawn H.264 render thread failed: {err}"))?;
        let initial = started_rx
            .recv()
            .map_err(|_| "render thread ended before initialization".to_string())??;
        Ok((handle, initial))
    }

    fn submit_render_frame(shared: &Arc<Mutex<RenderWorkerState>>, frame: PendingRender) {
        if let Ok(mut state) = shared.lock() {
            if state.pending.replace(frame).is_some() {
                state.frames_replaced += 1;
            }
            state.queue_peak = state.queue_peak.max(1);
        }
    }

    fn apply_av_sync_telemetry(
        state: &mut RenderWorkerState,
        mode: AvSyncMode,
        telemetry: crate::av_sync::AvSyncTelemetry,
    ) {
        state.av_sync_mode = mode;
        state.av_sync_state = telemetry.state;
        state.av_sync_bypass_reason = telemetry.bypass_reason;
        state.av_sync_state_transitions = telemetry.state_transitions;
        state.av_sync_forced_release_count = telemetry.forced_release_count;
        state.av_sync_hold_epoch_ms = telemetry.hold_epoch_ms;
        state.video_frames_held_for_av_sync = telemetry.video_frames_actually_held;
        state.video_frames_dropped_for_av_sync = telemetry.video_frames_actually_dropped;
        state.video_sync_gating_enabled = telemetry.state == AvSyncState::Active;
    }

    fn sync_render_worker_stats(shared: &Arc<Mutex<RenderWorkerState>>, stats: &mut ViewerStats) {
        let Ok(state) = shared.lock() else {
            return;
        };
        stats.frames_rendered = state.frames_rendered;
        stats.render_ms_total = state.render_ms_total;
        stats.render_state = state.render_state.clone();
        stats.frames_decoded_not_rendered = state.frames_replaced;
        stats.frames_render_skipped = state.frames_replaced;
        stats.render_queue_peak = state.queue_peak;
        stats.decoded_frame_queue_len = usize::from(state.pending.is_some());
        stats.decoded_frame_queue_drops = state.frames_replaced;
        stats.render_buffer_reused = state.frames_replaced;
        stats.video_frames_dropped_for_av_sync = state.video_frames_dropped_for_av_sync;
        stats.video_frames_held_for_av_sync = state.video_frames_held_for_av_sync;
        stats.av_sync_offset_ms_total = state.av_sync_offset_ms_total;
        stats.av_sync_offset_ms_max = state.av_sync_offset_ms_max;
        stats.av_sync_offset_samples = state.av_sync_offset_samples;
        stats.video_sync_gating_enabled = state.video_sync_gating_enabled;
        stats.av_sync_mode = state.av_sync_mode;
        stats.av_sync_state = state.av_sync_state;
        stats.av_sync_bypass_reason = state.av_sync_bypass_reason;
        stats.av_sync_state_transitions = state.av_sync_state_transitions;
        stats.av_sync_forced_release_count = state.av_sync_forced_release_count;
        stats.av_sync_hold_epoch_ms = state.av_sync_hold_epoch_ms;
    }

    #[allow(clippy::too_many_arguments)]
    fn print_stats(
        snapshot: NetworkSnapshot,
        stats: &ViewerStats,
        dimensions: Option<VideoDimensions>,
        decoder_waiting_keyframe: bool,
        decoder_last_keyframe_id: Option<u64>,
        strict_decode_order: bool,
        previous_network: ReassemblyStats,
        previous_decoded: u64,
        previous_decoder_input: u64,
        previous_rendered: u64,
        elapsed: Duration,
        mode: H264RecvViewMode,
        event_context: &crate::shutdown::RuntimeEventContext,
        cancellation: &crate::shutdown::CancellationToken,
        lifecycle_state: &str,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let dimensions = dimensions.unwrap_or(VideoDimensions {
            width: 0,
            height: 0,
        });
        let fps_decode = stats.frames_decoded.saturating_sub(previous_decoded) as f64 / elapsed_sec;
        let fps_render =
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec;
        let mbps = snapshot
            .reassembly
            .bytes_received
            .saturating_sub(previous_network.bytes_received) as f64
            * 8.0
            / elapsed_sec
            / 1_000_000.0;
        match mode {
            H264RecvViewMode::Probe => {
                println!(
                    r#"{{"type":"H264_RECV_VIEW_STATS","mode":"h264_recv_view","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_render_skipped":{},"frames_decoded_not_rendered":{},"frames_incomplete_expired":{},"frames_predecode_dropped":{},"frames_queue_dropped":{},"frames_waiting_keyframe_dropped":{},"keyframe_recovery_count":{},"decoder_errors":{},"decoder_resets":{},"decode_queue":{},"decode_queue_peak":{},"render_queue_peak":{},"render_frame_copies":{},"render_buffer_reused":{},"render_buffer_generation":{},"nv12_y_stride":{},"nv12_uv_stride":{},"nv12_uv_offset":{},"nv12_allocated_height":{},"nv12_buffer_len":{},"expected_tight_len":{},"decoder_used_2d_buffer":{},"fps_decode":{:.2},"fps_render":{:.2},"mbps":{:.3},"last_frame_id":{},"last_keyframe_id":{},"waiting_keyframe":{},"inflight_frames":{},"completed_waiting":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},{},{},{}}}"#,
                    strict_decode_order,
                    optional_u64_json(snapshot.session_id),
                    snapshot.reassembly.packets_received,
                    snapshot.reassembly.packets_invalid,
                    snapshot.reassembly.packets_lost_estimate,
                    snapshot.reassembly.frames_complete,
                    stats.frames_decoded,
                    stats.frames_rendered,
                    stats.frames_render_skipped,
                    stats.frames_decoded_not_rendered,
                    snapshot.reassembly.frames_incomplete_expired,
                    snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
                    snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
                    snapshot.frames_waiting_keyframe_dropped
                        + stats.frames_waiting_keyframe_dropped,
                    snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    snapshot.decode_queue,
                    snapshot.decode_queue_peak,
                    stats.render_queue_peak,
                    stats.render_frame_copies,
                    stats.render_buffer_reused,
                    stats.render_buffer_generation,
                    stats.nv12_y_stride,
                    stats.nv12_uv_stride,
                    stats.nv12_uv_offset,
                    stats.nv12_allocated_height,
                    stats.nv12_buffer_len,
                    stats.expected_tight_len,
                    stats.decoder_used_2d_buffer,
                    fps_decode,
                    fps_render,
                    mbps,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    snapshot.inflight_frames,
                    snapshot.completed_waiting,
                    average(stats.decode_ms_total, stats.frames_decoded),
                    average(stats.render_ms_total, stats.frames_rendered),
                    dimensions.width,
                    dimensions.height,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    receiver_transport_fragment(
                        snapshot,
                        stats,
                        previous_network,
                        elapsed_sec,
                        decoder_last_keyframe_id,
                        previous_decoder_input,
                        previous_rendered,
                    )
                );
            }
            H264RecvViewMode::Screen => {
                let context = event_context.json_fragment(
                    "receiver",
                    crate::STREAM_VIDEO,
                    snapshot.session_id,
                    snapshot.profile_generation,
                    "interval",
                    lifecycle_state,
                    cancellation,
                );
                println!(
                    r#"{{"type":"NATIVE_SCREEN_STATS","role":"receiver","mode":"screen-recv","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_incomplete_expired":{},"decoder_errors":{},"decoder_resets":{},"decode_queue":{},"decode_queue_peak":{},"fps_decode":{:.2},"fps_render":{:.2},"mbps":{:.3},"last_frame_id":{},"waiting_keyframe":{},"inflight_frames":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},{},{},{},{}}}"#,
                    strict_decode_order,
                    optional_u64_json(snapshot.session_id),
                    snapshot.reassembly.packets_received,
                    snapshot.reassembly.packets_lost_estimate,
                    snapshot.reassembly.frames_complete,
                    stats.frames_decoded,
                    stats.frames_rendered,
                    snapshot.reassembly.frames_incomplete_expired,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    snapshot.decode_queue,
                    snapshot.decode_queue_peak,
                    fps_decode,
                    fps_render,
                    mbps,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    snapshot.inflight_frames,
                    average(stats.decode_ms_total, stats.frames_decoded),
                    average(stats.render_ms_total, stats.frames_rendered),
                    dimensions.width,
                    dimensions.height,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    receiver_transport_fragment(
                        snapshot,
                        stats,
                        previous_network,
                        elapsed_sec,
                        decoder_last_keyframe_id,
                        previous_decoder_input,
                        previous_rendered,
                    ),
                    context,
                );
            }
        }
        io::stdout().flush().ok();
    }

    fn print_startup_failure(config: &H264RecvViewConfig, error: &str) {
        if config.mode != H264RecvViewMode::Screen {
            return;
        }
        let reason = if crate::shutdown::ctrl_c_requested() {
            crate::shutdown::StopReason::CtrlC
        } else {
            crate::shutdown::StopReason::StartupFailure
        };
        let cancellation = crate::shutdown::CancellationToken::new();
        cancellation.cancel(reason);
        let context = crate::shutdown::RuntimeEventContext::new(crate::make_session_id());
        let error_context = context.json_fragment(
            "receiver",
            crate::STREAM_VIDEO,
            None,
            0,
            "run_total",
            "failed",
            &cancellation,
        );
        println!(
            r#"{{"type":"NATIVE_SCREEN_ERROR","role":"receiver","mode":"screen-recv","error":"{}",{}}}"#,
            json_escape(error),
            error_context,
        );
        let cleanup_failed = crate::shutdown::worker_ownership_failed(error)
            || crate::shutdown::retained_worker_count() > 0;
        let final_event_type = crate::shutdown::terminal_event_type(!cleanup_failed);
        let final_state = crate::shutdown::terminal_lifecycle_name(!cleanup_failed);
        let stopped_context = context.json_fragment(
            "receiver",
            crate::STREAM_VIDEO,
            None,
            0,
            "run_total",
            final_state,
            &cancellation,
        );
        println!(
            r#"{{"type":"{}","role":"receiver","mode":"screen-recv","reason":"{}","bind":"{}","port":{},"frames_complete":0,"frames_decoded":0,"frames_rendered":0,"packets_received":0,"bytes_received":0,"duration_sec":0.0,"profile_transition_phase":"idle","profile_transition_active":false,"transition_timeout_count":0,"transition_failure_reason":null,"qsv_async_wait_timeouts":0,"qsv_async_wait_cancelled":0,"qsv_drain_timeouts":0,"worker_join_render":"not_started","worker_join_network":"not_started","worker_join_audio":"not_started","worker_join_all_clean":{},"retained_worker_count":{},"close_received":false,"close_ack_sent":false,"peer_timeout_triggered":false,"pending_nack_cancelled":0,"pending_repair_state":"not_started","final_complete_frames":0,"final_decoded_frames":0,"final_rendered_frames":0,"last_error":"{}","cleanup_duration_ms":0.0,{}}}"#,
            final_event_type,
            reason.name(),
            json_escape(&config.bind),
            config.port,
            !cleanup_failed,
            crate::shutdown::retained_worker_count(),
            json_escape(error),
            stopped_context,
        );
        io::stdout().flush().ok();
    }

    fn print_started(
        config: &H264RecvViewConfig,
        render: VideoRenderStats,
        event_context: &crate::shutdown::RuntimeEventContext,
        cancellation: &crate::shutdown::CancellationToken,
        lifecycle_state: &str,
    ) {
        if config.mode != H264RecvViewMode::Screen {
            return;
        }
        let context = event_context.json_fragment(
            "receiver",
            crate::STREAM_VIDEO,
            None,
            0,
            "session_total",
            lifecycle_state,
            cancellation,
        );
        println!(
            r#"{{"type":"NATIVE_SCREEN_STARTED","role":"receiver","mode":"screen-recv","bind":"{}","port":{},"strict_decode_order":{},"drop_damaged_gop":{},"playout_delay_ms":{},"audio_mode":"{}","av_sync_mode":"{}","audio_jitter_buffer_ms":{},"title":"{}",{},{}}}"#,
            json_escape(&config.bind),
            config.port,
            config.strict_decode_order,
            config.drop_damaged_gop,
            config.playout_delay_ms,
            config.audio_mode.name(),
            config.av_sync_mode.name(),
            config.audio_jitter_buffer_ms,
            json_escape(&config.title),
            render.json_fragment(),
            context,
        );
        io::stdout().flush().ok();
    }

    #[allow(clippy::too_many_arguments)]
    fn print_done(
        config: &H264RecvViewConfig,
        snapshot: NetworkSnapshot,
        stats: &ViewerStats,
        dimensions: VideoDimensions,
        decoder_last_keyframe_id: Option<u64>,
        decoder_waiting_keyframe: bool,
        closed_by_user: bool,
        reason: crate::shutdown::StopReason,
        duration_sec: f64,
        shutdown: &ReceiverWorkerShutdownSummary,
        event_context: &crate::shutdown::RuntimeEventContext,
        cancellation: &crate::shutdown::CancellationToken,
    ) {
        let final_summary = format!(
            r#""close_received":{},"close_ack_sent":{},"peer_timeout_triggered":{},"pending_nack_cancelled":{},"pending_repair_state":"{}","final_complete_frames":{},"final_decoded_frames":{},"final_rendered_frames":{},"last_error":{}"#,
            snapshot.stream_close_received > 0,
            snapshot.stream_close_ack_sent > 0,
            snapshot.peer_timeout_triggered,
            snapshot.repair_cancelled_frame_complete,
            if shutdown.network.clean() {
                "cleared"
            } else {
                "incomplete"
            },
            snapshot.reassembly.frames_complete,
            stats.frames_decoded,
            stats.frames_rendered,
            optional_json_string(
                shutdown
                    .runtime_error
                    .as_deref()
                    .or(shutdown.join_error.as_deref()),
            ),
        );
        let transport = format!(
            "{},{},{}",
            receiver_transport_fragment(
                snapshot,
                stats,
                ReassemblyStats::default(),
                duration_sec.max(0.001),
                decoder_last_keyframe_id,
                0,
                0,
            ),
            shutdown.json_fragment(),
            final_summary,
        );
        match config.mode {
            H264RecvViewMode::Probe => {
                println!(
                    r#"{{"type":"H264_RECV_VIEW_DONE","mode":"h264_recv_view","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_render_skipped":{},"frames_decoded_not_rendered":{},"frames_incomplete_expired":{},"frames_predecode_dropped":{},"frames_queue_dropped":{},"frames_waiting_keyframe_dropped":{},"keyframe_recovery_count":{},"decoder_errors":{},"decoder_resets":{},"decode_queue_peak":{},"render_queue_peak":{},"render_frame_copies":{},"render_buffer_reused":{},"render_buffer_generation":{},"nv12_y_stride":{},"nv12_uv_stride":{},"nv12_uv_offset":{},"nv12_allocated_height":{},"nv12_buffer_len":{},"expected_tight_len":{},"decoder_used_2d_buffer":{},"last_keyframe_id":{},"waiting_keyframe":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},"last_frame_id":{},"closed_by_user":{},"stopped_by_console":{},"duration_sec":{:.3},{},{},{}}}"#,
                    config.strict_decode_order,
                    optional_u64_json(snapshot.session_id),
                    snapshot.reassembly.packets_received,
                    snapshot.reassembly.packets_invalid,
                    snapshot.reassembly.packets_lost_estimate,
                    snapshot.reassembly.frames_complete,
                    stats.frames_decoded,
                    stats.frames_rendered,
                    stats.frames_render_skipped,
                    stats.frames_decoded_not_rendered,
                    snapshot.reassembly.frames_incomplete_expired,
                    snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
                    snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
                    snapshot.frames_waiting_keyframe_dropped
                        + stats.frames_waiting_keyframe_dropped,
                    snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    snapshot.decode_queue_peak,
                    stats.render_queue_peak,
                    stats.render_frame_copies,
                    stats.render_buffer_reused,
                    stats.render_buffer_generation,
                    stats.nv12_y_stride,
                    stats.nv12_uv_stride,
                    stats.nv12_uv_offset,
                    stats.nv12_allocated_height,
                    stats.nv12_buffer_len,
                    stats.expected_tight_len,
                    stats.decoder_used_2d_buffer,
                    optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    average(stats.decode_ms_total, stats.frames_decoded),
                    average(stats.render_ms_total, stats.frames_rendered),
                    dimensions.width,
                    dimensions.height,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    closed_by_user,
                    crate::shutdown::ctrl_c_requested(),
                    duration_sec,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    transport
                );
            }
            H264RecvViewMode::Screen => {
                let final_event_type = crate::shutdown::terminal_event_type(shutdown.clean());
                let final_state = crate::shutdown::terminal_lifecycle_name(shutdown.clean());
                let context = event_context.json_fragment(
                    "receiver",
                    crate::STREAM_VIDEO,
                    snapshot.session_id,
                    snapshot.profile_generation,
                    "run_total",
                    final_state,
                    cancellation,
                );
                println!(
                    r#"{{"type":"{}","role":"receiver","mode":"screen-recv","reason":"{}","bind":"{}","port":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"packets_received":{},"bytes_received":{},"packets_lost_estimate":{},"decoder_errors":{},"decoder_resets":{},"duration_sec":{:.3},"width":{},"height":{},"last_frame_id":{},"waiting_keyframe":{},{},{},{},{}}}"#,
                    final_event_type,
                    reason.name(),
                    json_escape(&config.bind),
                    config.port,
                    snapshot.reassembly.frames_complete,
                    stats.frames_decoded,
                    stats.frames_rendered,
                    snapshot.reassembly.packets_received,
                    snapshot.reassembly.bytes_received,
                    snapshot.reassembly.packets_lost_estimate,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    duration_sec,
                    dimensions.width,
                    dimensions.height,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    transport,
                    context,
                );
            }
        }
    }

    fn network_snapshot(state: &Arc<Mutex<SharedNetworkState>>) -> NetworkSnapshot {
        state
            .lock()
            .map_or_else(|_| NetworkSnapshot::default(), |state| state.snapshot)
    }

    fn validate_config(config: &H264RecvViewConfig) -> Result<(), String> {
        if config.port == 0 {
            return Err("port must be greater than zero".to_string());
        }
        if config.frame_timeout_ms == 0 {
            return Err("frame-timeout-ms must be greater than zero".to_string());
        }
        PlayoutBuffer::new(config.playout_delay_ms)?;
        if config.max_inflight_frames == 0 || config.max_decode_queue == 0 {
            return Err(
                "max-inflight-frames and max-decode-queue must be greater than zero".to_string(),
            );
        }
        if config.json_interval_ms == 0 {
            return Err("json-interval-ms must be greater than zero".to_string());
        }
        if config.title.trim().is_empty() {
            return Err("title must not be empty".to_string());
        }
        if config
            .debug_dump_frames
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err("debug-dump-frames must not be empty".to_string());
        }
        if config.debug_dump_limit == 0 {
            return Err("debug-dump-limit must be greater than zero".to_string());
        }
        if !(1..=50).contains(&config.nack_delay_ms)
            || !(1..=50).contains(&config.nack_repeat_ms)
            || !(1..=10).contains(&config.nack_max_rounds)
        {
            return Err("invalid NACK timing configuration".to_string());
        }
        if config.audio_jitter_buffer_ms > 500 {
            return Err("audio-jitter-buffer-ms must be between 0 and 500".to_string());
        }
        if !(500..=2000).contains(&config.capability_feedback_ms) {
            return Err("adaptive-feedback-ms must be between 500 and 2000".to_string());
        }
        Ok(())
    }

    fn average(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn receiver_transport_fragment(
        snapshot: NetworkSnapshot,
        stats: &ViewerStats,
        previous: ReassemblyStats,
        elapsed_sec: f64,
        decoder_last_keyframe_id: Option<u64>,
        previous_decoder_input: u64,
        previous_rendered: u64,
    ) -> String {
        let packets_per_second = snapshot
            .reassembly
            .packets_received
            .saturating_sub(previous.packets_received) as f64
            / elapsed_sec.max(0.001);
        let video_packet_dispatch_ms_avg = if snapshot.video_packet_dispatch_count == 0 {
            0.0
        } else {
            snapshot.video_packet_dispatch_ns_total as f64
                / snapshot.video_packet_dispatch_count as f64
                / 1_000_000.0
        };
        let video_packet_dispatch_ms_max =
            snapshot.video_packet_dispatch_ns_max as f64 / 1_000_000.0;
        let mut output = format!(
            r#""udp_recv_buffer_bytes":{},"udp_recv_buffer_bytes_requested":{},"udp_recv_buffer_bytes_actual":{},"packets_per_second":{:.2},"udp_recv_packets_per_second":{:.2},"udp_recv_loop_overruns":{},"complete_frame_queue_len":{},"complete_frame_queue_peak":{},"complete_frame_queue_drops":{},"reassembly_frames_active":{},"reassembly_packets_active":{},"reassembly_packet_slots_reserved":{},"reassembly_payload_bytes_reserved":{},"reassembly_budget_rejected_frames":{},"reassembly_oversize_frames":{},"reassembly_fast_path_enabled":true,"reassembly_allocations_estimate":{},"reassembly_complete_scan_count":{},"playout_delay_ms":{},"playout_buffer_frames":{},"playout_buffer_peak_frames":{},"playout_late_frames":{},"playout_dropped_late_frames":{},"playout_dropped_discontinuity_frames":{},"playout_delay_actual_ms_avg":{:.3},"playout_delay_actual_ms_max":{:.3},"media_clock":"instant",{},"decoder_configured_fps":{},"decoder_sample_duration_us":{},"decoder_input_fps":{:.2},"render_output_fps":{:.2},"active_render_fps":{:.2},"decode_thread_fps":{:.2},"render_thread_fps":{:.2},"decoded_frame_queue_len":{},"decoded_frame_queue_peak":{},"decoded_frame_queue_drops":{},"decoded_frames_replaced_by_latest":{},"render_latest_slot_replacements":{},"decoder_blocked_by_render_count":0,"frames_missing_packets":{},"frames_dropped_incomplete":{},"fec_mode":"{}","fec_packets_received":{},"fec_frames_recovered":{},"fec_packets_recovered":{},"fec_recovery_failed_multi_missing":{},"fec_recovery_failed_no_parity":{},"fec_recovery_failed_invalid":{},"frames_missing_after_fec":{},"frames_dropped_after_fec":{},"keyframe_recovery_count":{},"last_keyframe_id":{},"decoder_resets":{},"drop_damaged_gop":{},"damaged_gop_count":{},"frames_discarded_damaged_gop":{},"frames_discarded_waiting_keyframe":{},"waiting_keyframe_entries":{},"waiting_keyframe_exits":{},"idr_frames_received":{},"idr_frames_used_for_recovery":{},"non_idr_frames_discarded_waiting":{},"recovery_wait_ms_avg":{:.3},"recovery_wait_ms_max":{:.3},"recovery_wait_frames_avg":{:.3},"recovery_wait_frames_max":{},"next_decode_frame_id":{},"decode_gate_stalls":{},"decode_gate_gap_events":{},"decode_gate_gap_to_damage_ms_avg":{:.3},"decode_gate_gap_to_damage_ms_max":{:.3},"frames_buffered_waiting_order":{},"frames_discarded_decode_gate":{},"reorder_wait_ms":{},"video_packet_dispatch_ms_avg":{:.6},"video_packet_dispatch_ms_max":{:.6},{},{},{}"#,
            snapshot.udp_recv_buffer_bytes,
            snapshot.udp_recv_buffer_bytes_requested,
            snapshot.udp_recv_buffer_bytes,
            packets_per_second,
            packets_per_second,
            snapshot.udp_recv_loop_overruns,
            snapshot.decode_queue,
            snapshot.decode_queue_peak,
            snapshot.complete_frame_queue_drops,
            snapshot.reassembly.reassembly_frames_active,
            snapshot.reassembly.reassembly_packets_active,
            snapshot.reassembly.reassembly_packet_slots_reserved,
            snapshot.reassembly.reassembly_payload_bytes_reserved,
            snapshot.reassembly.reassembly_budget_rejected_frames,
            snapshot.reassembly.reassembly_oversize_frames,
            snapshot.reassembly.reassembly_allocations_estimate,
            snapshot.reassembly.reassembly_complete_scan_count,
            snapshot.playout_delay_ms,
            snapshot.playout_buffer_frames,
            snapshot.playout.buffer_peak_frames,
            snapshot.playout.late_frames,
            snapshot.playout.dropped_late_frames,
            snapshot.playout.dropped_discontinuity_frames,
            snapshot.playout.delay_actual_ms_avg(),
            snapshot.playout.delay_actual_ms_max,
            snapshot.media_anchor.json_fragment(),
            stats.decoder_configured_fps,
            crate::wmf_h264_decoder::decoder_sample_duration_us(
                stats.decoder_configured_fps.max(1),
            )
            .unwrap_or_default(),
            stats
                .frames_decoder_input
                .saturating_sub(previous_decoder_input) as f64
                / elapsed_sec.max(0.001),
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec.max(0.001),
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec.max(0.001),
            stats
                .frames_decoder_input
                .saturating_sub(previous_decoder_input) as f64
                / elapsed_sec.max(0.001),
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec.max(0.001),
            stats.decoded_frame_queue_len,
            stats.render_queue_peak,
            stats.decoded_frame_queue_drops,
            stats.frames_decoded_not_rendered,
            stats.frames_decoded_not_rendered,
            snapshot.reassembly.frames_incomplete_expired,
            snapshot.reassembly.frames_incomplete_expired,
            if snapshot.reassembly.fec_packets_received > 0
                || snapshot.reassembly.fec_protected_data_packets_received > 0
            {
                "single-xor"
            } else {
                "off"
            },
            snapshot.reassembly.fec_packets_received,
            snapshot.reassembly.fec_frames_recovered,
            snapshot.reassembly.fec_packets_recovered,
            snapshot.reassembly.fec_recovery_failed_multi_missing,
            snapshot.reassembly.fec_recovery_failed_no_parity,
            snapshot.reassembly.fec_recovery_failed_invalid,
            snapshot.reassembly.frames_missing_after_fec,
            snapshot.reassembly.frames_dropped_after_fec,
            snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
            optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
            stats.decoder_resets,
            snapshot.drop_damaged_gop,
            snapshot.damaged_gop.damaged_gop_count,
            snapshot.damaged_gop.frames_discarded_damaged_gop,
            snapshot.damaged_gop.frames_discarded_waiting_keyframe,
            snapshot.damaged_gop.waiting_keyframe_entries,
            snapshot.damaged_gop.waiting_keyframe_exits,
            snapshot.damaged_gop.idr_frames_received,
            snapshot.damaged_gop.idr_frames_used_for_recovery,
            snapshot.damaged_gop.non_idr_frames_discarded_waiting,
            snapshot.damaged_gop.recovery_wait_ms_avg(),
            snapshot.damaged_gop.recovery_wait_ms_max,
            snapshot.damaged_gop.recovery_wait_frames_avg(),
            snapshot.damaged_gop.recovery_wait_frames_max,
            optional_u64_json(snapshot.reassembly.next_decode_frame_id),
            snapshot.reassembly.decode_gate_stalls,
            snapshot.reassembly.decode_gate_gap_events,
            snapshot.reassembly.decode_gate_gap_to_damage_ms_avg(),
            snapshot.reassembly.decode_gate_gap_to_damage_ms_max,
            snapshot.completed_waiting,
            snapshot.reassembly.frames_discarded_decode_gate,
            snapshot.reassembly.reorder_wait_ms,
            video_packet_dispatch_ms_avg,
            video_packet_dispatch_ms_max,
            audio_receiver_fragment(stats),
            receiver_repair_fragment(snapshot),
            stats.render_state.json_fragment(),
        );
        let _ = std::fmt::Write::write_fmt(
            &mut output,
            format_args!(
                r#","repair_unique_packets_received":{},"repair_duplicate_packets_received":{},"repair_packets_received_after_frame_complete":{},"nack_candidates_first_round":{},"nack_candidates_late_discovery":{},"nack_requests_cancelled_by_progress":{},"missing_first_detected_age_ms":{:.3},"missing_first_detected_age_ms_min":{:.3},"missing_first_detected_age_ms_max":{:.3},"missing_first_detected_to_deadline_ms_avg":{:.3},"missing_first_detected_to_deadline_ms_min":{:.3},"missing_first_detected_to_deadline_ms_max":{:.3},"nack_first_sent_age_ms":{:.3},"nack_first_sent_age_ms_min":{:.3},"nack_first_sent_age_ms_max":{:.3},"first_nack_to_deadline_ms_avg":{:.3},"first_nack_to_deadline_ms_min":{:.3},"first_nack_to_deadline_ms_max":{:.3},"repair_arrival_to_deadline_ms_avg":{:.3},"repair_arrival_to_deadline_ms_min":{:.3},"repair_arrival_to_deadline_ms_max":{:.3},"nack_first_round_budget_ms_avg":{:.3},"nack_first_round_budget_ms_min":{:.3},"nack_second_round_budget_ms_avg":{:.3},"nack_second_round_budget_ms_min":{:.3}"#,
                snapshot.repair_unique_packets_received,
                snapshot.repair_duplicate_packets_received,
                snapshot.repair_packets_received_after_frame_complete,
                snapshot.nack_candidates_first_round,
                snapshot.nack_candidates_late_discovery,
                snapshot.reassembly.nack_requests_cancelled_by_progress,
                snapshot.missing_first_detected_age.avg_ms(),
                snapshot.missing_first_detected_age.min_ms,
                snapshot.missing_first_detected_age.max_ms,
                snapshot.missing_first_detected_to_deadline.avg_ms(),
                snapshot.missing_first_detected_to_deadline.min_ms,
                snapshot.missing_first_detected_to_deadline.max_ms,
                snapshot.first_nack_age.avg_ms(),
                snapshot.first_nack_age.min_ms,
                snapshot.first_nack_age.max_ms,
                snapshot.first_nack_to_deadline.avg_ms(),
                snapshot.first_nack_to_deadline.min_ms,
                snapshot.first_nack_to_deadline.max_ms,
                snapshot.repair_arrival_to_deadline.avg_ms(),
                snapshot.repair_arrival_to_deadline.min_ms,
                snapshot.repair_arrival_to_deadline.max_ms,
                snapshot.first_round_budget.avg_ms(),
                snapshot.first_round_budget.min_ms,
                snapshot.second_round_budget.avg_ms(),
                snapshot.second_round_budget.min_ms,
            ),
        );
        let _ = std::fmt::Write::write_fmt(
            &mut output,
            format_args!(
                r#","receiver_capability_feedback_sent":{},"receiver_capability_feedback_send_errors":{},"receiver_display_generation":{},"receiver_refresh_hz":{:.3},"receiver_present_fps_measured":{:.3},"profile_changes_received":{},"profile_changes_invalid":{},"profile_change_sequence":{},"profile_generation":{},"profile_target_width":{},"profile_target_height":{},"profile_target_fps":{},"profile_target_bitrate_mbps":{:.3},"profile_decoder_resets":{},"stale_profile_packets_dropped":{},"feedback_sample_eligible":{},"receiver_valid_feedback_windows":{},"receiver_render_ready":{},"receiver_profile_settled":{},"receiver_profile_acknowledged":{},"receiver_first_idr_decoded":{},"receiver_first_frame_rendered":{},"profile_transition_active":{},"profile_transition_phase":"{}","profile_transition_started_us":{},"profile_transition_deadline_remaining_ms":{},"transition_timeout_count":{},"transition_failure_reason":{},"transition_failure_stage":{},"transition_recovery_count":{},"transition_settle_restart_count":{},"transition_settle_deadline_remaining_ms":{},"transition_overall_deadline_remaining_ms":{},"transition_settle_windows":{},"transition_settle_duration_ms":{},"pending_profile_present":{},"pending_profile_age_ms":{},"pending_profile_deadline_remaining_ms":{},"pending_profile_change_sequence":{},"pending_profile_generation":{},"pending_profile_old_session_id":{},"pending_profile_new_session_id":{},"mprf_packets_received":{},"mprf_ack_packets_sent":{},"mprf_duplicate_reacked":{},"mprf_pending_expired":{},"mprf_rejected_foreign_peer":{},"mprf_rejected_old_session":{},"mprf_rejected_sequence":{},"mprf_rejected_generation":{},"mprf_rejected_invalid_fields":{},"new_session_activation_count":{},"new_session_first_packet_wait_ms_avg":{:.3},"new_session_first_packet_wait_ms_max":{:.3},"new_session_first_idr_wait_ms":{},{}"#,
                snapshot.capability_feedback_sent,
                snapshot.capability_feedback_send_errors,
                stats.render_state.window.display.display_generation,
                stats.render_state.window.display.refresh.hz(),
                stats.render_state.d3d11.present_fps_measured,
                snapshot.profile_changes_received,
                snapshot.profile_changes_invalid,
                snapshot.profile_change_sequence,
                snapshot.profile_generation,
                snapshot.profile_target_width,
                snapshot.profile_target_height,
                snapshot.profile_target_fps,
                snapshot.profile_target_bitrate_mbps,
                snapshot.profile_decoder_resets,
                snapshot.stale_profile_packets_dropped,
                snapshot.feedback_sample_eligible,
                snapshot.receiver_valid_feedback_windows,
                snapshot.receiver_render_ready,
                snapshot.receiver_profile_settled,
                snapshot.receiver_profile_acknowledged,
                snapshot.receiver_first_idr_decoded,
                snapshot.receiver_first_frame_rendered,
                snapshot.profile_transition_active,
                snapshot.profile_transition_phase.name(),
                snapshot.profile_transition_started_us,
                snapshot.transition_overall_deadline_remaining_ms,
                snapshot.transition_timeout_count,
                optional_json_string(snapshot.transition_failure_reason),
                optional_json_string(snapshot.transition_failure_stage),
                snapshot.transition_recovery_count,
                snapshot.transition_settle_restart_count,
                snapshot.transition_settle_deadline_remaining_ms,
                snapshot.transition_overall_deadline_remaining_ms,
                snapshot.transition_settle_windows,
                snapshot.transition_settle_duration_ms,
                snapshot.pending_profile_present,
                snapshot.pending_profile_age_ms,
                snapshot.pending_profile_deadline_remaining_ms,
                snapshot.pending_profile_change_sequence,
                snapshot.pending_profile_generation,
                snapshot.pending_profile_old_session_id,
                snapshot.pending_profile_new_session_id,
                snapshot.mprf_packets_received,
                snapshot.mprf_ack_packets_sent,
                snapshot.mprf_duplicate_reacked,
                snapshot.mprf_pending_expired,
                snapshot.mprf_rejected_foreign_peer,
                snapshot.mprf_rejected_old_session,
                snapshot.mprf_rejected_sequence,
                snapshot.mprf_rejected_generation,
                snapshot.mprf_rejected_invalid_fields,
                snapshot.new_session_activation_count,
                average(
                    snapshot.new_session_first_packet_wait_ms_total,
                    snapshot.new_session_activation_count,
                ),
                snapshot.new_session_first_packet_wait_ms_max,
                optional_f64_json(snapshot.new_session_first_idr_wait_ms),
                stats.render_state.window.display.json_fragment("receiver"),
            ),
        );
        output
    }

    fn audio_receiver_fragment(stats: &ViewerStats) -> String {
        let audio = &stats.audio_stats;
        let av_avg = if stats.av_sync_offset_samples == 0 {
            0.0
        } else {
            stats.av_sync_offset_ms_total / stats.av_sync_offset_samples as f64
        };
        format!(
            r#""audio_enabled":{},"audio_thread_started":{},"audio_playback_started":{},"audio_playhead_valid":{},"audio_master_valid":{},"audio_master_invalid_reason":{},"audio_prebuffering":{},"av_sync_enabled":{},"av_sync_mode":"{}","av_sync_state":"{}","av_sync_bypass_reason":{},"av_sync_state_transitions":{},"av_sync_forced_release_count":{},"av_sync_hold_epoch_ms":{},"video_sync_gating_enabled":{},"audio_packets_received":{},"audio_packets_invalid":{},"audio_packets_dropped_session_mismatch":{},"audio_session_matched":{},"audio_session_id":{},"expected_video_session_id":{},"audio_packets_lost_estimate":{},"audio_jitter_buffer_ms":{:.3},"audio_jitter_buffer_ms_current":{:.3},"audio_jitter_buffer_ms_avg":{:.3},"audio_jitter_buffer_ms_max":{:.3},"audio_jitter_buffer_target_ms":{},"audio_queue_depth_ms":{:.3},"audio_playhead_timestamp_us":{},"audio_submitted_timestamp_us":{},"latest_audio_packet_timestamp_us":{},"audio_samples_rendered_total":{},"audio_device_frames_submitted_total":{},"audio_media_samples_rendered_total":{},"audio_media_samples_submitted_total":{},"audio_media_samples_audible_estimated_total":{},"audio_samples_queued_current":{},"audio_samples_dropped_for_latency":{},"audio_latency_drop_discontinuities":{},"audio_master_reanchors":{},"audio_device_padding_frames":{},"audio_device_padding_ms":{:.3},"audio_device_padding_valid":{},"audio_callback_empty_polls":{},"audio_real_underruns":{},"audio_underruns":{},"audio_silence_filled_frames":{},"audio_device_silence_filled_frames":{},"audio_prestart_silence_frames":{},"audio_poststream_silence_frames":{},"audio_playhead_discontinuities":{},"audio_late_packets":{},"audio_packet_parse_ms_avg":{:.6},"audio_packet_parse_ms_max":{:.6},"audio_queue_drops":{},"audio_unavailable_reason":{},"audio_sample_rate":{},"audio_channels":{},"av_sync_offset_ms_avg":{:.3},"av_sync_offset_ms_max":{:.3},"video_frames_dropped_for_av_sync":{},"video_frames_held_for_av_sync":{},"video_frames_actually_dropped":{},"video_frames_actually_held":{}"#,
            audio.enabled && audio.unavailable_reason.is_none(),
            audio.thread_started,
            audio.playback_started,
            audio.audio_playhead_valid,
            audio.audio_playhead_valid,
            optional_json_string(stats.av_sync_bypass_reason.map(AvSyncBypassReason::name),),
            audio.thread_started && !audio.playback_started,
            stats.av_sync_mode == AvSyncMode::Conservative
                && audio.thread_started
                && audio.unavailable_reason.is_none(),
            stats.av_sync_mode.name(),
            stats.av_sync_state.name(),
            optional_json_string(stats.av_sync_bypass_reason.map(AvSyncBypassReason::name),),
            stats.av_sync_state_transitions,
            stats.av_sync_forced_release_count,
            stats.av_sync_hold_epoch_ms,
            stats.video_sync_gating_enabled,
            audio.packets_received,
            audio.audio_packets_invalid,
            audio.audio_packets_dropped_session_mismatch,
            audio.audio_session_matched,
            crate::media_clock::optional_u64_json(audio.audio_session_id),
            crate::media_clock::optional_u64_json(audio.expected_video_session_id),
            audio.packets_lost_estimate,
            audio.jitter_buffer_ms,
            audio.jitter_buffer_ms_current,
            audio.jitter_buffer_ms_avg,
            audio.jitter_buffer_ms_max,
            audio.jitter_buffer_target_ms,
            audio.audio_queue_depth_ms,
            crate::media_clock::optional_u64_json(audio.audio_playhead_timestamp_us),
            crate::media_clock::optional_u64_json(audio.audio_submitted_timestamp_us),
            crate::media_clock::optional_u64_json(audio.latest_audio_packet_timestamp_us),
            audio.audio_samples_rendered_total,
            audio.audio_samples_rendered_total,
            audio.audio_media_samples_rendered_total,
            audio.audio_media_samples_submitted_total,
            audio.audio_media_samples_audible_estimated_total,
            audio.audio_samples_queued_current,
            audio.audio_samples_dropped_for_latency,
            audio.audio_latency_drop_discontinuities,
            audio.audio_master_reanchors,
            audio.audio_device_padding_frames,
            audio.audio_device_padding_ms,
            audio.audio_device_padding_valid,
            audio.audio_callback_empty_polls,
            audio.audio_real_underruns,
            audio.audio_real_underruns,
            audio.audio_silence_filled_frames,
            audio.audio_device_silence_filled_frames,
            audio.audio_prestart_silence_frames,
            audio.audio_poststream_silence_frames,
            audio.audio_playhead_discontinuities,
            audio.late_packets,
            audio.audio_packet_parse_ms_avg,
            audio.audio_packet_parse_ms_max,
            audio.audio_queue_drops,
            optional_json_string(audio.unavailable_reason.as_deref()),
            crate::audio_udp::AUDIO_SAMPLE_RATE,
            crate::audio_udp::AUDIO_CHANNELS,
            av_avg,
            stats.av_sync_offset_ms_max,
            stats.video_frames_dropped_for_av_sync,
            stats.video_frames_held_for_av_sync,
            stats.video_frames_dropped_for_av_sync,
            stats.video_frames_held_for_av_sync,
        )
    }

    fn receiver_repair_fragment(snapshot: NetworkSnapshot) -> String {
        let repair_overhead_packets = snapshot.repair_packets_received;
        let repair_overhead_base = snapshot
            .reassembly
            .packets_received
            .saturating_sub(snapshot.repair_packets_received);
        let repair_overhead_ratio = if repair_overhead_base == 0 {
            0.0
        } else {
            repair_overhead_packets as f64 / repair_overhead_base as f64
        };
        let nack_items_per_requested_frame_avg = if snapshot.nack_frames_requested == 0 {
            0.0
        } else {
            snapshot.nack_items_per_requested_frame_total as f64
                / snapshot.nack_frames_requested as f64
        };
        let repair_deadline_ms_avg = if snapshot.repair_deadline_samples == 0 {
            0.0
        } else {
            snapshot.repair_deadline_ms_total / snapshot.repair_deadline_samples as f64
        };
        format!(
            r#""repair_mode":"{}","video_data_packets_received":{},"video_fec_packets_received":{},"audio_packets_received":{},"unknown_packets_received":{},"nack_control_packets_received":{},"packet_type_dispatch_counts":{{"video_data":{},"video_fec":{},"audio":{},"unknown":{},"nack_control":{}}},"nack_packets_sent":{},"nack_items_sent":{},"nack_frames_requested":{},"nack_rounds_sent":{},"nack_candidate_frames":{},"nack_suppressed_progressing_frames":{},"nack_suppressed_too_early":{},"nack_suppressed_already_requested":{},"nack_suppressed_item_limit":{},"nack_items_deduped":{},"nack_items_per_requested_frame_avg":{:.3},"nack_items_per_requested_frame_max":{},"repair_packets_received":{},"repair_packets_inserted":{},"repair_packets_matched_inflight":{},"repair_duplicate_packets":{},"repair_packets_dropped_wrong_type":{},"repair_packets_dropped_late":{},"repair_packets_dropped_no_frame":{},"repair_late_packets":{},"repair_frames_completed":{},"repair_deadline_missed":{},"repair_deadline_ms_avg":{:.3},"repair_deadline_ms_max":{:.3},"frames_missing_after_repair":{},"frames_dropped_after_repair":{},"repair_wait_ms_avg":{:.3},"repair_wait_ms_max":{:.3},"repair_send_errors":{},"repair_cancelled_frame_complete":{},"repair_overhead_packets":{},"repair_overhead_ratio_vs_data":{:.6},"stream_close_received":{},"stream_close_ack_sent":{},"stream_close_rejected_pre_session":{},"stream_close_rejected_peer":{},"stream_close_rejected_session":{},"stream_close_rejected_invalid":{},"stream_close_duplicate":{},"stream_close_sent":{},"stream_close_retry_count":{},"stream_close_ack_received":{},"stream_close_handshake_timeout":{},"peer_timeout_triggered":{},"peer_last_valid_age_ms":{}"#,
            snapshot.repair_mode.name(),
            snapshot.video_data_packets_received,
            snapshot.video_fec_packets_received,
            snapshot.audio_packets_received,
            snapshot.unknown_packets_received,
            snapshot.nack_control_packets_received,
            snapshot.video_data_packets_received,
            snapshot.video_fec_packets_received,
            snapshot.audio_packets_received,
            snapshot.unknown_packets_received,
            snapshot.nack_control_packets_received,
            snapshot.nack_packets_sent,
            snapshot.nack_items_sent,
            snapshot.nack_frames_requested,
            snapshot.nack_rounds_sent,
            snapshot.nack_candidate_frames,
            snapshot.nack_suppressed_progressing_frames,
            snapshot.nack_suppressed_too_early,
            snapshot.nack_suppressed_already_requested,
            snapshot.nack_suppressed_item_limit,
            snapshot.nack_items_deduped,
            nack_items_per_requested_frame_avg,
            snapshot.nack_items_per_requested_frame_max,
            snapshot.repair_packets_received,
            snapshot.repair_packets_inserted,
            snapshot.repair_packets_inserted,
            snapshot.repair_duplicate_packets,
            snapshot.repair_packets_dropped_wrong_type,
            snapshot.repair_packets_dropped_late,
            snapshot.repair_packets_dropped_no_frame,
            snapshot.repair_packets_dropped_late,
            snapshot.repair_frames_completed,
            snapshot.repair_deadline_missed,
            repair_deadline_ms_avg,
            snapshot.repair_deadline_ms_max,
            snapshot.reassembly.frames_missing_after_fec,
            snapshot.reassembly.frames_dropped_after_fec,
            if snapshot.repair_frames_completed == 0 {
                0.0
            } else {
                snapshot.repair_wait_ms_total / snapshot.repair_frames_completed as f64
            },
            snapshot.repair_wait_ms_max,
            snapshot.repair_send_errors,
            snapshot.repair_cancelled_frame_complete,
            repair_overhead_packets,
            repair_overhead_ratio,
            snapshot.stream_close_received,
            snapshot.stream_close_ack_sent,
            snapshot.stream_close_rejected_pre_session,
            snapshot.stream_close_rejected_peer,
            snapshot.stream_close_rejected_session,
            snapshot.stream_close_rejected_invalid,
            snapshot.stream_close_duplicate,
            snapshot.stream_close_sent,
            snapshot.stream_close_retry_count,
            snapshot.stream_close_ack_received,
            snapshot.stream_close_handshake_timeout,
            snapshot.peer_timeout_triggered,
            snapshot.peer_last_valid_age_ms,
        )
    }

    fn optional_u64_json(value: Option<u64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| value.to_string())
    }

    fn optional_f64_json(value: Option<f64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| format!("{value:.3}"))
    }

    fn optional_json_string(value: Option<&str>) -> String {
        value.map_or_else(
            || "null".to_string(),
            |value| format!(r#""{}""#, json_escape(value)),
        )
    }

    fn duration_elapsed(started_at: Instant, duration_sec: Option<u64>) -> bool {
        duration_sec
            .map(|seconds| started_at.elapsed() >= Duration::from_secs(seconds))
            .unwrap_or(false)
    }

    fn optional_duration_text(duration_sec: Option<u64>) -> String {
        duration_sec.map_or_else(|| "unlimited".to_string(), |seconds| seconds.to_string())
    }

    fn json_string_array(values: &[String]) -> String {
        format!(
            "[{}]",
            values
                .iter()
                .map(|value| format!(r#""{}""#, json_escape(value)))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }

    fn capability_feedback_readiness_self_test() -> Result<(), String> {
        let base = Instant::now();
        let mut source = CapabilityFeedbackSource::default();
        source.update_readiness(None, 0, false, 0, false, base);
        if source.feedback_sample_eligible || source.valid_feedback_windows != 0 {
            return Err("feedback became eligible before a session existed".to_string());
        }

        source.active_render_fps = 60.0;
        source.decoder_input_fps = 60.0;
        for (window, frames_rendered) in [1_u64, 2, 3].into_iter().enumerate() {
            source.update_readiness(
                Some(7),
                0,
                true,
                frames_rendered,
                true,
                base + Duration::from_millis(100 + window as u64 * 100),
            );
            if window < 2 && source.feedback_sample_eligible {
                return Err("feedback became eligible before three valid windows".to_string());
            }
        }
        if !source.feedback_sample_eligible
            || !source.profile_settled
            || source.profile_transition_active
            || source.new_session_first_idr_wait_ms.is_some()
        {
            return Err("initial receiver readiness or null IDR telemetry was invalid".to_string());
        }

        source.active_render_fps = 0.0;
        source.decoder_input_fps = 0.0;
        source.update_readiness(Some(7), 0, true, 3, true, base + Duration::from_millis(500));
        if source.feedback_sample_eligible || source.valid_feedback_windows != 0 {
            return Err("zero-FPS window retained stale feedback eligibility".to_string());
        }

        let transition_start = base + Duration::from_secs(1);
        source.begin_profile_transition(8, 1, transition_start, 2_000_000);
        if source.feedback_sample_eligible
            || !source.profile_transition_active
            || source.valid_feedback_windows != 0
        {
            return Err("profile transition did not close the feedback gate".to_string());
        }
        source.active_render_fps = 60.0;
        source.decoder_input_fps = 60.0;
        for (window, frames_rendered) in [4_u64, 5, 6, 7].into_iter().enumerate() {
            source.update_readiness(
                Some(8),
                1,
                true,
                frames_rendered,
                true,
                transition_start + Duration::from_millis(100 + window as u64 * 100),
            );
            if window < 3 && source.feedback_sample_eligible {
                return Err("transition feedback became eligible before settling".to_string());
            }
        }
        if !source.feedback_sample_eligible
            || source.profile_transition_active
            || source.transition_settle_windows != 3
            || source.transition_settle_duration_ms != 400
            || source.new_session_first_idr_wait_ms != Some(100.0)
        {
            return Err(
                "profile transition readiness did not settle deterministically".to_string(),
            );
        }

        let timeout_start = base + Duration::from_secs(2);
        source.begin_profile_transition(9, 2, timeout_start, 3_000_000);
        source.update_readiness(
            Some(9),
            2,
            true,
            source.frames_rendered_total,
            false,
            timeout_start + crate::profile_transition::RECEIVER_FIRST_IDR_DEADLINE,
        );
        if source.profile_transition_active
            || source.profile_transition_phase
                != crate::profile_transition::ReceiverTransitionPhase::Failed
            || source.transition_failure_reason != Some("receiver-first-idr-timeout")
            || source.transition_timeout_count != 1
        {
            return Err("missing new-session IDR did not fail at a finite deadline".to_string());
        }

        source.begin_profile_transition(10, 3, base + Duration::from_secs(3), 4_000_000);
        source.cancel_profile_transition();
        let mut cancelled_snapshot = NetworkSnapshot::default();
        source.write_transition_snapshot(&mut cancelled_snapshot);
        if source.profile_transition_active
            || cancelled_snapshot.profile_transition_active
            || source.profile_transition_phase
                != crate::profile_transition::ReceiverTransitionPhase::Failed
            || source.transition_failure_reason != Some("receiver-transition-cancelled")
            || source.transition_failure_stage != Some("await-first-idr")
        {
            return Err("receiver cancellation left a transition active".to_string());
        }
        Ok(())
    }

    pub fn run_self_test() -> Result<(), String> {
        capability_feedback_readiness_self_test()?;

        fn frame(frame_id: u64, keyframe: bool) -> EncodedFrame {
            EncodedFrame {
                frame_id,
                flags: if keyframe { crate::FLAG_KEYFRAME } else { 0 },
                timestamp_ms: frame_id * 33,
                bytes: if keyframe {
                    vec![
                        0,
                        0,
                        0,
                        1,
                        7,
                        0x64,
                        0,
                        0,
                        0,
                        1,
                        8,
                        0xee,
                        0,
                        0,
                        0,
                        1,
                        5,
                        frame_id as u8,
                    ]
                } else {
                    vec![0, 0, 0, 1, 1, frame_id as u8]
                },
            }
        }

        let mut queue = DecodeQueue::new(2, true);
        queue.enqueue_frame(frame(0, true), 2);
        queue.enqueue_frame(frame(1, false), 2);
        queue.enqueue_frame(frame(2, false), 2);
        if !queue.waiting_for_keyframe
            || queue.keyframe_recovery_count != 1
            || !matches!(queue.items.front(), Some(DecodeQueueItem::Reset))
        {
            return Err("decode queue overflow did not enter keyframe recovery".to_string());
        }
        queue.enqueue_frame(frame(3, false), 2);
        queue.enqueue_frame(frame(4, true), 2);
        if queue.waiting_for_keyframe
            || queue.frames_predecode_dropped != 4
            || queue.items.len() != 2
            || !matches!(queue.items.front(), Some(DecodeQueueItem::Reset))
            || !matches!(
                queue.items.back(),
                Some(DecodeQueueItem::Frame(frame)) if frame.frame_id == 4
            )
        {
            return Err("decode queue keyframe recovery ordering failed".to_string());
        }

        let now = Instant::now();
        let mut damaged_queue = DecodeQueue::new(4, true);
        damaged_queue.enqueue_frame(frame(10, true), 4);
        damaged_queue.begin_damaged_gop_recovery(now, Some(10));
        damaged_queue.enqueue_frame(frame(11, false), 4);
        damaged_queue.enqueue_frame(frame(12, true), 4);
        let damaged_stats = damaged_queue.damaged_gop_stats();
        if damaged_queue.waiting_for_keyframe
            || damaged_stats.damaged_gop_count != 1
            || damaged_stats.frames_discarded_damaged_gop != 2
            || damaged_stats.recovery_completed != 1
            || !matches!(damaged_queue.items.front(), Some(DecodeQueueItem::Reset))
            || !matches!(
                damaged_queue.items.back(),
                Some(DecodeQueueItem::Frame(frame)) if frame.frame_id == 12
            )
        {
            return Err("damaged GOP keyframe recovery failed".to_string());
        }

        let mut profile_queue = DecodeQueue::new(4, true);
        profile_queue.enqueue_frame(frame(20, true), 4);
        let damaged_before = profile_queue.damaged_gop_stats();
        profile_queue.begin_profile_transition();
        if !profile_queue.waiting_for_keyframe
            || profile_queue.items.len() != 1
            || !matches!(profile_queue.items.front(), Some(DecodeQueueItem::Reset))
            || profile_queue.keyframe_recovery_count != 0
            || profile_queue.damaged_gop_stats().damaged_gop_count
                != damaged_before.damaged_gop_count
        {
            return Err(
                "profile transition did not produce exactly one clean decoder reset".to_string(),
            );
        }
        profile_queue.enqueue_frame(frame(21, false), 4);
        if profile_queue.items.len() != 1 || !profile_queue.waiting_for_keyframe {
            return Err("profile transition accepted a non-IDR frame".to_string());
        }
        profile_queue.enqueue_frame(frame(0, true), 4);
        if profile_queue.waiting_for_keyframe
            || profile_queue.items.len() != 2
            || !matches!(profile_queue.items.front(), Some(DecodeQueueItem::Reset))
            || !matches!(
                profile_queue.items.back(),
                Some(DecodeQueueItem::Frame(frame)) if frame.frame_id == 0 && frame.is_idr()
            )
        {
            return Err("profile transition did not resume from the new session IDR".to_string());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use std::io;
        use std::net::UdpSocket;
        use std::thread;
        use std::time::Duration;

        fn test_close(close_id: u64) -> crate::media_control::StreamClose {
            crate::media_control::StreamClose {
                version: crate::media_control::MEDIA_CONTROL_VERSION,
                stream_id: crate::STREAM_VIDEO,
                reason_code: crate::shutdown::StopReason::LocalStop as u8,
                video_session_id: 17,
                close_id,
                timestamp_us: 42,
                last_frame_id: 99,
            }
        }

        fn close_test_config() -> crate::shutdown::ShutdownConfig {
            crate::shutdown::ShutdownConfig {
                close_retry_initial: Duration::from_millis(15),
                close_retry_max: Duration::from_millis(30),
                close_handshake_timeout: Duration::from_millis(250),
                ..crate::shutdown::ShutdownConfig::default()
            }
        }

        fn test_peer() -> std::net::SocketAddr {
            "127.0.0.1:55134".parse().unwrap()
        }

        #[test]
        fn pre_session_close_is_ignored() {
            let close = test_close(1);
            let decision = super::classify_incoming_stream_close(
                &close.encode().unwrap(),
                test_peer(),
                None,
                None,
                None,
            );
            assert_eq!(
                decision,
                super::IncomingStreamCloseDecision::RejectPreSession
            );
        }

        #[test]
        fn zero_session_close_is_ignored() {
            let close = crate::media_control::StreamClose {
                video_session_id: 0,
                ..test_close(2)
            };
            let decision = super::classify_incoming_stream_close(
                &close.encode().unwrap(),
                test_peer(),
                Some(test_peer()),
                Some(17),
                None,
            );
            assert_eq!(decision, super::IncomingStreamCloseDecision::RejectSession);
        }

        #[test]
        fn foreign_peer_close_is_ignored() {
            let close = test_close(3);
            let decision = super::classify_incoming_stream_close(
                &close.encode().unwrap(),
                "127.0.0.1:55135".parse().unwrap(),
                Some(test_peer()),
                Some(17),
                None,
            );
            assert_eq!(decision, super::IncomingStreamCloseDecision::RejectPeer);
        }

        #[test]
        fn stale_session_close_is_ignored() {
            let close = crate::media_control::StreamClose {
                video_session_id: 16,
                ..test_close(4)
            };
            let decision = super::classify_incoming_stream_close(
                &close.encode().unwrap(),
                test_peer(),
                Some(test_peer()),
                Some(17),
                None,
            );
            assert_eq!(decision, super::IncomingStreamCloseDecision::RejectSession);
        }

        #[test]
        fn valid_close_is_acked_and_stops() {
            let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
            let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
            receiver
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let close = test_close(5);
            let source = receiver.local_addr().unwrap();
            assert_eq!(
                super::classify_incoming_stream_close(
                    &close.encode().unwrap(),
                    source,
                    Some(source),
                    Some(17),
                    None,
                ),
                super::IncomingStreamCloseDecision::Accept(close)
            );
            assert!(super::send_stream_close_ack(&sender, source, close));
            let mut bytes = [0u8; 64];
            let (length, _) = receiver.recv_from(&mut bytes).unwrap();
            assert!(
                crate::media_control::StreamCloseAck::decode(&bytes[..length])
                    .unwrap()
                    .matches(close)
            );
            let cancellation = crate::shutdown::CancellationToken::new();
            assert!(cancellation.cancel(crate::shutdown::StopReason::PeerClosed));
            assert_eq!(
                cancellation.reason(),
                Some(crate::shutdown::StopReason::PeerClosed)
            );
        }

        #[test]
        fn duplicate_close_is_idempotent() {
            let close = test_close(6);
            let peer = test_peer();
            let cancellation = crate::shutdown::CancellationToken::new();
            assert!(cancellation.cancel(crate::shutdown::StopReason::PeerClosed));
            assert_eq!(
                super::classify_incoming_stream_close(
                    &close.encode().unwrap(),
                    peer,
                    Some(peer),
                    Some(17),
                    Some((peer, close)),
                ),
                super::IncomingStreamCloseDecision::Duplicate(close)
            );
            assert!(!cancellation.cancel(crate::shutdown::StopReason::PeerClosed));
        }

        #[test]
        fn short_datagram_uses_received_length() {
            let mut buffer = [0u8; 64];
            buffer[..4].copy_from_slice(b"NACK");
            for length in 0..4 {
                assert_eq!(
                    super::classify_short_or_nack(&buffer[..length]),
                    (true, false)
                );
            }
            buffer[..4].copy_from_slice(b"NOPE");
            assert_eq!(super::classify_short_or_nack(&buffer[..4]), (false, false));
        }

        #[test]
        fn reused_buffer_does_not_misclassify_short_packet() {
            let mut buffer = [0u8; 64];
            buffer[..4].copy_from_slice(b"NACK");
            assert_eq!(super::classify_short_or_nack(&buffer[..4]), (true, true));
            buffer[0] = b'X';
            assert_eq!(super::classify_short_or_nack(&buffer[..1]), (true, false));
            buffer[..4].copy_from_slice(b"MCLS");
            buffer[..2].copy_from_slice(b"OK");
            assert_eq!(super::classify_short_or_nack(&buffer[..2]), (true, false));
        }

        #[test]
        fn receiver_feedback_requires_readiness_and_transition_settle() {
            super::capability_feedback_readiness_self_test().unwrap();
        }

        #[test]
        fn runtime_failure_does_not_pollute_worker_join_status() {
            let summary = super::ReceiverWorkerShutdownSummary {
                render: crate::shutdown::WorkerJoinStatus::Joined,
                network: crate::shutdown::WorkerJoinStatus::Joined,
                audio: "not_started",
                join_error: None,
                runtime_error: Some("injected render runtime failure".to_string()),
                cleanup_ms: 1.0,
                cleanup_deadline_ms: 3_000,
                lifecycle_state: "stopped",
                retained_workers: Vec::new(),
            };
            assert!(summary.join_clean());
            assert!(!summary.runtime_completed_clean());
            assert!(!summary.clean());
            assert_eq!(
                crate::shutdown::terminal_event_type(summary.clean()),
                "NATIVE_SCREEN_SHUTDOWN_FAILED"
            );
            assert!(summary
                .json_fragment()
                .contains(r#""worker_join_all_clean":true"#));
            assert!(summary
                .json_fragment()
                .contains(r#""runtime_completed_clean":false"#));
            assert!(summary
                .json_fragment()
                .contains(r#""retained_worker_count":0"#));
        }

        #[test]
        fn udp_connection_reset_defers_to_liveness_for_a_known_peer() {
            assert!(super::is_expected_udp_peer_reset(
                io::ErrorKind::ConnectionReset,
                true,
            ));
            assert!(!super::is_expected_udp_peer_reset(
                io::ErrorKind::ConnectionReset,
                false,
            ));
            assert!(!super::is_expected_udp_peer_reset(
                io::ErrorKind::Other,
                true,
            ));
        }

        #[test]
        fn receiver_close_retries_after_socket_timeout_and_accepts_matching_ack() {
            let local = UdpSocket::bind("127.0.0.1:0").unwrap();
            let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
            local
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            let remote_address = remote.local_addr().unwrap();
            let close = test_close(101);
            let peer = thread::spawn(move || {
                let mut bytes = [0u8; 256];
                let (length, source) = remote.recv_from(&mut bytes).unwrap();
                let observed = crate::media_control::StreamClose::decode(&bytes[..length]).unwrap();
                thread::sleep(Duration::from_millis(35));
                let ack = observed.ack().encode().unwrap();
                remote.send_to(&ack, source).unwrap();
            });
            let result = super::perform_receiver_close_handshake_with_config(
                &local,
                remote_address,
                close,
                close_test_config(),
            );
            peer.join().unwrap();
            assert!(result.0 >= 2, "a timeout must cause a retry");
            assert!(result.1 >= 1);
            assert!(result.2);
            assert!(!result.3);
        }

        #[test]
        fn receiver_close_ignores_wrong_close_id_before_matching_ack() {
            let local = UdpSocket::bind("127.0.0.1:0").unwrap();
            let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
            local
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            let remote_address = remote.local_addr().unwrap();
            let close = test_close(202);
            let peer = thread::spawn(move || {
                let mut bytes = [0u8; 256];
                let (length, source) = remote.recv_from(&mut bytes).unwrap();
                let observed = crate::media_control::StreamClose::decode(&bytes[..length]).unwrap();
                let wrong = crate::media_control::StreamCloseAck {
                    close_id: observed.close_id + 1,
                    ..observed.ack()
                }
                .encode()
                .unwrap();
                remote.send_to(&wrong, source).unwrap();
                thread::sleep(Duration::from_millis(10));
                remote
                    .send_to(&observed.ack().encode().unwrap(), source)
                    .unwrap();
            });
            let result = super::perform_receiver_close_handshake_with_config(
                &local,
                remote_address,
                close,
                close_test_config(),
            );
            peer.join().unwrap();
            assert!(result.2);
            assert!(!result.3);
        }

        #[test]
        fn receiver_close_ignores_full_size_media_datagram_before_ack() {
            let local = UdpSocket::bind("127.0.0.1:0").unwrap();
            let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
            local
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            let remote_address = remote.local_addr().unwrap();
            let close = test_close(252);
            let peer = thread::spawn(move || {
                let mut bytes = [0u8; 256];
                let (length, source) = remote.recv_from(&mut bytes).unwrap();
                let observed = crate::media_control::StreamClose::decode(&bytes[..length]).unwrap();
                remote.send_to(&vec![0xA5; 1_452], source).unwrap();
                remote
                    .send_to(&observed.ack().encode().unwrap(), source)
                    .unwrap();
            });
            let result = super::perform_receiver_close_handshake_with_config(
                &local,
                remote_address,
                close,
                close_test_config(),
            );
            peer.join().unwrap();
            assert!(result.2);
            assert!(!result.3);
        }

        #[test]
        fn simultaneous_close_is_acknowledged_before_local_ack_arrives() {
            let local = UdpSocket::bind("127.0.0.1:0").unwrap();
            let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
            local
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            remote
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let remote_address = remote.local_addr().unwrap();
            let close = test_close(303);
            let peer = thread::spawn(move || {
                let mut bytes = [0u8; 256];
                let (length, source) = remote.recv_from(&mut bytes).unwrap();
                let observed = crate::media_control::StreamClose::decode(&bytes[..length]).unwrap();
                let peer_close = test_close(404);
                remote
                    .send_to(&peer_close.encode().unwrap(), source)
                    .unwrap();
                let (ack_length, _) = remote.recv_from(&mut bytes).unwrap();
                let ack =
                    crate::media_control::StreamCloseAck::decode(&bytes[..ack_length]).unwrap();
                assert!(ack.matches(peer_close));
                remote
                    .send_to(&observed.ack().encode().unwrap(), source)
                    .unwrap();
            });
            let result = super::perform_receiver_close_handshake_with_config(
                &local,
                remote_address,
                close,
                close_test_config(),
            );
            peer.join().unwrap();
            assert!(result.2);
            assert!(!result.3);
        }
    }
}

#[cfg(windows)]
pub use platform::{run, run_self_test};

#[cfg(not(windows))]
pub fn run(_config: H264RecvViewConfig) -> Result<(), String> {
    Err("h264-recv-view is only supported on Windows".to_string())
}

#[cfg(not(windows))]
pub fn run_self_test() -> Result<(), String> {
    Ok(())
}
