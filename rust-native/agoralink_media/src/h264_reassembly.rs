use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::fec::FecParity;
use crate::h264_annex_b::{summarize_nals, AnnexBParameterSets};
use crate::{MediaPacket, FLAG_FEC, FLAG_FEC_PROTECTED, FLAG_KEYFRAME, STREAM_VIDEO};

pub const DEFAULT_NACK_ITEMS_PER_FRAME: usize = 32;
const NACK_PLAYOUT_DEADLINE_GUARD: Duration = Duration::from_millis(100);
const NACK_CONFIRMED_GAP_DEBOUNCE: Duration = Duration::from_millis(12);
const REPAIR_DEADLINE_GUARD: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Default)]
pub struct TimingMetric {
    pub samples: u64,
    pub total_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

impl TimingMetric {
    pub fn observe(&mut self, duration: Duration) {
        let value = duration.as_secs_f64() * 1000.0;
        if self.samples == 0 {
            self.min_ms = value;
            self.max_ms = value;
        } else {
            self.min_ms = self.min_ms.min(value);
            self.max_ms = self.max_ms.max(value);
        }
        self.samples += 1;
        self.total_ms += value;
    }

    pub fn merge(&mut self, other: Self) {
        if other.samples == 0 {
            return;
        }
        if self.samples == 0 {
            self.min_ms = other.min_ms;
            self.max_ms = other.max_ms;
        } else {
            self.min_ms = self.min_ms.min(other.min_ms);
            self.max_ms = self.max_ms.max(other.max_ms);
        }
        self.samples += other.samples;
        self.total_ms += other.total_ms;
    }

