use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct AudioSendConfig {
    pub host: String,
    pub port: u16,
    pub duration_sec: Option<u64>,
    pub frame_ms: u32,
}

#[derive(Debug, Clone)]
pub struct AudioRecvPlayConfig {
    pub bind: String,
    pub port: u16,
    pub duration_sec: Option<u64>,
    pub jitter_buffer_ms: u32,
}

pub(crate) const AUDIO_MAGIC: &[u8; 4] = b"AGA1";
const AUDIO_VERSION: u8 = 1;
const AUDIO_TYPE_PCM16: u8 = 1;
const AUDIO_HEADER_LEN: usize = 44;
pub(crate) const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub(crate) const AUDIO_CHANNELS: u16 = 2;
pub(crate) const AUDIO_BITS_PER_SAMPLE: u16 = 16;
const AUDIO_BYTES_PER_FRAME: usize = AUDIO_CHANNELS as usize * 2;
const STARTUP_AUDIO_HOLD: Duration = Duration::from_millis(200);
const STARTUP_AUDIO_MAX_PACKETS: usize = 32;

#[derive(Debug, Clone)]
pub(crate) struct AudioPacket {
    pub(crate) session_id: u64,
    pub(crate) sequence: u64,
    pub(crate) timestamp_us: u64,
    pub(crate) sample_count: u16,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u16,
    pub(crate) bits_per_sample: u16,
    pub(crate) payload: Vec<u8>,
}

impl AudioPacket {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        if self.sample_count == 0 {
            return Err("sample_count must be greater than zero".to_string());
        }
        if self.sample_rate == 0 || self.channels == 0 {
            return Err("sample_rate and channels must be greater than zero".to_string());
        }
        if self.payload.len() > u16::MAX as usize {
            return Err("audio payload is too large".to_string());
        }
        let expected =
            self.sample_count as usize * self.channels as usize * self.bits_per_sample as usize / 8;
        if self.payload.len() != expected {
            return Err(format!(
                "audio payload size mismatch: got {}, expected {}",
                self.payload.len(),
                expected
            ));
        }

        let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + self.payload.len());
        out.extend_from_slice(AUDIO_MAGIC);
        out.push(AUDIO_VERSION);
        out.push(AUDIO_TYPE_PCM16);
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.timestamp_us.to_be_bytes());
        out.extend_from_slice(&self.sample_rate.to_be_bytes());
        out.extend_from_slice(&self.channels.to_be_bytes());
        out.extend_from_slice(&self.bits_per_sample.to_be_bytes());
        out.extend_from_slice(&self.sample_count.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < AUDIO_HEADER_LEN {
            return Err("audio packet too short".to_string());
        }
        if &buf[0..4] != AUDIO_MAGIC {
            return Err("bad audio packet magic".to_string());
        }
        if buf[4] != AUDIO_VERSION {
            return Err(format!("unsupported audio packet version: {}", buf[4]));
        }
        if buf[5] != AUDIO_TYPE_PCM16 {
            return Err(format!("unsupported audio packet type: {}", buf[5]));
        }
        let session_id = u64::from_be_bytes(buf[8..16].try_into().unwrap());
        let sequence = u64::from_be_bytes(buf[16..24].try_into().unwrap());
        let timestamp_us = u64::from_be_bytes(buf[24..32].try_into().unwrap());
        let sample_rate = u32::from_be_bytes(buf[32..36].try_into().unwrap());
        let channels = u16::from_be_bytes([buf[36], buf[37]]);
        let bits_per_sample = u16::from_be_bytes([buf[38], buf[39]]);
        let sample_count = u16::from_be_bytes([buf[40], buf[41]]);
        let payload_len = u16::from_be_bytes([buf[42], buf[43]]) as usize;
        if AUDIO_HEADER_LEN + payload_len > buf.len() {
            return Err("audio payload length exceeds datagram length".to_string());
        }
        let expected = sample_count as usize * channels as usize * bits_per_sample as usize / 8;
        if payload_len != expected {
            return Err("audio payload length does not match format".to_string());
        }
        Ok(Self {
            session_id,
            sequence,
            timestamp_us,
            sample_count,
            sample_rate,
            channels,
            bits_per_sample,
            payload: buf[AUDIO_HEADER_LEN..AUDIO_HEADER_LEN + payload_len].to_vec(),
        })
    }
}

pub(crate) fn is_audio_packet(buf: &[u8]) -> bool {
    buf.len() >= 4 && &buf[0..4] == AUDIO_MAGIC
}

#[derive(Debug, Default, Clone)]
pub(crate) struct IntegratedAudioSendStats {
    pub enabled: bool,
    pub thread_started: bool,
    pub capture_thread_started: bool,
    pub send_thread_started: bool,
    pub unavailable_reason: Option<String>,
    pub packets_sent: u64,
    pub bytes_sent: u64,
    pub capture_glitches: u64,
    pub capture_empty_polls: u64,
    pub silence_packets: u64,
    pub audio_capture_timestamp_source: String,
    pub audio_capture_qpc_available: bool,
    pub audio_capture_qpc_errors: u64,
    pub audio_capture_timestamp_discontinuities: u64,
    pub first_media_timestamp_us: Option<u64>,
    pub last_audio_timestamp_us: Option<u64>,
    pub audio_send_queue_depth_current: u64,
    pub audio_send_queue_depth_max: u64,
    pub audio_send_queue_drops: u64,
    pub audio_send_syscall_ms_avg: f64,
    pub audio_send_syscall_ms_max: f64,
    pub audio_worker_loop_ms_avg: f64,
    pub audio_worker_loop_ms_max: f64,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct IntegratedAudioRecvStats {
    pub enabled: bool,
    pub thread_started: bool,
    pub playback_started: bool,
    pub audio_playhead_valid: bool,
    pub unavailable_reason: Option<String>,
    pub packets_received: u64,
    pub audio_packets_invalid: u64,
    pub packets_lost_estimate: u64,
    pub late_packets: u64,
    pub audio_callback_empty_polls: u64,
    pub audio_real_underruns: u64,
    pub audio_silence_filled_frames: u64,
    pub frames_rendered: u64,
    pub audio_playhead_timestamp_us: Option<u64>,
    pub latest_audio_packet_timestamp_us: Option<u64>,
    pub audio_queue_depth_ms: f64,
    pub audio_samples_rendered_total: u64,
    pub audio_samples_queued_current: u64,
    pub audio_samples_dropped_for_latency: u64,
    pub audio_prestart_silence_frames: u64,
    pub audio_poststream_silence_frames: u64,
    pub audio_media_samples_rendered_total: u64,
    pub audio_media_samples_submitted_total: u64,
    pub audio_media_samples_audible_estimated_total: u64,
    pub audio_device_silence_filled_frames: u64,
    pub audio_device_padding_frames: u32,
    pub audio_device_padding_ms: f64,
    pub audio_device_padding_valid: bool,
    pub audio_playhead_discontinuities: u64,
    pub audio_latency_drop_discontinuities: u64,
    pub audio_master_reanchors: u64,
    pub audio_packets_dropped_session_mismatch: u64,
    pub audio_packets_received_before_video_session: u64,
    pub audio_startup_packets_buffered: u64,
    pub audio_startup_packets_replayed: u64,
    pub audio_startup_packets_dropped: u64,
    pub audio_session_matched: bool,
    pub audio_session_id: Option<u64>,
    pub expected_video_session_id: Option<u64>,
    pub audio_submitted_timestamp_us: Option<u64>,
    pub jitter_buffer_target_ms: u32,
    pub jitter_buffer_ms_current: f64,
    pub jitter_buffer_ms_avg: f64,
    pub jitter_buffer_ms_max: f64,
    // Retained for consumers of the first A/V integration stats schema.
    pub jitter_buffer_ms: f64,
    pub audio_packet_parse_ms_avg: f64,
    pub audio_packet_parse_ms_max: f64,
    pub audio_queue_drops: u64,
    pub media_anchor: crate::media_clock::ReceiverMediaClockAnchor,
}

#[derive(Default)]
struct AudioIngressMetrics {
    packets_received: AtomicU64,
    packets_invalid: AtomicU64,
    queue_drops: AtomicU64,
    parse_ns_total: AtomicU64,
    parse_ns_max: AtomicU64,
    parse_count: AtomicU64,
    expected_session_id: AtomicU64,
    expected_session_known: AtomicBool,
    packets_dropped_session_mismatch: AtomicU64,
    session_matched: AtomicBool,
    accepted_session_id: AtomicU64,
    packets_received_before_video_session: AtomicU64,
    startup_packets_buffered: AtomicU64,
    startup_packets_replayed: AtomicU64,
    startup_packets_dropped: AtomicU64,
}

impl AudioIngressMetrics {
    fn record_parse(&self, elapsed: Duration) {
        let nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        self.parse_ns_total.fetch_add(nanos, Ordering::Relaxed);
        self.parse_count.fetch_add(1, Ordering::Relaxed);
        self.parse_ns_max.fetch_max(nanos, Ordering::Relaxed);
    }

