use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::fec::FecParity;
use crate::h264_annex_b::{summarize_nals, AnnexBParameterSets};
use crate::{MediaPacket, FLAG_FEC, FLAG_FEC_PROTECTED, FLAG_KEYFRAME, STREAM_VIDEO};

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
}

struct FrameAssembly {
    packet_count: u16,
    packets: Vec<Option<Vec<u8>>>,
    received_count: u16,
    first_seen: Instant,
    flags: u16,
    timestamp_ms: u64,
    fec_parity: Option<FecParity>,
    fec_expected: bool,
    fec_invalid: bool,
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

    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
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
        if !self.inflight.contains_key(&packet.frame_id)
            && self.inflight.len() >= self.config.max_inflight_frames
        {
            if let Some(oldest) = self
                .inflight
                .iter()
                .min_by_key(|(_, frame)| frame.first_seen)
                .map(|(frame_id, _)| *frame_id)
            {
                self.expire_inflight_frame(oldest);
            }
        }

        if is_fec {
            self.accept_fec_packet(packet, now);
        } else {
            self.accept_data_packet(packet, now);
        }
    }

    fn accept_data_packet(&mut self, packet: MediaPacket, now: Instant) {
        let is_fec_protected = packet.flags & FLAG_FEC_PROTECTED != 0;
        let entry = self
            .inflight
            .entry(packet.frame_id)
            .or_insert_with(|| FrameAssembly {
                packet_count: packet.packet_count,
                packets: vec![None; packet.packet_count as usize],
                received_count: 0,
                first_seen: now,
                flags: 0,
                timestamp_ms: packet.timestamp_ms,
                fec_parity: None,
                fec_expected: is_fec_protected,
                fec_invalid: false,
            });
        if entry.packet_count != packet.packet_count {
            self.stats.packets_invalid += 1;
            return;
        }
        let index = packet.packet_index as usize;
        if index >= entry.packets.len() {
            self.stats.packets_invalid += 1;
            return;
        }
        entry.flags |= packet.flags;
        entry.fec_expected |= is_fec_protected;
        if entry.packets[index].is_none() {
            entry.packets[index] = Some(packet.payload);
            entry.received_count += 1;
        }

        self.try_complete_frame(packet.frame_id);
    }

    fn accept_fec_packet(&mut self, packet: MediaPacket, now: Instant) {
        let parity = match FecParity::decode(&packet.payload) {
            Ok(parity) if parity.data_packet_count == packet.packet_count => parity,
            Ok(_) | Err(_) => {
                self.stats.fec_recovery_failed_invalid += 1;
                if let Some(frame) = self.inflight.get_mut(&packet.frame_id) {
                    frame.fec_invalid = true;
                }
                return;
            }
        };
        let entry = self
            .inflight
            .entry(packet.frame_id)
            .or_insert_with(|| FrameAssembly {
                packet_count: parity.data_packet_count,
                packets: vec![None; parity.data_packet_count as usize],
                received_count: 0,
                first_seen: now,
                flags: 0,
                timestamp_ms: packet.timestamp_ms,
                fec_parity: None,
                fec_expected: true,
                fec_invalid: false,
            });
        if entry.packet_count != parity.data_packet_count {
            self.stats.fec_recovery_failed_invalid += 1;
            return;
        }
        entry.flags |= packet.flags & !FLAG_FEC;
        entry.fec_parity = Some(parity);
        entry.fec_expected = true;
        self.try_complete_frame(packet.frame_id);
    }

    fn try_complete_frame(&mut self, frame_id: u64) {
        let mut recovered_packet = false;
        let mut invalid_fec = false;
        if let Some(frame) = self.inflight.get_mut(&frame_id) {
            if frame.received_count != frame.packet_count {
                let missing: Vec<usize> = frame
                    .packets
                    .iter()
                    .enumerate()
                    .filter_map(|(index, payload)| payload.is_none().then_some(index))
                    .collect();
                if missing.len() == 1 {
                    if let Some(parity) = frame.fec_parity.as_ref() {
                        match recover_missing_payload(&frame.packets, parity, missing[0]) {
                            Ok(payload) => {
                                frame.packets[missing[0]] = Some(payload);
                                frame.received_count += 1;
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
            .is_some_and(|frame| frame.received_count == frame.packet_count);
        if complete {
            self.finish_complete_frame(frame_id);
        }
    }

    fn finish_complete_frame(&mut self, frame_id: u64) {
        if let Some(frame) = self.inflight.remove(&frame_id) {
            let total_len = frame
                .packets
                .iter()
                .filter_map(Option::as_ref)
                .map(Vec::len)
                .sum();
            let mut bytes = Vec::with_capacity(total_len);
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
        if let Some(frame) = self.inflight.remove(&frame_id) {
            let missing_count = frame.packet_count.saturating_sub(frame.received_count);
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

    let incomplete = crate::packetize_media_payload(7, 1, 1267, &[0x41; 3000], 0)?;
    reassembler.accept_datagram(&incomplete[0], now)?;
    reassembler.expire(now + Duration::from_millis(20));
    if reassembler.stats.frames_incomplete_expired != 1
        || reassembler.stats.packets_lost_estimate == 0
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
    Ok(())
}