    pub fn avg_ms(self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.total_ms / self.samples as f64
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReassemblyConfig {
    pub frame_timeout: Duration,
    pub reorder_wait: ReorderWait,
    pub max_inflight_frames: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum ReorderWait {
    Auto,
    Fixed(Duration),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReassemblyStats {
    pub packets_received: u64,
    pub packets_invalid: u64,
    pub packets_lost_estimate: u64,
    pub frames_complete: u64,
    pub frames_incomplete_expired: u64,
    pub bytes_received: u64,
    pub last_frame_id: Option<u64>,
    pub last_damaged_frame_id: Option<u64>,
    pub next_decode_frame_id: Option<u64>,
    pub decode_gate_stalls: u64,
    pub decode_gate_gap_events: u64,
    pub decode_gate_gap_to_damage_ms_total: f64,
    pub decode_gate_gap_to_damage_ms_max: f64,
    pub frames_discarded_decode_gate: u64,
    pub reorder_wait_ms: u64,
    pub fec_packets_received: u64,
    pub fec_protected_data_packets_received: u64,
    pub fec_frames_recovered: u64,
    pub fec_packets_recovered: u64,
    pub fec_recovery_failed_multi_missing: u64,
    pub fec_recovery_failed_no_parity: u64,
    pub fec_recovery_failed_invalid: u64,
    pub frames_missing_after_fec: u64,
    pub frames_dropped_after_fec: u64,
    pub reassembly_frames_active: u64,
    pub reassembly_packets_active: u64,
    pub reassembly_packet_slots_reserved: u64,
    pub reassembly_payload_bytes_reserved: u64,
    pub reassembly_budget_rejected_frames: u64,
    pub reassembly_oversize_frames: u64,
    pub reassembly_allocations_estimate: u64,
    pub reassembly_complete_scan_count: u64,
    pub nack_requests_cancelled_by_progress: u64,
}

impl ReassemblyStats {
    pub fn decode_gate_gap_to_damage_ms_avg(self) -> f64 {
        if self.decode_gate_gap_events == 0 {
            0.0
        } else {
            self.decode_gate_gap_to_damage_ms_total / self.decode_gate_gap_events as f64
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NackCollectionStats {
    pub candidate_frames: u64,
    pub suppressed_progressing_frames: u64,
    pub suppressed_too_early: u64,
    pub suppressed_already_requested: u64,
    pub suppressed_item_limit: u64,
    pub items_deduped: u64,
    pub requested_frames: u64,
    pub items_per_requested_frame_total: u64,
    pub items_per_requested_frame_max: u64,
    pub candidates_first_round: u64,
    pub candidates_late_discovery: u64,
    pub missing_first_detected_age: TimingMetric,
    pub missing_first_detected_to_deadline: TimingMetric,
    pub first_nack_to_deadline: TimingMetric,
    pub first_nack_age: TimingMetric,
    pub first_round_budget: TimingMetric,
    pub second_round_budget: TimingMetric,
}

#[derive(Debug, Default)]
pub struct NackCollection {
    pub items: Vec<crate::repair::PacketKey>,
    pub stats: NackCollectionStats,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DamagedGopStats {
    pub damaged_gop_count: u64,
    pub frames_discarded_damaged_gop: u64,
    pub frames_discarded_waiting_keyframe: u64,
    pub recovery_completed: u64,
    pub recovery_wait_ms_total: f64,
    pub recovery_wait_ms_max: f64,
    pub recovery_wait_frames_total: u64,
    pub recovery_wait_frames_max: u64,
    pub waiting_keyframe_entries: u64,
    pub waiting_keyframe_exits: u64,
    pub idr_frames_received: u64,
    pub idr_frames_used_for_recovery: u64,
    pub non_idr_frames_discarded_waiting: u64,
}

impl DamagedGopStats {
    pub fn recovery_wait_ms_avg(self) -> f64 {
        if self.recovery_completed == 0 {
            0.0
        } else {
            self.recovery_wait_ms_total / self.recovery_completed as f64
        }
    }

    pub fn recovery_wait_frames_avg(self) -> f64 {
        if self.recovery_completed == 0 {
            0.0
        } else {
            self.recovery_wait_frames_total as f64 / self.recovery_completed as f64
        }
    }
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub frame_id: u64,
    pub flags: u16,
    pub timestamp_ms: u64,
    pub bytes: Vec<u8>,
}

impl EncodedFrame {
    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }

    pub fn is_idr(&self) -> bool {
        summarize_nals(&self.bytes).has_idr_slice
    }
}

pub struct DamagedGopTracker {
    enabled: bool,
    waiting_keyframe: bool,
    recovery_started_at: Option<Instant>,
    recovery_started_frame_id: Option<u64>,
    parameter_sets: AnnexBParameterSets,
    stats: DamagedGopStats,
}

impl DamagedGopTracker {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            waiting_keyframe: false,
            recovery_started_at: None,
            recovery_started_frame_id: None,
            parameter_sets: AnnexBParameterSets::default(),
            stats: DamagedGopStats::default(),
        }
    }

    pub fn mark_damaged(&mut self, now: Instant, damaged_frame_id: Option<u64>) -> bool {
        if !self.enabled || self.waiting_keyframe {
            return false;
        }
        self.waiting_keyframe = true;
        self.recovery_started_at = Some(now);
        self.recovery_started_frame_id = damaged_frame_id;
        self.stats.damaged_gop_count += 1;
        self.stats.waiting_keyframe_entries += 1;
        true
    }

    pub fn prepare_frame(&mut self, mut frame: EncodedFrame, now: Instant) -> Option<EncodedFrame> {
        let summary = summarize_nals(&frame.bytes);
        self.parameter_sets.update_from(&frame.bytes);
        if summary.has_idr_slice {
            self.stats.idr_frames_received += 1;
        }

        if !self.enabled || !self.waiting_keyframe {
            if summary.has_idr_slice && (!summary.has_sps || !summary.has_pps) {
                if let Ok(repaired) = self
                    .parameter_sets
                    .prepend_missing_to_keyframe(&frame.bytes)
                {
                    frame.bytes = repaired;
                }
            }
            return Some(frame);
        }
        if !summary.has_idr_slice {
            self.stats.frames_discarded_damaged_gop += 1;
            self.stats.frames_discarded_waiting_keyframe += 1;
            self.stats.non_idr_frames_discarded_waiting += 1;
            return None;
        }

        frame.bytes = match self
            .parameter_sets
            .prepend_missing_to_keyframe(&frame.bytes)
        {
            Ok(bytes) => bytes,
            Err(_) => {
                self.stats.frames_discarded_damaged_gop += 1;
                self.stats.frames_discarded_waiting_keyframe += 1;
                return None;
            }
        };

        if let Some(started_at) = self.recovery_started_at.take() {
            let wait_ms = now.duration_since(started_at).as_secs_f64() * 1000.0;
            self.stats.recovery_completed += 1;
            self.stats.recovery_wait_ms_total += wait_ms;
            self.stats.recovery_wait_ms_max = self.stats.recovery_wait_ms_max.max(wait_ms);
        }
        if let Some(started_frame_id) = self.recovery_started_frame_id.take() {
            let wait_frames = frame.frame_id.saturating_sub(started_frame_id);
            self.stats.recovery_wait_frames_total += wait_frames;
            self.stats.recovery_wait_frames_max =
                self.stats.recovery_wait_frames_max.max(wait_frames);
        }
        self.waiting_keyframe = false;
        self.stats.waiting_keyframe_exits += 1;
        self.stats.idr_frames_used_for_recovery += 1;
        Some(frame)
    }

    pub fn discard_queued_frames(&mut self, count: u64) {
        if self.enabled && self.waiting_keyframe {
            self.stats.frames_discarded_damaged_gop += count;
            self.stats.frames_discarded_waiting_keyframe += count;
        }
    }

    pub fn waiting_keyframe(&self) -> bool {
        self.waiting_keyframe
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn stats(&self) -> DamagedGopStats {
        self.stats
    }

    pub fn reset_for_profile_change(&mut self) {
        self.waiting_keyframe = false;
        self.recovery_started_at = None;
        self.recovery_started_frame_id = None;
        self.parameter_sets = AnnexBParameterSets::default();
    }
}

struct FrameAssembly {
    packet_count: u16,
    reserved_payload_bytes: usize,
    packets: Vec<Option<Vec<u8>>>,
    received_count: u16,
    missing_count: u16,
    received_bytes: usize,
    first_seen: Instant,
    last_packet_arrival_time: Instant,
    last_progress_time: Instant,
    highest_received_packet_index: u16,
    flags: u16,
    timestamp_ms: u64,
    fec_parity: Option<FecParity>,
    fec_expected: bool,
    fec_invalid: bool,
    last_nack_at: Option<Instant>,
    nack_rounds: u8,
    nack_requested_at: Vec<Option<Instant>>,
    missing_first_detected_at: Option<Instant>,
    missing_detection_reported: bool,
    first_nack_at: Option<Instant>,
}

impl FrameAssembly {
    fn new(
        packet_count: u16,
        reserved_payload_bytes: usize,
        now: Instant,
        timestamp_ms: u64,
        fec_expected: bool,
    ) -> Self {
        Self {
            packet_count,
            reserved_payload_bytes,
            packets: (0..packet_count).map(|_| None).collect(),
            received_count: 0,
            missing_count: packet_count,
            received_bytes: 0,
            first_seen: now,
            last_packet_arrival_time: now,
            last_progress_time: now,
            highest_received_packet_index: 0,
            flags: 0,
            timestamp_ms,
            fec_parity: None,
            fec_expected,
            fec_invalid: false,
            last_nack_at: None,
            nack_rounds: 0,
            nack_requested_at: (0..packet_count).map(|_| None).collect(),
            missing_first_detected_at: None,
            missing_detection_reported: false,
            first_nack_at: None,
        }
    }

    fn confirmed_missing_count(&self) -> u16 {
        if self.received_count == 0 {
            return 0;
        }
        let covered_packets = self.highest_received_packet_index.saturating_add(1);
        covered_packets.saturating_sub(self.received_count)
    }
}

pub struct H264Reassembler {
    config: ReassemblyConfig,
    session_id: Option<u64>,
    inflight: HashMap<u64, FrameAssembly>,
    complete: BTreeMap<u64, EncodedFrame>,
    skipped: BTreeSet<u64>,
    next_frame: Option<u64>,
    highest_seen_frame: Option<u64>,
    blocked_since: Option<Instant>,
    last_timing_frame: Option<(u64, u64)>,
    observed_frame_interval_ms: Option<f64>,
    repair_window: Option<Duration>,
    inflight_packet_slots: usize,
    inflight_payload_bytes: usize,
    stats: ReassemblyStats,
}

impl H264Reassembler {
    pub fn new(config: ReassemblyConfig) -> Result<Self, String> {
        if config.frame_timeout.is_zero() {
            return Err("frame timeout must be greater than zero".to_string());
        }
        if matches!(config.reorder_wait, ReorderWait::Fixed(wait) if wait.is_zero()) {
            return Err("reorder wait must be greater than zero".to_string());
        }
        if config.max_inflight_frames == 0 {
            return Err("max inflight frames must be greater than zero".to_string());
        }
        let initial_reorder_wait_ms = match config.reorder_wait {
            ReorderWait::Auto => 50,
            ReorderWait::Fixed(wait) => duration_ms_rounded(wait),
        };
        Ok(Self {
            config,
            session_id: None,
            inflight: HashMap::new(),
            complete: BTreeMap::new(),
            skipped: BTreeSet::new(),
            next_frame: None,
            highest_seen_frame: None,
            blocked_since: None,
            last_timing_frame: None,
            observed_frame_interval_ms: None,
            repair_window: None,
            inflight_packet_slots: 0,
            inflight_payload_bytes: 0,
            stats: ReassemblyStats {
                reorder_wait_ms: initial_reorder_wait_ms,
                ..ReassemblyStats::default()
            },
        })
    }

    pub fn accept_datagram(
        &mut self,
        datagram: &[u8],
        now: Instant,
    ) -> Result<Vec<EncodedFrame>, String> {
        self.stats.bytes_received += datagram.len() as u64;
        let packet = match MediaPacket::decode(datagram) {
            Ok(packet) => packet,
            Err(err) => {
                self.stats.packets_invalid += 1;
                return Err(err);
            }
        };
        self.accept_packet(packet, now);
        Ok(self.take_ready(now, false))
    }

    pub fn expire(&mut self, now: Instant) -> Vec<EncodedFrame> {
        let expired: Vec<u64> = self
            .inflight
            .iter()
            .filter_map(|(frame_id, frame)| {
                (now.duration_since(frame.first_seen) >= self.config.frame_timeout)
                    .then_some(*frame_id)
            })
            .collect();
        for frame_id in expired {
            self.expire_inflight_frame(frame_id);
        }
        self.take_ready(now, false)
    }

    pub fn finish(&mut self) -> Vec<EncodedFrame> {
        let inflight: Vec<u64> = self.inflight.keys().copied().collect();
        for frame_id in inflight {
            self.expire_inflight_frame(frame_id);
        }
        self.take_ready(Instant::now() + self.config.frame_timeout, true)
    }

    pub fn stats(&self) -> ReassemblyStats {
        self.stats
    }

    pub fn session_id(&self) -> Option<u64> {
        self.session_id
    }

    pub fn switch_session(&mut self, session_id: u64) -> Result<(), String> {
        if session_id == 0 {
            return Err("reassembly session_id must be non-zero".to_string());
        }
        self.session_id = Some(session_id);
        self.inflight.clear();
        self.complete.clear();
        self.skipped.clear();
        self.next_frame = None;
        self.highest_seen_frame = None;
        self.blocked_since = None;
        self.last_timing_frame = None;
        self.observed_frame_interval_ms = None;
        self.inflight_packet_slots = 0;
        self.inflight_payload_bytes = 0;
        self.stats.next_decode_frame_id = None;
        self.stats.reassembly_frames_active = 0;
        self.stats.reassembly_packets_active = 0;
        self.stats.reassembly_packet_slots_reserved = 0;
        self.stats.reassembly_payload_bytes_reserved = 0;
        Ok(())
    }

    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    pub fn has_inflight_frame(&self, frame_id: u64) -> bool {
        self.inflight.contains_key(&frame_id)
    }

    pub fn set_repair_window(&mut self, repair_window: Option<Duration>) {
        self.repair_window = repair_window.filter(|window| !window.is_zero());
    }

    pub fn repair_deadline_remaining(&self, frame_id: u64, now: Instant) -> Option<Duration> {
        let frame = self.inflight.get(&frame_id)?;
        Some(
            self.frame_repair_deadline(frame)
                .saturating_duration_since(now),
        )
    }

    pub fn collect_nack_items(
        &mut self,
        now: Instant,
        delay: Duration,
        repeat: Duration,
        max_rounds: u8,
        repair_window: Duration,
        max_items_per_frame: usize,
    ) -> NackCollection {
        let mut collection = NackCollection::default();
        let highest_complete_frame = self.complete.keys().next_back().copied();
        let highest_seen_frame = self.highest_seen_frame;
        let max_items_per_frame = max_items_per_frame.max(1);
        for (&frame_id, frame) in &mut self.inflight {
            if frame.missing_count == 0 || frame.nack_rounds >= max_rounds {
                continue;
            }
            collection.stats.candidate_frames += 1;
            let age = now.saturating_duration_since(frame.first_seen);
            let since_progress = now.saturating_duration_since(frame.last_progress_time);
            let higher_complete_frame_seen =
                highest_complete_frame.is_some_and(|highest| highest > frame_id);
            let higher_frame_progress_seen =
                highest_seen_frame.is_some_and(|highest| highest > frame_id);
            let repair_window = self.config.frame_timeout.min(repair_window);
            let deadline_guard = REPAIR_DEADLINE_GUARD.min(repair_window / 2);
            let repair_deadline = repair_window.saturating_sub(deadline_guard);
            if age >= repair_deadline {
                continue;
            }
            let remaining_repair_time = repair_deadline.saturating_sub(age);
            let near_playout_deadline = remaining_repair_time <= NACK_PLAYOUT_DEADLINE_GUARD;
            let stalled = since_progress >= delay;
            let confirmed_gap = frame.confirmed_missing_count() > 0
                || frame.highest_received_packet_index.saturating_add(1) == frame.packet_count;
            let gap_evidence =
                confirmed_gap || higher_frame_progress_seen || higher_complete_frame_seen;
            let gap_debounce = delay.min(NACK_CONFIRMED_GAP_DEBOUNCE);
            let confirmed_gap_ready = gap_evidence && since_progress >= gap_debounce;
            if !stalled && !confirmed_gap_ready && !near_playout_deadline {
                if age < delay {
                    collection.stats.suppressed_too_early += 1;
                } else {
                    collection.stats.suppressed_progressing_frames += 1;
                }
                continue;
            }
            if frame.missing_first_detected_at.is_none()
                && (confirmed_gap || stalled || higher_frame_progress_seen || near_playout_deadline)
            {
                frame.missing_first_detected_at = Some(now);
                frame.missing_detection_reported = false;
            }
            if let Some(detected_at) = frame.missing_first_detected_at {
                if !frame.missing_detection_reported {
                    let detected_age = detected_at.saturating_duration_since(frame.first_seen);
                    let detected_budget = repair_deadline.saturating_sub(detected_age);
                    collection
                        .stats
                        .missing_first_detected_age
                        .observe(detected_age);
                    collection
                        .stats
                        .missing_first_detected_to_deadline
                        .observe(detected_budget);
                    if detected_budget <= NACK_PLAYOUT_DEADLINE_GUARD {
                        collection.stats.candidates_late_discovery += 1;
                    }
                    frame.missing_detection_reported = true;
                }
            }
            if frame.nack_rounds == 0 {
                collection.stats.candidates_first_round += 1;
            }
            if frame
                .last_nack_at
                .is_some_and(|last| now.saturating_duration_since(last) < repeat)
            {
                collection.stats.suppressed_already_requested += u64::from(frame.missing_count);
                collection.stats.items_deduped += u64::from(frame.missing_count);
                continue;
            }
            let mut frame_items = 0u64;
            for (packet_index, packet) in frame.packets.iter().enumerate() {
                if packet.is_some() {
                    continue;
                }
                if frame.nack_requested_at[packet_index]
                    .is_some_and(|last| now.saturating_duration_since(last) < repeat)
                {
                    collection.stats.suppressed_already_requested += 1;
                    collection.stats.items_deduped += 1;
                    continue;
                }
                if frame_items as usize >= max_items_per_frame {
                    collection.stats.suppressed_item_limit += 1;
                    continue;
                }
                frame.nack_requested_at[packet_index] = Some(now);
                collection.items.push(crate::repair::PacketKey {
                    frame_id,
                    packet_index: packet_index as u16,
                });
                frame_items += 1;
            }
            if frame_items > 0 {
                frame.last_nack_at = Some(now);
                if frame.first_nack_at.is_none() {
                    frame.first_nack_at = Some(now);
                    collection
                        .stats
                        .first_nack_to_deadline
                        .observe(remaining_repair_time);
                    collection.stats.first_nack_age.observe(age);
                    collection
                        .stats
                        .first_round_budget
                        .observe(remaining_repair_time);
                } else if frame.nack_rounds == 1 {
                    collection
                        .stats
                        .second_round_budget
                        .observe(remaining_repair_time);
                }
                frame.nack_rounds += 1;
                collection.stats.requested_frames += 1;
                collection.stats.items_per_requested_frame_total += frame_items;
                collection.stats.items_per_requested_frame_max = collection
                    .stats
                    .items_per_requested_frame_max
                    .max(frame_items);
            }
        }
        collection
    }

    fn frame_repair_deadline(&self, frame: &FrameAssembly) -> Instant {
        let repair_window = self
            .repair_window
            .unwrap_or(self.config.frame_timeout)
            .min(self.config.frame_timeout);
        let guard = REPAIR_DEADLINE_GUARD.min(repair_window / 2);
        frame
            .first_seen
            .checked_add(repair_window.saturating_sub(guard))
            .unwrap_or(frame.first_seen)
    }

    pub fn completed_waiting_len(&self) -> usize {
        self.complete.len()
    }

    pub fn reorder_wait_ms(&self) -> u64 {
        self.stats.reorder_wait_ms
    }

    fn accept_packet(&mut self, packet: MediaPacket, now: Instant) {
        self.stats.packets_received += 1;
        if packet.stream_id != STREAM_VIDEO {
            self.stats.packets_invalid += 1;
            return;
        }
        match self.session_id {
            Some(session_id) if session_id != packet.session_id => {
                self.stats.packets_invalid += 1;
                return;
            }
            None => self.session_id = Some(packet.session_id),
            _ => {}
        }

        let is_fec = packet.flags & FLAG_FEC != 0;
        let is_fec_protected = !is_fec && packet.flags & FLAG_FEC_PROTECTED != 0;
        if is_fec {
            self.stats.fec_packets_received += 1;
        } else if is_fec_protected {
            self.stats.fec_protected_data_packets_received += 1;
        }

        self.next_frame.get_or_insert(packet.frame_id);
        self.stats.next_decode_frame_id = self.next_frame;
        self.highest_seen_frame = Some(
            self.highest_seen_frame
                .map_or(packet.frame_id, |current| current.max(packet.frame_id)),
        );
        if self.next_frame.is_some_and(|next| packet.frame_id < next) {
            return;
        }
        if is_fec {
            self.accept_fec_packet(packet, now);
        } else {
            self.accept_data_packet(packet, now);
        }
    }

    fn accept_data_packet(&mut self, packet: MediaPacket, now: Instant) {
        let is_fec_protected = packet.flags & FLAG_FEC_PROTECTED != 0;
        if !self.ensure_inflight_frame(
            packet.frame_id,
            packet.packet_count,
            now,
            packet.timestamp_ms,
            is_fec_protected,
        ) {
            return;
        }
        if self.inflight.get(&packet.frame_id).is_some_and(|frame| {
            frame.received_bytes.saturating_add(packet.payload.len()) > crate::MAX_VIDEO_FRAME_BYTES
        }) {
            self.stats.reassembly_oversize_frames =
                self.stats.reassembly_oversize_frames.saturating_add(1);
            self.stats.packets_invalid = self.stats.packets_invalid.saturating_add(1);
            self.expire_inflight_frame(packet.frame_id);
            return;
        }
        let entry = self
            .inflight
            .get_mut(&packet.frame_id)
            .expect("reserved frame must exist before packet insertion");
        if entry.packet_count != packet.packet_count {
            self.stats.packets_invalid += 1;
            return;
        }
        let index = packet.packet_index as usize;
        if index >= entry.packets.len() {
            self.stats.packets_invalid += 1;
            return;
        }
        entry.last_packet_arrival_time = now;
        entry.flags |= packet.flags;
        entry.fec_expected |= is_fec_protected;
        let had_unrequested_gap =
            entry.missing_first_detected_at.is_some() && entry.first_nack_at.is_none();
        if entry.packets[index].is_none() {
            entry.received_bytes += packet.payload.len();
            entry.packets[index] = Some(packet.payload);
            entry.received_count += 1;
            entry.missing_count -= 1;
            entry.last_progress_time = now;
            entry.highest_received_packet_index =
                entry.highest_received_packet_index.max(packet.packet_index);
            entry.nack_requested_at[index] = None;
            self.stats.reassembly_packets_active += 1;
            self.stats.reassembly_allocations_estimate += 1;
        }
        let confirmed_missing = entry.confirmed_missing_count();
        if confirmed_missing > 0 {
            entry.missing_first_detected_at.get_or_insert(now);
        } else if had_unrequested_gap {
            entry.missing_first_detected_at = None;
            entry.missing_detection_reported = false;
            self.stats.nack_requests_cancelled_by_progress += 1;
        }

        self.try_complete_frame(packet.frame_id);
    }

    fn accept_fec_packet(&mut self, packet: MediaPacket, now: Instant) {
        let parity = match FecParity::decode_owned(packet.payload) {
            Ok(parity) if parity.data_packet_count == packet.packet_count => parity,
            Ok(_) | Err(_) => {
                self.stats.fec_recovery_failed_invalid += 1;
                if let Some(frame) = self.inflight.get_mut(&packet.frame_id) {
                    frame.fec_invalid = true;
                }
                return;
            }
        };
        if !self.ensure_inflight_frame(
            packet.frame_id,
            parity.data_packet_count,
            now,
            packet.timestamp_ms,
            true,
        ) {
            return;
        }
        let entry = self
            .inflight
            .get_mut(&packet.frame_id)
            .expect("reserved frame must exist before FEC insertion");
        if entry.packet_count != parity.data_packet_count {
            self.stats.fec_recovery_failed_invalid += 1;
            return;
        }
        entry.flags |= packet.flags & !FLAG_FEC;
        if entry.fec_parity.is_none() {
            self.stats.reassembly_packets_active += 1;
            self.stats.reassembly_allocations_estimate += 1;
        }
        entry.last_packet_arrival_time = now;
        entry.fec_parity = Some(parity);
        entry.fec_expected = true;
        self.try_complete_frame(packet.frame_id);
    }

    fn try_complete_frame(&mut self, frame_id: u64) {
        let mut recovered_packet = false;
        let mut invalid_fec = false;
        if let Some(frame) = self.inflight.get_mut(&frame_id) {
            if frame.missing_count == 1 {
                if let Some(parity) = frame.fec_parity.as_ref() {
                    self.stats.reassembly_complete_scan_count += 1;
                    if let Some(missing_index) = frame.packets.iter().position(Option::is_none) {
                        match recover_missing_payload(&frame.packets, parity, missing_index) {
                            Ok(payload) => {
                                frame.received_bytes += payload.len();
                                frame.packets[missing_index] = Some(payload);
                                frame.received_count += 1;
                                frame.missing_count = 0;
                                frame.nack_requested_at[missing_index] = None;
                                self.stats.reassembly_packets_active += 1;
                                self.stats.reassembly_allocations_estimate += 1;
                                recovered_packet = true;
                            }
                            Err(_) => invalid_fec = true,
                        }
                    }
                }
            }
        }
        if invalid_fec {
            self.stats.fec_recovery_failed_invalid += 1;
            if let Some(frame) = self.inflight.get_mut(&frame_id) {
                frame.fec_parity = None;
                frame.fec_invalid = true;
            }
        }
        if recovered_packet {
            self.stats.fec_frames_recovered += 1;
            self.stats.fec_packets_recovered += 1;
        }

        let complete = self
            .inflight
            .get(&frame_id)
            .is_some_and(|frame| frame.missing_count == 0);
        if complete {
            self.finish_complete_frame(frame_id);
        }
    }

    fn finish_complete_frame(&mut self, frame_id: u64) {
        if let Some(frame) = self.remove_inflight_frame(frame_id) {
            let mut bytes = Vec::with_capacity(frame.received_bytes);
            self.stats.reassembly_allocations_estimate += 1;
            for payload in frame.packets.into_iter().flatten() {
                bytes.extend_from_slice(&payload);
            }
            self.complete.insert(
                frame_id,
                EncodedFrame {
                    frame_id,
                    flags: frame.flags & !(FLAG_FEC | FLAG_FEC_PROTECTED),
                    timestamp_ms: frame.timestamp_ms,
                    bytes,
                },
            );
            self.stats.frames_complete += 1;
            self.stats.last_frame_id = Some(frame_id);
            self.observe_frame_timing(frame_id, frame.timestamp_ms);
        }
    }

    fn observe_frame_timing(&mut self, frame_id: u64, timestamp_ms: u64) {
        if let Some((previous_id, previous_timestamp_ms)) = self.last_timing_frame {
            if frame_id > previous_id && timestamp_ms > previous_timestamp_ms {
                let frame_delta = frame_id - previous_id;
                let sample_ms = (timestamp_ms - previous_timestamp_ms) as f64 / frame_delta as f64;
                if (1.0..=1000.0).contains(&sample_ms) {
                    self.observed_frame_interval_ms = Some(
                        self.observed_frame_interval_ms
                            .map_or(sample_ms, |current| current * 0.8 + sample_ms * 0.2),
                    );
                }
                self.last_timing_frame = Some((frame_id, timestamp_ms));
            }
        } else {
            self.last_timing_frame = Some((frame_id, timestamp_ms));
        }
        self.stats.reorder_wait_ms = duration_ms_rounded(self.current_reorder_wait());
    }

    fn current_reorder_wait(&self) -> Duration {
        match self.config.reorder_wait {
            ReorderWait::Fixed(wait) => wait,
            ReorderWait::Auto => {
                let wait_ms =
                    (self.observed_frame_interval_ms.unwrap_or(20.0) * 2.5).clamp(35.0, 100.0);
                Duration::from_micros((wait_ms * 1000.0).round() as u64)
            }
        }
    }

    fn expire_inflight_frame(&mut self, frame_id: u64) {
        if let Some(frame) = self.remove_inflight_frame(frame_id) {
            let missing_count = frame.missing_count;
            self.stats.frames_incomplete_expired += 1;
            self.stats.packets_lost_estimate += u64::from(missing_count);
            self.stats.frames_missing_after_fec += 1;
            self.stats.frames_dropped_after_fec += 1;
            if !frame.fec_invalid {
                if frame.fec_parity.is_some() && missing_count > 1 {
                    self.stats.fec_recovery_failed_multi_missing += 1;
                } else if frame.fec_parity.is_none() && frame.fec_expected {
                    self.stats.fec_recovery_failed_no_parity += 1;
                }
            }
            self.skipped.insert(frame_id);
            self.stats.last_damaged_frame_id = Some(frame_id);
        }
    }

    fn remove_inflight_frame(&mut self, frame_id: u64) -> Option<FrameAssembly> {
        let frame = self.inflight.remove(&frame_id)?;
        self.inflight_packet_slots = self
            .inflight_packet_slots
            .saturating_sub(usize::from(frame.packet_count));
        self.inflight_payload_bytes = self
            .inflight_payload_bytes
            .saturating_sub(frame.reserved_payload_bytes);
        self.stats.reassembly_frames_active = self.stats.reassembly_frames_active.saturating_sub(1);
        let active_packets =
            u64::from(frame.received_count) + u64::from(frame.fec_parity.is_some());
        self.stats.reassembly_packets_active = self
            .stats
            .reassembly_packets_active
            .saturating_sub(active_packets);
        self.stats.reassembly_packet_slots_reserved = self.inflight_packet_slots as u64;
        self.stats.reassembly_payload_bytes_reserved = self.inflight_payload_bytes as u64;
        Some(frame)
    }

    fn ensure_inflight_frame(
        &mut self,
        frame_id: u64,
        packet_count: u16,
        now: Instant,
        timestamp_ms: u64,
        fec_expected: bool,
    ) -> bool {
        if self.inflight.contains_key(&frame_id) {
            return true;
        }
        let packet_slots = usize::from(packet_count);
        let Some(reserved_payload_bytes) = frame_payload_reservation(packet_count) else {
            self.reject_new_frame_for_budget(frame_id, packet_count);
            return false;
        };
        let frame_limit = self
            .config
            .max_inflight_frames
            .min(crate::MAX_INFLIGHT_FRAMES);
        let slots_fit = self
            .inflight_packet_slots
            .checked_add(packet_slots)
            .is_some_and(|total| total <= crate::MAX_INFLIGHT_PACKET_SLOTS);
        let bytes_fit = self
            .inflight_payload_bytes
            .checked_add(reserved_payload_bytes)
            .is_some_and(|total| total <= crate::MAX_INFLIGHT_PAYLOAD_BYTES);
        if self.inflight.len() >= frame_limit || !slots_fit || !bytes_fit {
            self.reject_new_frame_for_budget(frame_id, packet_count);
            return false;
        }

        self.inflight_packet_slots += packet_slots;
        self.inflight_payload_bytes += reserved_payload_bytes;
        self.stats.reassembly_frames_active = self.stats.reassembly_frames_active.saturating_add(1);
        self.stats.reassembly_packet_slots_reserved = self.inflight_packet_slots as u64;
        self.stats.reassembly_payload_bytes_reserved = self.inflight_payload_bytes as u64;
        self.stats.reassembly_allocations_estimate =
            self.stats.reassembly_allocations_estimate.saturating_add(1);
        self.inflight.insert(
            frame_id,
            FrameAssembly::new(
                packet_count,
                reserved_payload_bytes,
                now,
                timestamp_ms,
                fec_expected,
            ),
        );
        true
    }

    fn reject_new_frame_for_budget(&mut self, frame_id: u64, packet_count: u16) {
        self.stats.packets_invalid = self.stats.packets_invalid.saturating_add(1);
        self.stats.reassembly_budget_rejected_frames = self
            .stats
            .reassembly_budget_rejected_frames
            .saturating_add(1);
        self.stats.frames_incomplete_expired =
            self.stats.frames_incomplete_expired.saturating_add(1);
        self.stats.frames_missing_after_fec = self.stats.frames_missing_after_fec.saturating_add(1);
        self.stats.frames_dropped_after_fec = self.stats.frames_dropped_after_fec.saturating_add(1);
        self.stats.packets_lost_estimate = self
            .stats
            .packets_lost_estimate
            .saturating_add(u64::from(packet_count));
        self.stats.last_damaged_frame_id = Some(frame_id);
        self.skipped.insert(frame_id);
    }

    fn take_ready(&mut self, now: Instant, force: bool) -> Vec<EncodedFrame> {
        let mut ready = Vec::new();
        loop {
            let Some(next) = self.next_frame else {
                break;
            };
            if let Some(frame) = self.complete.remove(&next) {
                ready.push(frame);
                self.next_frame = next.checked_add(1);
                self.stats.next_decode_frame_id = self.next_frame;
                self.blocked_since = None;
                continue;
            }
            if self.skipped.remove(&next) {
                self.next_frame = next.checked_add(1);
                self.stats.next_decode_frame_id = self.next_frame;
                self.blocked_since = None;
                continue;
            }

            let later_frame_known = self
                .complete
                .first_key_value()
                .is_some_and(|(frame_id, _)| *frame_id > next)
                || self
                    .highest_seen_frame
                    .is_some_and(|frame_id| frame_id > next);
            if !later_frame_known {
                self.blocked_since = None;
                break;
            }
            let blocked_since = match self.blocked_since {
                Some(blocked_since) => blocked_since,
                None => {
                    self.blocked_since = Some(now);
                    self.stats.decode_gate_stalls += 1;
                    now
                }
            };
            let reorder_wait = self.current_reorder_wait();
            self.stats.reorder_wait_ms = duration_ms_rounded(reorder_wait);
            if !force && now.duration_since(blocked_since) < reorder_wait {
                break;
            }
            if !force {
                if let Some(frame) = self.inflight.get(&next) {
                    if self.repair_window.is_some() && now < self.frame_repair_deadline(frame) {
                        // Reordering has been ruled out, but NACK repair still owns the
                        // remaining playout budget. Do not poison the GOP while a repair
                        // can still arrive before presentation.
                        break;
                    }
                }
            }
            let gap_to_damage_ms = now.duration_since(blocked_since).as_secs_f64() * 1000.0;
            self.stats.decode_gate_gap_events += 1;
            self.stats.decode_gate_gap_to_damage_ms_total += gap_to_damage_ms;
            self.stats.decode_gate_gap_to_damage_ms_max = self
                .stats
                .decode_gate_gap_to_damage_ms_max
                .max(gap_to_damage_ms);
            let next_complete = self
                .complete
                .first_key_value()
                .map(|(frame_id, _)| *frame_id);
            let skip_to = next_complete.unwrap_or_else(|| next.saturating_add(1));
            let skipped_count = skip_to.saturating_sub(next).max(1);
            let stale_inflight: Vec<u64> = self
                .inflight
                .keys()
                .copied()
                .filter(|frame_id| *frame_id < skip_to)
                .collect();
            for frame_id in stale_inflight {
                self.expire_inflight_frame(frame_id);
            }
            let already_counted = self.skipped.range(next..skip_to).count() as u64;
            let newly_missing = skipped_count.saturating_sub(already_counted);
            self.stats.frames_incomplete_expired += newly_missing;
            self.stats.packets_lost_estimate += newly_missing;
            self.stats.frames_missing_after_fec += newly_missing;
            self.stats.frames_dropped_after_fec += newly_missing;
            if self.stats.fec_packets_received > 0
                || self.stats.fec_protected_data_packets_received > 0
            {
                self.stats.fec_recovery_failed_no_parity += newly_missing;
            }
            self.stats.frames_discarded_decode_gate += skipped_count;
            self.stats.last_damaged_frame_id = Some(skip_to.saturating_sub(1));
            self.skipped.retain(|frame_id| *frame_id >= skip_to);
            self.next_frame = Some(skip_to);
            self.stats.next_decode_frame_id = self.next_frame;
            self.blocked_since = None;
        }
        ready
    }
}

fn frame_payload_reservation(packet_count: u16) -> Option<usize> {
    let packet_count = usize::from(packet_count);
    if packet_count == 0 || packet_count > crate::MAX_VIDEO_PACKET_COUNT {
        return None;
    }
    packet_count
        .checked_mul(crate::MAX_MEDIA_PAYLOAD)
        .map(|bytes| bytes.min(crate::MAX_VIDEO_FRAME_BYTES))?
        .checked_add(crate::MAX_MEDIA_PAYLOAD)
}

fn recover_missing_payload(
    packets: &[Option<Vec<u8>>],
    parity: &FecParity,
    missing_index: usize,
) -> Result<Vec<u8>, String> {
    if packets.len() != parity.data_packet_count as usize {
        return Err("FEC packet count does not match frame assembly".to_string());
    }
    let expected_len = parity.expected_payload_len(missing_index)?;
    if parity.parity.len() < expected_len {
        return Err("FEC parity is shorter than recovered payload".to_string());
    }
    let mut recovered = parity.parity.clone();
    for (packet_index, payload) in packets.iter().enumerate() {
        let Some(payload) = payload.as_ref() else {
            if packet_index != missing_index {
                return Err("FEC can recover exactly one missing packet".to_string());
            }
            continue;
        };
        let expected = parity.expected_payload_len(packet_index)?;
        if payload.len() != expected || payload.len() > recovered.len() {
            return Err("FEC data payload length mismatch".to_string());
        }
        for (index, byte) in payload.iter().enumerate() {
            recovered[index] ^= byte;
        }
    }
    recovered.truncate(expected_len);
    Ok(recovered)
}

fn duration_ms_rounded(duration: Duration) -> u64 {
    (duration.as_secs_f64() * 1000.0).round().max(1.0) as u64
}

fn run_fec_self_test(config: ReassemblyConfig, now: Instant) -> Result<(), String> {
    use crate::fec::{packetize_frame, FecMode, FEC_DATA_PAYLOAD_SIZE};

    let payload_len = FEC_DATA_PAYLOAD_SIZE * 2 + 137;
    let payload: Vec<u8> = (0..payload_len).map(|index| (index % 251) as u8).collect();
    for udp_payload_size in [1200, 1452, 1472] {
        for mode in [FecMode::Off, FecMode::SingleXor] {
            let sized = packetize_frame(
                20,
                udp_payload_size as u64,
                1000,
                &payload,
                0,
                mode,
                udp_payload_size,
            )?;
            if sized
                .packets
                .iter()
                .any(|datagram| datagram.len() > udp_payload_size)
            {
                return Err(format!(
                    "{} packet exceeded configured UDP payload size {udp_payload_size}",
                    mode.name()
                ));
            }
            if mode == FecMode::SingleXor && sized.fec_packet_count != 1 {
                return Err("single-XOR FEC parity packet count mismatch".to_string());
            }
        }
    }
    let packetized = packetize_frame(
        21,
        0,
        1000,
        &payload,
        FLAG_KEYFRAME,
        FecMode::SingleXor,
        crate::LEGACY_UDP_PAYLOAD_SIZE,
    )?;
    if packetized.fec_packet_count != 1 || packetized.data_packet_count != 3 {
        return Err("single-XOR FEC packetization count mismatch".to_string());
    }
    if packetized
        .packets
        .iter()
        .any(|datagram| datagram.len() > 1200)
    {
        return Err("single-XOR FEC exceeded the UDP payload limit".to_string());
    }

    let mut recover_middle = H264Reassembler::new(config)?;
    let mut recovered = Vec::new();
    for (index, datagram) in packetized.packets.iter().enumerate() {
        if index != 1 {
            recovered.extend(recover_middle.accept_datagram(datagram, now)?);
        }
    }
    let middle_stats = recover_middle.stats();
    if recovered.len() != 1
        || recovered[0].bytes != payload
        || middle_stats.fec_frames_recovered != 1
        || middle_stats.fec_packets_recovered != 1
        || middle_stats.frames_incomplete_expired != 0
    {
        return Err("single-XOR FEC one-packet recovery failed".to_string());
    }

    let mut recover_short_tail = H264Reassembler::new(config)?;
    let mut recovered_tail = Vec::new();
    for (index, datagram) in packetized.packets.iter().enumerate() {
        if index != packetized.data_packet_count - 1 {
            recovered_tail.extend(recover_short_tail.accept_datagram(datagram, now)?);
        }
    }
    if recovered_tail.len() != 1 || recovered_tail[0].bytes != payload {
        return Err("single-XOR FEC short final payload recovery failed".to_string());
    }

    let mut two_missing = H264Reassembler::new(config)?;
    for (index, datagram) in packetized.packets.iter().enumerate() {
        if index != 0 && index != 1 {
            if !two_missing.accept_datagram(datagram, now)?.is_empty() {
                return Err("single-XOR FEC recovered more than one missing packet".to_string());
            }
        }
    }
    if !two_missing
        .expire(now + Duration::from_millis(20))
        .is_empty()
        || two_missing.stats().fec_recovery_failed_multi_missing != 1
        || two_missing.stats().frames_dropped_after_fec != 1
    {
        return Err("single-XOR FEC multi-packet failure handling failed".to_string());
    }

    let mut parity_missing = H264Reassembler::new(config)?;
    for (index, datagram) in packetized
        .packets
        .iter()
        .take(packetized.data_packet_count)
        .enumerate()
    {
        if index != 1 {
            if !parity_missing.accept_datagram(datagram, now)?.is_empty() {
                return Err("single-XOR FEC recovered without a parity packet".to_string());
            }
        }
    }
    if !parity_missing
        .expire(now + Duration::from_millis(20))
        .is_empty()
        || parity_missing.stats().fec_recovery_failed_no_parity != 1
        || parity_missing.stats().fec_frames_recovered != 0
    {
        return Err("single-XOR FEC missing-parity handling failed".to_string());
    }

    let off = packetize_frame(
        22,
        0,
        1000,
        &payload,
        0,
        FecMode::Off,
        crate::LEGACY_UDP_PAYLOAD_SIZE,
    )?;
    if off.fec_packet_count != 0
        || off.fec_bytes != 0
        || off.packets.len() != off.data_packet_count
        || off.packets.iter().any(|datagram| {
            MediaPacket::decode(datagram)
                .is_ok_and(|packet| packet.flags & (FLAG_FEC | FLAG_FEC_PROTECTED) != 0)
        })
    {
        return Err("FEC off packetization changed the legacy data path".to_string());
    }
    Ok(())
}

pub fn run_self_test() -> Result<(), String> {
    let config = ReassemblyConfig {
        frame_timeout: Duration::from_millis(10),
        reorder_wait: ReorderWait::Fixed(Duration::from_millis(10)),
        max_inflight_frames: 4,
    };
    let mut reassembler = H264Reassembler::new(config)?;
    let payload = vec![0x65; 3000];
    let packets = crate::packetize_media_payload(7, 0, 1234, &payload, FLAG_KEYFRAME)?;
    let now = Instant::now();
    run_fec_self_test(config, now)?;
    let mut ready = Vec::new();
    for packet in packets.iter().rev() {
        ready.extend(reassembler.accept_datagram(packet, now)?);
    }
    if ready.len() != 1 || ready[0].bytes != payload || !ready[0].is_keyframe() {
        return Err("shared H.264 reassembler out-of-order test failed".to_string());
    }

    let mut duplicate_reassembler = H264Reassembler::new(config)?;
    let duplicate_packets = crate::packetize_media_payload(70, 0, 1000, &[0x41; 3000], 0)?;
    duplicate_reassembler.accept_datagram(&duplicate_packets[0], now)?;
    duplicate_reassembler.accept_datagram(&duplicate_packets[0], now)?;
    let duplicate_stats = duplicate_reassembler.stats();
    if duplicate_stats.reassembly_packets_active != 1 {
        return Err("duplicate packet changed the active received count".to_string());
    }
    for packet in duplicate_packets.iter().skip(1) {
        duplicate_reassembler.accept_datagram(packet, now)?;
    }
    let cleanup_stats = duplicate_reassembler.stats();
    if cleanup_stats.frames_complete != 1
        || cleanup_stats.reassembly_frames_active != 0
        || cleanup_stats.reassembly_packets_active != 0
    {
        return Err("completed frame did not release active reassembly state".to_string());
    }

    let nack_config = ReassemblyConfig {
        frame_timeout: Duration::from_millis(500),
        reorder_wait: ReorderWait::Fixed(Duration::from_millis(10)),
        max_inflight_frames: 8,
    };
    let nack_payload = vec![0x41; 70_000];
    let nack_packets = crate::packetize_media_payload(71, 0, 1000, &nack_payload, 0)?;
    let mut progressing_reassembler = H264Reassembler::new(nack_config)?;
    progressing_reassembler.accept_datagram(&nack_packets[0], now)?;
    progressing_reassembler.accept_datagram(&nack_packets[2], now + Duration::from_millis(30))?;
    let progressing = progressing_reassembler.collect_nack_items(
        now + Duration::from_millis(31),
        Duration::from_millis(20),
        Duration::from_millis(20),
        3,
        Duration::from_millis(500),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if !progressing.items.is_empty() || progressing.stats.suppressed_progressing_frames == 0 {
        return Err(
            "NACK was not suppressed while frame packets were still progressing".to_string(),
        );
    }

    let mut nack_reassembler = H264Reassembler::new(nack_config)?;
    for (index, packet) in nack_packets.iter().enumerate() {
        if index != 1 {
            nack_reassembler.accept_datagram(packet, now)?;
        }
    }
    let nack_collection = nack_reassembler.collect_nack_items(
        now + Duration::from_millis(60),
        Duration::from_millis(20),
        Duration::from_millis(20),
        3,
        Duration::from_millis(500),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if nack_collection.items.len() != 1 || nack_collection.items[0].packet_index != 1 {
        return Err("idle missing packet did not produce the expected NACK item".to_string());
    }
    let repeated = nack_reassembler.collect_nack_items(
        now + Duration::from_millis(70),
        Duration::from_millis(20),
        Duration::from_millis(20),
        3,
        Duration::from_millis(500),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if !repeated.items.is_empty() || repeated.stats.suppressed_already_requested == 0 {
        return Err("NACK repeat suppression was not enforced".to_string());
    }
    let repaired =
        nack_reassembler.accept_datagram(&nack_packets[1], now + Duration::from_millis(80))?;
    if repaired.len() != 1 || repaired[0].bytes != nack_payload {
        return Err("NACK repair packet did not complete the frame".to_string());
    }

    progressing_reassembler.accept_datagram(&nack_packets[1], now + Duration::from_millis(32))?;
    if progressing_reassembler
        .stats()
        .nack_requests_cancelled_by_progress
        != 1
    {
        return Err("progress did not cancel an unrequested packet-gap candidate".to_string());
    }

    // At 60 FPS, a later frame is useful evidence that an interior packet gap
    // is real. The first NACK must still retain most of the 250 ms playout
    // budget, and strict decode order must not declare damage at reorder_wait.
    let timing_payload_size = (crate::LEGACY_UDP_PAYLOAD_SIZE - crate::HEADER_LEN) * 100 - 1;
    let timing_packets =
        crate::packetize_media_payload(77, 80, 80 * 17, &vec![0x47; timing_payload_size], 0)?;
    if timing_packets.len() != 100 {
        return Err(format!(
            "repair timing fixture expected 100 packets, got {}",
            timing_packets.len()
        ));
    }
    let later_packets = crate::packetize_media_payload(77, 81, 81 * 17, &[0x48; 100], 0)?;
    let missing_index = 47usize;
    let mut timing_reassembler = H264Reassembler::new(nack_config)?;
    timing_reassembler.set_repair_window(Some(Duration::from_millis(250)));
    for (index, packet) in timing_packets.iter().enumerate() {
        if index != missing_index {
            timing_reassembler.accept_datagram(packet, now)?;
        }
    }
    timing_reassembler.accept_datagram(&later_packets[0], now + Duration::from_millis(17))?;
    let timing_nack = timing_reassembler.collect_nack_items(
        now + Duration::from_millis(25),
        Duration::from_millis(20),
        Duration::from_millis(20),
        3,
        Duration::from_millis(250),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if timing_nack.items.len() != 1
        || timing_nack.items[0].packet_index != missing_index as u16
        || timing_nack.stats.first_round_budget.min_ms < 150.0
        || timing_nack.stats.missing_first_detected_to_deadline.min_ms < 200.0
    {
        return Err(format!(
            "first NACK did not retain the repair budget: items={} first_round_min_ms={:.3} detected_budget_min_ms={:.3}",
            timing_nack.items.len(),
            timing_nack.stats.first_round_budget.min_ms,
            timing_nack
                .stats
                .missing_first_detected_to_deadline
                .min_ms,
        ));
    }
    if !timing_reassembler
        .expire(now + Duration::from_millis(80))
        .is_empty()
        || timing_reassembler.stats().frames_incomplete_expired != 0
    {
        return Err("strict decode gate damaged a repairable frame at reorder_wait".to_string());
    }
    let repaired_in_order = timing_reassembler.accept_datagram(
        &timing_packets[missing_index],
        now + Duration::from_millis(100),
    )?;
    if repaired_in_order.len() != 2
        || repaired_in_order[0].frame_id != 80
        || repaired_in_order[1].frame_id != 81
        || timing_reassembler.stats().frames_incomplete_expired != 0
    {
        return Err("repair did not release buffered frames in strict order".to_string());
    }

    let mut deadline_reassembler = H264Reassembler::new(nack_config)?;
    deadline_reassembler.set_repair_window(Some(Duration::from_millis(250)));
    for (index, packet) in timing_packets.iter().enumerate() {
        if index != missing_index {
            deadline_reassembler.accept_datagram(packet, now)?;
        }
    }
    deadline_reassembler.accept_datagram(&later_packets[0], now + Duration::from_millis(17))?;
    if !deadline_reassembler
        .expire(now + Duration::from_millis(80))
        .is_empty()
        || deadline_reassembler.stats().frames_incomplete_expired != 0
    {
        return Err("repair deadline regression expired a frame near 40 ms".to_string());
    }
    let after_repair_deadline = deadline_reassembler.expire(now + Duration::from_millis(245));
    let deadline_stats = deadline_reassembler.stats();
    if after_repair_deadline.len() != 1
        || after_repair_deadline[0].frame_id != 81
        || deadline_stats.frames_incomplete_expired != 1
        || deadline_stats.decode_gate_gap_to_damage_ms_avg() < 200.0
    {
        return Err(format!(
            "strict gate did not defer damage to the repair deadline: ready={} expired={} gap_ms={:.3}",
            after_repair_deadline.len(),
            deadline_stats.frames_incomplete_expired,
            deadline_stats.decode_gate_gap_to_damage_ms_avg(),
        ));
    }

    let mut cap_reassembler = H264Reassembler::new(nack_config)?;
    let cap_packets = crate::packetize_media_payload(72, 0, 1000, &[0x42; 70_000], 0)?;
    cap_reassembler.accept_datagram(&cap_packets[0], now)?;
    let capped = cap_reassembler.collect_nack_items(
        now + Duration::from_millis(40),
        Duration::from_millis(20),
        Duration::from_millis(20),
        3,
        Duration::from_millis(500),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if capped.items.len() != DEFAULT_NACK_ITEMS_PER_FRAME || capped.stats.suppressed_item_limit == 0
    {
        return Err("NACK per-frame item limit was not enforced".to_string());
    }

    let mut near_deadline_reassembler = H264Reassembler::new(nack_config)?;
    let deadline_packets = crate::packetize_media_payload(73, 0, 1000, &[0x43; 3000], 0)?;
    near_deadline_reassembler.accept_datagram(&deadline_packets[0], now)?;
    near_deadline_reassembler
        .accept_datagram(&deadline_packets[2], now + Duration::from_millis(25))?;
    let near_deadline = near_deadline_reassembler.collect_nack_items(
        now + Duration::from_millis(26),
        Duration::from_millis(100),
        Duration::from_millis(20),
        3,
        Duration::from_millis(120),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if near_deadline.items.is_empty() {
        return Err("NACK was not sent near the playout repair deadline".to_string());
    }

    let mut too_late_reassembler = H264Reassembler::new(nack_config)?;
    let late_packets = crate::packetize_media_payload(74, 0, 1000, &[0x44; 3000], 0)?;
    too_late_reassembler.accept_datagram(&late_packets[0], now)?;
    let too_late = too_late_reassembler.collect_nack_items(
        now + Duration::from_millis(115),
        Duration::from_millis(20),
        Duration::from_millis(20),
        3,
        Duration::from_millis(120),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if !too_late.items.is_empty() {
        return Err("NACK was sent after the repair deadline".to_string());
    }

    let mut max_round_reassembler = H264Reassembler::new(nack_config)?;
    let max_round_packets = crate::packetize_media_payload(75, 0, 1000, &[0x45; 3000], 0)?;
    max_round_reassembler.accept_datagram(&max_round_packets[0], now)?;
    let first_round = max_round_reassembler.collect_nack_items(
        now + Duration::from_millis(40),
        Duration::from_millis(20),
        Duration::from_millis(20),
        1,
        Duration::from_millis(500),
        DEFAULT_NACK_ITEMS_PER_FRAME,
    );
    if first_round.items.is_empty()
        || !max_round_reassembler
            .collect_nack_items(
                now + Duration::from_millis(80),
                Duration::from_millis(20),
                Duration::from_millis(20),
                1,
                Duration::from_millis(500),
                DEFAULT_NACK_ITEMS_PER_FRAME,
            )
            .items
            .is_empty()
    {
        return Err("NACK max rounds was not enforced".to_string());
    }

    use crate::fec::{packetize_frame, FecMode};
    let fec_payload = vec![0x46; 5000];
    let fec_packets = packetize_frame(
        76,
        0,
        1000,
        &fec_payload,
        0,
        FecMode::SingleXor,
        crate::LEGACY_UDP_PAYLOAD_SIZE,
    )?;
    let mut fec_success_reassembler = H264Reassembler::new(nack_config)?;
    for (index, packet) in fec_packets.packets.iter().enumerate() {
        if index != 1 {
            fec_success_reassembler.accept_datagram(packet, now)?;
        }
    }
    if !fec_success_reassembler
        .collect_nack_items(
            now + Duration::from_millis(40),
            Duration::from_millis(20),
            Duration::from_millis(20),
            3,
            Duration::from_millis(500),
            DEFAULT_NACK_ITEMS_PER_FRAME,
        )
        .items
        .is_empty()
    {
        return Err("FEC-recovered frame still generated NACK items".to_string());
    }
    let mut fec_failed_reassembler = H264Reassembler::new(nack_config)?;
    for (index, packet) in fec_packets.packets.iter().enumerate() {
        if index != 1 && index != 2 {
            fec_failed_reassembler.accept_datagram(packet, now)?;
        }
    }
    if fec_failed_reassembler
        .collect_nack_items(
            now + Duration::from_millis(40),
            Duration::from_millis(20),
            Duration::from_millis(20),
            3,
            Duration::from_millis(500),
            DEFAULT_NACK_ITEMS_PER_FRAME,
        )
        .items
        .is_empty()
    {
        return Err("FEC failure did not allow NACK fallback".to_string());
    }

    let incomplete = crate::packetize_media_payload(7, 1, 1267, &[0x41; 3000], 0)?;
    reassembler.accept_datagram(&incomplete[0], now)?;
    reassembler.expire(now + Duration::from_millis(20));
    if reassembler.stats.frames_incomplete_expired != 1
        || reassembler.stats.packets_lost_estimate == 0
        || reassembler.stats.reassembly_frames_active != 0
        || reassembler.stats.reassembly_packets_active != 0
    {
        return Err("shared H.264 reassembler expiration test failed".to_string());
    }

    let mut gap_reassembler = H264Reassembler::new(config)?;
    let frame_zero = crate::packetize_media_payload(8, 0, 1000, &[0x65; 100], FLAG_KEYFRAME)?;
    let frame_two = crate::packetize_media_payload(8, 2, 1066, &[0x41; 100], 0)?;
    let first = gap_reassembler.accept_datagram(&frame_zero[0], now)?;
    if first.len() != 1 || first[0].frame_id != 0 {
        return Err("shared H.264 reassembler initial frame test failed".to_string());
    }
    if !gap_reassembler
        .accept_datagram(&frame_two[0], now)?
        .is_empty()
    {
        return Err("shared H.264 reassembler released a frame before gap timeout".to_string());
    }
    let after_gap = gap_reassembler.expire(now + Duration::from_millis(20));
    if after_gap.len() != 1
        || after_gap[0].frame_id != 2
        || gap_reassembler.stats.frames_incomplete_expired != 1
    {
        return Err("shared H.264 reassembler gap recovery test failed".to_string());
    }
    let gap_stats = gap_reassembler.stats();
    if gap_stats.decode_gate_stalls != 1
        || gap_stats.decode_gate_gap_events != 1
        || gap_stats.frames_discarded_decode_gate != 1
        || gap_stats.decode_gate_gap_to_damage_ms_avg() < 10.0
        || gap_stats.next_decode_frame_id != Some(3)
    {
        return Err("shared H.264 decode gate stats mismatch".to_string());
    }

    let auto_config = ReassemblyConfig {
        frame_timeout: Duration::from_millis(300),
        reorder_wait: ReorderWait::Auto,
        max_inflight_frames: 4,
    };
    let mut auto_reassembler = H264Reassembler::new(auto_config)?;
    for frame_id in 0..3u64 {
        let timestamp_ms = frame_id * 17;
        let packet = crate::packetize_media_payload(9, frame_id, timestamp_ms, &[0x41; 100], 0)?;
        auto_reassembler.accept_datagram(&packet[0], now)?;
    }
    if !(35..=50).contains(&auto_reassembler.reorder_wait_ms()) {
        return Err(format!(
            "60 FPS auto reorder wait out of range: {}ms",
            auto_reassembler.reorder_wait_ms()
        ));
    }
    let mut auto_30fps = H264Reassembler::new(auto_config)?;
    for frame_id in 0..3u64 {
        let packet = crate::packetize_media_payload(10, frame_id, frame_id * 33, &[0x41; 100], 0)?;
        auto_30fps.accept_datagram(&packet[0], now)?;
    }
    if !(70..=100).contains(&auto_30fps.reorder_wait_ms()) {
        return Err(format!(
            "30 FPS auto reorder wait out of range: {}ms",
            auto_30fps.reorder_wait_ms()
        ));
    }

    let mut damaged = DamagedGopTracker::new(true);
    let config_frame = EncodedFrame {
        frame_id: 1,
        flags: crate::FLAG_CONFIG,
        timestamp_ms: 1033,
        bytes: vec![0, 0, 0, 1, 7, 0x64, 0, 0, 0, 1, 8, 0xee],
    };
    if damaged.prepare_frame(config_frame, now).is_none() {
        return Err("damaged GOP tracker rejected codec configuration".to_string());
    }
    if !damaged.mark_damaged(now, Some(2))
        || damaged
            .prepare_frame(
                EncodedFrame {
                    frame_id: 3,
                    flags: FLAG_KEYFRAME,
                    timestamp_ms: 1099,
                    bytes: vec![0, 0, 0, 1, 1, 0x80],
                },
                now + Duration::from_millis(5),
            )
            .is_some()
        || !damaged.waiting_keyframe()
    {
        return Err("damaged GOP tracker did not discard a dependent frame".to_string());
    }
    let recovered = damaged.prepare_frame(
        EncodedFrame {
            frame_id: 4,
            flags: FLAG_KEYFRAME,
            timestamp_ms: 1132,
            bytes: vec![0, 0, 0, 1, 5, 0x80],
        },
        now + Duration::from_millis(10),
    );
    let recovered_summary = recovered
        .as_ref()
        .map(|frame| summarize_nals(&frame.bytes))
        .unwrap_or_default();
    if recovered.is_none()
        || damaged.waiting_keyframe()
        || !recovered_summary.has_idr_slice
        || !recovered_summary.has_sps
        || !recovered_summary.has_pps
    {
        return Err("damaged GOP tracker did not recover on a keyframe".to_string());
    }
    let damaged_stats = damaged.stats();
    if damaged_stats.damaged_gop_count != 1
        || damaged_stats.frames_discarded_damaged_gop != 1
        || damaged_stats.recovery_completed != 1
        || damaged_stats.idr_frames_used_for_recovery != 1
        || damaged_stats.non_idr_frames_discarded_waiting != 1
        || damaged_stats.recovery_wait_frames_max != 2
        || damaged_stats.recovery_wait_ms_avg() <= 0.0
    {
        return Err("damaged GOP tracker stats mismatch".to_string());
    }

    let mut profile_reassembler = H264Reassembler::new(config)?;
    let old_frame = crate::packetize_media_payload(100, 0, 1000, &[0x41; 3000], 0)?;
    profile_reassembler.accept_datagram(&old_frame[0], now)?;
    if profile_reassembler.session_id() != Some(100) || profile_reassembler.inflight_len() != 1 {
        return Err("profile switch setup did not retain the old partial frame".to_string());
    }
    profile_reassembler.switch_session(101)?;
    if profile_reassembler.session_id() != Some(101)
        || profile_reassembler.inflight_len() != 0
        || profile_reassembler.completed_waiting_len() != 0
        || profile_reassembler.stats().next_decode_frame_id.is_some()
    {
        return Err("profile switch did not atomically clear old reassembly state".to_string());
    }
    let invalid_before = profile_reassembler.stats().packets_invalid;
    if !profile_reassembler
        .accept_datagram(&old_frame[1], now + Duration::from_millis(1))?
        .is_empty()
        || profile_reassembler.stats().packets_invalid != invalid_before + 1
    {
        return Err("late packet from the old profile session was not rejected".to_string());
    }
    let new_frame = crate::packetize_media_payload(
        101,
        0,
        1033,
        &[
            0, 0, 0, 1, 7, 0x64, 0, 0, 0, 1, 8, 0xee, 0, 0, 0, 1, 5, 0x80,
        ],
        FLAG_KEYFRAME | crate::FLAG_CONFIG,
    )?;
    let ready =
        profile_reassembler.accept_datagram(&new_frame[0], now + Duration::from_millis(2))?;
    if ready.len() != 1 || ready[0].frame_id != 0 || !ready[0].is_idr() {
        return Err("new profile session did not restart cleanly from frame zero IDR".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{H264Reassembler, ReassemblyConfig, ReorderWait};

    fn config() -> ReassemblyConfig {
        ReassemblyConfig {
            frame_timeout: Duration::from_millis(10),
            reorder_wait: ReorderWait::Fixed(Duration::from_millis(5)),
            max_inflight_frames: crate::MAX_INFLIGHT_FRAMES,
        }
    }

    fn packet(frame_id: u64, packet_count: u16, packet_index: u16) -> crate::MediaPacket {
        crate::MediaPacket {
            stream_id: crate::STREAM_VIDEO,
            flags: 0,
            session_id: 1,
            frame_id,
            packet_index,
            packet_count,
            timestamp_ms: frame_id,
            payload: vec![0x41],
        }
    }

    #[test]
    fn deterministic_reassembly_and_recovery_regressions() {
        super::run_self_test().expect("H.264 reassembly self-test");
    }

    #[test]
    fn aggregate_slot_budget_enforced() {
        let now = Instant::now();
        let mut reassembler = H264Reassembler::new(config()).unwrap();
        let packet_count = 30_000u16;
        reassembler.accept_packet(packet(0, packet_count, 0), now);
        reassembler.accept_packet(packet(1, packet_count, 0), now);
        reassembler.accept_packet(packet(2, packet_count, 0), now);
        let stats = reassembler.stats();
        assert_eq!(reassembler.inflight_len(), 2);
        assert_eq!(stats.reassembly_budget_rejected_frames, 1);
        assert!(
            stats.reassembly_packet_slots_reserved as usize <= crate::MAX_INFLIGHT_PACKET_SLOTS
        );
    }

    #[test]
    fn aggregate_byte_budget_enforced() {
        let now = Instant::now();
        let mut reassembler = H264Reassembler::new(config()).unwrap();
        for frame_id in 0..10 {
            reassembler.accept_packet(packet(frame_id, 5_000, 0), now);
        }
        let stats = reassembler.stats();
        assert_eq!(reassembler.inflight_len(), 9);
        assert_eq!(stats.reassembly_budget_rejected_frames, 1);
        assert!(
            stats.reassembly_payload_bytes_reserved as usize <= crate::MAX_INFLIGHT_PAYLOAD_BYTES
        );
    }

    #[test]
    fn budget_released_after_completion() {
        let now = Instant::now();
        let mut reassembler = H264Reassembler::new(config()).unwrap();
        reassembler.accept_packet(packet(0, 1, 0), now);
        let stats = reassembler.stats();
        assert_eq!(reassembler.inflight_len(), 0);
        assert_eq!(stats.reassembly_packet_slots_reserved, 0);
        assert_eq!(stats.reassembly_payload_bytes_reserved, 0);
    }

    #[test]
    fn budget_released_after_timeout() {
        let now = Instant::now();
        let mut reassembler = H264Reassembler::new(config()).unwrap();
        reassembler.accept_packet(packet(0, 2, 0), now);
        assert!(reassembler.stats().reassembly_packet_slots_reserved > 0);
        reassembler.expire(now + Duration::from_millis(20));
        let stats = reassembler.stats();
        assert_eq!(reassembler.inflight_len(), 0);
        assert_eq!(stats.reassembly_packet_slots_reserved, 0);
        assert_eq!(stats.reassembly_payload_bytes_reserved, 0);
    }

    #[test]
    fn duplicate_packet_does_not_change_reservation() {
        let now = Instant::now();
        let mut reassembler = H264Reassembler::new(config()).unwrap();
        reassembler.accept_packet(packet(0, 2, 0), now);
        let before = reassembler.stats();
        reassembler.accept_packet(packet(0, 2, 0), now + Duration::from_millis(1));
        let after = reassembler.stats();
        assert_eq!(
            after.reassembly_packet_slots_reserved,
            before.reassembly_packet_slots_reserved
        );
        assert_eq!(
            after.reassembly_payload_bytes_reserved,
            before.reassembly_payload_bytes_reserved
        );
    }
}