    fn apply_to(&self, stats: &mut IntegratedAudioRecvStats) {
        stats.packets_received = self.packets_received.load(Ordering::Relaxed);
        stats.audio_packets_invalid = self.packets_invalid.load(Ordering::Relaxed);
        stats.audio_queue_drops = self.queue_drops.load(Ordering::Relaxed);
        let parse_count = self.parse_count.load(Ordering::Relaxed);
        let parse_ns_total = self.parse_ns_total.load(Ordering::Relaxed);
        stats.audio_packet_parse_ms_avg = if parse_count == 0 {
            0.0
        } else {
            parse_ns_total as f64 / parse_count as f64 / 1_000_000.0
        };
        stats.audio_packet_parse_ms_max =
            self.parse_ns_max.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        stats.audio_packets_dropped_session_mismatch = self
            .packets_dropped_session_mismatch
            .load(Ordering::Relaxed);
        stats.audio_packets_received_before_video_session = self
            .packets_received_before_video_session
            .load(Ordering::Relaxed);
        stats.audio_startup_packets_buffered =
            self.startup_packets_buffered.load(Ordering::Relaxed);
        stats.audio_startup_packets_replayed =
            self.startup_packets_replayed.load(Ordering::Relaxed);
        stats.audio_startup_packets_dropped = self.startup_packets_dropped.load(Ordering::Relaxed);
        stats.audio_session_matched = self.session_matched.load(Ordering::Relaxed);
        stats.audio_session_id = stats
            .audio_session_matched
            .then(|| self.accepted_session_id.load(Ordering::Relaxed));
        stats.expected_video_session_id = self
            .expected_session_known
            .load(Ordering::Relaxed)
            .then(|| self.expected_session_id.load(Ordering::Relaxed));
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AudioMasterClockState {
    pub started: bool,
    pub valid: bool,
    pub playhead_ts_us: u64,
    pub submitted_playhead_ts_us: u64,
    pub device_padding_frames: u32,
    pub device_padding_valid: bool,
    pub media_samples_submitted_total: u64,
    pub media_samples_audible_estimated_total: u64,
    pub updated_at: Option<Instant>,
    pub last_media_submit_at: Option<Instant>,
    pub valid_since: Option<Instant>,
    pub suspended_until: Option<Instant>,
    pub discontinuity_count: u64,
    pub reanchor_count: u64,
    pub session_matched: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AudioMasterSyncStatus {
    pub playhead_us: Option<u64>,
    pub stable: bool,
    pub stale: bool,
    pub timeline_discontinuity: bool,
    pub device_padding_valid: bool,
    pub session_matched: bool,
}

pub(crate) fn audio_master_sync_status(
    master: &Arc<Mutex<AudioMasterClockState>>,
) -> AudioMasterSyncStatus {
    const STALE_AFTER: Duration = Duration::from_millis(250);
    const STABLE_AFTER: Duration = Duration::from_millis(500);
    let Ok(mut state) = master.lock() else {
        return AudioMasterSyncStatus::default();
    };
    let stale = state
        .last_media_submit_at
        .is_some_and(|submitted_at| submitted_at.elapsed() > STALE_AFTER);
    let discontinuity = state
        .suspended_until
        .is_some_and(|until| Instant::now() < until);
    if stale {
        state.valid = false;
        state.valid_since = None;
    }
    let valid =
        state.started && state.valid && state.device_padding_valid && !stale && !discontinuity;
    AudioMasterSyncStatus {
        playhead_us: valid.then_some(state.playhead_ts_us),
        stable: valid
            && state
                .valid_since
                .is_some_and(|valid_since| valid_since.elapsed() >= STABLE_AFTER),
        stale,
        timeline_discontinuity: discontinuity,
        device_padding_valid: state.device_padding_valid,
        session_matched: state.session_matched,
    }
}

pub(crate) fn audio_master_playhead_us(master: &Arc<Mutex<AudioMasterClockState>>) -> Option<u64> {
    audio_master_sync_status(master).playhead_us
}

struct CapturedAudioChunk {
    timestamp_us: u64,
    sample_count: u16,
    payload: Vec<u8>,
}

pub(crate) struct IntegratedAudioSender {
    stop: Arc<AtomicBool>,
    capture_worker: Option<JoinHandle<()>>,
    send_worker: Option<JoinHandle<()>>,
    stats: Arc<Mutex<IntegratedAudioSendStats>>,
    queue: Arc<crate::sender_scheduling::BoundedDropNewestQueue<CapturedAudioChunk>>,
}

impl IntegratedAudioSender {
    pub(crate) fn stats(&self) -> IntegratedAudioSendStats {
        let mut snapshot = self
            .stats
            .lock()
            .map(|stats| stats.clone())
            .unwrap_or_default();
        snapshot.audio_send_queue_depth_current = self.queue.depth();
        snapshot.audio_send_queue_depth_max = self.queue.peak();
        snapshot.audio_send_queue_drops = self.queue.drops();
        snapshot
    }

    pub(crate) fn stop_and_join(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        self.queue.close();
        let capture = crate::shutdown::try_join_until(
            &mut self.capture_worker,
            Instant::now() + Duration::from_secs(2),
        );
        let sender = crate::shutdown::try_join_until(
            &mut self.send_worker,
            Instant::now() + Duration::from_secs(2),
        );
        if capture == crate::shutdown::WorkerJoinStatus::TimedOut {
            crate::shutdown::retain_unjoined_worker(
                "integrated-audio-capture",
                &mut self.capture_worker,
            );
        }
        if sender == crate::shutdown::WorkerJoinStatus::TimedOut {
            crate::shutdown::retain_unjoined_worker(
                "integrated-audio-sender",
                &mut self.send_worker,
            );
        }
        if capture.clean() && sender.clean() {
            Ok(())
        } else {
            Err(format!(
                "{}: integrated audio shutdown incomplete: capture={}, sender={}",
                crate::shutdown::WORKER_OWNERSHIP_FAILURE_TAG,
                capture.name(),
                sender.name()
            ))
        }
    }
}

impl Drop for IntegratedAudioSender {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

#[derive(Clone)]
pub(crate) struct IntegratedAudioIngest {
    sender: mpsc::SyncSender<AudioPacket>,
    metrics: Arc<AudioIngressMetrics>,
    startup_packets: Arc<Mutex<VecDeque<(Instant, AudioPacket)>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AudioIngressOutcome {
    NotAudio,
    Accepted,
    Invalid,
    DroppedSessionMismatch,
}

impl IntegratedAudioIngest {
    pub(crate) fn set_expected_session_id(&self, session_id: u64) {
        self.metrics
            .expected_session_id
            .store(session_id, Ordering::Release);
        self.metrics
            .expected_session_known
            .store(true, Ordering::Release);
        let now = Instant::now();
        let buffered = self
            .startup_packets
            .lock()
            .map(|mut packets| packets.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        for (received_at, packet) in buffered {
            if now.saturating_duration_since(received_at) > STARTUP_AUDIO_HOLD {
                self.metrics
                    .startup_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if packet.session_id != session_id {
                self.metrics
                    .packets_dropped_session_mismatch
                    .fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .startup_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if self.dispatch_matching_packet(packet) {
                self.metrics
                    .startup_packets_replayed
                    .fetch_add(1, Ordering::Relaxed);
            } else {
                self.metrics
                    .startup_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn accept_datagram(&self, buf: &[u8]) -> AudioIngressOutcome {
        if !is_audio_packet(buf) {
            return AudioIngressOutcome::NotAudio;
        }
        let parse_started = Instant::now();
        let Ok(packet) = AudioPacket::decode(buf) else {
            self.metrics.record_parse(parse_started.elapsed());
            self.metrics.packets_invalid.fetch_add(1, Ordering::Relaxed);
            return AudioIngressOutcome::Invalid;
        };
        self.metrics.record_parse(parse_started.elapsed());
        let expected_known = self.metrics.expected_session_known.load(Ordering::Acquire);
        let expected_session_id = self.metrics.expected_session_id.load(Ordering::Acquire);
        if !expected_known {
            self.metrics
                .packets_received_before_video_session
                .fetch_add(1, Ordering::Relaxed);
            let Ok(mut startup) = self.startup_packets.lock() else {
                self.metrics
                    .startup_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
                return AudioIngressOutcome::DroppedSessionMismatch;
            };
            let now = Instant::now();
            while startup.front().is_some_and(|(received_at, _)| {
                now.saturating_duration_since(*received_at) > STARTUP_AUDIO_HOLD
            }) {
                startup.pop_front();
                self.metrics
                    .startup_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
            if startup.len() >= STARTUP_AUDIO_MAX_PACKETS {
                startup.pop_front();
                self.metrics
                    .startup_packets_dropped
                    .fetch_add(1, Ordering::Relaxed);
            }
            startup.push_back((now, packet));
            self.metrics
                .startup_packets_buffered
                .fetch_add(1, Ordering::Relaxed);
            return AudioIngressOutcome::Accepted;
        }
        if packet.session_id != expected_session_id {
            self.metrics
                .packets_dropped_session_mismatch
                .fetch_add(1, Ordering::Relaxed);
            return AudioIngressOutcome::DroppedSessionMismatch;
        }
        self.dispatch_matching_packet(packet);
        AudioIngressOutcome::Accepted
    }

    fn dispatch_matching_packet(&self, packet: AudioPacket) -> bool {
        self.metrics
            .packets_received
            .fetch_add(1, Ordering::Relaxed);
        self.metrics.session_matched.store(true, Ordering::Release);
        self.metrics
            .accepted_session_id
            .store(packet.session_id, Ordering::Release);
        match self.sender.try_send(packet) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                self.metrics.queue_drops.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

pub(crate) struct IntegratedAudioReceiver {
    ingest: IntegratedAudioIngest,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    stats: Arc<Mutex<IntegratedAudioRecvStats>>,
    metrics: Arc<AudioIngressMetrics>,
    master: Arc<Mutex<AudioMasterClockState>>,
}

impl IntegratedAudioReceiver {
    pub(crate) fn ingest(&self) -> IntegratedAudioIngest {
        self.ingest.clone()
    }

    pub(crate) fn master_clock(&self) -> Arc<Mutex<AudioMasterClockState>> {
        Arc::clone(&self.master)
    }

    pub(crate) fn stats(&self) -> IntegratedAudioRecvStats {
        let mut snapshot = self
            .stats
            .lock()
            .map(|stats| stats.clone())
            .unwrap_or_default();
        self.metrics.apply_to(&mut snapshot);
        snapshot
    }

    pub(crate) fn stop_and_join(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::SeqCst);
        let status = crate::shutdown::try_join_until(
            &mut self.worker,
            Instant::now() + Duration::from_secs(2),
        );
        if status == crate::shutdown::WorkerJoinStatus::TimedOut {
            crate::shutdown::retain_unjoined_worker("integrated-audio-receiver", &mut self.worker);
        }
        if status.clean() {
            Ok(())
        } else {
            Err(format!(
                "{}: integrated audio receiver shutdown incomplete: {}",
                crate::shutdown::WORKER_OWNERSHIP_FAILURE_TAG,
                status.name()
            ))
        }
    }
}

impl Drop for IntegratedAudioReceiver {
    fn drop(&mut self) {
        let _ = self.stop_and_join();
    }
}

pub fn run_self_test() -> Result<(), String> {
    let packet = AudioPacket {
        session_id: 11,
        sequence: 7,
        timestamp_us: 20_000,
        sample_count: 480,
        sample_rate: AUDIO_SAMPLE_RATE,
        channels: AUDIO_CHANNELS,
        bits_per_sample: AUDIO_BITS_PER_SAMPLE,
        payload: vec![3; 480 * AUDIO_BYTES_PER_FRAME],
    };
    let encoded = packet.encode()?;
    if encoded.len() != AUDIO_HEADER_LEN + packet.payload.len() {
        return Err("audio packet encoded length mismatch".to_string());
    }
    let decoded = AudioPacket::decode(&encoded)?;
    if decoded.sequence != packet.sequence
        || decoded.session_id != packet.session_id
        || decoded.timestamp_us != packet.timestamp_us
        || decoded.sample_count != packet.sample_count
        || decoded.payload != packet.payload
    {
        return Err("audio packet roundtrip mismatch".to_string());
    }
    if AudioPacket::decode(b"AGM1").is_ok() {
        return Err("invalid audio packet was accepted".to_string());
    }
    let (ingest_sender, ingest_receiver) = mpsc::sync_channel(1);
    let ingest_metrics = Arc::new(AudioIngressMetrics::default());
    let ingest = IntegratedAudioIngest {
        sender: ingest_sender,
        metrics: Arc::clone(&ingest_metrics),
        startup_packets: Arc::new(Mutex::new(VecDeque::new())),
    };
    if ingest.accept_datagram(&encoded) != AudioIngressOutcome::Accepted
        || ingest_receiver.try_recv().is_ok()
    {
        return Err("audio ingress startup holding queue failed".to_string());
    }
    ingest.set_expected_session_id(packet.session_id);
    if ingest_receiver
        .try_recv()
        .map(|received| received.session_id)
        .ok()
        != Some(packet.session_id)
    {
        return Err("audio ingress did not replay startup audio".to_string());
    }
    if ingest.accept_datagram(&encoded) != AudioIngressOutcome::Accepted
        || ingest_receiver.try_recv().is_err()
    {
        return Err("audio ingress rejected a matching video session".to_string());
    }
    let mut ingress_stats = IntegratedAudioRecvStats::default();
    ingest_metrics.apply_to(&mut ingress_stats);
    if !ingress_stats.audio_session_matched
        || ingress_stats.audio_session_id != Some(packet.session_id)
        || ingress_stats.audio_packets_dropped_session_mismatch != 0
        || ingress_stats.audio_startup_packets_buffered != 1
        || ingress_stats.audio_startup_packets_replayed != 1
    {
        return Err("audio ingress session telemetry failed".to_string());
    }
    if ingest.accept_datagram(AUDIO_MAGIC) != AudioIngressOutcome::Invalid {
        return Err("audio ingress did not distinguish malformed audio".to_string());
    }
    let master = Arc::new(Mutex::new(AudioMasterClockState {
        started: true,
        valid: true,
        playhead_ts_us: 123_000,
        device_padding_valid: true,
        updated_at: Some(Instant::now()),
        last_media_submit_at: Some(Instant::now()),
        suspended_until: None,
        discontinuity_count: 0,
        ..AudioMasterClockState::default()
    }));
    if audio_master_playhead_us(&master) != Some(123_000) {
        return Err("fresh audio master clock was not readable".to_string());
    }
    if let Ok(mut state) = master.lock() {
        state.last_media_submit_at = Instant::now().checked_sub(Duration::from_millis(251));
    }
    if audio_master_playhead_us(&master).is_some() {
        return Err("stale audio master clock remained valid".to_string());
    }
    if let Ok(mut state) = master.lock() {
        state.last_media_submit_at = Some(Instant::now());
        state.suspended_until = Some(Instant::now() + Duration::from_millis(50));
    }
    if audio_master_playhead_us(&master).is_some() {
        return Err("suspended audio master clock remained valid".to_string());
    }
    #[cfg(windows)]
    imp::run_jitter_buffer_self_test()?;
    Ok(())
}

#[cfg(test)]
mod deterministic_tests {
    use super::*;

    fn packet(session_id: u64) -> AudioPacket {
        AudioPacket {
            session_id,
            sequence: 1,
            timestamp_us: 10_000,
            sample_count: 480,
            sample_rate: AUDIO_SAMPLE_RATE,
            channels: AUDIO_CHANNELS,
            bits_per_sample: AUDIO_BITS_PER_SAMPLE,
            payload: vec![0; 480 * AUDIO_BYTES_PER_FRAME],
        }
    }

    #[test]
    fn ingress_accepts_only_the_bound_video_session() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let metrics = Arc::new(AudioIngressMetrics::default());
        let ingest = IntegratedAudioIngest {
            sender,
            metrics: Arc::clone(&metrics),
            startup_packets: Arc::new(Mutex::new(VecDeque::new())),
        };
        let foreign = packet(7).encode().unwrap();
        assert_eq!(
            ingest.accept_datagram(&foreign),
            AudioIngressOutcome::Accepted
        );
        ingest.set_expected_session_id(11);
        assert_eq!(
            ingest.accept_datagram(&foreign),
            AudioIngressOutcome::DroppedSessionMismatch
        );
        let matching = packet(11).encode().unwrap();
        assert_eq!(
            ingest.accept_datagram(&matching),
            AudioIngressOutcome::Accepted
        );
        assert_eq!(receiver.try_recv().unwrap().session_id, 11);
        assert_eq!(
            ingest.accept_datagram(AUDIO_MAGIC),
            AudioIngressOutcome::Invalid
        );

        let mut stats = IntegratedAudioRecvStats::default();
        metrics.apply_to(&mut stats);
        assert_eq!(stats.audio_packets_invalid, 1);
        assert_eq!(stats.audio_packets_dropped_session_mismatch, 2);
        assert_eq!(stats.audio_packets_received_before_video_session, 1);
        assert_eq!(stats.audio_startup_packets_buffered, 1);
        assert_eq!(stats.audio_startup_packets_replayed, 0);
        assert_eq!(stats.audio_startup_packets_dropped, 1);
        assert_eq!(stats.expected_video_session_id, Some(11));
        assert_eq!(stats.audio_session_id, Some(11));
    }

    #[test]
    fn startup_audio_is_replayed_after_video_session_binding() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let metrics = Arc::new(AudioIngressMetrics::default());
        let ingest = IntegratedAudioIngest {
            sender,
            metrics: Arc::clone(&metrics),
            startup_packets: Arc::new(Mutex::new(VecDeque::new())),
        };
        let encoded = packet(22).encode().unwrap();
        assert_eq!(
            ingest.accept_datagram(&encoded),
            AudioIngressOutcome::Accepted
        );
        assert!(receiver.try_recv().is_err());
        ingest.set_expected_session_id(22);
        assert_eq!(receiver.try_recv().unwrap().session_id, 22);

        let mut stats = IntegratedAudioRecvStats::default();
        metrics.apply_to(&mut stats);
        assert_eq!(stats.audio_packets_received_before_video_session, 1);
        assert_eq!(stats.audio_startup_packets_buffered, 1);
        assert_eq!(stats.audio_startup_packets_replayed, 1);
        assert_eq!(stats.audio_startup_packets_dropped, 0);
    }

    #[test]
    fn stale_or_suspended_master_is_not_usable_for_av_sync() {
        let master = Arc::new(Mutex::new(AudioMasterClockState {
            started: true,
            valid: true,
            playhead_ts_us: 50_000,
            last_media_submit_at: Some(Instant::now()),
            valid_since: Some(Instant::now() - Duration::from_millis(500)),
            device_padding_valid: true,
            session_matched: true,
            ..AudioMasterClockState::default()
        }));
        assert!(audio_master_sync_status(&master).playhead_us.is_some());
        master.lock().unwrap().suspended_until = Some(Instant::now() + Duration::from_millis(1));
        let status = audio_master_sync_status(&master);
        assert!(status.playhead_us.is_none());
        assert!(status.timeline_discontinuity);
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::audio_timeline::AudioTimeline;
    use crate::json_escape;
    use crate::media_clock::{
        MediaClock, MediaTimestampUs, QpcMediaTimestampMapper, ReceiverMediaClockAnchor,
    };
    use std::convert::TryFrom;
    use std::ffi::c_void;
    use std::ptr;
    use std::slice;
    use std::thread;
    use windows::core::{BSTR, GUID};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
        IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
        AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
        WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    const REFTIMES_PER_SEC: i64 = 10_000_000;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    const PCM_SUBFORMAT: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
    const FLOAT_SUBFORMAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SampleKind {
        Pcm,
        Float,
        Unsupported,
    }

    #[derive(Debug, Clone)]
    struct AudioFormat {
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        block_align: u16,
        sample_kind: SampleKind,
    }

    struct ComGuard;

    impl ComGuard {
        fn init() -> Result<Self, String> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() }
                .map_err(|err| format!("CoInitializeEx failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct LoopbackCapture {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        format: AudioFormat,
        device_name: String,
        format_ptr: *mut WAVEFORMATEX,
        format_allocated: bool,
        event: HANDLE,
        _com: ComGuard,
    }

    impl Drop for LoopbackCapture {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
                let _ = CloseHandle(self.event);
                maybe_free_wave_format(self.format_ptr, self.format_allocated);
            }
        }
    }

    impl LoopbackCapture {
        fn new() -> Result<Self, String> {
            let com = ComGuard::init()?;
            let endpoint = default_render_endpoint()?;
            let device_name = endpoint_friendly_name(&endpoint)
                .unwrap_or_else(|| "default render endpoint".to_string());
            let client: IAudioClient = unsafe {
                endpoint
                    .Activate(CLSCTX_ALL, None)
                    .map_err(|err| format!("IAudioClient activation failed: {err}"))?
            };
            let mut desired = desired_pcm_format();
            let desired_ptr = &mut desired as *mut WAVEFORMATEX;
            unsafe {
                client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_LOOPBACK
                            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                        REFTIMES_PER_SEC,
                        0,
                        desired_ptr,
                        None,
                    )
                    .map_err(|err| format!("WASAPI loopback 48k PCM init failed: {err}"))?;
            }
            let format = audio_format_from_ptr(desired_ptr)?;
            validate_wire_format(&format)?;
            let capture: IAudioCaptureClient = unsafe {
                client
                    .GetService()
                    .map_err(|err| format!("IAudioCaptureClient GetService failed: {err}"))?
            };
            let event = unsafe { CreateEventW(None, false, false, None) }
                .map_err(|err| format!("create WASAPI capture event failed: {err}"))?;
            if let Err(err) = unsafe { client.SetEventHandle(event) } {
                unsafe {
                    let _ = CloseHandle(event);
                }
                return Err(format!("set WASAPI capture event failed: {err}"));
            }
            unsafe {
                client
                    .Start()
                    .map_err(|err| format!("IAudioClient Start failed: {err}"))?;
            }
            Ok(Self {
                client,
                capture,
                format,
                device_name,
                format_ptr: desired_ptr,
                format_allocated: false,
                event,
                _com: com,
            })
        }

        fn wait_for_data(&self, timeout: Duration) -> Result<bool, String> {
            let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
            match unsafe { WaitForSingleObject(self.event, timeout_ms) } {
                WAIT_OBJECT_0 => Ok(true),
                WAIT_TIMEOUT => Ok(false),
                other => Err(format!("wait for WASAPI capture event failed: {other:?}")),
            }
        }
    }

    struct WasapiRenderer {
        client: IAudioClient,
        render: IAudioRenderClient,
        buffer_frames: u32,
        device_name: String,
        _com: ComGuard,
    }

    impl Drop for WasapiRenderer {
        fn drop(&mut self) {
            unsafe {
                let _ = self.client.Stop();
            }
        }
    }

    impl WasapiRenderer {
        fn new() -> Result<Self, String> {
            let com = ComGuard::init()?;
            let endpoint = default_render_endpoint()?;
            let device_name = endpoint_friendly_name(&endpoint)
                .unwrap_or_else(|| "default render endpoint".to_string());
            let client: IAudioClient = unsafe {
                endpoint
                    .Activate(CLSCTX_ALL, None)
                    .map_err(|err| format!("IAudioClient activation failed: {err}"))?
            };
            let mut desired = desired_pcm_format();
            let desired_ptr = &mut desired as *mut WAVEFORMATEX;
            unsafe {
                client
                    .Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                        REFTIMES_PER_SEC / 10,
                        0,
                        desired_ptr,
                        None,
                    )
                    .map_err(|err| format!("WASAPI render 48k PCM init failed: {err}"))?;
            }
            let render: IAudioRenderClient = unsafe {
                client
                    .GetService()
                    .map_err(|err| format!("IAudioRenderClient GetService failed: {err}"))?
            };
            let buffer_frames = unsafe {
                client
                    .GetBufferSize()
                    .map_err(|err| format!("GetBufferSize failed: {err}"))?
            };
            unsafe {
                client
                    .Start()
                    .map_err(|err| format!("IAudioClient Start failed: {err}"))?;
            }
            Ok(Self {
                client,
                render,
                buffer_frames,
                device_name,
                _com: com,
            })
        }

        fn available_frames(&self) -> Result<u32, String> {
            let padding = self.padding_frames()?;
            Ok(self.buffer_frames.saturating_sub(padding))
        }

        fn padding_frames(&self) -> Result<u32, String> {
            unsafe {
                self.client
                    .GetCurrentPadding()
                    .map_err(|err| format!("GetCurrentPadding failed: {err}"))
            }
        }

        fn write_pcm16(&self, frames: u32, bytes: &[u8]) -> Result<(), String> {
            let expected = frames as usize * AUDIO_BYTES_PER_FRAME;
            if bytes.len() != expected {
                return Err("render byte count mismatch".to_string());
            }
            if frames == 0 {
                return Ok(());
            }
            let data = unsafe {
                self.render
                    .GetBuffer(frames)
                    .map_err(|err| format!("IAudioRenderClient GetBuffer failed: {err}"))?
            };
            unsafe {
                slice::from_raw_parts_mut(data.cast::<u8>(), expected).copy_from_slice(bytes);
                self.render
                    .ReleaseBuffer(frames, 0)
                    .map_err(|err| format!("IAudioRenderClient ReleaseBuffer failed: {err}"))?;
            }
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct SendStats {
        packets_sent: u64,
        audio_frames_sent: u64,
        bytes_sent: u64,
        capture_callbacks: u64,
        capture_empty_polls: u64,
        silence_packets: u64,
        discontinuity_count: u64,
        glitch_count: u64,
        capture_timestamp_source: &'static str,
        capture_qpc_available: bool,
        capture_qpc_errors: u64,
        capture_timestamp_discontinuities: u64,
        first_media_timestamp_us: Option<u64>,
        last_audio_timestamp_us: Option<u64>,
    }

    #[derive(Debug)]
    struct PendingAudio {
        bytes: Vec<u8>,
        first_timestamp_us: Option<u64>,
    }

    impl PendingAudio {
        fn with_capacity(capacity: usize) -> Self {
            Self {
                bytes: Vec::with_capacity(capacity),
                first_timestamp_us: None,
            }
        }

        fn len(&self) -> usize {
            self.bytes.len()
        }

        fn append_bytes(&mut self, timestamp_us: u64, bytes: &[u8]) {
            if bytes.is_empty() {
                return;
            }
            if self.bytes.is_empty() {
                self.first_timestamp_us = Some(timestamp_us);
            }
            self.bytes.extend_from_slice(bytes);
        }

        fn reanchor(&mut self, timestamp_us: u64) {
            self.bytes.clear();
            self.first_timestamp_us = Some(timestamp_us);
        }

        fn append_silence(&mut self, timestamp_us: u64, byte_len: usize) {
            if byte_len == 0 {
                return;
            }
            if self.bytes.is_empty() {
                self.first_timestamp_us = Some(timestamp_us);
            }
            self.bytes.resize(self.bytes.len() + byte_len, 0);
        }

        fn drain_frame(
            &mut self,
            frame_bytes: usize,
            frame_duration_us: u64,
        ) -> Option<(u64, Vec<u8>)> {
            if self.bytes.len() < frame_bytes {
                return None;
            }
            let timestamp_us = self.first_timestamp_us.unwrap_or_default();
            let payload: Vec<u8> = self.bytes.drain(..frame_bytes).collect();
            self.first_timestamp_us = if self.bytes.is_empty() {
                None
            } else {
                Some(timestamp_us.saturating_add(frame_duration_us))
            };
            Some((timestamp_us, payload))
        }
    }

    pub fn run_audio_send(config: AudioSendConfig) -> Result<(), String> {
        if config.frame_ms != 10 && config.frame_ms != 20 {
            return Err("--frame-ms must be 10 or 20".to_string());
        }
        let socket =
            UdpSocket::bind("0.0.0.0:0").map_err(|err| format!("audio-send bind failed: {err}"))?;
        let target = format!("{}:{}", config.host, config.port);
        socket
            .connect(&target)
            .map_err(|err| format!("audio-send connect failed: {err}"))?;
        let capture = LoopbackCapture::new()?;
        let frame_samples = AUDIO_SAMPLE_RATE * config.frame_ms / 1000;
        let frame_bytes = frame_samples as usize * AUDIO_BYTES_PER_FRAME;
        let frame_duration_us = u64::from(frame_samples) * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE);
        let media_clock = MediaClock::new();
        let mut timestamp_mapper = QpcMediaTimestampMapper::new();
        let mut pending = PendingAudio::with_capacity(frame_bytes * 2);
        let session_id = crate::make_session_id();
        let mut sequence = 0u64;
        let mut stats = SendStats::default();
        let started = Instant::now();
        let deadline = config
            .duration_sec
            .map(|duration| started + Duration::from_secs(duration));
        let mut window_at = Instant::now();
        let mut window_bytes = 0u64;
        let mut window_packets = 0u64;
        eprintln!(
            "audio-send target={} device=\"{}\" frame_ms={} format=48000Hz/2ch/PCM16",
            target, capture.device_name, config.frame_ms
        );

        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            if !capture.wait_for_data(Duration::from_millis(20))? {
                stats.capture_empty_polls += 1;
            } else {
                drain_capture_packets(
                    &capture,
                    &media_clock,
                    &mut timestamp_mapper,
                    &mut pending,
                    &mut stats,
                )?;
            }

            while pending.len() >= frame_bytes {
                let (timestamp_us, payload) = pending
                    .drain_frame(frame_bytes, frame_duration_us)
                    .ok_or_else(|| {
                    "pending audio frame unexpectedly unavailable".to_string()
                })?;
                let packet = AudioPacket {
                    session_id,
                    sequence,
                    timestamp_us,
                    sample_count: frame_samples as u16,
                    sample_rate: AUDIO_SAMPLE_RATE,
                    channels: AUDIO_CHANNELS,
                    bits_per_sample: AUDIO_BITS_PER_SAMPLE,
                    payload,
                };
                let encoded = packet.encode()?;
                let sent = socket
                    .send(&encoded)
                    .map_err(|err| format!("audio-send send failed: {err}"))?;
                stats.packets_sent += 1;
                stats.audio_frames_sent += u64::from(frame_samples);
                stats.bytes_sent += sent as u64;
                stats.first_media_timestamp_us.get_or_insert(timestamp_us);
                stats.last_audio_timestamp_us = Some(timestamp_us);
                window_packets += 1;
                window_bytes += sent as u64;
                sequence += 1;
            }

            if window_at.elapsed() >= Duration::from_secs(1) {
                let elapsed = window_at.elapsed().as_secs_f64().max(0.001);
                let send_mbps = window_bytes as f64 * 8.0 / elapsed / 1_000_000.0;
                print_send_stats(&stats, send_mbps, window_packets, false);
                window_at = Instant::now();
                window_bytes = 0;
                window_packets = 0;
            }
        }

        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        let send_mbps = stats.bytes_sent as f64 * 8.0 / elapsed / 1_000_000.0;
        print_send_stats(&stats, send_mbps, 0, true);
        Ok(())
    }

    pub(crate) fn spawn_integrated_audio_sender(
        host: String,
        port: u16,
        frame_ms: u32,
        session_id: u64,
        media_clock: MediaClock,
    ) -> IntegratedAudioSender {
        const AUDIO_SEND_QUEUE_CAPACITY: usize = 32;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(IntegratedAudioSendStats {
            enabled: true,
            ..IntegratedAudioSendStats::default()
        }));
        let queue = Arc::new(
            crate::sender_scheduling::BoundedDropNewestQueue::new(AUDIO_SEND_QUEUE_CAPACITY)
                .expect("fixed integrated audio queue capacity is valid"),
        );

        let send_stop = Arc::clone(&stop);
        let send_stats = Arc::clone(&stats);
        let send_queue = Arc::clone(&queue);
        let send_worker = thread::Builder::new()
            .name("agoralink-audio-udp-send".to_string())
            .spawn(move || {
                if let Err(err) = run_integrated_audio_send_worker(
                    host,
                    port,
                    session_id,
                    send_stop,
                    send_queue,
                    Arc::clone(&send_stats),
                ) {
                    if let Ok(mut stats) = send_stats.lock() {
                        stats.unavailable_reason = Some(err);
                    }
                }
            })
            .ok();

        let capture_stop = Arc::clone(&stop);
        let capture_stats = Arc::clone(&stats);
        let capture_queue = Arc::clone(&queue);
        let capture_worker = thread::Builder::new()
            .name("agoralink-audio-capture".to_string())
            .spawn(move || {
                if let Err(err) = run_integrated_audio_capture_worker(
                    frame_ms,
                    media_clock,
                    capture_stop,
                    Arc::clone(&capture_queue),
                    Arc::clone(&capture_stats),
                ) {
                    if let Ok(mut stats) = capture_stats.lock() {
                        stats.unavailable_reason = Some(err);
                    }
                }
                capture_queue.close();
            })
            .ok();
        if let Ok(mut current) = stats.lock() {
            current.capture_thread_started = capture_worker.is_some();
            current.send_thread_started = send_worker.is_some();
            current.thread_started = capture_worker.is_some() && send_worker.is_some();
            if capture_worker.is_none() || send_worker.is_none() {
                current.unavailable_reason =
                    Some("failed to start isolated audio capture/send workers".to_string());
            }
        }
        IntegratedAudioSender {
            stop,
            capture_worker,
            send_worker,
            stats,
            queue,
        }
    }

    fn run_integrated_audio_capture_worker(
        frame_ms: u32,
        media_clock: MediaClock,
        stop: Arc<AtomicBool>,
        queue: Arc<crate::sender_scheduling::BoundedDropNewestQueue<CapturedAudioChunk>>,
        shared_stats: Arc<Mutex<IntegratedAudioSendStats>>,
    ) -> Result<(), String> {
        let capture = LoopbackCapture::new()?;
        let frame_samples = AUDIO_SAMPLE_RATE * frame_ms / 1000;
        let frame_bytes = frame_samples as usize * AUDIO_BYTES_PER_FRAME;
        let frame_duration_us = u64::from(frame_samples) * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE);
        let mut pending = PendingAudio::with_capacity(frame_bytes * 2);
        let mut timestamp_mapper = QpcMediaTimestampMapper::new();
        let mut local_stats = SendStats::default();
        let mut publish_at = Instant::now();

        while !stop.load(Ordering::SeqCst) {
            if !capture.wait_for_data(Duration::from_millis(20))? {
                local_stats.capture_empty_polls += 1;
            } else {
                drain_capture_packets(
                    &capture,
                    &media_clock,
                    &mut timestamp_mapper,
                    &mut pending,
                    &mut local_stats,
                )?;
            }

            while pending.len() >= frame_bytes {
                let (timestamp_us, payload) = pending
                    .drain_frame(frame_bytes, frame_duration_us)
                    .ok_or_else(|| {
                    "pending audio frame unexpectedly unavailable".to_string()
                })?;
                let chunk = CapturedAudioChunk {
                    timestamp_us,
                    sample_count: frame_samples as u16,
                    payload,
                };
                local_stats
                    .first_media_timestamp_us
                    .get_or_insert(timestamp_us);
                local_stats.last_audio_timestamp_us = Some(timestamp_us);
                let _ = queue.try_push(chunk);
            }

            if publish_at.elapsed() >= Duration::from_millis(100) {
                publish_capture_stats(&shared_stats, &local_stats);
                publish_at = Instant::now();
            }
        }
        publish_capture_stats(&shared_stats, &local_stats);
        Ok(())
    }

    fn run_integrated_audio_send_worker(
        host: String,
        port: u16,
        session_id: u64,
        stop: Arc<AtomicBool>,
        queue: Arc<crate::sender_scheduling::BoundedDropNewestQueue<CapturedAudioChunk>>,
        shared_stats: Arc<Mutex<IntegratedAudioSendStats>>,
    ) -> Result<(), String> {
        let socket =
            UdpSocket::bind("0.0.0.0:0").map_err(|err| format!("audio UDP bind failed: {err}"))?;
        let target = format!("{host}:{port}");
        socket
            .connect(&target)
            .map_err(|err| format!("audio UDP connect failed: {err}"))?;
        let mut sequence = 0u64;
        let mut packets_sent = 0u64;
        let mut bytes_sent = 0u64;
        let mut send_ns_total = 0u64;
        let mut send_ns_max = 0u64;
        let mut worker_ns_total = 0u64;
        let mut worker_ns_max = 0u64;
        let mut worker_loops = 0u64;
        let mut publish_at = Instant::now();

        loop {
            let loop_started = Instant::now();
            let chunk = match queue.pop_timeout(Duration::from_millis(20)) {
                crate::sender_scheduling::QueuePop::Item(chunk) => chunk,
                crate::sender_scheduling::QueuePop::Timeout => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    continue;
                }
                crate::sender_scheduling::QueuePop::Closed => break,
            };
            let packet = AudioPacket {
                session_id,
                sequence,
                timestamp_us: chunk.timestamp_us,
                sample_count: chunk.sample_count,
                sample_rate: AUDIO_SAMPLE_RATE,
                channels: AUDIO_CHANNELS,
                bits_per_sample: AUDIO_BITS_PER_SAMPLE,
                payload: chunk.payload,
            };
            let encoded = packet.encode()?;
            let send_started = Instant::now();
            let sent = socket
                .send(&encoded)
                .map_err(|err| format!("audio UDP send failed: {err}"))?;
            let send_ns = duration_ns(send_started.elapsed());
            send_ns_total = send_ns_total.saturating_add(send_ns);
            send_ns_max = send_ns_max.max(send_ns);
            packets_sent += 1;
            bytes_sent += sent as u64;
            sequence += 1;
            let loop_ns = duration_ns(loop_started.elapsed());
            worker_ns_total = worker_ns_total.saturating_add(loop_ns);
            worker_ns_max = worker_ns_max.max(loop_ns);
            worker_loops += 1;

            if publish_at.elapsed() >= Duration::from_millis(100) {
                publish_audio_send_stats(
                    &shared_stats,
                    packets_sent,
                    bytes_sent,
                    send_ns_total,
                    send_ns_max,
                    worker_ns_total,
                    worker_ns_max,
                    worker_loops,
                );
                publish_at = Instant::now();
            }
        }
        publish_audio_send_stats(
            &shared_stats,
            packets_sent,
            bytes_sent,
            send_ns_total,
            send_ns_max,
            worker_ns_total,
            worker_ns_max,
            worker_loops,
        );
        Ok(())
    }

    fn publish_capture_stats(
        shared_stats: &Arc<Mutex<IntegratedAudioSendStats>>,
        local: &SendStats,
    ) {
        if let Ok(mut stats) = shared_stats.lock() {
            stats.capture_glitches = local.glitch_count;
            stats.capture_empty_polls = local.capture_empty_polls;
            stats.silence_packets = local.silence_packets;
            stats.audio_capture_timestamp_source = local.capture_timestamp_source.to_string();
            stats.audio_capture_qpc_available = local.capture_qpc_available;
            stats.audio_capture_qpc_errors = local.capture_qpc_errors;
            stats.audio_capture_timestamp_discontinuities = local.capture_timestamp_discontinuities;
            stats.first_media_timestamp_us = local.first_media_timestamp_us;
            stats.last_audio_timestamp_us = local.last_audio_timestamp_us;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_audio_send_stats(
        shared_stats: &Arc<Mutex<IntegratedAudioSendStats>>,
        packets_sent: u64,
        bytes_sent: u64,
        send_ns_total: u64,
        send_ns_max: u64,
        worker_ns_total: u64,
        worker_ns_max: u64,
        worker_loops: u64,
    ) {
        if let Ok(mut stats) = shared_stats.lock() {
            stats.packets_sent = packets_sent;
            stats.bytes_sent = bytes_sent;
            stats.audio_send_syscall_ms_avg = average_ns_ms(send_ns_total, packets_sent);
            stats.audio_send_syscall_ms_max = send_ns_max as f64 / 1_000_000.0;
            stats.audio_worker_loop_ms_avg = average_ns_ms(worker_ns_total, worker_loops);
            stats.audio_worker_loop_ms_max = worker_ns_max as f64 / 1_000_000.0;
        }
    }

    fn duration_ns(duration: Duration) -> u64 {
        duration.as_nanos().min(u128::from(u64::MAX)) as u64
    }

    fn average_ns_ms(total_ns: u64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total_ns as f64 / count as f64 / 1_000_000.0
        }
    }

    fn drain_capture_packets(
        capture: &LoopbackCapture,
        media_clock: &MediaClock,
        timestamp_mapper: &mut QpcMediaTimestampMapper,
        pending: &mut PendingAudio,
        stats: &mut SendStats,
    ) -> Result<(), String> {
        loop {
            let packet_size = unsafe {
                capture
                    .capture
                    .GetNextPacketSize()
                    .map_err(|err| format!("GetNextPacketSize failed: {err}"))?
            };
            if packet_size == 0 {
                return Ok(());
            }
            read_one_capture_packet(capture, media_clock, timestamp_mapper, pending, stats)?;
        }
    }

    fn read_one_capture_packet(
        capture: &LoopbackCapture,
        media_clock: &MediaClock,
        timestamp_mapper: &mut QpcMediaTimestampMapper,
        pending: &mut PendingAudio,
        stats: &mut SendStats,
    ) -> Result<(), String> {
        let mut data = ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        let mut _device_position = 0u64;
        let mut qpc_position = 0u64;
        unsafe {
            capture
                .capture
                .GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut _device_position),
                    Some(&mut qpc_position),
                )
                .map_err(|err| format!("IAudioCaptureClient GetBuffer failed: {err}"))?;
        }
        let result = (|| {
            if frames == 0 {
                return Ok(());
            }
            stats.capture_callbacks += 1;
            let captured_at_us = media_clock.now_us();
            let packet_duration_us = u64::from(frames) * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE);
            let fallback_first_sample_us = captured_at_us.saturating_sub(packet_duration_us);
            let (first_sample_timestamp_us, used_qpc) =
                timestamp_mapper.map_capture_timestamp(qpc_position, fallback_first_sample_us);
            stats.capture_qpc_available = timestamp_mapper.qpc_available();
            stats.capture_qpc_errors = timestamp_mapper.qpc_errors();
            stats.capture_timestamp_source = if used_qpc { "qpc" } else { "fallback_now" };
            let timestamp_discontinuity = flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0;
            let data_discontinuity = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
            if timestamp_discontinuity || data_discontinuity {
                pending.reanchor(first_sample_timestamp_us);
                stats.capture_timestamp_discontinuities += 1;
            }
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                stats.silence_packets += 1;
                pending.append_silence(
                    first_sample_timestamp_us,
                    frames as usize * AUDIO_BYTES_PER_FRAME,
                );
            } else {
                let byte_len = frames as usize * capture.format.block_align as usize;
                let bytes = unsafe { slice::from_raw_parts(data.cast::<u8>(), byte_len) };
                let mut converted = Vec::with_capacity(frames as usize * AUDIO_BYTES_PER_FRAME);
                append_converted_pcm16(&mut converted, bytes, frames, &capture.format)?;
                pending.append_bytes(first_sample_timestamp_us, &converted);
            }
            if data_discontinuity {
                stats.discontinuity_count += 1;
                stats.glitch_count += 1;
            }
            if timestamp_discontinuity {
                stats.glitch_count += 1;
            }
            Ok(())
        })();
        unsafe {
            capture
                .capture
                .ReleaseBuffer(frames)
                .map_err(|err| format!("IAudioCaptureClient ReleaseBuffer failed: {err}"))?;
        }
        result
    }

    fn print_send_stats(stats: &SendStats, send_mbps: f64, window_packets: u64, done: bool) {
        let audio_ms_sent = stats.audio_frames_sent as f64 * 1000.0 / AUDIO_SAMPLE_RATE as f64;
        let event = if done {
            "AUDIO_SEND_DONE"
        } else {
            "AUDIO_SEND_STATS"
        };
        println!(
            r#"{{"type":"{}","mode":"audio_send","packets_sent":{},"audio_ms_sent":{:.3},"send_mbps":{:.6},"window_packets_sent":{},"capture_empty_polls":{},"capture_underruns":0,"capture_callbacks":{},"silence_packets":{},"discontinuity_count":{},"glitch_count":{},"sample_rate":{},"channels":{},"bits_per_sample":{},"media_clock":"instant","first_media_timestamp_us":{},"last_audio_timestamp_us":{},"receiver_clock_anchor_us":null,"playout_delay_ms":null,"audio_video_timestamp_delta_ms":null}}"#,
            event,
            stats.packets_sent,
            audio_ms_sent,
            send_mbps,
            window_packets,
            stats.capture_empty_polls,
            stats.capture_callbacks,
            stats.silence_packets,
            stats.discontinuity_count,
            stats.glitch_count,
            AUDIO_SAMPLE_RATE,
            AUDIO_CHANNELS,
            AUDIO_BITS_PER_SAMPLE,
            crate::media_clock::optional_u64_json(stats.first_media_timestamp_us),
            crate::media_clock::optional_u64_json(stats.last_audio_timestamp_us)
        );
        io::stdout().flush().ok();
    }

    #[derive(Debug, Default)]
    struct RecvStats {
        packets_received: u64,
        packets_lost_estimate: u64,
        late_packets: u64,
        audio_callback_empty_polls: u64,
        audio_real_underruns: u64,
        audio_silence_filled_frames: u64,
        packet_loss_concealment_frames: u64,
        frames_rendered: u64,
        bytes_received: u64,
        jitter_buffer_ms_current: f64,
        jitter_buffer_ms_total: f64,
        jitter_buffer_ms_max: f64,
        jitter_buffer_samples: u64,
        latest_audio_packet_timestamp_us: Option<u64>,
        latest_audio_packet_arrival: Option<Instant>,
        audio_samples_rendered_total: u64,
        audio_samples_queued_current: u64,
        audio_samples_dropped_for_latency: u64,
        audio_prestart_silence_frames: u64,
        audio_poststream_silence_frames: u64,
        audio_media_samples_rendered_total: u64,
        audio_latency_drop_discontinuities: u64,
        audio_device_silence_filled_frames: u64,
    }

    #[derive(Debug, Default)]
    struct AudioRenderFill {
        callback_empty_poll: bool,
        real_underrun: bool,
        silence_filled_frames: u64,
        prestart_silence_frames: u64,
        poststream_silence_frames: u64,
        media_frames: u64,
        playhead_discontinuity: bool,
    }

    struct JitterBuffer {
        packets: BTreeMap<u64, AudioPacket>,
        pending_audio: VecDeque<u8>,
        pending_audio_timestamp_us: Option<u64>,
        playhead_timestamp_us: Option<u64>,
        first_arrival: Option<Instant>,
        expected_sequence: Option<u64>,
        started: bool,
        packet_sample_count: u16,
        pending_media_frames: usize,
        playhead_discontinuity_pending: bool,
        target_samples: usize,
        stats: RecvStats,
        media_anchor: ReceiverMediaClockAnchor,
    }

    impl JitterBuffer {
        fn new(playout_delay_ms: u32) -> Self {
            Self {
                packets: BTreeMap::new(),
                pending_audio: VecDeque::new(),
                pending_audio_timestamp_us: None,
                playhead_timestamp_us: None,
                first_arrival: None,
                expected_sequence: None,
                started: false,
                packet_sample_count: 480,
                pending_media_frames: 0,
                playhead_discontinuity_pending: false,
                target_samples: AUDIO_SAMPLE_RATE as usize * playout_delay_ms as usize / 1000,
                stats: RecvStats::default(),
                media_anchor: ReceiverMediaClockAnchor::new(u64::from(playout_delay_ms)),
            }
        }

        fn insert(&mut self, packet: AudioPacket, now: Instant, receiver_clock: &MediaClock) {
            if packet.sample_rate != AUDIO_SAMPLE_RATE
                || packet.channels != AUDIO_CHANNELS
                || packet.bits_per_sample != AUDIO_BITS_PER_SAMPLE
            {
                return;
            }
            self.media_anchor
                .observe_audio(MediaTimestampUs(packet.timestamp_us), receiver_clock);
            if self.first_arrival.is_none() {
                self.first_arrival = Some(now);
                self.expected_sequence = Some(packet.sequence);
                self.packet_sample_count = packet.sample_count;
            }
            if self
                .expected_sequence
                .is_some_and(|expected| packet.sequence < expected)
            {
                self.stats.late_packets += 1;
                return;
            }
            self.stats.latest_audio_packet_timestamp_us = Some(packet.timestamp_us);
            self.stats.latest_audio_packet_arrival = Some(now);
            self.packets.entry(packet.sequence).or_insert(packet);
            self.trim_excess_latency();
            self.record_buffer_depth();
        }

        fn maybe_start(&mut self, now: Instant, jitter_delay: Duration) {
            if self.started {
                return;
            }
            self.prime_contiguous_packets();
            if self.first_arrival.is_some_and(|first| {
                now.duration_since(first) >= jitter_delay
                    && self.buffered_samples() >= self.target_samples
            }) {
                self.started = true;
                self.playhead_timestamp_us = self.pending_audio_timestamp_us;
            }
            self.record_buffer_depth();
        }

        fn fill_render_bytes(&mut self, out: &mut [u8], now: Instant) -> AudioRenderFill {
            let requested_frames = (out.len() / AUDIO_BYTES_PER_FRAME) as u64;
            let mut result = AudioRenderFill::default();
            result.playhead_discontinuity =
                std::mem::take(&mut self.playhead_discontinuity_pending);
            if !self.started {
                out.fill(0);
                result.callback_empty_poll = true;
                result.silence_filled_frames = requested_frames;
                result.prestart_silence_frames = requested_frames;
                self.record_buffer_depth();
                return result;
            }

            let mut offset = 0usize;
            if self.playhead_timestamp_us.is_none() {
                self.playhead_timestamp_us = self.pending_audio_timestamp_us;
            }
            while offset < out.len() {
                if self.pending_audio.is_empty() {
                    self.load_next_packet_or_gap();
                }
                if self.pending_audio.is_empty() {
                    out[offset..].fill(0);
                    let silence_frames = ((out.len() - offset) / AUDIO_BYTES_PER_FRAME) as u64;
                    result.silence_filled_frames += silence_frames;
                    if self.is_poststream(now) {
                        result.poststream_silence_frames += silence_frames;
                    } else {
                        result.real_underrun = true;
                    }
                    break;
                }
                let copy_len = (out.len() - offset).min(self.pending_audio.len());
                for item in &mut out[offset..offset + copy_len] {
                    *item = self.pending_audio.pop_front().unwrap_or(0);
                }
                if let Some(timestamp) = self.pending_audio_timestamp_us.as_mut() {
                    let samples = copy_len / AUDIO_BYTES_PER_FRAME;
                    *timestamp = timestamp
                        .saturating_add(samples as u64 * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE));
                }
                if self.playhead_timestamp_us.is_none() {
                    self.playhead_timestamp_us = self.pending_audio_timestamp_us;
                }
                let copied_frames = copy_len / AUDIO_BYTES_PER_FRAME;
                let media_frames = copied_frames.min(self.pending_media_frames);
                self.pending_media_frames = self.pending_media_frames.saturating_sub(media_frames);
                result.media_frames += media_frames as u64;
                offset += copy_len;
            }
            if let Some(timestamp) = self.playhead_timestamp_us.as_mut() {
                *timestamp = timestamp
                    .saturating_add(result.media_frames * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE));
            }
            self.record_buffer_depth();
            result
        }

        fn prime_contiguous_packets(&mut self) {
            while self.buffered_samples() < self.target_samples {
                let Some(expected) = self.expected_sequence else {
                    return;
                };
                let Some(packet) = self.packets.remove(&expected) else {
                    return;
                };
                self.append_packet(packet);
                self.expected_sequence = Some(expected + 1);
            }
        }

        fn load_next_packet_or_gap(&mut self) {
            let expected = match self.expected_sequence {
                Some(value) => value,
                None => return,
            };
            if let Some(packet) = self.packets.remove(&expected) {
                self.append_packet(packet);
                self.expected_sequence = Some(expected + 1);
                return;
            }
            if self
                .packets
                .keys()
                .next()
                .is_some_and(|next| *next > expected)
            {
                self.stats.packets_lost_estimate += 1;
                let gap_duration_us =
                    u64::from(self.packet_sample_count) * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE);
                self.pending_audio_timestamp_us = self
                    .pending_audio_timestamp_us
                    .or(self.playhead_timestamp_us)
                    .map(|timestamp| timestamp.saturating_add(gap_duration_us));
                self.pending_audio
                    .resize(self.packet_sample_count as usize * AUDIO_BYTES_PER_FRAME, 0);
                self.pending_media_frames += self.packet_sample_count as usize;
                self.stats.packet_loss_concealment_frames += u64::from(self.packet_sample_count);
                self.stats.audio_silence_filled_frames += u64::from(self.packet_sample_count);
                self.expected_sequence = Some(expected + 1);
            }
        }

        fn append_packet(&mut self, packet: AudioPacket) {
            if self.pending_audio.is_empty() {
                self.pending_audio_timestamp_us = Some(packet.timestamp_us);
            }
            self.packet_sample_count = packet.sample_count;
            if self.started
                && self
                    .playhead_timestamp_us
                    .is_some_and(|playhead| packet.timestamp_us > playhead.saturating_add(200_000))
            {
                self.playhead_timestamp_us = Some(packet.timestamp_us);
                self.playhead_discontinuity_pending = true;
            }
            self.pending_media_frames += packet.sample_count as usize;
            self.pending_audio.extend(packet.payload);
        }

        fn buffered_samples(&self) -> usize {
            self.packets
                .values()
                .map(|packet| packet.sample_count as usize)
                .sum::<usize>()
                + self.pending_audio.len() / AUDIO_BYTES_PER_FRAME
        }

        fn jitter_buffer_ms(&self) -> f64 {
            self.buffered_samples() as f64 * 1000.0 / AUDIO_SAMPLE_RATE as f64
        }

        fn record_buffer_depth(&mut self) {
            let queued_samples = self.buffered_samples();
            let depth_ms = queued_samples as f64 * 1000.0 / AUDIO_SAMPLE_RATE as f64;
            self.stats.audio_samples_queued_current = queued_samples as u64;
            self.stats.jitter_buffer_ms_current = depth_ms;
            self.stats.jitter_buffer_ms_total += depth_ms;
            self.stats.jitter_buffer_ms_max = self.stats.jitter_buffer_ms_max.max(depth_ms);
            self.stats.jitter_buffer_samples += 1;
        }

        fn jitter_buffer_ms_avg(&self) -> f64 {
            if self.stats.jitter_buffer_samples == 0 {
                0.0
            } else {
                self.stats.jitter_buffer_ms_total / self.stats.jitter_buffer_samples as f64
            }
        }

        fn is_poststream(&self, now: Instant) -> bool {
            self.stats
                .latest_audio_packet_arrival
                .is_some_and(|arrival| {
                    now.saturating_duration_since(arrival) >= Duration::from_millis(250)
                })
        }

        fn trim_excess_latency(&mut self) {
            let maximum_samples = self
                .target_samples
                .saturating_add(AUDIO_SAMPLE_RATE as usize / 10);
            let queued_samples = self.buffered_samples();
            if queued_samples <= maximum_samples {
                return;
            }
            let samples_to_drop = queued_samples.saturating_sub(self.target_samples);
            let dropped = self.drop_oldest_samples(samples_to_drop);
            self.stats.audio_samples_dropped_for_latency += dropped as u64;
            if dropped > 0 {
                // The next media sample is no longer contiguous with what has
                // already been handed to the device. Suspend/re-anchor the AV
                // master rather than letting video gate against the old head.
                self.playhead_discontinuity_pending = true;
                self.stats.audio_latency_drop_discontinuities = self
                    .stats
                    .audio_latency_drop_discontinuities
                    .saturating_add(1);
                self.playhead_timestamp_us = self.pending_audio_timestamp_us;
            }
        }

        fn drop_oldest_samples(&mut self, mut samples_to_drop: usize) -> usize {
            let mut dropped = 0usize;
            let pending_samples = self.pending_audio.len() / AUDIO_BYTES_PER_FRAME;
            let pending_drop = pending_samples.min(samples_to_drop);
            if pending_drop > 0 {
                let bytes_to_drop = pending_drop * AUDIO_BYTES_PER_FRAME;
                self.pending_audio.drain(..bytes_to_drop);
                self.pending_media_frames = self.pending_media_frames.saturating_sub(pending_drop);
                self.advance_pending_timestamp(pending_drop);
                samples_to_drop -= pending_drop;
                dropped += pending_drop;
            }

            while samples_to_drop > 0 {
                let Some(expected) = self.expected_sequence else {
                    break;
                };
                let packet = self.packets.remove(&expected).or_else(|| {
                    let next = self.packets.keys().next().copied()?;
                    self.expected_sequence = Some(next);
                    self.packets.remove(&next)
                });
                let Some(packet) = packet else {
                    break;
                };
                let packet_samples = packet.sample_count as usize;
                self.expected_sequence = Some(packet.sequence.saturating_add(1));
                samples_to_drop = samples_to_drop.saturating_sub(packet_samples);
                dropped += packet_samples;
            }
            dropped
        }

        fn advance_pending_timestamp(&mut self, samples: usize) {
            if let Some(timestamp) = self.pending_audio_timestamp_us.as_mut() {
                *timestamp = timestamp
                    .saturating_add(samples as u64 * 1_000_000 / u64::from(AUDIO_SAMPLE_RATE));
            }
        }
    }

