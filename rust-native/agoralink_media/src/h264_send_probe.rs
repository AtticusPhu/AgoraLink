#[derive(Debug)]
pub struct H264SendConfig {
    pub host: String,
    pub port: u16,
    pub duration_sec: Option<u64>,
    pub target_fps: u32,
    pub bitrate_mbps: f64,
    pub bitrate_selection: crate::bitrate::BitrateSelection,
    pub out_width: u32,
    pub out_height: u32,
    pub color_spec: crate::color_spec::ColorSpec,
    pub encoder: crate::wmf_h264_encoder::EncoderChoice,
    pub convert_backend: crate::capture_encode_probe::ConvertBackend,
    pub packet_pacing: PacketPacing,
    pub fec_mode: crate::fec::FecMode,
    pub udp_payload_size: usize,
    pub keyframe_interval_sec: f64,
    pub repair_mode: crate::repair::RepairMode,
    pub repair_cache_ms: u64,
    pub audio_mode: AudioSendMode,
    pub adaptive: AdaptiveRuntimeConfig,
    pub mode: H264SendMode,
    pub verbose: bool,
}

#[derive(Clone, Debug)]
pub struct AdaptiveRuntimeConfig {
    pub quality: crate::adaptive_quality::AdaptiveConfig,
    pub display_refresh_detect: crate::display_capability::DisplayRefreshDetect,
    pub max_fps: crate::frame_rate_policy::MaxFps,
    pub enable_high_refresh: bool,
    pub feedback_ms: u64,
}

impl Default for AdaptiveRuntimeConfig {
    fn default() -> Self {
        Self {
            quality: crate::adaptive_quality::AdaptiveConfig::default(),
            display_refresh_detect: crate::display_capability::DisplayRefreshDetect::Auto,
            max_fps: crate::frame_rate_policy::MaxFps::Fixed(60),
            enable_high_refresh: false,
            feedback_ms: 1000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketPacing {
    Auto,
    Batch,
    Off,
}

impl PacketPacing {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Batch => "batch",
            Self::Off => "off",
        }
    }

    pub const fn effective_name(self) -> &'static str {
        match self {
            Self::Auto | Self::Batch => "batch",
            Self::Off => "off",
        }
    }