    pub(super) fn run_jitter_buffer_self_test() -> Result<(), String> {
        let receiver_clock = MediaClock::new();
        let now = Instant::now();
        let mut jitter = JitterBuffer::new(120);

        let mut empty_output = vec![0u8; 480 * AUDIO_BYTES_PER_FRAME];
        let empty = jitter.fill_render_bytes(&mut empty_output, now);
        if !empty.callback_empty_poll || empty.real_underrun {
            return Err("prebuffer silence was counted as an audio underrun".to_string());
        }
        if empty.media_frames != 0 || jitter.playhead_timestamp_us.is_some() {
            return Err("prebuffer silence advanced the audio media playhead".to_string());
        }

        for sequence in 0..12u64 {
            jitter.insert(
                AudioPacket {
                    session_id: 1,
                    sequence,
                    timestamp_us: sequence * 10_000,
                    sample_count: 480,
                    sample_rate: AUDIO_SAMPLE_RATE,
                    channels: AUDIO_CHANNELS,
                    bits_per_sample: AUDIO_BITS_PER_SAMPLE,
                    payload: vec![sequence as u8; 480 * AUDIO_BYTES_PER_FRAME],
                },
                now,
                &receiver_clock,
            );
        }
        jitter.maybe_start(now + Duration::from_millis(120), Duration::from_millis(120));
        if !jitter.started || jitter.jitter_buffer_ms() < 120.0 {
            return Err("audio jitter buffer did not prime to its target".to_string());
        }

        let mut output = vec![0u8; 480 * AUDIO_BYTES_PER_FRAME];
        if jitter.fill_render_bytes(&mut output, now).real_underrun {
            return Err("primed audio jitter buffer unexpectedly underflowed".to_string());
        }
        for _ in 0..11 {
            jitter.fill_render_bytes(&mut output, now);
        }
        if !jitter.fill_render_bytes(&mut output, now).real_underrun {
            return Err(
                "active-stream audio starvation was not counted as an underrun".to_string(),
            );
        }
        let playhead_before_poststream = jitter.playhead_timestamp_us;
        let poststream = jitter.fill_render_bytes(&mut output, now + Duration::from_millis(300));
        if poststream.real_underrun
            || poststream.poststream_silence_frames == 0
            || jitter.playhead_timestamp_us != playhead_before_poststream
        {
            return Err("post-stream silence was counted as an audio underrun".to_string());
        }
        if jitter.stats.jitter_buffer_ms_max < 120.0 {
            return Err("audio jitter buffer maximum depth was not recorded".to_string());
        }

        let mut capped = JitterBuffer::new(120);
        for sequence in 0..60u64 {
            capped.insert(
                AudioPacket {
                    session_id: 2,
                    sequence,
                    timestamp_us: sequence * 10_000,
                    sample_count: 480,
                    sample_rate: AUDIO_SAMPLE_RATE,
                    channels: AUDIO_CHANNELS,
                    bits_per_sample: AUDIO_BITS_PER_SAMPLE,
                    payload: vec![0; 480 * AUDIO_BYTES_PER_FRAME],
                },
                now,
                &receiver_clock,
            );
        }
        if capped.jitter_buffer_ms() > 220.0 || capped.stats.audio_samples_dropped_for_latency == 0
        {
            return Err("audio jitter latency cap did not trim queued samples".to_string());
        }
        if !capped.playhead_discontinuity_pending
            || capped.stats.audio_latency_drop_discontinuities == 0
        {
            return Err("audio jitter latency trim did not force a master re-anchor".to_string());
        }
        Ok(())
    }

    #[cfg(test)]
    mod deterministic_jitter_tests {
        use super::*;

        #[test]
        fn latency_trim_marks_a_timeline_discontinuity_before_reanchor() {
            let receiver_clock = MediaClock::new();
            let now = Instant::now();
            let mut jitter = JitterBuffer::new(120);
            for sequence in 0..60u64 {
                jitter.insert(
                    AudioPacket {
                        session_id: 1,
                        sequence,
                        timestamp_us: sequence * 10_000,
                        sample_count: 480,
                        sample_rate: AUDIO_SAMPLE_RATE,
                        channels: AUDIO_CHANNELS,
                        bits_per_sample: AUDIO_BITS_PER_SAMPLE,
                        payload: vec![0; 480 * AUDIO_BYTES_PER_FRAME],
                    },
                    now,
                    &receiver_clock,
                );
            }
            assert!(jitter.jitter_buffer_ms() <= 220.0);
            assert!(jitter.stats.audio_samples_dropped_for_latency > 0);
            assert!(jitter.playhead_discontinuity_pending);
            assert!(jitter.stats.audio_latency_drop_discontinuities > 0);
            let mut output = vec![0u8; 480 * AUDIO_BYTES_PER_FRAME];
            assert!(
                jitter
                    .fill_render_bytes(&mut output, now)
                    .playhead_discontinuity
            );
        }
    }