    pub const fn uses_batch(self) -> bool {
        matches!(self, Self::Auto | Self::Batch)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum H264SendMode {
    Probe,
    Screen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioSendMode {
    Off,
    System,
}

impl AudioSendMode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::System => "system",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "off" => Ok(Self::Off),
            "system" => Ok(Self::System),
            other => Err(format!("invalid audio mode: {other}")),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RunRateSummary {
    global_duration_sec: f64,
    global_fps: f64,
    global_mbps: f64,
    current_profile_duration_sec: f64,
    current_profile_frames_sent: u64,
    current_profile_fps: f64,
    current_profile_mbps: f64,
}

fn summarize_run_rates(
    total_frames: u64,
    total_bytes: u64,
    global_duration_sec: f64,
    profile_start_frames: u64,
    profile_start_bytes: u64,
    current_profile_duration_sec: f64,
) -> RunRateSummary {
    let global_duration_sec = global_duration_sec.max(0.001);
    let current_profile_duration_sec = current_profile_duration_sec.max(0.001);
    let current_profile_frames_sent = total_frames.saturating_sub(profile_start_frames);
    let current_profile_bytes_sent = total_bytes.saturating_sub(profile_start_bytes);
    RunRateSummary {
        global_duration_sec,
        global_fps: total_frames as f64 / global_duration_sec,
        global_mbps: total_bytes as f64 * 8.0 / global_duration_sec / 1_000_000.0,
        current_profile_duration_sec,
        current_profile_frames_sent,
        current_profile_fps: current_profile_frames_sent as f64 / current_profile_duration_sec,
        current_profile_mbps: current_profile_bytes_sent as f64 * 8.0
            / current_profile_duration_sec
            / 1_000_000.0,
    }
}

fn adaptive_action_changes_video_structure(
    action: &crate::adaptive_quality::AdaptiveAction,
) -> bool {
    matches!(
        action,
        crate::adaptive_quality::AdaptiveAction::SetResolution { .. }
            | crate::adaptive_quality::AdaptiveAction::SetFps { .. }
    )
}

#[cfg(windows)]
mod platform {
    use std::collections::VecDeque;
    use std::io::{self, Write};
    use std::net::UdpSocket;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use super::{H264SendConfig, H264SendMode, PacketPacing};
    use crate::capture_encode_probe::{
        self, CaptureEncodeObserver, CapturePipelineControl, CapturePipelineDone,
        CapturePipelineStarted, CapturePipelineStats,
    };
    use crate::fec::{packetize_frame, FecMode};
    use crate::h264_annex_b::{
        nal_types, parameter_set_presence, summarize_nals, AnnexBParameterSets,
    };
    use crate::media_clock::MediaClock;
    use crate::wmf_h264_encoder::EncodedSample;
    use crate::{make_session_id, now_millis, FLAG_CONFIG, FLAG_H264_ANNEX_B, FLAG_KEYFRAME};

    const SEND_QUEUE_DEPTH: usize = 4;
    const PACING_TICK: Duration = Duration::from_millis(1);

    struct PacedFrame {
        packets: Vec<Vec<u8>>,
        data_packet_count: usize,
        fec_packet_count: usize,
        fec_bytes: usize,
        frame_interval: Duration,
    }

    enum PacerCommand {
        Frame(PacedFrame),
        Barrier(mpsc::SyncSender<()>),
    }

    #[derive(Clone, Default)]
    struct SendCounters {
        packets_sent: u64,
        frames_sent: u64,
        bytes_sent: u64,
        data_packets_sent: u64,
        fec_packets_sent: u64,
        fec_bytes_sent: u64,
        max_packets_per_frame: u64,
        pacing_sleep_ms_total: f64,
        pacing_overrun_frames: u64,
        pacing_batches: u64,
        pacing_batch_packets: u64,
        pacing_batch_max: u64,
        pacing_overrun_ticks: u64,
        pacing_late_us_total: f64,
        pacing_late_us_max: f64,
        pacing_late_us_p50: f64,
        pacing_late_us_p95: f64,
        pacing_late_us_p99: f64,
        video_send_syscall_ns_total: u64,
        video_send_syscall_ns_max: u64,
        video_send_syscalls: u64,
        video_send_syscall_us_p95: u64,
        video_send_syscall_us_p99: u64,
        video_worker_loop_ns_total: u64,
        video_worker_loop_ns_max: u64,
        video_worker_loops: u64,
        video_worker_loop_us_p50: u64,
        video_worker_loop_us_p95: u64,
        video_worker_loop_us_p99: u64,
        send_errors: u64,
        error: Option<String>,
        nack_packets_received: u64,
        nack_items_received: u64,
        repair_packets_resent: u64,
        repair_unique_packets_resent: u64,
        repair_duplicate_packets_resent: u64,
        repair_cache_hits: u64,
        repair_cache_misses: u64,
        repair_cache_miss_not_found: u64,
        repair_cache_miss_expired: u64,
        repair_cache_miss_evicted: u64,
        repair_cache_miss_wrong_session: u64,
        repair_rate_limited: u64,
        repair_send_errors: u64,
        repair_send_socket_errors: u64,
        repair_send_bytes: u64,
        repair_request_total: u64,
        repair_request_deduped: u64,
        repair_send_suppressed: u64,
        repair_cancelled_deadline: u64,
        capability_feedback_received: u64,
        capability_feedback_invalid: u64,
        capability_feedback_stale: u64,
        mprf_ack_packets_received: u64,
        mprf_ack_invalid: u64,
        stream_close_packets_received: u64,
        stream_close_ack_packets_received: u64,
        stream_close_ack_packets_sent: u64,
        stream_close_invalid: u64,
    }

    struct UdpObserver {
        sender: Option<SyncSender<PacerCommand>>,
        worker: Option<JoinHandle<()>>,
        send_counters: Arc<Mutex<SendCounters>>,
        target: String,
        host: String,
        port: u16,
        session_id: u64,
        mode: H264SendMode,
        duration_sec: Option<u64>,
        next_frame_id: u64,
        h264_bytes: u64,
        keyframes: u64,
        idr_frames: u64,
        sps_pps_repeated: u64,
        last_idr_frame_id: Option<u64>,
        idr_interval_frames_total: u64,
        idr_interval_count: u64,
        config_frames: u64,
        previous_packets: u64,
        previous_frames: u64,
        previous_bytes: u64,
        previous_repair_packets: u64,
        previous_repair_bytes: u64,
        previous_nack_items: u64,
        previous_duplicate_repairs: u64,
        packetize_send_ms_total: f64,
        packetize_latency: crate::sender_scheduling::LatencyHistogram,
        packetize_send_ms_p50: f64,
        packetize_send_ms_p95: f64,
        packetize_send_ms_p99: f64,
        packet_pacing: PacketPacing,
        fec_mode: FecMode,
        udp_payload_size: usize,
        data_payload_size: usize,
        keyframe_interval_sec: f64,
        current_target_fps: u32,
        keyframe_control_configured: bool,
        keyframe_interval_target_frames: u32,
        keyframe_force_supported: bool,
        keyframe_force_requests: u64,
        keyframe_force_failures: u64,
        keyframe_config_method: String,
        keyframe_config_applied: bool,
        keyframe_config_error: Option<String>,
        keyframe_force_last_requested_frame_id: Option<u64>,
        keyframe_force_last_effective_frame_id: Option<u64>,
        keyframe_force_latency_frames_avg: f64,
        keyframe_force_latency_frames_max: u64,
        keyframe_force_request_frame_ids: Vec<u64>,
        keyframe_force_effective_frame_ids: Vec<u64>,
        keyframe_force_latency_frames: Vec<u64>,
        udp_send_buffer_bytes: i32,
        parameter_sets: AnnexBParameterSets,
        repair_mode: crate::repair::RepairMode,
        repair_cache_ms: u64,
        repair_cache: Arc<Mutex<crate::repair::RepairCache>>,
        repair_stop: Arc<AtomicBool>,
        repair_worker: Option<JoinHandle<()>>,
        current_session_id: Arc<AtomicU64>,
        capability_feedback: Arc<Mutex<crate::media_control::CapabilityFeedbackTracker>>,
        profile_ack_inbox: Arc<(Mutex<VecDeque<crate::media_control::ProfileAck>>, Condvar)>,
        close_ack_inbox: Arc<(
            Mutex<VecDeque<crate::media_control::StreamCloseAck>>,
            Condvar,
        )>,
        control_send_socket: UdpSocket,
        media_clock: Option<MediaClock>,
        first_media_timestamp_us: Option<u64>,
        last_video_timestamp_us: Option<u64>,
        audio_mode: super::AudioSendMode,
        audio_sender: Option<crate::audio_udp::IntegratedAudioSender>,
        cancellation: crate::shutdown::CancellationToken,
        shutdown_coordinator: crate::shutdown::ShutdownCoordinator,
        event_context: crate::shutdown::RuntimeEventContext,
        av_delta_ms_total: f64,
        av_delta_ms_max: f64,
        av_delta_samples: u64,
        runtime_started: Instant,
        current_profile_started_at: Instant,
        current_profile_frames_start: u64,
        current_profile_bytes_start: u64,
        adaptive_runtime: super::AdaptiveRuntimeConfig,
        adaptive_controller: crate::adaptive_quality::AdaptiveQualityController,
        adaptive_windows: crate::adaptive_quality::AdaptiveWindowTracker,
        adaptive_window_metrics: crate::adaptive_quality::AdaptiveWindowMetrics,
        sustainable_fps: crate::frame_rate_policy::SustainableFpsEstimator,
        source_display: crate::display_capability::DisplayCapability,
        frame_rate_decision: crate::frame_rate_policy::FrameRateDecision,
        pending_action: Option<crate::adaptive_quality::AdaptiveAction>,
        pending_profile: Option<crate::adaptive_quality::QualityProfile>,
        previous_encode_lag_skips: u64,
        previous_capture_dropped: u64,
        previous_send_errors: u64,
        profile_change_sequence: u64,
        profile_change_started_us: Option<u64>,
        profile_change_completed_us: Option<u64>,
        profile_change_started_at: Option<Instant>,
        profile_change_duration_ms: f64,
        profile_change_idr_frame_id: Option<u64>,
        old_video_session_id: Option<u64>,
        encoder_reconfigure_success: bool,
        encoder_reconfigure_error: Option<String>,
        bitrate_update_requested_mbps: Option<f64>,
        bitrate_update_applied_mbps: Option<f64>,
        bitrate_update_method: String,
        bitrate_update_success: Option<bool>,
        bitrate_update_error: Option<String>,
        bitrate_reconfigure_started_at: Option<Instant>,
        bitrate_reconfigure_old_mbps: f64,
        bitrate_reconfigure_latency_ms: f64,
        bitrate_reconfigure_idr_requested: bool,
        bitrate_fallback_to_full_transition: bool,
        profile_change_reason: Option<String>,
        video_profile_generation: u64,
        profile_transition_active: bool,
        profile_transition: Option<crate::profile_transition::SenderProfileTransition>,
        profile_transition_rollback_count: u64,
        profile_transition_failures: crate::profile_transition::TransitionFailureTelemetry,
        mprf_packets_sent: u64,
        mprf_retry_attempts: u64,
        mprf_ack_timeout: u64,
        mprf_ack_rtt_ms_total: f64,
        mprf_ack_rtt_ms_max: f64,
        mprf_ack_rtt_samples: u64,
        new_pipeline_prepare_success: u64,
        new_pipeline_prepare_error: u64,
        new_pipeline_prepare_last_error: Option<String>,
        new_pipeline_first_idr_ready: bool,
        receiver_profile_acknowledged: bool,
        receiver_first_idr_decoded: bool,
        receiver_first_frame_rendered: bool,
        transition_feedback_ignored: u64,
        transition_settle_windows: u32,
        transition_settle_duration_ms: f64,
        feedback_sample_eligible: bool,
        feedback_ineligible_reason: Option<String>,
        receiver_valid_feedback_windows: u32,
        receiver_render_ready: bool,
        receiver_profile_settled: bool,
        started_event_emitted: bool,
        last_pipeline_started: Option<CapturePipelineStarted>,
        last_pipeline_stats: Option<CapturePipelineStats>,
        terminal_encoder_stats: crate::wmf_h264_encoder::EncoderStats,
    }

    #[derive(Debug)]
    struct WorkerShutdownSummary {
        audio: &'static str,
        pacer: crate::shutdown::WorkerJoinStatus,
        control: crate::shutdown::WorkerJoinStatus,
        join_error: Option<String>,
        runtime_error: Option<String>,
        cleanup_ms: f64,
        close_sent: u64,
        close_retry_count: u64,
        close_ack_received: bool,
        close_handshake_timeout: bool,
        peer_timeout_triggered: bool,
        cleanup_deadline_ms: u64,
        lifecycle_state: &'static str,
        retained_workers: Vec<String>,
    }

    impl WorkerShutdownSummary {
        fn join_clean(&self) -> bool {
            self.join_error.is_none()
                && self.audio != "incomplete"
                && self.pacer.clean()
                && self.control.clean()
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
                r#""worker_join_audio":"{}","worker_join_pacer":"{}","worker_join_control":"{}","worker_join_all_clean":{},"worker_join_error":{},"runtime_completed_clean":{},"runtime_error":{},"terminal_success":{},"capture_thread_state":"{}","encode_thread_state":"owner_thread_returned","send_thread_state":"{}","feedback_thread_state":"{}","repair_thread_state":"{}","transition_thread_state":"not_started","retained_worker_count":{},"retained_workers":{},"cleanup_duration_ms":{:.3},"cleanup_deadline_ms":{},"stream_close_sent":{},"close_sent":{},"stream_close_retry_count":{},"stream_close_ack_received":{},"stream_close_handshake_timeout":{},"peer_timeout_triggered":{},"cleanup_lifecycle_state":"{}","cleanup_stop_requested":true"#,
                self.audio,
                self.pacer.name(),
                self.control.name(),
                self.join_clean(),
                optional_json_string(self.join_error.as_deref()),
                self.runtime_completed_clean(),
                optional_json_string(self.runtime_error.as_deref()),
                self.clean(),
                if self
                    .retained_workers
                    .iter()
                    .any(|name| name.starts_with("wgc-"))
                {
                    "retained_unjoined"
                } else {
                    "joined_before_shutdown"
                },
                self.pacer.name(),
                self.control.name(),
                self.control.name(),
                self.retained_workers.len(),
                json_string_array(&self.retained_workers),
                self.cleanup_ms,
                self.cleanup_deadline_ms,
                self.close_sent,
                self.close_sent > 0,
                self.close_retry_count,
                self.close_ack_received,
                self.close_handshake_timeout,
                self.peer_timeout_triggered,
                self.lifecycle_state,
            )
        }
    }

    impl UdpObserver {
        fn new(config: &H264SendConfig) -> Result<Self, String> {
            let target = format!("{}:{}", config.host, config.port);
            let socket =
                UdpSocket::bind("0.0.0.0:0").map_err(|err| format!("UDP bind failed: {err}"))?;
            socket
                .connect(&target)
                .map_err(|err| format!("UDP connect to {target} failed: {err}"))?;
            let udp_send_buffer_bytes = crate::udp_socket::configure_send_buffer(
                &socket,
                crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES,
            )?;
            let (sender, receiver) = mpsc::sync_channel::<PacerCommand>(SEND_QUEUE_DEPTH);
            let send_counters = Arc::new(Mutex::new(SendCounters::default()));
            let repair_cache = Arc::new(Mutex::new(crate::repair::RepairCache::new(
                Duration::from_millis(config.repair_cache_ms),
            )?));
            let repair_stop = Arc::new(AtomicBool::new(false));
            let shutdown_coordinator = crate::shutdown::ShutdownCoordinator::new();
            let cancellation = shutdown_coordinator.token();
            let event_context = crate::shutdown::RuntimeEventContext::new(make_session_id());
            let control_socket = socket
                .try_clone()
                .map_err(|err| format!("clone UDP socket for media control failed: {err}"))?;
            let control_send_socket = socket
                .try_clone()
                .map_err(|err| format!("clone UDP socket for profile control failed: {err}"))?;
            control_socket
                .set_read_timeout(Some(Duration::from_millis(20)))
                .map_err(|err| format!("set media control socket timeout failed: {err}"))?;
            let worker_counters = Arc::clone(&send_counters);
            let worker_cache = Arc::clone(&repair_cache);
            let worker_target = target.clone();
            let packet_pacing = config.packet_pacing;
            let frame_interval =
                Duration::from_nanos(1_000_000_000u64 / u64::from(config.target_fps));
            let repair_mode = config.repair_mode;
            let mut worker = Some(
                thread::Builder::new()
                    .name("agoralink-udp-pacer".to_string())
                    .spawn(move || {
                    let pacing_epoch = Instant::now();
                    let mut frame_cadence =
                        crate::sender_scheduling::DeadlineCadence::new(frame_interval)
                            .expect("validated target FPS creates a non-zero interval");
                    let mut cadence_interval = frame_interval;
                    let mut latency_histogram =
                        crate::sender_scheduling::LatencyHistogram::default();
                    let mut send_syscall_histogram =
                        crate::sender_scheduling::LatencyHistogram::default();
                    let mut worker_loop_histogram =
                        crate::sender_scheduling::LatencyHistogram::default();
                    let mut histogram_published_at = Instant::now();
                    while let Ok(command) = receiver.recv() {
                        let frame = match command {
                            PacerCommand::Frame(frame) => frame,
                            PacerCommand::Barrier(done) => {
                                let _ = done.send(());
                                continue;
                            }
                        };
                        let frame_interval = frame.frame_interval;
                        if frame_interval != cadence_interval {
                            frame_cadence =
                                crate::sender_scheduling::DeadlineCadence::new(frame_interval)
                                    .expect("validated target FPS creates a non-zero interval");
                            cadence_interval = frame_interval;
                        }
                        let worker_loop_started = Instant::now();
                        let now = Instant::now();
                        let now_ns = now
                            .saturating_duration_since(pacing_epoch)
                            .as_nanos()
                            .min(u128::from(u64::MAX)) as u64;
                        let deadline_ns = frame_cadence.next_deadline_ns(now_ns);
                        let frame_started = pacing_epoch + Duration::from_nanos(deadline_ns);
                        let packet_count = frame.packets.len().max(1);
                        let mut bytes_sent = 0u64;
                        let mut packets_sent = 0u64;
                        let mut sleep_ms = 0.0;
                        let mut pacing_batches = 0u64;
                        let mut pacing_batch_packets = 0u64;
                        let mut pacing_batch_max = 0u64;
                        let mut pacing_overrun_ticks = 0u64;
                        let mut pacing_late_us_total = 0.0;
                        let mut pacing_late_us_max: f64 = 0.0;
                        let mut video_send_syscall_ns_total = 0u64;
                        let mut video_send_syscall_ns_max = 0u64;
                        let mut video_send_syscalls = 0u64;
                        let mut failure = None;
                        if packet_pacing.uses_batch() {
                            sleep_ms += wait_until(frame_started);
                        }
                        let batch_size = if packet_pacing.uses_batch() {
                            batch_size_for_frame(packet_count, frame_interval, PACING_TICK)
                        } else {
                            packet_count
                        };
                        let mut packet_index = 0usize;
                        let mut tick_index = 0u32;
                        let mut schedule_origin = frame_started;
                        while packet_index < frame.packets.len() {
                            if packet_pacing.uses_batch() {
                                let mut target = schedule_origin
                                    + PACING_TICK.saturating_mul(tick_index);
                                let now = Instant::now();
                                let late = now.checked_duration_since(target).unwrap_or_default();
                                if late > PACING_TICK.saturating_mul(2) {
                                    pacing_overrun_ticks += 1;
                                    schedule_origin = now;
                                    tick_index = 0;
                                    target = now;
                                }
                                let late_us = late.as_secs_f64() * 1_000_000.0;
                                latency_histogram.record_us(late.as_micros().min(u128::from(u64::MAX)) as u64);
                                pacing_late_us_total += late_us;
                                pacing_late_us_max = pacing_late_us_max.max(late_us);
                                sleep_ms += wait_until(target);
                            }
                            let batch_end = (packet_index + batch_size).min(frame.packets.len());
                            let mut sent_in_batch = 0u64;
                            for (offset, packet) in frame.packets[packet_index..batch_end].iter().enumerate() {
                                let send_started = Instant::now();
                                let send_result = socket.send(packet);
                                let send_ns = send_started
                                    .elapsed()
                                    .as_nanos()
                                    .min(u128::from(u64::MAX)) as u64;
                                send_syscall_histogram.record_us(send_ns / 1000);
                                video_send_syscall_ns_total =
                                    video_send_syscall_ns_total.saturating_add(send_ns);
                                video_send_syscall_ns_max = video_send_syscall_ns_max.max(send_ns);
                                video_send_syscalls += 1;
                                match send_result {
                                    Ok(sent) if sent == packet.len() => {
                                        packets_sent += 1;
                                        sent_in_batch += 1;
                                        bytes_sent += sent as u64;
                                        if repair_mode == crate::repair::RepairMode::Nack
                                            && packet_index + offset < frame.data_packet_count
                                        {
                                            if let Some((_session, key, flags)) =
                                                crate::repair::media_packet_key(packet)
                                            {
                                                if flags & crate::FLAG_FEC == 0 {
                                                    if let Ok(mut cache) = worker_cache.lock() {
                                                        cache.insert(key, packet.clone(), Instant::now());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Ok(sent) => {
                                        failure = Some(format!(
                                            "UDP short send to {worker_target}: expected {}, sent {sent}",
                                            packet.len()
                                        ));
                                        break;
                                    }
                                    Err(err) => {
                                        failure = Some(format!(
                                            "UDP send to {worker_target} failed: {err}"
                                        ));
                                        break;
                                    }
                                }
                            }
                            if packet_pacing.uses_batch() {
                                pacing_batches += 1;
                                pacing_batch_packets += sent_in_batch;
                                pacing_batch_max = pacing_batch_max.max(sent_in_batch);
                            }
                            if failure.is_some() {
                                break;
                            }
                            packet_index = batch_end;
                            tick_index = tick_index.saturating_add(1);
                        }
                        let elapsed = frame_started.elapsed();
                        let worker_loop_ns = worker_loop_started
                            .elapsed()
                            .as_nanos()
                            .min(u128::from(u64::MAX)) as u64;
                        worker_loop_histogram.record_us(worker_loop_ns / 1000);
                        let mut counters = match worker_counters.lock() {
                            Ok(counters) => counters,
                            Err(_) => break,
                        };
                        counters.packets_sent += packets_sent;
                        counters.bytes_sent += bytes_sent;
                        counters.data_packets_sent +=
                            packets_sent.min(frame.data_packet_count as u64);
                        if packets_sent > frame.data_packet_count as u64 {
                            counters.fec_packets_sent += frame.fec_packet_count as u64;
                            counters.fec_bytes_sent += frame.fec_bytes as u64;
                        }
                        counters.max_packets_per_frame = counters
                            .max_packets_per_frame
                            .max(frame.packets.len() as u64);
                        counters.pacing_sleep_ms_total += sleep_ms;
                        counters.pacing_batches += pacing_batches;
                        counters.pacing_batch_packets += pacing_batch_packets;
                        counters.pacing_batch_max =
                            counters.pacing_batch_max.max(pacing_batch_max);
                        counters.pacing_overrun_ticks += pacing_overrun_ticks;
                        counters.pacing_late_us_total += pacing_late_us_total;
                        counters.pacing_late_us_max =
                            counters.pacing_late_us_max.max(pacing_late_us_max);
                        if histogram_published_at.elapsed() >= Duration::from_secs(1) {
                            counters.pacing_late_us_p50 =
                                latency_histogram.percentile_us(50) as f64;
                            counters.pacing_late_us_p95 =
                                latency_histogram.percentile_us(95) as f64;
                            counters.pacing_late_us_p99 =
                                latency_histogram.percentile_us(99) as f64;
                            counters.pacing_late_us_max =
                                latency_histogram.max_us() as f64;
                            counters.video_send_syscall_us_p95 =
                                send_syscall_histogram.percentile_us(95);
                            counters.video_send_syscall_us_p99 =
                                send_syscall_histogram.percentile_us(99);
                            counters.video_worker_loop_us_p50 =
                                worker_loop_histogram.percentile_us(50);
                            counters.video_worker_loop_us_p95 =
                                worker_loop_histogram.percentile_us(95);
                            counters.video_worker_loop_us_p99 =
                                worker_loop_histogram.percentile_us(99);
                            latency_histogram =
                                crate::sender_scheduling::LatencyHistogram::default();
                            send_syscall_histogram =
                                crate::sender_scheduling::LatencyHistogram::default();
                            worker_loop_histogram =
                                crate::sender_scheduling::LatencyHistogram::default();
                            histogram_published_at = Instant::now();
                        }
                        counters.video_send_syscall_ns_total = counters
                            .video_send_syscall_ns_total
                            .saturating_add(video_send_syscall_ns_total);
                        counters.video_send_syscall_ns_max = counters
                            .video_send_syscall_ns_max
                            .max(video_send_syscall_ns_max);
                        counters.video_send_syscalls = counters
                            .video_send_syscalls
                            .saturating_add(video_send_syscalls);
                        counters.video_worker_loop_ns_total = counters
                            .video_worker_loop_ns_total
                            .saturating_add(worker_loop_ns);
                        counters.video_worker_loop_ns_max =
                            counters.video_worker_loop_ns_max.max(worker_loop_ns);
                        counters.video_worker_loops += 1;
                        if elapsed > frame_interval {
                            counters.pacing_overrun_frames += 1;
                        }
                        if let Some(error) = failure {
                            counters.send_errors += 1;
                            counters.error = Some(error);
                            break;
                        }
                        counters.frames_sent += 1;
                    }
                    })
                    .map_err(|err| format!("spawn UDP pacing worker failed: {err}"))?,
            );
            let session_id = make_session_id();
            let current_session_id = Arc::new(AtomicU64::new(session_id));
            let capability_feedback = Arc::new(Mutex::new(
                crate::media_control::CapabilityFeedbackTracker::new(),
            ));
            let profile_ack_inbox = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
            let close_ack_inbox = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
            let cache = Arc::clone(&repair_cache);
            let counters = Arc::clone(&send_counters);
            let stop = Arc::clone(&repair_stop);
            let worker_session = Arc::clone(&current_session_id);
            let worker_feedback = Arc::clone(&capability_feedback);
            let worker_profile_acks = Arc::clone(&profile_ack_inbox);
            let worker_close_acks = Arc::clone(&close_ack_inbox);
            let worker_cancellation = cancellation.clone();
            let repair_mode = config.repair_mode;
            let worker_send_mode = config.mode;
            let repair_worker = match thread::Builder::new()
                .name("agoralink-media-control".to_string())
                .spawn(move || {
                    run_media_control_receiver(
                        control_socket,
                        cache,
                        counters,
                        stop,
                        worker_session,
                        worker_feedback,
                        worker_profile_acks,
                        worker_close_acks,
                        worker_cancellation,
                        repair_mode,
                        worker_send_mode,
                    )
                }) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    repair_stop.store(true, Ordering::SeqCst);
                    drop(sender);
                    let status = crate::shutdown::try_join_until(
                        &mut worker,
                        Instant::now()
                            + crate::shutdown::ShutdownConfig::default().worker_join_timeout,
                    );
                    if status == crate::shutdown::WorkerJoinStatus::TimedOut {
                        crate::shutdown::retain_unjoined_worker(
                            "udp-pacer-constructor-rollback",
                            &mut worker,
                        );
                    }
                    let detail = format!(
                        "spawn media control thread failed: {error}; pacer rollback={}",
                        status.name()
                    );
                    return Err(if status == crate::shutdown::WorkerJoinStatus::TimedOut {
                        format!(
                            "{}: {detail}",
                            crate::shutdown::WORKER_OWNERSHIP_FAILURE_TAG
                        )
                    } else {
                        detail
                    });
                }
            };
            let media_clock =
                (config.audio_mode == super::AudioSendMode::System).then(MediaClock::new);
            let audio_sender = if config.audio_mode == super::AudioSendMode::System {
                Some(crate::audio_udp::spawn_integrated_audio_sender(
                    config.host.clone(),
                    config.port,
                    10,
                    session_id,
                    media_clock
                        .as_ref()
                        .expect("audio mode creates a media clock")
                        .clone(),
                ))
            } else {
                None
            };
            let source_display = crate::display_capability::detect_primary_display(
                config.adaptive.display_refresh_detect,
                0,
            );
            let initial_profile = crate::adaptive_quality::QualityProfile {
                width: config.out_width,
                height: config.out_height,
                fps: config.target_fps,
                bitrate_mbps: config.bitrate_mbps,
            };
            let mut adaptive_controller = crate::adaptive_quality::AdaptiveQualityController::new(
                config.adaptive.quality.clone(),
                initial_profile,
                Duration::ZERO,
            );
            let frame_rate_decision = crate::frame_rate_policy::select_target_fps(
                crate::frame_rate_policy::FrameRatePolicyInput {
                    source_refresh: source_display
                        .is_available()
                        .then_some(source_display.refresh),
                    receiver_refresh: None,
                    user_max_fps: config.adaptive.max_fps,
                    configured_fps: config.target_fps,
                    capture_sustainable_fps: None,
                    encoder_sustainable_fps: None,
                    adaptive_enabled: config.adaptive.quality.mode
                        != crate::adaptive_quality::AdaptiveMode::Off,
                    high_refresh_enabled: config.adaptive.enable_high_refresh,
                    feedback_is_fresh: false,
                },
            );
            adaptive_controller.set_nominal_fps(frame_rate_decision.nominal_target_fps);
            let runtime_started = Instant::now();
            shutdown_coordinator.mark_running();
            Ok(Self {
                sender: Some(sender),
                worker,
                send_counters,
                target,
                host: config.host.clone(),
                port: config.port,
                session_id,
                mode: config.mode,
                duration_sec: config.duration_sec,
                next_frame_id: 0,
                h264_bytes: 0,
                keyframes: 0,
                idr_frames: 0,
                sps_pps_repeated: 0,
                last_idr_frame_id: None,
                idr_interval_frames_total: 0,
                idr_interval_count: 0,
                config_frames: 0,
                previous_packets: 0,
                previous_frames: 0,
                previous_bytes: 0,
                previous_repair_packets: 0,
                previous_repair_bytes: 0,
                previous_nack_items: 0,
                previous_duplicate_repairs: 0,
                packetize_send_ms_total: 0.0,
                packetize_latency: crate::sender_scheduling::LatencyHistogram::default(),
                packetize_send_ms_p50: 0.0,
                packetize_send_ms_p95: 0.0,
                packetize_send_ms_p99: 0.0,
                packet_pacing: config.packet_pacing,
                fec_mode: config.fec_mode,
                udp_payload_size: config.udp_payload_size,
                data_payload_size: config.udp_payload_size
                    - crate::HEADER_LEN
                    - if config.fec_mode == FecMode::SingleXor {
                        crate::fec::FEC_METADATA_LEN
                    } else {
                        0
                    },
                keyframe_interval_sec: config.keyframe_interval_sec,
                current_target_fps: config.target_fps,
                keyframe_control_configured: false,
                keyframe_interval_target_frames: (config.keyframe_interval_sec
                    * f64::from(config.target_fps))
                .round()
                .max(1.0) as u32,
                keyframe_force_supported: false,
                keyframe_force_requests: 0,
                keyframe_force_failures: 0,
                keyframe_config_method: "uninitialized".to_string(),
                keyframe_config_applied: false,
                keyframe_config_error: None,
                keyframe_force_last_requested_frame_id: None,
                keyframe_force_last_effective_frame_id: None,
                keyframe_force_latency_frames_avg: 0.0,
                keyframe_force_latency_frames_max: 0,
                keyframe_force_request_frame_ids: Vec::new(),
                keyframe_force_effective_frame_ids: Vec::new(),
                keyframe_force_latency_frames: Vec::new(),
                udp_send_buffer_bytes,
                parameter_sets: AnnexBParameterSets::default(),
                repair_mode: config.repair_mode,
                repair_cache_ms: config.repair_cache_ms,
                repair_cache,
                repair_stop,
                repair_worker,
                current_session_id,
                capability_feedback,
                profile_ack_inbox,
                close_ack_inbox,
                control_send_socket,
                media_clock,
                first_media_timestamp_us: None,
                last_video_timestamp_us: None,
                audio_mode: config.audio_mode,
                audio_sender,
                cancellation,
                shutdown_coordinator,
                event_context,
                av_delta_ms_total: 0.0,
                av_delta_ms_max: 0.0,
                av_delta_samples: 0,
                runtime_started,
                current_profile_started_at: runtime_started,
                current_profile_frames_start: 0,
                current_profile_bytes_start: 0,
                adaptive_runtime: config.adaptive.clone(),
                adaptive_controller,
                adaptive_windows: crate::adaptive_quality::AdaptiveWindowTracker::default(),
                adaptive_window_metrics: crate::adaptive_quality::AdaptiveWindowMetrics::default(),
                sustainable_fps: crate::frame_rate_policy::SustainableFpsEstimator::default(),
                source_display,
                frame_rate_decision,
                pending_action: None,
                pending_profile: None,
                previous_encode_lag_skips: 0,
                previous_capture_dropped: 0,
                previous_send_errors: 0,
                profile_change_sequence: 0,
                profile_change_started_us: None,
                profile_change_completed_us: None,
                profile_change_started_at: None,
                profile_change_duration_ms: 0.0,
                profile_change_idr_frame_id: None,
                old_video_session_id: None,
                encoder_reconfigure_success: false,
                encoder_reconfigure_error: None,
                bitrate_update_requested_mbps: None,
                bitrate_update_applied_mbps: None,
                bitrate_update_method: "none".to_string(),
                bitrate_update_success: None,
                bitrate_update_error: None,
                bitrate_reconfigure_started_at: None,
                bitrate_reconfigure_old_mbps: config.bitrate_mbps,
                bitrate_reconfigure_latency_ms: 0.0,
                bitrate_reconfigure_idr_requested: false,
                bitrate_fallback_to_full_transition: false,
                profile_change_reason: None,
                video_profile_generation: 0,
                profile_transition_active: false,
                profile_transition: None,
                profile_transition_rollback_count: 0,
                profile_transition_failures:
                    crate::profile_transition::TransitionFailureTelemetry::default(),
                mprf_packets_sent: 0,
                mprf_retry_attempts: 0,
                mprf_ack_timeout: 0,
                mprf_ack_rtt_ms_total: 0.0,
                mprf_ack_rtt_ms_max: 0.0,
                mprf_ack_rtt_samples: 0,
                new_pipeline_prepare_success: 0,
                new_pipeline_prepare_error: 0,
                new_pipeline_prepare_last_error: None,
                new_pipeline_first_idr_ready: false,
                receiver_profile_acknowledged: false,
                receiver_first_idr_decoded: false,
                receiver_first_frame_rendered: false,
                transition_feedback_ignored: 0,
                transition_settle_windows: 0,
                transition_settle_duration_ms: 0.0,
                feedback_sample_eligible: false,
                feedback_ineligible_reason: Some("receiver-startup".to_string()),
                receiver_valid_feedback_windows: 0,
                receiver_render_ready: false,
                receiver_profile_settled: false,
                started_event_emitted: false,
                last_pipeline_started: None,
                last_pipeline_stats: None,
                terminal_encoder_stats: crate::wmf_h264_encoder::EncoderStats::default(),
            })
        }

        fn send_snapshot(&self) -> SendCounters {
            self.send_counters
                .lock()
                .map(|counters| counters.clone())
                .unwrap_or_default()
        }

        fn observed_idr_interval_frames(&self) -> Option<f64> {
            (self.idr_interval_count > 0)
                .then(|| self.idr_interval_frames_total as f64 / self.idr_interval_count as f64)
        }

        fn keyframe_interval_warning(&self) -> Option<&'static str> {
            evaluate_keyframe_interval(
                self.keyframe_control_configured,
                self.keyframe_interval_target_frames,
                self.observed_idr_interval_frames(),
                self.keyframe_force_failures,
            )
        }

        fn keyframe_interval_applied(&self) -> bool {
            self.keyframe_interval_warning().is_none()
        }

        fn keyframe_metrics_fragment(&self) -> String {
            let observed_frames = self.observed_idr_interval_frames();
            let observed_sec = observed_frames.map(|frames| {
                frames / f64::from(self.keyframe_interval_target_frames)
                    * self.keyframe_interval_sec
            });
            format!(
                r#""keyframe_interval_requested_sec":{:.3},"keyframe_interval_target_frames":{},"keyframe_interval_observed_sec":{},"keyframe_interval_observed_frames_avg":{},"keyframe_interval_warning":{},"keyframe_config_method":"{}","keyframe_config_applied":{},"keyframe_config_error":{},"keyframe_force_supported":{},"keyframe_force_requests":{},"keyframe_force_failures":{},"keyframe_force_last_requested_frame_id":{},"keyframe_force_last_effective_frame_id":{},"keyframe_force_request_frame_ids":{},"keyframe_force_effective_frame_ids":{},"keyframe_force_latency_frames":{},"keyframe_force_latency_frames_avg":{:.3},"keyframe_force_latency_frames_max":{},"idr_frames_sent":{},"sps_pps_repeated":{}"#,
                self.keyframe_interval_sec,
                self.keyframe_interval_target_frames,
                optional_f64_json(observed_sec),
                optional_f64_json(observed_frames),
                optional_json_string(self.keyframe_interval_warning()),
                json_escape(&self.keyframe_config_method),
                self.keyframe_config_applied,
                optional_json_string(self.keyframe_config_error.as_deref()),
                self.keyframe_force_supported,
                self.keyframe_force_requests,
                self.keyframe_force_failures,
                crate::media_clock::optional_u64_json(self.keyframe_force_last_requested_frame_id,),
                crate::media_clock::optional_u64_json(self.keyframe_force_last_effective_frame_id,),
                u64_json_array(&self.keyframe_force_request_frame_ids),
                u64_json_array(&self.keyframe_force_effective_frame_ids),
                u64_json_array(&self.keyframe_force_latency_frames),
                self.keyframe_force_latency_frames_avg,
                self.keyframe_force_latency_frames_max,
                self.idr_frames,
                self.sps_pps_repeated,
            )
        }

        fn perform_stream_close_handshake(
            &self,
            reason: crate::shutdown::StopReason,
        ) -> (u64, u64, bool, bool) {
            if self.mode != H264SendMode::Screen || !reason.should_notify_peer() {
                return (0, 0, false, false);
            }
            let config = crate::shutdown::ShutdownConfig::default();
            let close = crate::media_control::StreamClose {
                version: crate::media_control::MEDIA_CONTROL_VERSION,
                stream_id: crate::STREAM_VIDEO,
                reason_code: reason as u8,
                video_session_id: self.session_id,
                close_id: make_session_id(),
                timestamp_us: self.media_timestamp_us(),
                last_frame_id: self.next_frame_id.saturating_sub(1),
            };
            let Ok(bytes) = close.encode() else {
                return (0, 0, false, true);
            };
            let deadline = Instant::now() + config.close_handshake_timeout;
            let mut retry_delay = config.close_retry_initial;
            let mut sent = 0u64;
            let mut ack_received = false;
            let (inbox, wake) = &*self.close_ack_inbox;
            if let Ok(mut inbox) = inbox.lock() {
                inbox.retain(|ack| !ack.matches(close));
            }
            while Instant::now() < deadline && !ack_received {
                if self
                    .control_send_socket
                    .send(&bytes)
                    .is_ok_and(|written| written == bytes.len())
                {
                    sent = sent.saturating_add(1);
                }
                let wait = retry_delay.min(deadline.saturating_duration_since(Instant::now()));
                let Ok(inbox) = inbox.lock() else {
                    break;
                };
                let Ok((mut inbox, _)) = wake.wait_timeout(inbox, wait) else {
                    break;
                };
                if let Some(index) = inbox.iter().position(|ack| ack.matches(close)) {
                    inbox.remove(index);
                    ack_received = true;
                }
                retry_delay = retry_delay.saturating_mul(2).min(config.close_retry_max);
            }
            (
                sent,
                sent.saturating_sub(1),
                ack_received,
                sent > 0 && !ack_received,
            )
        }

        fn finish_worker(&mut self, reason: crate::shutdown::StopReason) -> WorkerShutdownSummary {
            let cleanup_started = Instant::now();
            self.shutdown_coordinator.request_stop(reason);
            let _cleanup_owner = self.shutdown_coordinator.begin_cleanup();
            if crate::shutdown::ctrl_c_requested() {
                self.cancellation.cancel(crate::shutdown::StopReason::CtrlC);
            }
            self.pending_action = None;
            self.pending_profile = None;
            let mut join_errors = Vec::new();
            let (close_sent, close_retry_count, close_ack_received, close_handshake_timeout) =
                self.perform_stream_close_handshake(reason);
            self.sender.take();
            self.repair_stop.store(true, Ordering::SeqCst);
            let audio_status = if let Some(audio) = self.audio_sender.as_mut() {
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
            let shutdown_config = crate::shutdown::ShutdownConfig::default();
            let pacer = crate::shutdown::try_join_until(
                &mut self.worker,
                Instant::now() + shutdown_config.worker_join_timeout,
            );
            if pacer == crate::shutdown::WorkerJoinStatus::TimedOut {
                crate::shutdown::retain_unjoined_worker("udp-pacer", &mut self.worker);
            }
            if !pacer.clean() {
                join_errors.push(format!("UDP pacing worker shutdown: {}", pacer.name()));
            }
            let control = crate::shutdown::try_join_until(
                &mut self.repair_worker,
                Instant::now() + shutdown_config.worker_join_timeout,
            );
            if control == crate::shutdown::WorkerJoinStatus::TimedOut {
                crate::shutdown::retain_unjoined_worker(
                    "media-control-repair",
                    &mut self.repair_worker,
                );
            }
            if !control.clean() {
                join_errors.push(format!("media control worker shutdown: {}", control.name()));
            }
            let runtime_error = self.send_snapshot().error;
            let retained_workers = crate::shutdown::retained_worker_names();
            if !retained_workers.is_empty() {
                join_errors.push(format!(
                    "{}: retained workers: {}",
                    crate::shutdown::WORKER_OWNERSHIP_FAILURE_TAG,
                    retained_workers.join(",")
                ));
            }
            let joins_clean = join_errors.is_empty()
                && pacer.clean()
                && control.clean()
                && audio_status != "incomplete"
                && retained_workers.is_empty();
            self.shutdown_coordinator.finish_cleanup(joins_clean);
            let lifecycle_state = self.shutdown_coordinator.state().name();
            WorkerShutdownSummary {
                audio: audio_status,
                pacer,
                control,
                join_error: (!join_errors.is_empty()).then(|| join_errors.join("; ")),
                runtime_error,
                cleanup_ms: cleanup_started.elapsed().as_secs_f64() * 1_000.0,
                close_sent,
                close_retry_count,
                close_ack_received,
                close_handshake_timeout,
                peer_timeout_triggered: self.cancellation.reason()
                    == Some(crate::shutdown::StopReason::PeerTimeout),
                cleanup_deadline_ms: shutdown_config
                    .worker_join_timeout
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64,
                lifecycle_state,
                retained_workers,
            }
        }

        fn audio_stats(&self) -> crate::audio_udp::IntegratedAudioSendStats {
            self.audio_sender
                .as_ref()
                .map(|audio| audio.stats())
                .unwrap_or_default()
        }

        fn update_av_delta(&mut self) {
            let audio = self.audio_stats();
            let (Some(audio_ts), Some(video_ts)) =
                (audio.last_audio_timestamp_us, self.last_video_timestamp_us)
            else {
                return;
            };
            let delta_ms = (audio_ts as i128 - video_ts as i128).unsigned_abs() as f64 / 1000.0;
            self.av_delta_ms_total += delta_ms;
            self.av_delta_ms_max = self.av_delta_ms_max.max(delta_ms);
            self.av_delta_samples += 1;
        }

        fn media_timestamp_us(&self) -> u64 {
            self.media_clock
                .as_ref()
                .map_or_else(|| now_millis().saturating_mul(1000), MediaClock::now_us)
        }

        fn flush_pacer(&self) -> Result<(), String> {
            let sender = self
                .sender
                .as_ref()
                .ok_or_else(|| "UDP pacing worker is stopped".to_string())?;
            let (done_sender, done_receiver) = mpsc::sync_channel(0);
            sender
                .send(PacerCommand::Barrier(done_sender))
                .map_err(|_| "UDP pacing worker stopped before profile barrier".to_string())?;
            done_receiver
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "UDP pacing profile barrier timed out".to_string())
        }

        fn transition_profile(
            &mut self,
            profile: crate::adaptive_quality::QualityProfile,
            action: &crate::adaptive_quality::AdaptiveAction,
            _prepared_pipeline: &capture_encode_probe::PreparedCapturePipeline,
        ) -> Result<(), String> {
            self.flush_pacer()?;
            let profile_baseline = self.send_snapshot();
            let old_session_id = self.session_id;
            let mut new_session_id = make_session_id();
            if new_session_id == old_session_id {
                new_session_id = new_session_id.wrapping_add(1).max(1);
            }
            self.profile_change_sequence = self.profile_change_sequence.saturating_add(1);
            let controller_generation = self
                .adaptive_controller
                .telemetry(self.runtime_started.elapsed())
                .profile_generation;
            let generation = controller_generation
                .max(self.video_profile_generation.saturating_add(1))
                .max(1);
            let started_at = Instant::now();
            let started_us = self.media_timestamp_us();
            let change = crate::media_control::ProfileChange {
                version: crate::media_control::MEDIA_CONTROL_VERSION,
                old_session_id,
                new_session_id,
                change_sequence: self.profile_change_sequence,
                profile_generation: generation,
                width: profile.width,
                height: profile.height,
                fps: profile.fps,
                bitrate_mbps: profile.bitrate_mbps as f32,
                timestamp_us: started_us,
                reason_code: match action {
                    crate::adaptive_quality::AdaptiveAction::SetBitrate { .. } => 1,
                    crate::adaptive_quality::AdaptiveAction::SetResolution { .. } => 2,
                    crate::adaptive_quality::AdaptiveAction::SetFps { .. } => 3,
                },
            };
            let encoded_change = change.encode()?;
            let mut transition =
                crate::profile_transition::SenderProfileTransition::prepared(change, started_at);
            self.profile_transition_active = true;
            self.profile_transition_failures.clear_current();
            self.profile_transition = Some(transition.clone());
            if let Ok(mut feedback) = self.capability_feedback.lock() {
                feedback.begin_transition_grace(
                    started_at + crate::profile_transition::SENDER_TRANSITION_TOTAL_DEADLINE,
                );
            }
            let profile_ack_inbox = Arc::clone(&self.profile_ack_inbox);
            let (inbox, wake) = &*profile_ack_inbox;
            match inbox.lock() {
                Ok(mut pending) => pending.clear(),
                Err(_) => {
                    let reason = "profile ACK inbox lock was poisoned";
                    self.abort_profile_transition(
                        &mut transition,
                        crate::profile_transition::SenderTransitionPhase::ControlPending.name(),
                        reason,
                        false,
                        true,
                    );
                    return Err(reason.to_string());
                }
            }
            let mut last_send_error = None;

            loop {
                let now = Instant::now();
                let cancelled = self.cancellation.is_cancelled();
                let duration_elapsed = self.duration_sec.is_some_and(|seconds| {
                    self.runtime_started.elapsed() >= Duration::from_secs(seconds)
                });
                if cancelled || duration_elapsed {
                    let reason = if cancelled {
                        "profile-control-cancelled"
                    } else {
                        "profile-control-duration-expired"
                    };
                    let failure_stage = transition.phase.name();
                    transition.cancel(reason);
                    self.profile_transition_rollback_count =
                        self.profile_transition_rollback_count.saturating_add(1);
                    self.profile_transition_failures.last_failure_reason = Some(reason.to_string());
                    self.profile_transition_failures.last_failure_stage =
                        Some(failure_stage.to_string());
                    self.profile_transition_active = false;
                    self.profile_transition = Some(transition);
                    self.clear_profile_transition_grace();
                    return Err(reason.to_string());
                }
                if transition.should_send_control(now) {
                    let attempt = transition.attempts;
                    match self.control_send_socket.send(&encoded_change) {
                        Ok(length) if length == encoded_change.len() => {
                            self.mprf_packets_sent = self.mprf_packets_sent.saturating_add(1);
                            if attempt > 0 {
                                self.mprf_retry_attempts =
                                    self.mprf_retry_attempts.saturating_add(1);
                            }
                        }
                        Ok(_) => {
                            last_send_error = Some("profile change UDP short send".to_string())
                        }
                        Err(error) => {
                            last_send_error =
                                Some(format!("profile change UDP send failed: {error}"))
                        }
                    }
                    transition.record_control_sent(now);
                    self.profile_transition = Some(transition.clone());
                }

                let mut ack_received = false;
                let mut ack_rejected_reason = None;
                {
                    let mut pending = match inbox.lock() {
                        Ok(pending) => pending,
                        Err(_) => {
                            let reason = "profile ACK inbox lock was poisoned";
                            let stage = transition.phase.name();
                            self.abort_profile_transition(
                                &mut transition,
                                stage,
                                reason,
                                false,
                                true,
                            );
                            return Err(reason.to_string());
                        }
                    };
                    while let Some(ack) = pending.pop_front() {
                        let ack_now = Instant::now();
                        match transition.observe_ack(ack, ack_now) {
                            crate::profile_transition::SenderAckDecision::Accepted => {
                                let rtt_ms = transition
                                    .last_control_sent_at
                                    .map(|sent_at| {
                                        ack_now.saturating_duration_since(sent_at).as_secs_f64()
                                            * 1_000.0
                                    })
                                    .unwrap_or(0.0);
                                self.mprf_ack_rtt_ms_total += rtt_ms;
                                self.mprf_ack_rtt_ms_max = self.mprf_ack_rtt_ms_max.max(rtt_ms);
                                self.mprf_ack_rtt_samples =
                                    self.mprf_ack_rtt_samples.saturating_add(1);
                                ack_received = true;
                                break;
                            }
                            crate::profile_transition::SenderAckDecision::Rejected(reason) => {
                                ack_rejected_reason = Some(reason);
                                break;
                            }
                            crate::profile_transition::SenderAckDecision::Unmatched => {}
                        }
                        if let Ok(mut counters) = self.send_counters.lock() {
                            counters.mprf_ack_invalid = counters.mprf_ack_invalid.saturating_add(1);
                        }
                    }
                }
                if let Some(reason_code) = ack_rejected_reason {
                    let reason = format!("profile-control-rejected:{reason_code}");
                    self.profile_transition_rollback_count =
                        self.profile_transition_rollback_count.saturating_add(1);
                    self.record_profile_transition_failure(
                        crate::profile_transition::SenderTransitionPhase::AwaitControlAck.name(),
                        &reason,
                        false,
                    );
                    self.profile_transition_active = false;
                    transition.failure_reason = Some(reason.clone());
                    self.profile_transition = Some(transition);
                    return Err(reason);
                }
                if ack_received {
                    break;
                }
                let failure_stage = transition.phase.name();
                if let Some(reason) = transition.check_deadline(Instant::now()) {
                    self.profile_transition_rollback_count =
                        self.profile_transition_rollback_count.saturating_add(1);
                    self.mprf_ack_timeout = self.mprf_ack_timeout.saturating_add(1);
                    self.record_profile_transition_failure(failure_stage, reason, true);
                    self.profile_transition_active = false;
                    self.profile_transition = Some(transition);
                    return Err(last_send_error.map_or_else(
                        || reason.to_string(),
                        |send_error| format!("{reason}: {send_error}"),
                    ));
                }
                let wait_until = transition.next_retry_at.min(transition.ack_deadline);
                let wait_for = wait_until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(50));
                let pending = match inbox.lock() {
                    Ok(pending) => pending,
                    Err(_) => {
                        let reason = "profile ACK inbox lock was poisoned";
                        let stage = transition.phase.name();
                        self.abort_profile_transition(&mut transition, stage, reason, false, true);
                        return Err(reason.to_string());
                    }
                };
                if wake.wait_timeout(pending, wait_for).is_err() {
                    let reason = "profile ACK inbox lock was poisoned";
                    let stage = transition.phase.name();
                    self.abort_profile_transition(&mut transition, stage, reason, false, true);
                    return Err(reason.to_string());
                }
            }

            let audio_stop_result = self
                .audio_sender
                .as_mut()
                .map_or(Ok(()), |audio| audio.stop_and_join());
            self.audio_sender = None;
            if let Err(error) = audio_stop_result {
                let stage = transition.phase.name();
                self.abort_profile_transition(&mut transition, stage, &error, false, true);
                return Err(error);
            }

            if let Err(error) = transition.activate(Instant::now()) {
                let stage =
                    crate::profile_transition::SenderTransitionPhase::ActivateNewSession.name();
                self.abort_profile_transition(&mut transition, stage, &error, false, true);
                return Err(error);
            }

            if let Ok(mut cache) = self.repair_cache.lock() {
                cache.clear();
            }
            if let Ok(mut feedback) = self.capability_feedback.lock() {
                feedback.reset_for_session();
            }
            self.old_video_session_id = Some(old_session_id);
            self.video_profile_generation = generation;
            self.session_id = new_session_id;
            self.current_session_id
                .store(new_session_id, Ordering::Release);
            self.next_frame_id = 0;
            self.current_target_fps = profile.fps;
            self.keyframe_interval_target_frames = (self.keyframe_interval_sec
                * f64::from(profile.fps))
            .round()
            .max(1.0) as u32;
            self.parameter_sets = AnnexBParameterSets::default();
            self.last_idr_frame_id = None;
            self.profile_change_started_us = Some(started_us);
            self.profile_change_completed_us = None;
            self.profile_change_started_at = Some(started_at);
            self.profile_change_duration_ms = 0.0;
            self.profile_change_idr_frame_id = None;
            self.new_pipeline_first_idr_ready = false;
            self.profile_change_reason =
                Some(format!("{}:{}", action.dimension(), action.reason()));
            self.encoder_reconfigure_success = false;
            self.encoder_reconfigure_error = None;
            self.profile_transition_active = true;
            self.profile_transition = Some(transition);
            self.receiver_profile_acknowledged = false;
            self.receiver_first_idr_decoded = false;
            self.receiver_first_frame_rendered = false;
            self.transition_settle_windows = 0;
            self.transition_settle_duration_ms = 0.0;
            self.feedback_sample_eligible = false;
            self.feedback_ineligible_reason = Some("profile-transition".to_string());
            self.receiver_valid_feedback_windows = 0;
            self.receiver_render_ready = false;
            self.receiver_profile_settled = false;
            self.current_profile_started_at = started_at;
            self.current_profile_frames_start = profile_baseline.frames_sent;
            self.current_profile_bytes_start = profile_baseline.bytes_sent;
            self.sustainable_fps.reset_for_transition();
            self.adaptive_windows.reset(new_session_id);
            self.adaptive_window_metrics =
                crate::adaptive_quality::AdaptiveWindowMetrics::default();
            self.adaptive_controller.begin_profile_transition();
            if matches!(
                action,
                crate::adaptive_quality::AdaptiveAction::SetBitrate { .. }
            ) {
                self.bitrate_update_method = "encoder-rebuild".to_string();
                self.bitrate_update_success = Some(false);
            }

            if self.audio_mode == super::AudioSendMode::System {
                self.audio_sender = Some(crate::audio_udp::spawn_integrated_audio_sender(
                    self.host.clone(),
                    self.port,
                    10,
                    new_session_id,
                    self.media_clock
                        .as_ref()
                        .expect("audio mode creates a media clock")
                        .clone(),
                ));
            }
            Ok(())
        }

        fn refresh_adaptive_state(
            &mut self,
            stats: &CapturePipelineStats,
            sent: &SendCounters,
            repair_packets_delta: u64,
            data_bytes_delta: u64,
            repair_bytes_delta: u64,
            nack_items_delta: u64,
            duplicate_repairs_delta: u64,
        ) -> Result<(), String> {
            let detected = crate::display_capability::detect_primary_display(
                self.adaptive_runtime.display_refresh_detect,
                0,
            );
            self.source_display = crate::display_capability::reconcile_display_generation(
                &self.source_display,
                detected,
            );

            let now = Instant::now();
            let (receiver_feedback, feedback_stats) = self
                .capability_feedback
                .lock()
                .map(|mut tracker| (tracker.latest_fresh(now), tracker.stats()))
                .unwrap_or_default();
            if let Ok(mut counters) = self.send_counters.lock() {
                counters.capability_feedback_received = feedback_stats.received;
                counters.capability_feedback_invalid = feedback_stats.invalid;
                counters.capability_feedback_stale = feedback_stats.stale_events;
            }
            let feedback_is_fresh = receiver_feedback.is_some();
            let feedback = receiver_feedback.unwrap_or_default();
            let generation_matches = feedback_is_fresh
                && feedback.version >= crate::media_control::CAPABILITY_FEEDBACK_VERSION
                && feedback.profile_generation == self.video_profile_generation;
            self.receiver_profile_acknowledged =
                generation_matches && feedback.profile_acknowledged();
            self.receiver_first_idr_decoded = generation_matches && feedback.first_idr_decoded();
            self.receiver_first_frame_rendered =
                generation_matches && feedback.first_frame_rendered();
            self.receiver_valid_feedback_windows = if generation_matches {
                feedback.valid_feedback_windows
            } else {
                0
            };
            self.receiver_render_ready = generation_matches && feedback.render_ready();
            self.receiver_profile_settled = generation_matches && feedback.profile_settled();

            let transition_settled = self.profile_transition_active
                && self.receiver_profile_acknowledged
                && self.receiver_first_idr_decoded
                && self.receiver_first_frame_rendered
                && self.receiver_profile_settled
                && !feedback.profile_transition_active()
                && feedback.transition_settle_windows >= 3;
            if transition_settled {
                let commit_error = self
                    .profile_transition
                    .as_mut()
                    .and_then(|transition| transition.commit().err());
                if let Some(error) = commit_error {
                    if let Some(transition) = self.profile_transition.as_mut() {
                        transition.cancel(&error);
                    }
                    self.record_profile_transition_failure(
                        crate::profile_transition::SenderTransitionPhase::AwaitReceiverReadiness
                            .name(),
                        &error,
                        false,
                    );
                    self.profile_transition_active = false;
                    return Err(error);
                }
                self.profile_transition_active = false;
                self.clear_profile_transition_grace();
                self.transition_settle_windows = feedback.transition_settle_windows;
                self.transition_settle_duration_ms = self
                    .profile_change_started_at
                    .map(|started| started.elapsed().as_secs_f64() * 1_000.0)
                    .unwrap_or(f64::from(feedback.transition_settle_duration_ms));
                self.adaptive_controller
                    .finish_profile_transition(self.runtime_started.elapsed());
            } else if self.profile_transition_active && feedback_is_fresh {
                self.transition_feedback_ignored =
                    self.transition_feedback_ignored.saturating_add(1);
                self.transition_settle_windows = feedback.transition_settle_windows;
                self.transition_settle_duration_ms = self
                    .profile_change_started_at
                    .map(|started| started.elapsed().as_secs_f64() * 1_000.0)
                    .unwrap_or(0.0);
            }
            if self.profile_transition_active {
                let failure_stage = self
                    .profile_transition
                    .as_ref()
                    .map(|transition| transition.phase.name())
                    .unwrap_or("unknown");
                if let Some(reason) = self
                    .profile_transition
                    .as_mut()
                    .and_then(|transition| transition.check_deadline(Instant::now()))
                {
                    self.record_profile_transition_failure(failure_stage, reason, true);
                    self.profile_transition_active = false;
                    self.adaptive_controller
                        .finish_profile_transition(self.runtime_started.elapsed());
                    return Err(reason.to_string());
                }
            }

            let feedback_sample_eligible = generation_matches
                && feedback.sample_eligible()
                && self.receiver_profile_acknowledged
                && self.receiver_first_idr_decoded
                && self.receiver_first_frame_rendered
                && self.receiver_render_ready
                && self.receiver_profile_settled
                && feedback.valid_feedback_windows >= 3
                && !feedback.profile_transition_active()
                && !self.profile_transition_active;
            let feedback_ineligible_reason = if feedback_sample_eligible {
                None
            } else if !feedback_is_fresh {
                Some("feedback-stale-or-missing")
            } else if feedback.version < crate::media_control::CAPABILITY_FEEDBACK_VERSION {
                Some("legacy-feedback-without-readiness")
            } else if !generation_matches {
                Some("feedback-profile-generation-mismatch")
            } else if self.profile_transition_active || feedback.profile_transition_active() {
                Some("profile-transition")
            } else if !self.receiver_profile_acknowledged {
                Some("receiver-profile-not-acknowledged")
            } else if !self.receiver_first_idr_decoded {
                Some("receiver-waiting-first-idr")
            } else if !self.receiver_first_frame_rendered {
                Some("receiver-waiting-first-frame")
            } else if !self.receiver_render_ready {
                Some("receiver-render-not-ready")
            } else if feedback.valid_feedback_windows < 3 {
                Some("receiver-valid-window-warmup")
            } else {
                Some("receiver-profile-not-settled")
            };
            self.feedback_sample_eligible = feedback_sample_eligible;
            self.feedback_ineligible_reason = feedback_ineligible_reason.map(ToString::to_string);
            self.sustainable_fps.observe(
                feedback_sample_eligible,
                self.profile_transition_active,
                stats.accepted_fps,
                stats.encode_fps,
            );
            let (capture_sustainable_fps, encoder_sustainable_fps) =
                self.sustainable_fps.estimates();

            let receiver_refresh = feedback_sample_eligible
                .then(|| {
                    crate::display_capability::RefreshRate::new(
                        feedback.display_refresh_numerator,
                        feedback.display_refresh_denominator,
                    )
                    .validate()
                    .ok()
                })
                .flatten();
            self.frame_rate_decision = crate::frame_rate_policy::select_target_fps(
                crate::frame_rate_policy::FrameRatePolicyInput {
                    source_refresh: self
                        .source_display
                        .is_available()
                        .then_some(self.source_display.refresh),
                    receiver_refresh,
                    user_max_fps: self.adaptive_runtime.max_fps,
                    configured_fps: self.current_target_fps,
                    capture_sustainable_fps,
                    encoder_sustainable_fps,
                    adaptive_enabled: self.adaptive_runtime.quality.mode
                        != crate::adaptive_quality::AdaptiveMode::Off,
                    high_refresh_enabled: self.adaptive_runtime.enable_high_refresh,
                    feedback_is_fresh: feedback_sample_eligible,
                },
            );
            self.adaptive_controller
                .set_nominal_fps(self.frame_rate_decision.nominal_target_fps);

            let elapsed = self.runtime_started.elapsed();
            self.adaptive_window_metrics = self.adaptive_windows.observe(
                self.session_id,
                elapsed,
                crate::adaptive_quality::AdaptiveIntervalSample {
                    data_bytes: data_bytes_delta,
                    repair_bytes: repair_bytes_delta,
                    packets_lost: feedback.packets_lost_delta,
                    nack_items: nack_items_delta,
                    repair_packets: repair_packets_delta,
                    late_repairs: 0,
                    duplicate_repairs: duplicate_repairs_delta,
                    complete_frame_ratio: ratio_to_target(
                        f64::from(feedback.decoder_input_fps),
                        self.current_target_fps,
                    ),
                    decoded_frame_ratio: ratio_to_target(
                        f64::from(feedback.decoder_input_fps),
                        self.current_target_fps,
                    ),
                    rendered_frame_ratio: if feedback.decoder_input_fps <= 0.0 {
                        0.0
                    } else {
                        (f64::from(feedback.active_render_fps)
                            / f64::from(feedback.decoder_input_fps))
                        .clamp(0.0, 1.5)
                    },
                },
            );
            let snapshot = crate::adaptive_quality::AdaptiveSnapshot {
                target_fps: self.current_target_fps,
                actual_sender_fps: stats.encode_fps,
                capture_actual_fps: stats.accepted_fps,
                encoder_actual_fps: stats.encode_fps,
                encode_lag_skips_delta: stats
                    .encode_lag_skips
                    .saturating_sub(self.previous_encode_lag_skips),
                capture_dropped_delta: stats
                    .capture_dropped
                    .saturating_sub(self.previous_capture_dropped),
                video_worker_loop_ms_p99: sent.video_worker_loop_us_p99 as f64 / 1000.0,
                packetize_send_ms_p95: self.packetize_send_ms_p95,
                packetize_send_ms_p99: self.packetize_send_ms_p99,
                pacing_late_us_p95: sent.pacing_late_us_p95,
                pacing_late_us_p99: sent.pacing_late_us_p99,
                send_syscall_ms_p95: sent.video_send_syscall_us_p95 as f64 / 1000.0,
                actual_mbps: stats.mbps,
                target_mbps: stats.target_bitrate_mbps,
                repair_packets_resent_delta: repair_packets_delta,
                send_errors_delta: sent.send_errors.saturating_sub(self.previous_send_errors),
                receiver_active_fps: f64::from(feedback.active_render_fps),
                receiver_decoder_input_fps: f64::from(feedback.decoder_input_fps),
                decode_queue_drops_delta: feedback.decode_queue_drops_delta,
                render_replacements_delta: feedback.render_replacements_delta,
                repair_deadline_missed_delta: feedback.repair_deadline_missed_delta,
                damaged_gop_delta: feedback.damaged_gop_delta,
                packets_lost_delta: feedback.packets_lost_delta,
                present_fps_measured: f64::from(feedback.present_fps_measured),
                present_interval_p95_ms: f64::from(feedback.present_interval_p95_ms),
                feedback_fresh: feedback_is_fresh,
                feedback_sample_eligible,
                profile_transition_active: self.profile_transition_active,
                audio_queue_dropping: false,
            };
            if self.pending_action.is_none() {
                let action = if !feedback_sample_eligible {
                    self.adaptive_controller.observe_ineligible_feedback(
                        self.profile_transition_active,
                        feedback_ineligible_reason.unwrap_or("feedback-sample-ineligible"),
                    );
                    None
                } else {
                    let hard_fps_cap = matches!(
                        self.frame_rate_decision.limit_source.as_str(),
                        "source-display" | "receiver-display" | "receiver-safe-default" | "user"
                    );
                    (if hard_fps_cap {
                        self.adaptive_controller
                            .enforce_fps_cap(elapsed, self.frame_rate_decision.nominal_target_fps)
                    } else {
                        None
                    })
                    .or_else(|| {
                        self.adaptive_controller.observe_windowed(
                            elapsed,
                            snapshot,
                            self.adaptive_window_metrics.window_ready,
                        )
                    })
                };
                if let Some(action) = action {
                    self.pending_profile = Some(self.adaptive_controller.current());
                    self.pending_action = Some(action);
                }
            }
            self.frame_rate_decision.effective_target_fps = self.adaptive_controller.current().fps;
            self.previous_encode_lag_skips = stats.encode_lag_skips;
            self.previous_capture_dropped = stats.capture_dropped;
            self.previous_send_errors = sent.send_errors;
            Ok(())
        }

        fn record_profile_transition_failure(
            &mut self,
            stage: &str,
            reason: &str,
            timed_out: bool,
        ) {
            self.profile_transition_failures
                .record(stage, reason, timed_out);
            self.clear_profile_transition_grace();
        }

        fn clear_profile_transition_grace(&self) {
            if let Ok(mut feedback) = self.capability_feedback.lock() {
                feedback.clear_transition_grace();
            }
        }

        fn abort_profile_transition(
            &mut self,
            transition: &mut crate::profile_transition::SenderProfileTransition,
            stage: &str,
            reason: &str,
            timed_out: bool,
            rollback: bool,
        ) {
            transition.cancel(reason);
            if rollback {
                self.profile_transition_rollback_count =
                    self.profile_transition_rollback_count.saturating_add(1);
            }
            self.record_profile_transition_failure(stage, reason, timed_out);
            self.profile_transition_active = false;
            self.profile_transition = Some(transition.clone());
        }

        fn fail_active_profile_transition(&mut self, reason: &str) {
            if !self.profile_transition_active {
                return;
            }
            if let Some(transition) = self.profile_transition.as_mut() {
                transition.cancel(reason);
            }
            self.profile_transition_active = false;
            let controlled_stop = reason.contains("console")
                || reason.contains("duration")
                || reason.contains("cancelled");
            if controlled_stop {
                self.profile_transition_failures.last_failure_reason = Some(reason.to_string());
                self.profile_transition_failures.last_failure_stage = Some("cancelled".to_string());
                self.clear_profile_transition_grace();
            } else {
                let stage = self
                    .profile_transition
                    .as_ref()
                    .map(|transition| transition.phase.name())
                    .unwrap_or("unknown");
                self.record_profile_transition_failure(stage, reason, reason.contains("timeout"));
            }
            self.feedback_sample_eligible = false;
            self.feedback_ineligible_reason = Some("profile-transition-failed".to_string());
            self.adaptive_controller
                .finish_profile_transition(self.runtime_started.elapsed());
        }

        fn profile_transition_metrics_fragment(
            &self,
            sent: &SendCounters,
            qsv_async_wait_timeouts: u64,
            qsv_async_wait_cancelled: u64,
            qsv_drain_timeouts: u64,
        ) -> String {
            let now = Instant::now();
            let transition = self.profile_transition.as_ref();
            let phase = transition
                .map(|transition| transition.phase.name())
                .unwrap_or("idle");
            let started_us = transition.map(|transition| {
                transition
                    .started_at
                    .saturating_duration_since(self.runtime_started)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64
            });
            let elapsed_ms = transition.map(|transition| {
                now.saturating_duration_since(transition.started_at)
                    .as_secs_f64()
                    * 1_000.0
            });
            let deadline = transition.and_then(|transition| match transition.phase {
                crate::profile_transition::SenderTransitionPhase::ControlPending
                | crate::profile_transition::SenderTransitionPhase::AwaitControlAck => {
                    Some(transition.ack_deadline)
                }
                crate::profile_transition::SenderTransitionPhase::AwaitReceiverReadiness => {
                    transition.readiness_deadline
                }
                crate::profile_transition::SenderTransitionPhase::Committed
                | crate::profile_transition::SenderTransitionPhase::Rollback
                | crate::profile_transition::SenderTransitionPhase::Failed
                | crate::profile_transition::SenderTransitionPhase::Idle => None,
                _ => Some(transition.total_deadline),
            });
            let deadline_remaining_ms = deadline.map(|deadline| {
                deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            });
            let ack_rtt_avg = if self.mprf_ack_rtt_samples == 0 {
                0.0
            } else {
                self.mprf_ack_rtt_ms_total / self.mprf_ack_rtt_samples as f64
            };
            format!(
                concat!(
                    r#""profile_transition_phase":"{}","profile_transition_active":{},"#,
                    r#""profile_transition_change_sequence":{},"profile_transition_generation":{},"#,
                    r#""profile_transition_old_session_id":{},"profile_transition_new_session_id":{},"#,
                    r#""profile_transition_started_us":{},"profile_transition_elapsed_ms":{},"#,
                    r#""profile_transition_deadline_remaining_ms":{},"profile_transition_timeout_count":{},"#,
                    r#""profile_transition_rollback_count":{},"profile_transition_failure_count":{},"#,
                    r#""profile_transition_last_failure_reason":{},"profile_transition_failure_stage":{},"mprf_packets_sent":{},"#,
                    r#""mprf_retry_attempts":{},"mprf_ack_packets_received":{},"#,
                    r#""mprf_ack_invalid":{},"mprf_ack_timeout":{},"#,
                    r#""mprf_ack_rtt_ms_avg":{:.3},"mprf_ack_rtt_ms_max":{:.3},"#,
                    r#""new_pipeline_prepare_success":{},"new_pipeline_prepare_error":{},"#,
                    r#""new_pipeline_prepare_last_error":{},"new_pipeline_first_idr_ready":{},"#,
                    r#""qsv_async_wait_timeouts":{},"qsv_async_wait_cancelled":{},"#,
                    r#""qsv_drain_timeouts":{}"#
                ),
                phase,
                self.profile_transition_active,
                transition
                    .map(|transition| transition.change.change_sequence)
                    .unwrap_or(0),
                transition
                    .map(|transition| transition.change.profile_generation)
                    .unwrap_or(0),
                crate::media_clock::optional_u64_json(
                    transition.map(|transition| transition.change.old_session_id)
                ),
                crate::media_clock::optional_u64_json(
                    transition.map(|transition| transition.change.new_session_id)
                ),
                crate::media_clock::optional_u64_json(started_us),
                optional_f64_json(elapsed_ms),
                crate::media_clock::optional_u64_json(deadline_remaining_ms),
                self.profile_transition_failures.timeout_count,
                self.profile_transition_rollback_count,
                self.profile_transition_failures.failure_count,
                optional_json_string(
                    self.profile_transition_failures
                        .last_failure_reason
                        .as_deref(),
                ),
                optional_json_string(
                    self.profile_transition_failures
                        .last_failure_stage
                        .as_deref(),
                ),
                self.mprf_packets_sent,
                self.mprf_retry_attempts,
                sent.mprf_ack_packets_received,
                sent.mprf_ack_invalid,
                self.mprf_ack_timeout,
                ack_rtt_avg,
                self.mprf_ack_rtt_ms_max,
                self.new_pipeline_prepare_success,
                self.new_pipeline_prepare_error,
                optional_json_string(self.new_pipeline_prepare_last_error.as_deref()),
                self.new_pipeline_first_idr_ready,
                qsv_async_wait_timeouts,
                qsv_async_wait_cancelled,
                qsv_drain_timeouts,
            )
        }

        fn adaptive_metrics_fragment(
            &mut self,
            capture_fps: f64,
            encode_fps: f64,
            sent: &SendCounters,
            qsv_async_wait_timeouts: u64,
            qsv_async_wait_cancelled: u64,
            qsv_drain_timeouts: u64,
        ) -> String {
            let now = Instant::now();
            let (feedback, tracker_stats) = self
                .capability_feedback
                .lock()
                .map(|mut tracker| (tracker.latest_fresh(now), tracker.stats()))
                .unwrap_or_default();
            let feedback_fresh = feedback.is_some();
            let feedback = feedback.unwrap_or_default();
            let telemetry = self
                .adaptive_controller
                .telemetry(self.runtime_started.elapsed());
            let capture_ratio = if self.current_target_fps == 0 {
                0.0
            } else {
                capture_fps / f64::from(self.current_target_fps)
            };
            let encode_ratio = if self.current_target_fps == 0 {
                0.0
            } else {
                encode_fps / f64::from(self.current_target_fps)
            };
            format!(
                concat!(
                    r#"{},"receiver_display_generation":{},"receiver_refresh_num":{},"#,
                    r#""receiver_refresh_den":{},"receiver_refresh_hz":{:.3},"#,
                    r#""receiver_present_fps":{:.3},"receiver_present_interval_p95_ms":{:.3},"#,
                    r#""sender_capability_feedback_received":{},"sender_capability_feedback_invalid":{},"#,
                    r#""sender_capability_feedback_stale":{},"receiver_capability_stale":{},"#,
                    r#""feedback_sample_eligible":{},"feedback_ineligible_reason":{},"#,
                    r#""receiver_valid_feedback_windows":{},"receiver_render_ready":{},"#,
                    r#""receiver_profile_settled":{},"receiver_feedback_profile_generation":{},"#,
                    r#"{},{},"frame_budget_ms":{:.3},"capture_fps_ratio":{:.4},"#,
                    r#""encode_fps_ratio":{:.4},"sender_fps_ratio":{:.4},"#,
                    r#""receiver_fps_ratio":{:.4},"profile_change_sequence":{},"#,
                    r#""old_video_session_id":{},"new_video_session_id":{},"#,
                    r#""profile_change_started_us":{},"profile_change_completed_us":{},"#,
                    r#""profile_change_duration_ms":{:.3},"profile_change_idr_frame_id":{},"#,
                    r#""encoder_reconfigure_success":{},"#,
                    r#""encoder_reconfigure_error":{},"bitrate_update_requested_mbps":{},"#,
                    r#""bitrate_update_applied_mbps":{},"bitrate_update_method":"{}","#,
                    r#""bitrate_update_success":{},"bitrate_update_error":{},"profile_change_reason":{},"#,
                    r#""bitrate_reconfigure_requested":{},"bitrate_reconfigure_success":{},"#,
                    r#""bitrate_reconfigure_latency_ms":{:.3},"old_bitrate_bps":{},"new_bitrate_bps":{},"#,
                    r#""bitrate_reconfigure_idr_requested":{},"fallback_to_full_transition":{},"#,
                    r#""receiver_profile_acknowledged":{},"receiver_first_idr_decoded":{},"#,
                    r#""receiver_first_frame_rendered":{},"transition_feedback_ignored":{},"#,
                    r#""transition_settle_windows":{},"transition_settle_duration_ms":{:.3},"#,
                    r#""display_refresh_detect":"{}","max_fps_requested":"{}","#,
                    r#"{}"#
                ),
                self.source_display.json_fragment("source"),
                feedback.display_generation,
                feedback.display_refresh_numerator,
                feedback.display_refresh_denominator,
                if feedback.display_refresh_denominator == 0 {
                    0.0
                } else {
                    f64::from(feedback.display_refresh_numerator)
                        / f64::from(feedback.display_refresh_denominator)
                },
                feedback.present_fps_measured,
                feedback.present_interval_p95_ms,
                tracker_stats.received,
                tracker_stats.invalid,
                tracker_stats.stale_events,
                !feedback_fresh,
                self.feedback_sample_eligible,
                optional_json_string(self.feedback_ineligible_reason.as_deref()),
                self.receiver_valid_feedback_windows,
                self.receiver_render_ready,
                self.receiver_profile_settled,
                feedback.profile_generation,
                self.frame_rate_decision.json_fragment(),
                format!(
                    "{},{}",
                    telemetry.json_fragment(),
                    self.adaptive_window_metrics.json_fragment()
                ),
                1000.0 / f64::from(self.current_target_fps.max(1)),
                capture_ratio,
                encode_ratio,
                encode_ratio,
                if self.current_target_fps == 0 {
                    0.0
                } else {
                    f64::from(feedback.active_render_fps) / f64::from(self.current_target_fps)
                },
                self.profile_change_sequence,
                crate::media_clock::optional_u64_json(self.old_video_session_id),
                self.session_id,
                crate::media_clock::optional_u64_json(self.profile_change_started_us),
                crate::media_clock::optional_u64_json(self.profile_change_completed_us),
                self.profile_change_duration_ms,
                crate::media_clock::optional_u64_json(self.profile_change_idr_frame_id),
                self.encoder_reconfigure_success,
                optional_json_string(self.encoder_reconfigure_error.as_deref()),
                optional_f64_json(self.bitrate_update_requested_mbps),
                optional_f64_json(self.bitrate_update_applied_mbps),
                json_escape(&self.bitrate_update_method),
                optional_bool_json(self.bitrate_update_success),
                optional_json_string(self.bitrate_update_error.as_deref()),
                optional_json_string(self.profile_change_reason.as_deref()),
                self.bitrate_update_requested_mbps.is_some(),
                optional_bool_json(self.bitrate_update_success),
                self.bitrate_reconfigure_latency_ms,
                self.bitrate_reconfigure_old_mbps.mul_add(1_000_000.0, 0.0) as u64,
                self.bitrate_update_requested_mbps
                    .unwrap_or(self.adaptive_controller.current().bitrate_mbps)
                    .mul_add(1_000_000.0, 0.0) as u64,
                self.bitrate_reconfigure_idr_requested,
                self.bitrate_fallback_to_full_transition,
                self.receiver_profile_acknowledged,
                self.receiver_first_idr_decoded,
                self.receiver_first_frame_rendered,
                self.transition_feedback_ignored,
                self.transition_settle_windows,
                self.transition_settle_duration_ms,
                self.adaptive_runtime.display_refresh_detect.name(),
                self.adaptive_runtime.max_fps.name(),
                self.profile_transition_metrics_fragment(
                    sent,
                    qsv_async_wait_timeouts,
                    qsv_async_wait_cancelled,
                    qsv_drain_timeouts,
                ),
            )
        }

        fn print_started(&mut self, started: &CapturePipelineStarted) {
            if self.mode != H264SendMode::Screen || self.started_event_emitted {
                return;
            }
            let event_context = self.event_context.json_fragment(
                "sender",
                crate::STREAM_VIDEO,
                Some(self.session_id),
                self.video_profile_generation,
                "session_total",
                self.shutdown_coordinator.state().name(),
                &self.cancellation,
            );
            println!(
                r#"{{"type":"NATIVE_SCREEN_STARTED","role":"sender","mode":"screen-send","host":"{}","port":{},"width":{},"height":{},"fps":{},"bitrate_mbps":{:.3},{},{},{},{},{},{},{}}}"#,
                json_escape(&self.host),
                self.port,
                started.width,
                started.height,
                started.target_fps,
                started.bitrate_mbps,
                started.bitrate_selection.json_fragment(None),
                started.conversion_selection.json_fragment(),
                started.encoder_selection.json_fragment(),
                started.color_spec.json_fragment(),
                started
                    .encoder_input_color_metadata
                    .json_fragment("encoder_input"),
                started
                    .encoder_output_color_metadata
                    .json_fragment("encoder_output"),
                event_context,
            );
            io::stdout().flush().ok();
            self.started_event_emitted = true;
        }

        fn final_sender_summary_fragment(
            &self,
            final_frames_captured: u64,
            final_frames_encoded: u64,
            sent: &SendCounters,
            last_error: Option<&str>,
        ) -> String {
            let pending_session_id = self
                .profile_transition
                .as_ref()
                .map(|transition| transition.change.new_session_id);
            format!(
                r#""final_frames_captured":{},"final_frames_encoded":{},"final_frames_sent":{},"final_bytes_sent":{},"final_repair_bytes":{},"final_profile":"{}","active_video_session_id":{},"pending_video_session_id":{},"last_error":{}"#,
                final_frames_captured,
                final_frames_encoded,
                sent.frames_sent,
                sent.bytes_sent,
                sent.repair_send_bytes,
                self.adaptive_controller.current_profile_id().name(),
                self.session_id,
                crate::media_clock::optional_u64_json(pending_session_id),
                optional_json_string(last_error),
            )
        }

        fn print_done(
            &mut self,
            done: CapturePipelineDone,
            reason: crate::shutdown::StopReason,
            shutdown: &WorkerShutdownSummary,
        ) {
            let sent = self.send_snapshot();
            let rates = super::summarize_run_rates(
                sent.frames_sent,
                sent.bytes_sent,
                self.runtime_started.elapsed().as_secs_f64(),
                self.current_profile_frames_start,
                self.current_profile_bytes_start,
                self.current_profile_started_at.elapsed().as_secs_f64(),
            );
            let adaptive_metrics = format!(
                "{},{},{}",
                self.adaptive_metrics_fragment(
                    done.processing_fps,
                    done.processing_fps,
                    &sent,
                    done.encoder.async_wait_timeouts,
                    done.encoder.async_wait_cancelled,
                    done.encoder.async_drain_timeouts,
                ),
                shutdown.json_fragment(),
                self.final_sender_summary_fragment(
                    done.capture_raw_frames,
                    done.frames_encoded,
                    &sent,
                    None,
                ),
            );
            let event_context = self.event_context.json_fragment(
                "sender",
                crate::STREAM_VIDEO,
                Some(self.session_id),
                self.video_profile_generation,
                "run_total",
                "stopped",
                &self.cancellation,
            );
            match self.mode {
                H264SendMode::Probe => {
                    println!(
                        r#"{{"type":"H264_SEND_DONE","mode":"h264_send_probe","encoder":"{}","target":"{}","session_id":{},"packets_sent":{},"frames_sent":{},"bytes_sent":{},"h264_bytes":{},"keyframes":{},"config_frames":{},"capture_raw_frames":{},"capture_latest_updates":{},"capture_callback_skipped":{},"capture_dropped":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"samples_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"media_duration_sec":{:.3},"fps":{:.2},"mbps":{:.3},"current_profile_duration_sec":{:.3},"current_profile_frames_sent":{},"current_profile_fps":{:.2},"current_profile_mbps":{:.3},"target_bitrate_mbps":{:.3},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},{},{},{},{},{},{},{},{}}}"#,
                        json_escape(&done.encoder_selection.selected_name),
                        json_escape(&self.target),
                        self.session_id,
                        sent.packets_sent,
                        sent.frames_sent,
                        sent.bytes_sent,
                        self.h264_bytes,
                        self.keyframes,
                        self.config_frames,
                        done.capture_raw_frames,
                        done.capture_latest_updates,
                        done.capture_callback_skipped,
                        done.capture_dropped,
                        done.encode_ticks,
                        done.no_new_frame_skipped,
                        done.no_new_frame_reused,
                        done.frames_encoded,
                        done.encode_lag_skips,
                        done.encoder.samples_out,
                        rates.global_duration_sec,
                        rates.global_duration_sec,
                        done.media_duration_sec,
                        rates.global_fps,
                        rates.global_mbps,
                        rates.current_profile_duration_sec,
                        rates.current_profile_frames_sent,
                        rates.current_profile_fps,
                        rates.current_profile_mbps,
                        done.bitrate_mbps,
                        done.width,
                        done.height,
                        done.copy_ms_avg,
                        done.convert_ms_avg,
                        done.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        done.bitrate_selection
                            .json_fragment(Some(rates.global_mbps)),
                        conversion_metrics_fragment(
                            &done.conversion_selection,
                            done.gpu_convert_ms_avg,
                            done.cpu_convert_ms_avg,
                        ),
                        done.encoder_selection.json_fragment(),
                        done.color_spec.json_fragment(),
                        done.encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        done.encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(
                            self,
                            &sent,
                            sent.packets_sent as f64 / rates.global_duration_sec,
                            sent.repair_packets_resent as f64 / rates.global_duration_sec,
                            None,
                        ),
                        adaptive_metrics
                    );
                }
                H264SendMode::Screen => {
                    println!(
                        r#"{{"type":"NATIVE_SCREEN_STOPPED","role":"sender","mode":"screen-send","reason":"{}","host":"{}","port":{},"frames_sent":{},"packets_sent":{},"bytes_sent":{},"duration_sec":{:.3},"fps":{:.2},"mbps":{:.3},"current_profile_duration_sec":{:.3},"current_profile_frames_sent":{},"current_profile_fps":{:.2},"current_profile_mbps":{:.3},"target_bitrate_mbps":{:.3},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},{},{},{},{},{},{},{},{},{}}}"#,
                        reason.name(),
                        json_escape(&self.host),
                        self.port,
                        sent.frames_sent,
                        sent.packets_sent,
                        sent.bytes_sent,
                        rates.global_duration_sec,
                        rates.global_fps,
                        rates.global_mbps,
                        rates.current_profile_duration_sec,
                        rates.current_profile_frames_sent,
                        rates.current_profile_fps,
                        rates.current_profile_mbps,
                        done.bitrate_mbps,
                        done.width,
                        done.height,
                        done.copy_ms_avg,
                        done.convert_ms_avg,
                        done.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        done.bitrate_selection
                            .json_fragment(Some(rates.global_mbps)),
                        conversion_metrics_fragment(
                            &done.conversion_selection,
                            done.gpu_convert_ms_avg,
                            done.cpu_convert_ms_avg,
                        ),
                        done.encoder_selection.json_fragment(),
                        done.color_spec.json_fragment(),
                        done.encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        done.encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(
                            self,
                            &sent,
                            sent.packets_sent as f64 / rates.global_duration_sec,
                            sent.repair_packets_resent as f64 / rates.global_duration_sec,
                            None,
                        ),
                        adaptive_metrics,
                        event_context,
                    );
                }
            }
            io::stdout().flush().ok();
        }

        fn print_failure(
            &mut self,
            error: &str,
            reason: crate::shutdown::StopReason,
            shutdown: &WorkerShutdownSummary,
        ) {
            if self.mode != H264SendMode::Screen {
                return;
            }
            let sent = self.send_snapshot();
            let elapsed = self.runtime_started.elapsed().as_secs_f64().max(0.001);
            let stats = self.last_pipeline_stats.clone();
            let capture_fps = stats.as_ref().map_or(0.0, |stats| stats.accepted_fps);
            let encode_fps = stats.as_ref().map_or(0.0, |stats| stats.encode_fps);
            let qsv_wait_timeouts = stats
                .as_ref()
                .map_or(0, |stats| stats.qsv_async_wait_timeouts)
                .max(self.terminal_encoder_stats.async_wait_timeouts);
            let qsv_wait_cancelled = stats
                .as_ref()
                .map_or(0, |stats| stats.qsv_async_wait_cancelled)
                .max(self.terminal_encoder_stats.async_wait_cancelled);
            let qsv_drain_timeouts = stats
                .as_ref()
                .map_or(0, |stats| stats.qsv_drain_timeouts)
                .max(self.terminal_encoder_stats.async_drain_timeouts);
            let expected_shutdown_cancellation =
                is_expected_mft_shutdown_cancellation(reason, error);
            let adaptive_metrics = format!(
                "{},{},{}",
                self.adaptive_metrics_fragment(
                    capture_fps,
                    encode_fps,
                    &sent,
                    qsv_wait_timeouts,
                    qsv_wait_cancelled,
                    qsv_drain_timeouts,
                ),
                shutdown.json_fragment(),
                self.final_sender_summary_fragment(
                    stats.as_ref().map_or(0, |stats| stats.capture_raw_frames),
                    stats.as_ref().map_or(0, |stats| stats.frames_encoded),
                    &sent,
                    (!expected_shutdown_cancellation).then_some(error),
                ),
            );
            let transport_metrics = transport_metrics_fragment(
                self,
                &sent,
                sent.packets_sent as f64 / elapsed,
                sent.repair_packets_resent as f64 / elapsed,
                None,
            );
            let (width, height, bitrate) = self
                .last_pipeline_started
                .as_ref()
                .map_or((0, 0, 0.0), |started| {
                    (started.width, started.height, started.bitrate_mbps)
                });
            if !expected_shutdown_cancellation {
                let error_context = self.event_context.json_fragment(
                    "sender",
                    crate::STREAM_VIDEO,
                    Some(self.session_id),
                    self.video_profile_generation,
                    "run_total",
                    "stopping",
                    &self.cancellation,
                );
                println!(
                    r#"{{"type":"NATIVE_SCREEN_ERROR","role":"sender","mode":"screen-send","error":"{}","frames_sent":{},"packets_sent":{},"bytes_sent":{},"duration_sec":{:.3},{},{},{}}}"#,
                    json_escape(error),
                    sent.frames_sent,
                    sent.packets_sent,
                    sent.bytes_sent,
                    elapsed,
                    transport_metrics,
                    adaptive_metrics,
                    error_context,
                );
            }
            let final_event_type = crate::shutdown::terminal_event_type(shutdown.clean());
            let final_state = crate::shutdown::terminal_lifecycle_name(shutdown.clean());
            let stopped_context = self.event_context.json_fragment(
                "sender",
                crate::STREAM_VIDEO,
                Some(self.session_id),
                self.video_profile_generation,
                "run_total",
                final_state,
                &self.cancellation,
            );
            println!(
                r#"{{"type":"{}","role":"sender","mode":"screen-send","reason":"{}","host":"{}","port":{},"frames_sent":{},"packets_sent":{},"bytes_sent":{},"duration_sec":{:.3},"fps":{:.2},"mbps":{:.3},"target_bitrate_mbps":{:.3},"width":{},"height":{},{},{},{}}}"#,
                final_event_type,
                reason.name(),
                json_escape(&self.host),
                self.port,
                sent.frames_sent,
                sent.packets_sent,
                sent.bytes_sent,
                elapsed,
                sent.frames_sent as f64 / elapsed,
                sent.bytes_sent as f64 * 8.0 / elapsed / 1_000_000.0,
                bitrate,
                width,
                height,
                transport_metrics,
                adaptive_metrics,
                stopped_context,
            );
            io::stdout().flush().ok();
        }
    }

    impl Drop for UdpObserver {
        fn drop(&mut self) {
            if self.worker.is_none() && self.repair_worker.is_none() {
                return;
            }
            self.cancellation
                .cancel(crate::shutdown::StopReason::FatalError);
            self.sender.take();
            self.repair_stop.store(true, Ordering::SeqCst);
            let audio_clean = self
                .audio_sender
                .as_mut()
                .is_none_or(|audio| audio.stop_and_join().is_ok());
            let pacer = crate::shutdown::try_join_until(
                &mut self.worker,
                Instant::now() + Duration::from_secs(1),
            );
            if pacer == crate::shutdown::WorkerJoinStatus::TimedOut {
                crate::shutdown::retain_unjoined_worker("udp-pacer-drop", &mut self.worker);
            }
            let control = crate::shutdown::try_join_until(
                &mut self.repair_worker,
                Instant::now() + Duration::from_secs(1),
            );
            if control == crate::shutdown::WorkerJoinStatus::TimedOut {
                crate::shutdown::retain_unjoined_worker(
                    "media-control-repair-drop",
                    &mut self.repair_worker,
                );
            }
            let all_clean = audio_clean
                && pacer.clean()
                && control.clean()
                && crate::shutdown::retained_worker_count() == 0;
            self.shutdown_coordinator.finish_cleanup(all_clean);
        }
    }

    impl CaptureEncodeObserver for UdpObserver {
        fn on_started(&mut self, started: &CapturePipelineStarted) -> Result<(), String> {
            self.last_pipeline_started = Some(started.clone());
            self.keyframe_control_configured = started.keyframe_interval_applied;
            self.keyframe_interval_target_frames = started
                .keyframe_interval_target_frames
                .unwrap_or(self.keyframe_interval_target_frames);
            self.keyframe_force_supported = started.keyframe_force_supported;
            self.keyframe_config_method = started.keyframe_control.config_method.clone();
            self.keyframe_config_applied = started.keyframe_control.config_applied;
            self.keyframe_config_error = started.keyframe_control.config_error.clone();
            if self.profile_change_sequence > 0 {
                self.encoder_reconfigure_success = true;
                self.encoder_reconfigure_error = None;
                if self.bitrate_update_method == "encoder-rebuild" {
                    self.bitrate_update_applied_mbps = self.bitrate_update_requested_mbps;
                    self.bitrate_update_success = Some(true);
                }
            }
            self.print_started(started);
            Ok(())
        }

        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String> {
            let packetize_started = Instant::now();
            let mut flags = 0;
            let mut encoded_bytes = sample.bytes;
            if sample.keyframe == Some(true) {
                self.keyframes += 1;
                if self.profile_transition_active {
                    self.new_pipeline_first_idr_ready = true;
                }
            }
            let summary = summarize_nals(&encoded_bytes);
            let is_idr = summary.has_idr_slice;
            self.parameter_sets.update_from(&encoded_bytes);
            if is_idr {
                if let Some(last_idr) = self.last_idr_frame_id {
                    self.idr_interval_frames_total += self.next_frame_id.saturating_sub(last_idr);
                    self.idr_interval_count += 1;
                }
                self.last_idr_frame_id = Some(self.next_frame_id);
                self.idr_frames += 1;
                if !summary.has_sps || !summary.has_pps {
                    self.sps_pps_repeated += 1;
                }
                encoded_bytes = self
                    .parameter_sets
                    .prepend_missing_to_keyframe(&encoded_bytes)?;
                flags |= FLAG_KEYFRAME;
            }
            let nal_types = nal_types(&encoded_bytes);
            if !nal_types.is_empty() {
                flags |= FLAG_H264_ANNEX_B;
            }
            let (has_sps, has_pps) = parameter_set_presence(&encoded_bytes);
            if has_sps || has_pps {
                flags |= FLAG_CONFIG;
                self.config_frames += 1;
            }

            let video_timestamp_us = self
                .media_clock
                .as_ref()
                .map_or_else(|| now_millis().saturating_mul(1000), MediaClock::now_us);
            if self.media_clock.is_some() {
                self.first_media_timestamp_us
                    .get_or_insert(video_timestamp_us);
                self.last_video_timestamp_us = Some(video_timestamp_us);
            }
            let packetized = packetize_frame(
                self.session_id,
                self.next_frame_id,
                video_timestamp_us / 1000,
                &encoded_bytes,
                flags,
                self.fec_mode,
                self.udp_payload_size,
            )?;
            if let Some(error) = self.send_snapshot().error {
                return Err(error);
            }
            self.sender
                .as_ref()
                .ok_or_else(|| "UDP pacing worker is stopped".to_string())?
                .send(PacerCommand::Frame(PacedFrame {
                    packets: packetized.packets,
                    data_packet_count: packetized.data_packet_count,
                    fec_packet_count: packetized.fec_packet_count,
                    fec_bytes: packetized.fec_bytes,
                    frame_interval: Duration::from_nanos(
                        1_000_000_000u64 / u64::from(self.current_target_fps.max(1)),
                    ),
                }))
                .map_err(|_| "UDP pacing worker stopped before frame enqueue".to_string())?;
            if is_idr
                && self.profile_change_sequence > 0
                && self.profile_change_idr_frame_id.is_none()
            {
                self.profile_change_idr_frame_id = Some(self.next_frame_id);
                self.profile_change_completed_us = Some(self.media_timestamp_us());
                self.profile_change_duration_ms = self
                    .profile_change_started_at
                    .map(|started| started.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0);
            }
            self.h264_bytes += encoded_bytes.len() as u64;
            self.next_frame_id += 1;
            let packetize_elapsed = packetize_started.elapsed();
            self.packetize_send_ms_total += packetize_elapsed.as_secs_f64() * 1000.0;
            self.packetize_latency
                .record_us(packetize_elapsed.as_micros().min(u128::from(u64::MAX)) as u64);
            Ok(())
        }

        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String> {
            self.last_pipeline_stats = Some(stats.clone());
            self.packetize_send_ms_p50 = self.packetize_latency.percentile_us(50) as f64 / 1000.0;
            self.packetize_send_ms_p95 = self.packetize_latency.percentile_us(95) as f64 / 1000.0;
            self.packetize_send_ms_p99 = self.packetize_latency.percentile_us(99) as f64 / 1000.0;
            self.packetize_latency = crate::sender_scheduling::LatencyHistogram::default();
            self.keyframe_force_requests = stats.keyframe_force_requests;
            self.keyframe_force_failures = stats.keyframe_force_failures;
            self.keyframe_force_last_requested_frame_id =
                stats.keyframe_force_last_requested_frame_id;
            self.keyframe_force_last_effective_frame_id =
                stats.keyframe_force_last_effective_frame_id;
            self.keyframe_force_latency_frames_avg = stats.keyframe_force_latency_frames_avg;
            self.keyframe_force_latency_frames_max = stats.keyframe_force_latency_frames_max;
            self.keyframe_force_request_frame_ids = stats.keyframe_force_request_frame_ids.clone();
            self.keyframe_force_effective_frame_ids =
                stats.keyframe_force_effective_frame_ids.clone();
            self.keyframe_force_latency_frames = stats.keyframe_force_latency_frames.clone();
            if self.media_clock.is_some() {
                self.update_av_delta();
            }
            let sent = self.send_snapshot();
            if let Some(error) = sent.error.clone() {
                return Err(error);
            }
            let packets_delta = sent.packets_sent.saturating_sub(self.previous_packets);
            let frames_delta = sent.frames_sent.saturating_sub(self.previous_frames);
            let bytes_delta = sent.bytes_sent.saturating_sub(self.previous_bytes);
            let repair_packets_delta = sent
                .repair_packets_resent
                .saturating_sub(self.previous_repair_packets);
            let repair_bytes_delta = sent
                .repair_send_bytes
                .saturating_sub(self.previous_repair_bytes);
            let nack_items_delta = sent
                .nack_items_received
                .saturating_sub(self.previous_nack_items);
            let duplicate_repairs_delta = sent
                .repair_duplicate_packets_resent
                .saturating_sub(self.previous_duplicate_repairs);
            self.refresh_adaptive_state(
                stats,
                &sent,
                repair_packets_delta,
                bytes_delta,
                repair_bytes_delta,
                nack_items_delta,
                duplicate_repairs_delta,
            )?;
            let adaptive_metrics = self.adaptive_metrics_fragment(
                stats.accepted_fps,
                stats.encode_fps,
                &sent,
                stats.qsv_async_wait_timeouts,
                stats.qsv_async_wait_cancelled,
                stats.qsv_drain_timeouts,
            );
            let event_context = self.event_context.json_fragment(
                "sender",
                crate::STREAM_VIDEO,
                Some(self.session_id),
                self.video_profile_generation,
                "interval",
                self.shutdown_coordinator.state().name(),
                &self.cancellation,
            );
            match self.mode {
                H264SendMode::Probe => {
                    println!(
                        r#"{{"type":"H264_SEND_STATS","mode":"h264_send_probe","encoder":"{}","target":"{}","session_id":{},"packets_sent":{},"frames_sent":{},"bytes_sent":{},"packets_per_sec":{},"fps":{},"mbps":{:.3},"target_bitrate_mbps":{:.3},"capture_raw_frames":{},"capture_latest_updates":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"raw_fps":{:.2},"accepted_fps":{:.2},"encode_fps":{:.2},"target_fps":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},"capture_dropped":{},{},{},{},{},{},{},{},{}}}"#,
                        json_escape(&stats.encoder_selection.selected_name),
                        json_escape(&self.target),
                        self.session_id,
                        sent.packets_sent,
                        sent.frames_sent,
                        sent.bytes_sent,
                        packets_delta,
                        frames_delta,
                        bytes_delta as f64 * 8.0 / 1_000_000.0,
                        stats.target_bitrate_mbps,
                        stats.capture_raw_frames,
                        stats.capture_latest_updates,
                        stats.encode_ticks,
                        stats.no_new_frame_skipped,
                        stats.no_new_frame_reused,
                        stats.frames_encoded,
                        stats.encode_lag_skips,
                        stats.raw_fps,
                        stats.accepted_fps,
                        stats.encode_fps,
                        stats.target_fps,
                        stats.width,
                        stats.height,
                        stats.copy_ms_avg,
                        stats.convert_ms_avg,
                        stats.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        stats.capture_dropped,
                        stats
                            .bitrate_selection
                            .json_fragment(Some(bytes_delta as f64 * 8.0 / 1_000_000.0)),
                        conversion_metrics_fragment(
                            &stats.conversion_selection,
                            stats.gpu_convert_ms_avg,
                            stats.cpu_convert_ms_avg,
                        ),
                        stats.encoder_selection.json_fragment(),
                        stats.color_spec.json_fragment(),
                        stats
                            .encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        stats
                            .encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(
                            self,
                            &sent,
                            packets_delta as f64,
                            repair_packets_delta as f64,
                            Some(if repair_packets_delta == 0 {
                                0.0
                            } else {
                                duplicate_repairs_delta as f64 / repair_packets_delta as f64
                            }),
                        ),
                        adaptive_metrics
                    );
                }
                H264SendMode::Screen => {
                    println!(
                        r#"{{"type":"NATIVE_SCREEN_STATS","role":"sender","mode":"screen-send","host":"{}","port":{},"session_id":{},"frames_sent":{},"packets_sent":{},"bytes_sent":{},"packets_per_sec":{},"fps":{},"mbps":{:.3},"target_bitrate_mbps":{:.3},"capture_raw_frames":{},"capture_latest_updates":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"target_fps":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},"capture_dropped":{},{},{},{},{},{},{},{},{},{}}}"#,
                        json_escape(&self.host),
                        self.port,
                        self.session_id,
                        sent.frames_sent,
                        sent.packets_sent,
                        sent.bytes_sent,
                        packets_delta,
                        frames_delta,
                        bytes_delta as f64 * 8.0 / 1_000_000.0,
                        stats.target_bitrate_mbps,
                        stats.capture_raw_frames,
                        stats.capture_latest_updates,
                        stats.encode_ticks,
                        stats.no_new_frame_skipped,
                        stats.no_new_frame_reused,
                        stats.frames_encoded,
                        stats.encode_lag_skips,
                        stats.target_fps,
                        stats.width,
                        stats.height,
                        stats.copy_ms_avg,
                        stats.convert_ms_avg,
                        stats.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        stats.capture_dropped,
                        stats
                            .bitrate_selection
                            .json_fragment(Some(bytes_delta as f64 * 8.0 / 1_000_000.0)),
                        conversion_metrics_fragment(
                            &stats.conversion_selection,
                            stats.gpu_convert_ms_avg,
                            stats.cpu_convert_ms_avg,
                        ),
                        stats.encoder_selection.json_fragment(),
                        stats.color_spec.json_fragment(),
                        stats
                            .encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        stats
                            .encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(
                            self,
                            &sent,
                            packets_delta as f64,
                            repair_packets_delta as f64,
                            Some(if repair_packets_delta == 0 {
                                0.0
                            } else {
                                duplicate_repairs_delta as f64 / repair_packets_delta as f64
                            }),
                        ),
                        adaptive_metrics,
                        event_context,
                    );
                }
            }
            io::stdout().flush().ok();
            self.previous_packets = sent.packets_sent;
            self.previous_frames = sent.frames_sent;
            self.previous_bytes = sent.bytes_sent;
            self.previous_repair_packets = sent.repair_packets_resent;
            self.previous_repair_bytes = sent.repair_send_bytes;
            self.previous_nack_items = sent.nack_items_received;
            self.previous_duplicate_repairs = sent.repair_duplicate_packets_resent;
            Ok(())
        }

        fn on_encoder_terminal_stats(&mut self, stats: crate::wmf_h264_encoder::EncoderStats) {
            self.terminal_encoder_stats = stats;
        }

        fn stop_requested(&self) -> bool {
            self.cancellation.is_cancelled()
        }

        fn cancellation_token(&self) -> Option<crate::shutdown::CancellationToken> {
            Some(self.cancellation.clone())
        }

        fn take_control(&mut self) -> CapturePipelineControl {
            match self.pending_action.as_ref() {
                Some(crate::adaptive_quality::AdaptiveAction::SetBitrate {
                    bitrate_mbps, ..
                }) => {
                    self.bitrate_reconfigure_old_mbps = self
                        .last_pipeline_stats
                        .as_ref()
                        .map(|stats| stats.target_bitrate_mbps)
                        .unwrap_or(self.bitrate_reconfigure_old_mbps);
                    self.bitrate_reconfigure_started_at = Some(Instant::now());
                    CapturePipelineControl::UpdateBitrate(*bitrate_mbps)
                }
                Some(action) if super::adaptive_action_changes_video_structure(action) => {
                    CapturePipelineControl::Restart
                }
                Some(_) => CapturePipelineControl::Continue,
                None => CapturePipelineControl::Continue,
            }
        }

        fn on_bitrate_update_result(
            &mut self,
            requested_mbps: f64,
            result: &Result<(), String>,
            idr_requested: bool,
        ) -> Result<bool, String> {
            self.bitrate_update_requested_mbps = Some(requested_mbps);
            self.bitrate_reconfigure_latency_ms = self
                .bitrate_reconfigure_started_at
                .take()
                .map(|started| started.elapsed().as_secs_f64() * 1_000.0)
                .unwrap_or(0.0);
            self.bitrate_reconfigure_idr_requested = idr_requested;
            match result {
                Ok(()) => {
                    self.bitrate_update_applied_mbps = Some(requested_mbps);
                    self.bitrate_update_method = "runtime-icodecapi".to_string();
                    self.bitrate_update_success = Some(true);
                    self.bitrate_update_error = None;
                    self.encoder_reconfigure_success = true;
                    self.encoder_reconfigure_error = None;
                    self.bitrate_fallback_to_full_transition = false;
                    self.pending_action = None;
                    self.pending_profile = None;
                    self.adaptive_windows.reset(self.session_id);
                    self.adaptive_window_metrics =
                        crate::adaptive_quality::AdaptiveWindowMetrics::default();
                    Ok(false)
                }
                Err(error) => {
                    self.bitrate_update_applied_mbps = None;
                    self.bitrate_update_method = "encoder-rebuild".to_string();
                    self.bitrate_update_success = Some(false);
                    self.bitrate_update_error = Some(error.clone());
                    self.encoder_reconfigure_success = false;
                    self.encoder_reconfigure_error = Some(error.clone());
                    self.bitrate_fallback_to_full_transition = true;
                    Ok(true)
                }
            }
        }
    }

    pub fn run(config: H264SendConfig) -> Result<(), String> {
        if let Err(error) = validate_config(&config) {
            print_startup_failure(&config, &error);
            return Err(error);
        }
        let _console_ctrl = match capture_encode_probe::install_console_ctrl_guard() {
            Ok(guard) => guard,
            Err(error) => {
                print_startup_failure(&config, &error);
                return Err(error);
            }
        };
        let mut observer = match UdpObserver::new(&config) {
            Ok(observer) => observer,
            Err(error) => {
                print_startup_failure(&config, &error);
                return Err(error);
            }
        };
        let _local_control = if config.mode == H264SendMode::Screen {
            match crate::local_control::spawn_stdin_listener(observer.cancellation.clone()) {
                Ok(listener) => Some(listener),
                Err(error) => {
                    print_startup_failure(&config, &error);
                    return Err(error);
                }
            }
        } else {
            None
        };
        if config.verbose {
            eprintln!(
                "h264-send-probe target={} duration_sec={} target_fps={} bitrate_mbps={} output={}x{} encoder={} convert_backend={} packet_pacing={} keyframe_interval_sec={:.3} udp_send_buffer_bytes={} color_matrix={} range={} packet_payload_max=1200",
                observer.target,
                optional_duration_text(config.duration_sec),
                config.target_fps,
                config.bitrate_mbps,
                config.out_width,
                config.out_height,
                config.encoder.name(),
                config.convert_backend.name(),
                config.packet_pacing.name(),
                config.keyframe_interval_sec,
                observer.udp_send_buffer_bytes,
                config.color_spec.yuv_matrix(),
                config.color_spec.color_range()
            );
        }
        let overall_started = observer.runtime_started;
        let mut profile = crate::adaptive_quality::QualityProfile {
            width: config.out_width,
            height: config.out_height,
            fps: config.target_fps,
            bitrate_mbps: config.bitrate_mbps,
        };
        let mut first_pipeline = true;
        let mut prepared_pipeline = None;
        let pipeline_result = loop {
            let duration_sec = config.duration_sec.map(|total| {
                let elapsed = overall_started.elapsed().as_secs_f64();
                (total as f64 - elapsed).ceil().max(1.0) as u64
            });
            let bitrate_selection = if first_pipeline {
                config.bitrate_selection.clone()
            } else {
                match crate::bitrate::BitrateSelection::resolve(
                    profile.width,
                    profile.height,
                    profile.fps,
                    profile.bitrate_mbps,
                    Some(profile.bitrate_mbps),
                    None,
                ) {
                    Ok(selection) => selection,
                    Err(error) => break Err(error),
                }
            };
            let pipeline_config = capture_encode_probe::CaptureEncodeConfig {
                duration_sec,
                target_fps: profile.fps,
                bitrate_mbps: profile.bitrate_mbps,
                bitrate_selection,
                out_width: profile.width,
                out_height: profile.height,
                output: String::new(),
                color_spec: config.color_spec,
                encoder: config.encoder,
                convert_backend: config.convert_backend,
                keyframe_interval_sec: Some(config.keyframe_interval_sec),
                verbose: config.verbose,
            };
            let pipeline_run = if let Some(prepared) = prepared_pipeline.take() {
                capture_encode_probe::run_prepared_with_observer(
                    &pipeline_config,
                    &mut observer,
                    prepared,
                )
            } else {
                capture_encode_probe::run_with_observer(&pipeline_config, &mut observer)
            };
            let done = match pipeline_run {
                Ok(done) => done,
                Err(error) => {
                    observer.fail_active_profile_transition(&error);
                    break Err(error);
                }
            };
            let duration_complete = config
                .duration_sec
                .is_some_and(|total| overall_started.elapsed() >= Duration::from_secs(total));
            if observer.profile_transition_active
                && (!done.reconfigure_requested || duration_complete)
            {
                let reason = if done.stopped_by_console {
                    "profile-transition-console-stop"
                } else if duration_complete {
                    "profile-transition-duration-expired"
                } else {
                    "profile-transition-ended-before-readiness"
                };
                observer.fail_active_profile_transition(reason);
            }
            if !done.reconfigure_requested || duration_complete {
                break Ok(done);
            }
            let Some(action) = observer.pending_action.clone() else {
                break Err(
                    "capture pipeline requested reconfiguration without an adaptive action"
                        .to_string(),
                );
            };
            let Some(next_profile) = observer.pending_profile else {
                break Err("adaptive action has no target profile".to_string());
            };
            let next_bitrate_selection = match crate::bitrate::BitrateSelection::resolve(
                next_profile.width,
                next_profile.height,
                next_profile.fps,
                next_profile.bitrate_mbps,
                Some(next_profile.bitrate_mbps),
                None,
            ) {
                Ok(selection) => selection,
                Err(error) => break Err(error),
            };
            let next_pipeline_config = capture_encode_probe::CaptureEncodeConfig {
                duration_sec: config.duration_sec.map(|total| {
                    let elapsed = overall_started.elapsed().as_secs_f64();
                    (total as f64 - elapsed).ceil().max(1.0) as u64
                }),
                target_fps: next_profile.fps,
                bitrate_mbps: next_profile.bitrate_mbps,
                bitrate_selection: next_bitrate_selection,
                out_width: next_profile.width,
                out_height: next_profile.height,
                output: String::new(),
                color_spec: config.color_spec,
                encoder: config.encoder,
                convert_backend: config.convert_backend,
                keyframe_interval_sec: Some(config.keyframe_interval_sec),
                verbose: config.verbose,
            };
            let prepared = match capture_encode_probe::prepare_pipeline(
                &next_pipeline_config,
                Some(observer.cancellation.clone()),
            ) {
                Ok(prepared) => {
                    observer.new_pipeline_prepare_success =
                        observer.new_pipeline_prepare_success.saturating_add(1);
                    observer.new_pipeline_prepare_last_error = None;
                    prepared
                }
                Err(error) => {
                    observer.new_pipeline_prepare_error =
                        observer.new_pipeline_prepare_error.saturating_add(1);
                    observer.new_pipeline_prepare_last_error = Some(error.clone());
                    break Err(format!("prepare new profile pipeline failed: {error}"));
                }
            };
            if let Err(error) = observer.transition_profile(next_profile, &action, &prepared) {
                let _ = prepared.discard();
                if matches!(
                    error.as_str(),
                    "profile-control-cancelled" | "profile-control-duration-expired"
                ) {
                    break Ok(done);
                }
                break Err(error);
            }
            prepared_pipeline = Some(prepared);
            observer.pending_action = None;
            observer.pending_profile = None;
            profile = next_profile;
            first_pipeline = false;
        };
        let shutdown_reason = match &pipeline_result {
            Ok(done) => observer.cancellation.reason().unwrap_or_else(|| {
                if done.stopped_by_console || crate::shutdown::ctrl_c_requested() {
                    crate::shutdown::StopReason::CtrlC
                } else {
                    crate::shutdown::StopReason::Duration
                }
            }),
            Err(error) => observer
                .cancellation
                .reason()
                .unwrap_or_else(|| crate::shutdown::classify_error(error)),
        };
        observer.cancellation.cancel(shutdown_reason);
        if observer.profile_transition_active {
            observer.fail_active_profile_transition(match shutdown_reason {
                crate::shutdown::StopReason::CtrlC => "profile-transition-cancelled-by-ctrl-c",
                crate::shutdown::StopReason::PeerClosed => {
                    "profile-transition-cancelled-by-peer-close"
                }
                crate::shutdown::StopReason::PeerTimeout => {
                    "profile-transition-cancelled-by-peer-timeout"
                }
                _ => "profile-transition-cancelled-by-shutdown",
            });
        }
        let shutdown = observer.finish_worker(shutdown_reason);
        match pipeline_result {
            Ok(done) if shutdown.clean() => {
                observer.keyframe_force_requests = done.keyframe_force_requests;
                observer.keyframe_force_failures = done.keyframe_force_failures;
                observer.keyframe_force_last_requested_frame_id =
                    done.keyframe_force_last_requested_frame_id;
                observer.keyframe_force_last_effective_frame_id =
                    done.keyframe_force_last_effective_frame_id;
                observer.keyframe_force_latency_frames_avg = done.keyframe_force_latency_frames_avg;
                observer.keyframe_force_latency_frames_max = done.keyframe_force_latency_frames_max;
                observer.keyframe_force_request_frame_ids =
                    done.keyframe_force_request_frame_ids.clone();
                observer.keyframe_force_effective_frame_ids =
                    done.keyframe_force_effective_frame_ids.clone();
                observer.keyframe_force_latency_frames = done.keyframe_force_latency_frames.clone();
                let reason = observer.cancellation.reason().unwrap_or_else(|| {
                    if done.stopped_by_console {
                        crate::shutdown::StopReason::CtrlC
                    } else {
                        crate::shutdown::StopReason::Duration
                    }
                });
                if observer.profile_transition_active {
                    let transition_reason = if reason == crate::shutdown::StopReason::CtrlC {
                        "profile-transition-cancelled-by-ctrl-c"
                    } else {
                        "profile-transition-cancelled-by-duration"
                    };
                    observer.fail_active_profile_transition(transition_reason);
                }
                observer.print_done(done, reason, &shutdown);
                Ok(())
            }
            Ok(_) => {
                let error = shutdown
                    .join_error
                    .clone()
                    .or_else(|| shutdown.runtime_error.clone())
                    .unwrap_or_else(|| "worker shutdown did not complete".to_string());
                observer.fail_active_profile_transition(&error);
                observer.print_failure(
                    &error,
                    crate::shutdown::StopReason::InternalError,
                    &shutdown,
                );
                Err(error)
            }
            Err(error) => {
                let reason = observer
                    .cancellation
                    .reason()
                    .unwrap_or_else(|| crate::shutdown::classify_error(&error));
                observer.cancellation.cancel(reason);
                observer.fail_active_profile_transition(&error);
                observer.print_failure(&error, reason, &shutdown);
                if is_expected_mft_shutdown_cancellation(reason, &error) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    fn print_startup_failure(config: &H264SendConfig, error: &str) {
        if config.mode != H264SendMode::Screen {
            return;
        }
        let reason = if crate::shutdown::ctrl_c_requested() {
            crate::shutdown::StopReason::CtrlC
        } else {
            crate::shutdown::StopReason::StartupFailure
        };
        let cancellation = crate::shutdown::CancellationToken::new();
        cancellation.cancel(reason);
        let context = crate::shutdown::RuntimeEventContext::new(make_session_id());
        let error_context = context.json_fragment(
            "sender",
            crate::STREAM_VIDEO,
            None,
            0,
            "run_total",
            "failed",
            &cancellation,
        );
        println!(
            r#"{{"type":"NATIVE_SCREEN_ERROR","role":"sender","mode":"screen-send","error":"{}",{}}}"#,
            json_escape(error),
            error_context,
        );
        let cleanup_failed = crate::shutdown::worker_ownership_failed(error)
            || crate::shutdown::retained_worker_count() > 0;
        let final_event_type = crate::shutdown::terminal_event_type(!cleanup_failed);
        let final_state = crate::shutdown::terminal_lifecycle_name(!cleanup_failed);
        let stopped_context = context.json_fragment(
            "sender",
            crate::STREAM_VIDEO,
            None,
            0,
            "run_total",
            final_state,
            &cancellation,
        );
        println!(
            r#"{{"type":"{}","role":"sender","mode":"screen-send","reason":"{}","host":"{}","port":{},"frames_sent":0,"packets_sent":0,"bytes_sent":0,"duration_sec":0.0,"profile_transition_phase":"idle","profile_transition_active":false,"profile_transition_timeout_count":0,"profile_transition_failure_count":0,"profile_transition_last_failure_reason":null,"qsv_async_wait_timeouts":0,"qsv_async_wait_cancelled":0,"qsv_drain_timeouts":0,"worker_join_audio":"not_started","worker_join_pacer":"not_started","worker_join_control":"not_started","worker_join_all_clean":{},"capture_thread_state":"{}","encode_thread_state":"not_started","send_thread_state":"not_started","feedback_thread_state":"not_started","repair_thread_state":"not_started","transition_thread_state":"not_started","retained_worker_count":{},"close_sent":false,"peer_timeout_triggered":false,"final_frames_captured":0,"final_frames_encoded":0,"final_frames_sent":0,"final_bytes_sent":0,"final_repair_bytes":0,"final_profile":null,"active_video_session_id":null,"pending_video_session_id":null,"last_error":"{}","cleanup_duration_ms":0.0,{}}}"#,
            final_event_type,
            reason.name(),
            json_escape(&config.host),
            config.port,
            !cleanup_failed,
            if cleanup_failed {
                "retained_unjoined"
            } else {
                "not_started"
            },
            crate::shutdown::retained_worker_count(),
            json_escape(error),
            stopped_context,
        );
        io::stdout().flush().ok();
    }

    fn is_expected_mft_shutdown_cancellation(
        reason: crate::shutdown::StopReason,
        error: &str,
    ) -> bool {
        matches!(
            reason,
            crate::shutdown::StopReason::CtrlC
                | crate::shutdown::StopReason::WindowClosed
                | crate::shutdown::StopReason::PeerClosed
                | crate::shutdown::StopReason::PeerTimeout
                | crate::shutdown::StopReason::Duration
                | crate::shutdown::StopReason::LocalStop
        ) && crate::async_mft_wait::is_typed_cancellation_message(error)
    }

    fn validate_config(config: &H264SendConfig) -> Result<(), String> {
        if config.host.trim().is_empty() {
            return Err("host must not be empty".to_string());
        }
        if config.port == 0 {
            return Err("port must be greater than zero".to_string());
        }
        crate::validate_udp_payload_size(config.udp_payload_size)?;
        if !(500..=10_000).contains(&config.repair_cache_ms) {
            return Err("repair-cache-ms must be between 500 and 10000".to_string());
        }
        if !(500..=2000).contains(&config.adaptive.feedback_ms) {
            return Err("adaptive-feedback-ms must be between 500 and 2000".to_string());
        }
        if config.adaptive.quality.mode != crate::adaptive_quality::AdaptiveMode::Off
            && (config.adaptive.quality.min_width > config.out_width
                || config.adaptive.quality.min_height > config.out_height
                || config.adaptive.quality.min_fps > config.target_fps)
        {
            return Err(
                "adaptive minimum width, height, and FPS must not exceed the configured profile"
                    .to_string(),
            );
        }
        if config
            .adaptive
            .quality
            .max_bitrate_mbps
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err("adaptive-max-bitrate-mbps must be positive and finite".to_string());
        }
        Ok(())
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }

    fn average_ms(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn ratio_to_target(actual: f64, target: u32) -> f64 {
        if target == 0 {
            0.0
        } else {
            (actual / f64::from(target)).clamp(0.0, 1.5)
        }
    }

    fn average_ns_ms(total_ns: u64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total_ns as f64 / count as f64 / 1_000_000.0
        }
    }

    fn wait_until(target: Instant) -> f64 {
        const PACING_GRANULARITY: Duration = Duration::from_micros(500);
        let started = Instant::now();
        loop {
            let now = Instant::now();
            if now >= target {
                break;
            }
            let remaining = target.duration_since(now);
            if remaining <= PACING_GRANULARITY {
                break;
            }
            thread::sleep(remaining.saturating_sub(PACING_GRANULARITY));
        }
        started.elapsed().as_secs_f64() * 1000.0
    }

    fn batch_size_for_frame(
        packet_count: usize,
        frame_interval: Duration,
        tick: Duration,
    ) -> usize {
        if packet_count == 0 || tick.is_zero() {
            return packet_count.max(1);
        }
        let ticks = frame_interval.as_nanos().div_ceil(tick.as_nanos()).max(1);
        packet_count.div_ceil(ticks.min(usize::MAX as u128) as usize)
    }

    pub(super) fn sender_peer_timeout_due(
        send_mode: H264SendMode,
        media_active_age: Option<Duration>,
        valid_peer_activity_age: Option<Duration>,
        transition_grace_active: bool,
        hard_timeout: Duration,
    ) -> bool {
        if send_mode != H264SendMode::Screen || transition_grace_active {
            return false;
        }
        valid_peer_activity_age.map_or_else(
            || media_active_age.is_some_and(|age| age >= hard_timeout),
            |age| age >= hard_timeout,
        )
    }

    fn run_media_control_receiver(
        socket: UdpSocket,
        cache: Arc<Mutex<crate::repair::RepairCache>>,
        counters: Arc<Mutex<SendCounters>>,
        stop: Arc<AtomicBool>,
        current_session_id: Arc<AtomicU64>,
        capability_feedback: Arc<Mutex<crate::media_control::CapabilityFeedbackTracker>>,
        profile_ack_inbox: Arc<(Mutex<VecDeque<crate::media_control::ProfileAck>>, Condvar)>,
        close_ack_inbox: Arc<(
            Mutex<VecDeque<crate::media_control::StreamCloseAck>>,
            Condvar,
        )>,
        cancellation: crate::shutdown::CancellationToken,
        repair_mode: crate::repair::RepairMode,
        send_mode: H264SendMode,
    ) {
        let mut buffer = [0u8; 2048];
        let mut rate_window = Instant::now();
        let mut resent_in_window = 0u64;
        let mut resent_keys = crate::repair::PacketUniquenessTracker::default();
        let mut repair_suppression =
            crate::repair::RepairSuppression::new(Duration::from_millis(60))
                .expect("constant repair suppression interval is valid");
        let mut observed_session_id = current_session_id.load(Ordering::Acquire);
        let mut session_started_at = Instant::now();
        let mut session_frames_baseline = counters
            .lock()
            .map(|stats| stats.frames_sent)
            .unwrap_or_default();
        let mut first_media_seen_at: Option<Instant> = None;
        let mut session_media_first_sent_at: Option<Instant> = None;
        let mut peer_close_linger_until: Option<Instant> = None;
        let mut pending_peer_close_ack: Option<(Vec<u8>, Instant)> = None;
        while !stop.load(Ordering::SeqCst)
            || peer_close_linger_until.is_some_and(|deadline| Instant::now() < deadline)
        {
            let now = Instant::now();
            if let (Some(deadline), Some((ack, next_send_at))) =
                (peer_close_linger_until, pending_peer_close_ack.as_mut())
            {
                if now < deadline && now >= *next_send_at {
                    if socket.send(ack).is_ok_and(|sent| sent == ack.len()) {
                        if let Ok(mut stats) = counters.lock() {
                            stats.stream_close_ack_packets_sent =
                                stats.stream_close_ack_packets_sent.saturating_add(1);
                        }
                    }
                    *next_send_at =
                        now + crate::shutdown::ShutdownConfig::default().close_retry_initial;
                }
            }
            let current_observed_session = current_session_id.load(Ordering::Acquire);
            if current_observed_session != observed_session_id {
                observed_session_id = current_observed_session;
                session_started_at = Instant::now();
                session_frames_baseline = counters
                    .lock()
                    .map(|stats| stats.frames_sent)
                    .unwrap_or_default();
                first_media_seen_at = None;
                session_media_first_sent_at = None;
            }
            let length = match socket.recv(&mut buffer) {
                Ok(length) => length,
                Err(err) if is_retryable_control_receive_error(err.kind()) => {
                    let now = Instant::now();
                    let frames_sent = counters
                        .lock()
                        .map(|stats| stats.frames_sent)
                        .unwrap_or_default();
                    if first_media_seen_at.is_none() && frames_sent > session_frames_baseline {
                        first_media_seen_at = Some(now);
                        session_media_first_sent_at = Some(now);
                    }
                    let hard_timeout = crate::shutdown::ShutdownConfig::default().peer_hard_timeout;
                    let media_active_age = session_media_first_sent_at.map(|started| {
                        now.saturating_duration_since(started.max(session_started_at))
                    });
                    let peer_timed_out = capability_feedback.lock().ok().is_some_and(|tracker| {
                        sender_peer_timeout_due(
                            send_mode,
                            media_active_age,
                            tracker.peer_activity_age(now),
                            tracker.transition_grace_active(now),
                            hard_timeout,
                        )
                    });
                    if peer_timed_out {
                        cancellation.cancel(crate::shutdown::StopReason::PeerTimeout);
                        profile_ack_inbox.1.notify_all();
                        close_ack_inbox.1.notify_all();
                    }
                    continue;
                }
                Err(_) => break,
            };
            let session_id = current_session_id.load(Ordering::Acquire);
            if crate::media_control::is_stream_close(&buffer[..length]) {
                match crate::media_control::StreamClose::decode(&buffer[..length]) {
                    Ok(close)
                        if close.stream_id == crate::STREAM_VIDEO
                            && (close.video_session_id == 0
                                || close.video_session_id == session_id) =>
                    {
                        if let Ok(mut tracker) = capability_feedback.lock() {
                            tracker.observe_valid_peer_control(Instant::now());
                        }
                        let ack = close.ack().encode();
                        let ack_sent = ack
                            .as_ref()
                            .ok()
                            .and_then(|bytes| {
                                socket.send(bytes).ok().map(|sent| sent == bytes.len())
                            })
                            .unwrap_or(false);
                        if let Ok(mut stats) = counters.lock() {
                            stats.stream_close_packets_received =
                                stats.stream_close_packets_received.saturating_add(1);
                            if ack_sent {
                                stats.stream_close_ack_packets_sent =
                                    stats.stream_close_ack_packets_sent.saturating_add(1);
                            }
                        }
                        if let Ok(bytes) = ack {
                            pending_peer_close_ack = Some((
                                bytes,
                                Instant::now()
                                    + crate::shutdown::ShutdownConfig::default()
                                        .close_retry_initial,
                            ));
                        }
                        peer_close_linger_until.get_or_insert_with(|| {
                            Instant::now()
                                + crate::shutdown::ShutdownConfig::default().close_handshake_timeout
                        });
                        cancellation.cancel(crate::shutdown::StopReason::PeerClosed);
                        profile_ack_inbox.1.notify_all();
                        close_ack_inbox.1.notify_all();
                    }
                    _ => {
                        if let Ok(mut stats) = counters.lock() {
                            stats.stream_close_invalid =
                                stats.stream_close_invalid.saturating_add(1);
                        }
                    }
                }
                continue;
            }
            if crate::media_control::is_stream_close_ack(&buffer[..length]) {
                match crate::media_control::StreamCloseAck::decode(&buffer[..length]) {
                    Ok(ack) => {
                        if let Ok(mut tracker) = capability_feedback.lock() {
                            tracker.observe_valid_peer_control(Instant::now());
                        }
                        let (inbox, wake) = &*close_ack_inbox;
                        if let Ok(mut inbox) = inbox.lock() {
                            inbox.push_back(ack);
                            wake.notify_all();
                        }
                        if let Ok(mut stats) = counters.lock() {
                            stats.stream_close_ack_packets_received =
                                stats.stream_close_ack_packets_received.saturating_add(1);
                        }
                    }
                    Err(_) => {
                        if let Ok(mut stats) = counters.lock() {
                            stats.stream_close_invalid =
                                stats.stream_close_invalid.saturating_add(1);
                        }
                    }
                }
                continue;
            }
            if stop.load(Ordering::SeqCst) {
                continue;
            }
            if crate::media_control::is_profile_ack(&buffer[..length]) {
                match crate::media_control::ProfileAck::decode(&buffer[..length]) {
                    Ok(ack) => {
                        if let Ok(mut tracker) = capability_feedback.lock() {
                            tracker.observe_valid_peer_control(Instant::now());
                        }
                        let (inbox, wake) = &*profile_ack_inbox;
                        if let Ok(mut inbox) = inbox.lock() {
                            inbox.push_back(ack);
                            wake.notify_all();
                        }
                        if let Ok(mut counters) = counters.lock() {
                            counters.mprf_ack_packets_received =
                                counters.mprf_ack_packets_received.saturating_add(1);
                        }
                    }
                    Err(_) => {
                        if let Ok(mut counters) = counters.lock() {
                            counters.mprf_ack_invalid = counters.mprf_ack_invalid.saturating_add(1);
                        }
                    }
                }
                continue;
            }
            if crate::media_control::is_capability_feedback(&buffer[..length]) {
                let stats = capability_feedback.lock().ok().map(|mut tracker| {
                    let _ = tracker.observe(&buffer[..length], session_id, Instant::now());
                    tracker.stats()
                });
                if let (Some(tracker_stats), Ok(mut counters)) = (stats, counters.lock()) {
                    counters.capability_feedback_received = tracker_stats.received;
                    counters.capability_feedback_invalid = tracker_stats.invalid;
                    counters.capability_feedback_stale = tracker_stats.stale_events;
                }
                continue;
            }
            if repair_mode != crate::repair::RepairMode::Nack {
                continue;
            }
            let Ok(nack) = crate::repair::NackPacket::decode(&buffer[..length]) else {
                continue;
            };
            if nack.session_id != session_id {
                if let Ok(mut stats) = counters.lock() {
                    stats.repair_cache_miss_wrong_session = stats
                        .repair_cache_miss_wrong_session
                        .saturating_add(nack.items.len() as u64);
                }
                continue;
            }
            if let Ok(mut tracker) = capability_feedback.lock() {
                tracker.observe_valid_peer_control(Instant::now());
            }
            if rate_window.elapsed() >= Duration::from_secs(1) {
                rate_window = Instant::now();
                resent_in_window = 0;
            }
            if let Ok(mut stats) = counters.lock() {
                stats.nack_packets_received += 1;
                stats.nack_items_received += nack.items.len() as u64;
            }
            for item in nack.items {
                if let Ok(mut stats) = counters.lock() {
                    stats.repair_request_total = stats.repair_request_total.saturating_add(1);
                }
                if resent_in_window >= 5000 {
                    if let Ok(mut stats) = counters.lock() {
                        stats.repair_rate_limited += 1;
                    }
                    continue;
                }
                if !repair_suppression.should_send(item, Instant::now()) {
                    if let Ok(mut stats) = counters.lock() {
                        stats.repair_request_deduped =
                            stats.repair_request_deduped.saturating_add(1);
                        stats.repair_send_suppressed =
                            stats.repair_send_suppressed.saturating_add(1);
                    }
                    continue;
                }
                let (packet, expired_during_lookup) = cache
                    .lock()
                    .map(|mut cache| {
                        let evictions_before = cache.evictions();
                        let packet = cache.get(item, Instant::now());
                        (packet, cache.evictions() > evictions_before)
                    })
                    .unwrap_or((None, false));
                let Some(packet) = packet else {
                    if let Ok(mut stats) = counters.lock() {
                        stats.repair_cache_misses += 1;
                        if expired_during_lookup {
                            stats.repair_cache_miss_expired += 1;
                            stats.repair_cancelled_deadline =
                                stats.repair_cancelled_deadline.saturating_add(1);
                        } else {
                            stats.repair_cache_miss_not_found += 1;
                        }
                    }
                    continue;
                };
                match socket.send(&packet) {
                    Ok(sent) if sent == packet.len() => {
                        resent_in_window += 1;
                        if let Ok(mut stats) = counters.lock() {
                            stats.repair_packets_resent += 1;
                            if resent_keys.observe(item) {
                                stats.repair_unique_packets_resent += 1;
                            } else {
                                stats.repair_duplicate_packets_resent += 1;
                            }
                            stats.repair_cache_hits += 1;
                            stats.repair_send_bytes += sent as u64;
                        }
                    }
                    _ => {
                        if let Ok(mut stats) = counters.lock() {
                            stats.repair_send_errors += 1;
                            stats.repair_send_socket_errors += 1;
                        }
                    }
                }
            }
        }
    }

    pub(super) fn is_retryable_control_receive_error(kind: io::ErrorKind) -> bool {
        matches!(
            kind,
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::ConnectionReset
        )
    }

    fn transport_metrics_fragment(
        observer: &UdpObserver,
        sent: &SendCounters,
        packets_per_second: f64,
        repair_packets_per_second: f64,
        duplicate_repair_ratio_1s: Option<f64>,
    ) -> String {
        let mut output = format!(
            r#""udp_send_buffer_bytes":{},"udp_send_buffer_bytes_requested":{},"udp_send_buffer_bytes_actual":{},"udp_payload_size":{},"packets_per_second":{:.2},"avg_packets_per_frame":{:.3},"max_packets_per_frame":{},"packet_pacing":"{}","packet_pacing_mode":"{}","packet_pacing_tick_ms":{:.3},"packet_pacing_batch_avg":{:.3},"packet_pacing_batch_max":{},"packet_pacing_overrun_ticks":{},"packet_pacing_late_us_avg":{:.3},"packet_pacing_late_us_max":{:.3},"packet_pacing_sleep_ms_avg":{:.3},"packet_pacing_overrun_frames":{},"video_pacing_overrun_frames":{},"video_pacing_overrun_ticks":{},"video_pacing_late_us_p50":{:.3},"video_pacing_late_us_p95":{:.3},"video_pacing_late_us_p99":{:.3},"video_pacing_late_us_max":{:.3},"video_send_syscall_ms_avg":{:.6},"video_send_syscall_ms_max":{:.6},"video_worker_loop_ms_avg":{:.3},"video_worker_loop_ms_max":{:.3},"send_errors":{},"fec_mode":"{}","fec_packets_sent":{},"fec_overhead_packets":{},"fec_overhead_bytes":{},"fec_overhead_ratio":{:.6},"data_payload_size":{},"keyframe_interval_sec":{:.3},"keyframe_interval_applied":{},"keyframes_sent":{},"media_clock":"instant","first_media_timestamp_us":{},"last_video_timestamp_us":{},"receiver_clock_anchor_us":null,"playout_delay_ms":null,"audio_video_timestamp_delta_ms":null,{},{},{}"#,
            observer.udp_send_buffer_bytes,
            crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES,
            observer.udp_send_buffer_bytes,
            observer.udp_payload_size,
            packets_per_second,
            if sent.frames_sent == 0 {
                0.0
            } else {
                sent.packets_sent as f64 / sent.frames_sent as f64
            },
            sent.max_packets_per_frame,
            observer.packet_pacing.name(),
            observer.packet_pacing.effective_name(),
            if observer.packet_pacing.uses_batch() {
                PACING_TICK.as_secs_f64() * 1000.0
            } else {
                0.0
            },
            if sent.pacing_batches == 0 {
                0.0
            } else {
                sent.pacing_batch_packets as f64 / sent.pacing_batches as f64
            },
            sent.pacing_batch_max,
            sent.pacing_overrun_ticks,
            if sent.pacing_batches == 0 {
                0.0
            } else {
                sent.pacing_late_us_total / sent.pacing_batches as f64
            },
            sent.pacing_late_us_max,
            average_ms(sent.pacing_sleep_ms_total, sent.frames_sent),
            sent.pacing_overrun_frames,
            sent.pacing_overrun_frames,
            sent.pacing_overrun_ticks,
            sent.pacing_late_us_p50,
            sent.pacing_late_us_p95,
            sent.pacing_late_us_p99,
            sent.pacing_late_us_max,
            average_ns_ms(sent.video_send_syscall_ns_total, sent.video_send_syscalls),
            sent.video_send_syscall_ns_max as f64 / 1_000_000.0,
            average_ns_ms(sent.video_worker_loop_ns_total, sent.video_worker_loops),
            sent.video_worker_loop_ns_max as f64 / 1_000_000.0,
            sent.send_errors,
            observer.fec_mode.name(),
            sent.fec_packets_sent,
            sent.fec_packets_sent,
            sent.fec_bytes_sent,
            if sent.data_packets_sent == 0 {
                0.0
            } else {
                sent.fec_packets_sent as f64 / sent.data_packets_sent as f64
            },
            observer.data_payload_size,
            observer.keyframe_interval_sec,
            observer.keyframe_interval_applied(),
            observer.keyframes,
            crate::media_clock::optional_u64_json(observer.first_media_timestamp_us),
            crate::media_clock::optional_u64_json(observer.last_video_timestamp_us),
            audio_metrics_fragment(observer),
            repair_metrics_fragment(
                observer,
                sent,
                repair_packets_per_second,
                duplicate_repair_ratio_1s,
            ),
            observer.keyframe_metrics_fragment(),
        );
        let _ = std::fmt::Write::write_fmt(
            &mut output,
            format_args!(
                concat!(
                    r#","repair_unique_packets_resent":{},"repair_duplicate_packets_resent":{},"#,
                    r#""packetize_send_ms_p50":{:.3},"packetize_send_ms_p95":{:.3},"#,
                    r#""packetize_send_ms_p99":{:.3},"video_worker_loop_ms_p50":{:.3},"#,
                    r#""video_worker_loop_ms_p95":{:.3},"video_worker_loop_ms_p99":{:.3},"#,
                    r#""video_send_syscall_ms_p95":{:.6},"video_send_syscall_ms_p99":{:.6}"#
                ),
                sent.repair_unique_packets_resent,
                sent.repair_duplicate_packets_resent,
                observer.packetize_send_ms_p50,
                observer.packetize_send_ms_p95,
                observer.packetize_send_ms_p99,
                sent.video_worker_loop_us_p50 as f64 / 1000.0,
                sent.video_worker_loop_us_p95 as f64 / 1000.0,
                sent.video_worker_loop_us_p99 as f64 / 1000.0,
                sent.video_send_syscall_us_p95 as f64 / 1000.0,
                sent.video_send_syscall_us_p99 as f64 / 1000.0,
            ),
        );
        output
    }

    fn audio_metrics_fragment(observer: &UdpObserver) -> String {
        let audio = observer.audio_stats();
        let av_avg = if observer.av_delta_samples == 0 {
            0.0
        } else {
            observer.av_delta_ms_total / observer.av_delta_samples as f64
        };
        format!(
            r#""audio_enabled":{},"audio_mode":"{}","audio_thread_started":{},"audio_capture_thread_started":{},"audio_send_thread_started":{},"audio_capture_wait_mode":"{}","timer_resolution_changed":false,"audio_playback_started":false,"av_sync_enabled":{},"video_sync_gating_enabled":false,"audio_session_id":{},"audio_sample_rate":{},"audio_channels":{},"audio_packets_sent":{},"audio_bytes_sent":{},"audio_capture_glitches":{},"audio_capture_empty_polls":{},"audio_capture_timestamp_source":"{}","audio_capture_qpc_available":{},"audio_capture_qpc_errors":{},"audio_capture_timestamp_discontinuities":{},"audio_capture_underruns":0,"audio_send_queue_depth_current":{},"audio_send_queue_depth_max":{},"audio_send_queue_drops":{},"audio_send_syscall_ms_avg":{:.6},"audio_send_syscall_ms_max":{:.6},"audio_worker_loop_ms_avg":{:.6},"audio_worker_loop_ms_max":{:.6},"audio_unavailable_reason":{},"last_audio_timestamp_us":{},"av_timestamp_delta_ms_avg":{:.3},"av_timestamp_delta_ms_max":{:.3}"#,
            audio.enabled && audio.unavailable_reason.is_none(),
            observer.audio_mode.name(),
            audio.thread_started,
            audio.capture_thread_started,
            audio.send_thread_started,
            if audio.capture_thread_started {
                "wasapi-event"
            } else {
                "disabled"
            },
            audio.thread_started && audio.unavailable_reason.is_none(),
            crate::media_clock::optional_u64_json(
                (audio.enabled && audio.unavailable_reason.is_none())
                    .then_some(observer.session_id),
            ),
            crate::audio_udp::AUDIO_SAMPLE_RATE,
            crate::audio_udp::AUDIO_CHANNELS,
            audio.packets_sent,
            audio.bytes_sent,
            audio.capture_glitches,
            audio.capture_empty_polls,
            if audio.audio_capture_timestamp_source.is_empty() {
                "fallback_now"
            } else {
                &audio.audio_capture_timestamp_source
            },
            audio.audio_capture_qpc_available,
            audio.audio_capture_qpc_errors,
            audio.audio_capture_timestamp_discontinuities,
            audio.audio_send_queue_depth_current,
            audio.audio_send_queue_depth_max,
            audio.audio_send_queue_drops,
            audio.audio_send_syscall_ms_avg,
            audio.audio_send_syscall_ms_max,
            audio.audio_worker_loop_ms_avg,
            audio.audio_worker_loop_ms_max,
            optional_json_string(audio.unavailable_reason.as_deref()),
            crate::media_clock::optional_u64_json(audio.last_audio_timestamp_us),
            av_avg,
            observer.av_delta_ms_max,
        )
    }

    fn repair_metrics_fragment(
        observer: &UdpObserver,
        sent: &SendCounters,
        repair_packets_per_second: f64,
        duplicate_repair_ratio_1s: Option<f64>,
    ) -> String {
        let (packets, bytes, evictions) = observer
            .repair_cache
            .lock()
            .map(|cache| (cache.len(), cache.bytes(), cache.evictions()))
            .unwrap_or_default();
        let repair_overhead_ratio = if sent.data_packets_sent == 0 {
            0.0
        } else {
            sent.repair_packets_resent as f64 / sent.data_packets_sent as f64
        };
        let cache_lookups = sent.repair_cache_hits + sent.repair_cache_misses;
        let repair_cache_hit_rate = if cache_lookups == 0 {
            0.0
        } else {
            sent.repair_cache_hits as f64 / cache_lookups as f64
        };
        format!(
            r#""repair_mode":"{}","repair_cache_ms":{},"repair_cache_packets":{},"repair_cache_bytes":{},"repair_cache_evictions":{},"video_data_packets_sent":{},"nack_packets_received":{},"nack_items_received":{},"repair_packets_resent":{},"repair_send_bytes":{},"repair_cache_hits":{},"repair_cache_misses":{},"repair_cache_miss_not_found":{},"repair_cache_miss_expired":{},"repair_cache_miss_evicted":{},"repair_cache_miss_wrong_session":{},"repair_cache_expired":{},"repair_rate_limited":{},"repair_send_errors":{},"repair_send_socket_errors":{},"repair_recv_thread_running":{},"repair_overhead_packets":{},"repair_overhead_ratio_vs_data":{:.6},"repair_overhead_ratio_run_total":{:.6},"repair_resend_packets_per_second":{:.2},"repair_cache_hit_rate":{:.6},"repair_request_total":{},"repair_request_deduped":{},"repair_send_suppressed":{},"repair_cancelled_frame_complete":0,"repair_cancelled_deadline":{},"duplicate_repair_ratio_1s":{},"duplicate_repair_ratio_run_total":{:.6},"late_repair_ratio_1s":null,"stream_close_received":{},"stream_close_ack_sent":{},"stream_close_ack_received":{},"stream_close_invalid":{}"#,
            observer.repair_mode.name(),
            observer.repair_cache_ms,
            packets,
            bytes,
            evictions,
            sent.data_packets_sent,
            sent.nack_packets_received,
            sent.nack_items_received,
            sent.repair_packets_resent,
            sent.repair_send_bytes,
            sent.repair_cache_hits,
            sent.repair_cache_misses,
            sent.repair_cache_miss_not_found,
            sent.repair_cache_miss_expired,
            sent.repair_cache_miss_evicted,
            sent.repair_cache_miss_wrong_session,
            evictions,
            sent.repair_rate_limited,
            sent.repair_send_errors,
            sent.repair_send_socket_errors,
            observer.repair_worker.is_some(),
            sent.repair_packets_resent,
            repair_overhead_ratio,
            repair_overhead_ratio,
            repair_packets_per_second,
            repair_cache_hit_rate,
            sent.repair_request_total,
            sent.repair_request_deduped,
            sent.repair_send_suppressed,
            sent.repair_cancelled_deadline,
            optional_f64_json(duplicate_repair_ratio_1s),
            if sent.repair_packets_resent == 0 {
                0.0
            } else {
                sent.repair_duplicate_packets_resent as f64 / sent.repair_packets_resent as f64
            },
            sent.stream_close_packets_received,
            sent.stream_close_ack_packets_sent,
            sent.stream_close_ack_packets_received,
            sent.stream_close_invalid,
        )
    }

    fn conversion_metrics_fragment(
        selection: &capture_encode_probe::ConversionSelection,
        gpu_convert_ms_avg: f64,
        cpu_convert_ms_avg: f64,
    ) -> String {
        format!(
            r#"{},"gpu_convert_ms_avg":{:.3},"cpu_convert_ms_avg":{:.3}"#,
            selection.json_fragment(),
            gpu_convert_ms_avg,
            cpu_convert_ms_avg,
        )
    }

    fn optional_duration_text(duration_sec: Option<u64>) -> String {
        duration_sec.map_or_else(|| "unlimited".to_string(), |seconds| seconds.to_string())
    }

    fn evaluate_keyframe_interval(
        configured: bool,
        target_frames: u32,
        observed_frames: Option<f64>,
        force_failures: u64,
    ) -> Option<&'static str> {
        if !configured {
            return Some("encoder-keyframe-control-unavailable");
        }
        if force_failures > 0 {
            return Some("forced-idr-request-failed");
        }
        let Some(observed) = observed_frames else {
            return Some("insufficient-idr-observations");
        };
        let target = f64::from(target_frames);
        let tolerance = (target * 0.25).max(2.0);
        ((observed - target).abs() > tolerance).then_some("observed-idr-interval-deviates")
    }

    fn optional_f64_json(value: Option<f64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| format!("{value:.3}"))
    }

    fn optional_bool_json(value: Option<bool>) -> &'static str {
        match value {
            Some(true) => "true",
            Some(false) => "false",
            None => "null",
        }
    }

    fn u64_json_array(values: &[u64]) -> String {
        let mut output = String::from("[");
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{value}"));
        }
        output.push(']');
        output
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

    fn optional_json_string(value: Option<&str>) -> String {
        value.map_or_else(
            || "null".to_string(),
            |value| format!(r#""{}""#, json_escape(value)),
        )
    }

    pub fn run_self_test() -> Result<(), String> {
        crate::capture_encode_probe::run_keyframe_schedule_self_test()?;
        let batch = batch_size_for_frame(
            120,
            Duration::from_nanos(1_000_000_000 / 60),
            Duration::from_millis(1),
        );
        if batch == 0 || batch > 120 || batch_size_for_frame(0, Duration::ZERO, Duration::ZERO) == 0
        {
            return Err("batch pacing calculation failed".to_string());
        }
        if evaluate_keyframe_interval(true, 30, Some(30.0), 0).is_some() {
            return Err("matching IDR interval was reported as a warning".to_string());
        }
        if evaluate_keyframe_interval(true, 30, Some(120.0), 0)
            != Some("observed-idr-interval-deviates")
        {
            return Err("IDR interval deviation was not detected".to_string());
        }
        if evaluate_keyframe_interval(true, 30, None, 0) != Some("insufficient-idr-observations") {
            return Err("missing IDR observations were not detected".to_string());
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use platform::{run, run_self_test};

#[cfg(not(windows))]
pub fn run(_config: H264SendConfig) -> Result<(), String> {
    Err("h264-send-probe is only supported on Windows".to_string())
}

#[cfg(not(windows))]
pub fn run_self_test() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bitrate_only_action_is_not_a_structural_profile_change() {
        let bitrate = crate::adaptive_quality::AdaptiveAction::SetBitrate {
            bitrate_mbps: 42.0,
            reason: "test".to_string(),
        };
        let resolution = crate::adaptive_quality::AdaptiveAction::SetResolution {
            width: 1600,
            height: 900,
            reason: "test".to_string(),
        };
        assert!(!adaptive_action_changes_video_structure(&bitrate));
        assert!(adaptive_action_changes_video_structure(&resolution));
    }

    #[test]
    fn global_rates_survive_many_profile_rebuilds() {
        // Eleven profile segments still share one 60-second command clock.
        let total_frames = 3_600;
        let total_bytes = 375_000_000;
        let summary = summarize_run_rates(
            total_frames,
            total_bytes,
            60.0,
            total_frames - 120,
            total_bytes - 12_500_000,
            2.0,
        );
        assert!((summary.global_duration_sec - 60.0).abs() < f64::EPSILON);
        assert!((summary.global_fps - 60.0).abs() < 0.001);
        assert!((summary.global_mbps - 50.0).abs() < 0.001);
        assert_eq!(summary.current_profile_frames_sent, 120);
        assert!((summary.current_profile_duration_sec - 2.0).abs() < f64::EPSILON);
        assert!((summary.current_profile_fps - 60.0).abs() < 0.001);
    }

    #[test]
    #[cfg(windows)]
    fn sender_peer_timeout_requires_screen_media_or_valid_feedback_age() {
        let timeout = Duration::from_secs(5);
        assert!(!platform::sender_peer_timeout_due(
            H264SendMode::Screen,
            Some(Duration::from_millis(4_999)),
            None,
            false,
            timeout,
        ));
        assert!(platform::sender_peer_timeout_due(
            H264SendMode::Screen,
            Some(timeout),
            None,
            false,
            timeout,
        ));
        assert!(!platform::sender_peer_timeout_due(
            H264SendMode::Probe,
            Some(Duration::from_secs(30)),
            None,
            false,
            timeout,
        ));
        assert!(!platform::sender_peer_timeout_due(
            H264SendMode::Screen,
            Some(Duration::from_secs(30)),
            Some(Duration::from_millis(4_999)),
            false,
            timeout,
        ));
        assert!(platform::sender_peer_timeout_due(
            H264SendMode::Screen,
            Some(Duration::from_secs(1)),
            Some(timeout),
            false,
            timeout,
        ));
        assert!(!platform::sender_peer_timeout_due(
            H264SendMode::Screen,
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(30)),
            true,
            timeout,
        ));
    }

    #[test]
    #[cfg(windows)]
    fn control_socket_connection_reset_keeps_liveness_polling() {
        assert!(platform::is_retryable_control_receive_error(
            std::io::ErrorKind::ConnectionReset,
        ));
        assert!(platform::is_retryable_control_receive_error(
            std::io::ErrorKind::TimedOut,
        ));
        assert!(!platform::is_retryable_control_receive_error(
            std::io::ErrorKind::PermissionDenied,
        ));
    }
}