    pub fn run_audio_recv_play(config: AudioRecvPlayConfig) -> Result<(), String> {
        let socket = UdpSocket::bind(format!("{}:{}", config.bind, config.port))
            .map_err(|err| format!("audio-recv-play bind failed: {err}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|err| format!("audio-recv-play set_nonblocking failed: {err}"))?;
        let renderer = WasapiRenderer::new()?;
        let jitter_delay = Duration::from_millis(u64::from(config.jitter_buffer_ms));
        let started = Instant::now();
        let deadline = config
            .duration_sec
            .map(|duration| started + Duration::from_secs(duration));
        let receiver_clock = MediaClock::new();
        let mut jitter = JitterBuffer::new(config.jitter_buffer_ms);
        let mut buf = [0u8; 8192];
        let mut stats_at = Instant::now();
        let mut window_bytes = 0u64;
        eprintln!(
            "audio-recv-play bind={}:{} device=\"{}\" jitter_buffer_ms={}",
            config.bind, config.port, renderer.device_name, config.jitter_buffer_ms
        );

        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            loop {
                match socket.recv_from(&mut buf) {
                    Ok((len, _addr)) => {
                        if let Ok(packet) = AudioPacket::decode(&buf[..len]) {
                            jitter.stats.packets_received += 1;
                            jitter.stats.bytes_received += len as u64;
                            window_bytes += len as u64;
                            jitter.insert(packet, Instant::now(), &receiver_clock);
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) => return Err(format!("audio-recv-play recv failed: {err}")),
                }
            }

            let now = Instant::now();
            jitter.maybe_start(now, jitter_delay);
            let available = renderer.available_frames()?;
            if available > 0 {
                let mut render_bytes = vec![0u8; available as usize * AUDIO_BYTES_PER_FRAME];
                let fill = jitter.fill_render_bytes(&mut render_bytes, now);
                renderer.write_pcm16(available, &render_bytes)?;
                jitter.stats.frames_rendered += u64::from(available);
                jitter.stats.audio_samples_rendered_total += u64::from(available);
                jitter.stats.audio_silence_filled_frames += fill.silence_filled_frames;
                jitter.stats.audio_prestart_silence_frames += fill.prestart_silence_frames;
                jitter.stats.audio_poststream_silence_frames += fill.poststream_silence_frames;
                jitter.stats.audio_media_samples_rendered_total += fill.media_frames;
                jitter.stats.audio_device_silence_filled_frames += fill.silence_filled_frames;
                if fill.callback_empty_poll {
                    jitter.stats.audio_callback_empty_polls += 1;
                }
                if fill.real_underrun {
                    jitter.stats.audio_real_underruns += 1;
                }
            }

            if stats_at.elapsed() >= Duration::from_secs(1) {
                let elapsed = stats_at.elapsed().as_secs_f64().max(0.001);
                let recv_mbps = window_bytes as f64 * 8.0 / elapsed / 1_000_000.0;
                print_recv_stats(&jitter, recv_mbps, config.jitter_buffer_ms, false);
                stats_at = Instant::now();
                window_bytes = 0;
            }
            thread::sleep(Duration::from_millis(2));
        }

        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        let recv_mbps = jitter.stats.bytes_received as f64 * 8.0 / elapsed / 1_000_000.0;
        print_recv_stats(&jitter, recv_mbps, config.jitter_buffer_ms, true);
        Ok(())
    }

    pub(crate) fn spawn_integrated_audio_receiver(
        jitter_buffer_ms: u32,
        receiver_clock: MediaClock,
    ) -> IntegratedAudioReceiver {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(IntegratedAudioRecvStats {
            enabled: true,
            jitter_buffer_target_ms: jitter_buffer_ms,
            media_anchor: ReceiverMediaClockAnchor::new(u64::from(jitter_buffer_ms)),
            ..IntegratedAudioRecvStats::default()
        }));
        let master = Arc::new(Mutex::new(AudioMasterClockState::default()));
        let (sender, receiver) = mpsc::sync_channel::<AudioPacket>(256);
        let metrics = Arc::new(AudioIngressMetrics::default());
        let ingest = IntegratedAudioIngest {
            sender,
            metrics: Arc::clone(&metrics),
            startup_packets: Arc::new(Mutex::new(VecDeque::new())),
        };
        let worker_stop = Arc::clone(&stop);
        let worker_stats = Arc::clone(&stats);
        let worker_master = Arc::clone(&master);
        let worker = thread::Builder::new()
            .name("agoralink-audio-play".to_string())
            .spawn(move || {
                if let Err(err) = run_integrated_audio_receiver(
                    receiver,
                    jitter_buffer_ms,
                    receiver_clock,
                    worker_stop,
                    Arc::clone(&worker_stats),
                    worker_master,
                ) {
                    if let Ok(mut stats) = worker_stats.lock() {
                        stats.unavailable_reason = Some(err);
                    }
                }
            })
            .ok();
        if let Ok(mut current) = stats.lock() {
            current.thread_started = worker.is_some();
        }
        IntegratedAudioReceiver {
            ingest,
            stop,
            worker,
            stats,
            metrics,
            master,
        }
    }

    fn run_integrated_audio_receiver(
        receiver: mpsc::Receiver<AudioPacket>,
        jitter_buffer_ms: u32,
        _receiver_clock: MediaClock,
        stop: Arc<AtomicBool>,
        shared_stats: Arc<Mutex<IntegratedAudioRecvStats>>,
        master: Arc<Mutex<AudioMasterClockState>>,
    ) -> Result<(), String> {
        let renderer = WasapiRenderer::new()?;
        let mut jitter = JitterBuffer::new(jitter_buffer_ms);
        let mut timeline = AudioTimeline::new(AUDIO_SAMPLE_RATE);
        let jitter_delay = Duration::from_millis(u64::from(jitter_buffer_ms));

        while !stop.load(Ordering::SeqCst) {
            loop {
                match receiver.try_recv() {
                    Ok(packet) => {
                        if let Ok(mut master) = master.lock() {
                            master.session_matched = true;
                        }
                        timeline.observe_received_packet(packet.timestamp_us);
                        jitter.insert(packet, Instant::now(), &_receiver_clock);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                }
            }

            let now = Instant::now();
            jitter.maybe_start(now, jitter_delay);
            let available = renderer.available_frames()?;
            if available > 0 {
                let mut render_bytes = vec![0u8; available as usize * AUDIO_BYTES_PER_FRAME];
                let fill = jitter.fill_render_bytes(&mut render_bytes, now);
                renderer.write_pcm16(available, &render_bytes)?;
                let device_padding_frames = renderer.padding_frames()?;
                let device_padding_valid = device_padding_frames <= renderer.buffer_frames;
                jitter.stats.frames_rendered += u64::from(available);
                jitter.stats.audio_samples_rendered_total += u64::from(available);
                jitter.stats.audio_silence_filled_frames += fill.silence_filled_frames;
                jitter.stats.audio_prestart_silence_frames += fill.prestart_silence_frames;
                jitter.stats.audio_poststream_silence_frames += fill.poststream_silence_frames;
                jitter.stats.audio_media_samples_rendered_total += fill.media_frames;
                jitter.stats.audio_device_silence_filled_frames += fill.silence_filled_frames;
                if fill.callback_empty_poll {
                    jitter.stats.audio_callback_empty_polls += 1;
                }
                if fill.real_underrun {
                    jitter.stats.audio_real_underruns += 1;
                }
                if fill.playhead_discontinuity {
                    let timeline_snapshot = timeline.mark_discontinuity();
                    if let Ok(mut master) = master.lock() {
                        master.valid = false;
                        master.suspended_until = Some(now + Duration::from_millis(250));
                        master.valid_since = None;
                        master.discontinuity_count = timeline_snapshot.discontinuities;
                        master.reanchor_count = timeline_snapshot.master_reanchors;
                    }
                } else if fill.media_frames > 0 {
                    if let Some(playhead_ts_us) = jitter.playhead_timestamp_us {
                        let timeline_snapshot = timeline.submit_media(
                            playhead_ts_us,
                            fill.media_frames,
                            device_padding_frames,
                        );
                        if let Ok(mut master) = master.lock() {
                            let became_valid = !master.valid;
                            master.started = jitter.started;
                            master.valid = timeline_snapshot.valid && device_padding_valid;
                            master.submitted_playhead_ts_us = timeline_snapshot
                                .submitted_media_timestamp_us
                                .unwrap_or(playhead_ts_us);
                            master.playhead_ts_us = timeline_snapshot
                                .audible_media_timestamp_us
                                .unwrap_or(playhead_ts_us);
                            master.device_padding_frames = device_padding_frames;
                            master.device_padding_valid = device_padding_valid;
                            master.media_samples_submitted_total =
                                timeline_snapshot.media_samples_submitted_total;
                            master.media_samples_audible_estimated_total =
                                timeline_snapshot.media_samples_audible_estimated_total;
                            master.discontinuity_count = timeline_snapshot.discontinuities;
                            master.reanchor_count = timeline_snapshot.master_reanchors;
                            master.updated_at = Some(now);
                            master.last_media_submit_at = master.updated_at;
                            if master.valid && (became_valid || master.valid_since.is_none()) {
                                master.valid_since = Some(now);
                            } else if !master.valid {
                                master.valid_since = None;
                            }
                        }
                    }
                } else if fill.poststream_silence_frames > 0 {
                    timeline.mark_inactive();
                    if let Ok(mut master) = master.lock() {
                        master.valid = false;
                        master.valid_since = None;
                    }
                }
            }

            if let Ok(mut stats) = shared_stats.lock() {
                let timeline_snapshot = timeline.snapshot();
                stats.packets_lost_estimate = jitter.stats.packets_lost_estimate;
                stats.late_packets = jitter.stats.late_packets;
                stats.audio_callback_empty_polls = jitter.stats.audio_callback_empty_polls;
                stats.audio_real_underruns = jitter.stats.audio_real_underruns;
                stats.audio_silence_filled_frames = jitter.stats.audio_silence_filled_frames;
                stats.frames_rendered = jitter.stats.frames_rendered;
                stats.playback_started = jitter.started;
                stats.latest_audio_packet_timestamp_us = timeline_snapshot
                    .latest_received_timestamp_us
                    .or(jitter.stats.latest_audio_packet_timestamp_us);
                stats.audio_queue_depth_ms = jitter.jitter_buffer_ms();
                stats.audio_samples_rendered_total = jitter.stats.audio_samples_rendered_total;
                stats.audio_samples_queued_current = jitter.stats.audio_samples_queued_current;
                stats.audio_samples_dropped_for_latency =
                    jitter.stats.audio_samples_dropped_for_latency;
                stats.audio_prestart_silence_frames = jitter.stats.audio_prestart_silence_frames;
                stats.audio_poststream_silence_frames =
                    jitter.stats.audio_poststream_silence_frames;
                stats.audio_media_samples_rendered_total =
                    jitter.stats.audio_media_samples_rendered_total;
                stats.audio_device_silence_filled_frames =
                    jitter.stats.audio_device_silence_filled_frames;
                if let Ok(master) = master.lock() {
                    let playhead_valid = master.started
                        && master.valid
                        && !master
                            .suspended_until
                            .is_some_and(|until| Instant::now() < until)
                        && !master.last_media_submit_at.is_some_and(|submitted_at| {
                            submitted_at.elapsed() > Duration::from_millis(250)
                        });
                    stats.audio_playhead_valid = playhead_valid;
                    stats.audio_playhead_timestamp_us =
                        playhead_valid.then_some(master.playhead_ts_us);
                    stats.audio_device_padding_frames = master.device_padding_frames;
                    stats.audio_device_padding_ms =
                        master.device_padding_frames as f64 * 1000.0 / AUDIO_SAMPLE_RATE as f64;
                    stats.audio_device_padding_valid = master.device_padding_valid;
                    stats.audio_media_samples_audible_estimated_total =
                        master.media_samples_audible_estimated_total;
                    stats.audio_media_samples_submitted_total =
                        master.media_samples_submitted_total;
                    stats.audio_submitted_timestamp_us =
                        (master.started && master.valid).then_some(master.submitted_playhead_ts_us);
                    stats.audio_playhead_discontinuities = master.discontinuity_count;
                    stats.audio_master_reanchors = master.reanchor_count;
                }
                stats.audio_latency_drop_discontinuities =
                    jitter.stats.audio_latency_drop_discontinuities;
                stats.jitter_buffer_ms_current = jitter.stats.jitter_buffer_ms_current;
                stats.jitter_buffer_ms_avg = jitter.jitter_buffer_ms_avg();
                stats.jitter_buffer_ms_max = jitter.stats.jitter_buffer_ms_max;
                stats.jitter_buffer_ms = stats.jitter_buffer_ms_current;
                stats.media_anchor = jitter.media_anchor;
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    fn print_recv_stats(
        jitter: &JitterBuffer,
        recv_mbps: f64,
        jitter_buffer_target_ms: u32,
        done: bool,
    ) {
        let event = if done {
            "AUDIO_RECV_PLAY_DONE"
        } else {
            "AUDIO_RECV_PLAY_STATS"
        };
        println!(
            r#"{{"type":"{}","mode":"audio_recv_play","packets_received":{},"packets_lost_estimate":{},"jitter_buffer_ms":{:.3},"jitter_buffer_ms_current":{:.3},"jitter_buffer_ms_avg":{:.3},"jitter_buffer_ms_max":{:.3},"jitter_buffer_target_ms":{},"playout_delay_ms":{},"media_clock":"instant",{},"late_packets":{},"audio_callback_empty_polls":{},"audio_real_underruns":{},"audio_output_underruns":{},"audio_silence_filled_frames":{},"frames_rendered":{},"recv_mbps":{:.6},"sample_rate":{},"channels":{},"bits_per_sample":{}}}"#,
            event,
            jitter.stats.packets_received,
            jitter.stats.packets_lost_estimate,
            jitter.jitter_buffer_ms(),
            jitter.stats.jitter_buffer_ms_current,
            jitter.jitter_buffer_ms_avg(),
            jitter.stats.jitter_buffer_ms_max,
            jitter_buffer_target_ms,
            jitter_buffer_target_ms,
            jitter.media_anchor.json_fragment(),
            jitter.stats.late_packets,
            jitter.stats.audio_callback_empty_polls,
            jitter.stats.audio_real_underruns,
            jitter.stats.audio_real_underruns,
            jitter.stats.audio_silence_filled_frames,
            jitter.stats.frames_rendered,
            recv_mbps,
            AUDIO_SAMPLE_RATE,
            AUDIO_CHANNELS,
            AUDIO_BITS_PER_SAMPLE
        );
        io::stdout().flush().ok();
    }

    fn default_render_endpoint() -> Result<IMMDevice, String> {
        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|err| format!("MMDeviceEnumerator creation failed: {err}"))?
        };
        unsafe {
            enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|err| format!("GetDefaultAudioEndpoint failed: {err}"))
        }
    }

    fn endpoint_friendly_name(device: &IMMDevice) -> Option<String> {
        let store = unsafe { device.OpenPropertyStore(STGM_READ).ok()? };
        let prop = unsafe { store.GetValue(&PKEY_Device_FriendlyName).ok()? };
        let value = BSTR::try_from(&prop).ok()?;
        let text = value.to_string();
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn desired_pcm_format() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: AUDIO_CHANNELS,
            nSamplesPerSec: AUDIO_SAMPLE_RATE,
            nAvgBytesPerSec: AUDIO_SAMPLE_RATE * AUDIO_CHANNELS as u32 * 2,
            nBlockAlign: AUDIO_CHANNELS * 2,
            wBitsPerSample: AUDIO_BITS_PER_SAMPLE,
            cbSize: 0,
        }
    }

    fn audio_format_from_ptr(ptr: *const WAVEFORMATEX) -> Result<AudioFormat, String> {
        if ptr.is_null() {
            return Err("null WAVEFORMATEX".to_string());
        }
        let base = unsafe { *ptr };
        let tag = base.wFormatTag;
        let mut bits = base.wBitsPerSample;
        let kind = match u32::from(tag) {
            WAVE_FORMAT_PCM => SampleKind::Pcm,
            x if x == u32::from(WAVE_FORMAT_IEEE_FLOAT) => SampleKind::Float,
            x if x == u32::from(WAVE_FORMAT_EXTENSIBLE) => {
                let ext_ptr = ptr as *const WAVEFORMATEXTENSIBLE;
                let sub_format = unsafe { ptr::addr_of!((*ext_ptr).SubFormat).read_unaligned() };
                if sub_format == PCM_SUBFORMAT {
                    SampleKind::Pcm
                } else if sub_format == FLOAT_SUBFORMAT {
                    bits = 32;
                    SampleKind::Float
                } else {
                    SampleKind::Unsupported
                }
            }
            _ => SampleKind::Unsupported,
        };
        Ok(AudioFormat {
            sample_rate: base.nSamplesPerSec,
            channels: base.nChannels,
            bits_per_sample: bits,
            block_align: base.nBlockAlign,
            sample_kind: kind,
        })
    }

    fn validate_wire_format(format: &AudioFormat) -> Result<(), String> {
        if format.sample_rate != AUDIO_SAMPLE_RATE
            || format.channels != AUDIO_CHANNELS
            || format.bits_per_sample != AUDIO_BITS_PER_SAMPLE
            || format.sample_kind != SampleKind::Pcm
        {
            return Err(format!(
                "audio-send requires 48kHz stereo PCM16 after WASAPI conversion, got {}Hz/{}ch/{:?}/{}bits",
                format.sample_rate, format.channels, format.sample_kind, format.bits_per_sample
            ));
        }
        Ok(())
    }

    fn append_converted_pcm16(
        out: &mut Vec<u8>,
        bytes: &[u8],
        frames: u32,
        format: &AudioFormat,
    ) -> Result<(), String> {
        let channels = format.channels as usize;
        let block_align = format.block_align as usize;
        out.reserve(frames as usize * channels * 2);
        for frame in 0..frames as usize {
            let frame_offset = frame * block_align;
            for channel in 0..channels {
                let sample_offset = frame_offset + sample_offset_for_channel(format, channel);
                let sample = sample_to_i16(bytes, sample_offset, format)?;
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
        Ok(())
    }

    fn sample_offset_for_channel(format: &AudioFormat, channel: usize) -> usize {
        let bytes_per_sample = (format.bits_per_sample as usize).saturating_div(8).max(1);
        channel * bytes_per_sample
    }

    fn sample_to_i16(bytes: &[u8], offset: usize, format: &AudioFormat) -> Result<i16, String> {
        match format.sample_kind {
            SampleKind::Pcm => match format.bits_per_sample {
                16 => {
                    let raw = bytes
                        .get(offset..offset + 2)
                        .ok_or("PCM16 sample out of range")?;
                    Ok(i16::from_le_bytes([raw[0], raw[1]]))
                }
                24 => {
                    let raw = bytes
                        .get(offset..offset + 3)
                        .ok_or("PCM24 sample out of range")?;
                    let value =
                        ((raw[0] as i32) << 8) | ((raw[1] as i32) << 16) | ((raw[2] as i32) << 24);
                    Ok((value >> 16) as i16)
                }
                32 => {
                    let raw = bytes
                        .get(offset..offset + 4)
                        .ok_or("PCM32 sample out of range")?;
                    let value = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                    Ok((value >> 16) as i16)
                }
                other => Err(format!("unsupported PCM bits_per_sample={other}")),
            },
            SampleKind::Float => {
                if format.bits_per_sample != 32 {
                    return Err(format!(
                        "unsupported float bits_per_sample={}",
                        format.bits_per_sample
                    ));
                }
                let raw = bytes
                    .get(offset..offset + 4)
                    .ok_or("float32 sample out of range")?;
                let value = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                Ok((value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
            }
            SampleKind::Unsupported => Err("unsupported sample format".to_string()),
        }
    }

    unsafe fn maybe_free_wave_format(ptr: *mut WAVEFORMATEX, allocated: bool) {
        if allocated && !ptr.is_null() {
            CoTaskMemFree(Some(ptr.cast::<c_void>()));
        }
    }

    #[allow(dead_code)]
    fn _json_device_name(name: &str) -> String {
        json_escape(name)
    }
}

#[cfg(windows)]
pub(crate) use imp::{
    run_audio_recv_play, run_audio_send, spawn_integrated_audio_receiver,
    spawn_integrated_audio_sender,
};

#[cfg(not(windows))]
pub fn run_audio_send(_config: AudioSendConfig) -> Result<(), String> {
    Err("audio-send is only supported on Windows".to_string())
}

#[cfg(not(windows))]
pub fn run_audio_recv_play(_config: AudioRecvPlayConfig) -> Result<(), String> {
    Err("audio-recv-play is only supported on Windows".to_string())
}
