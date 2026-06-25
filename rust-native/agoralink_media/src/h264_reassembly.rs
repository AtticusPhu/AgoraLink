use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::{MediaPacket, FLAG_KEYFRAME, STREAM_VIDEO};

#[derive(Clone, Copy, Debug)]
pub struct ReassemblyConfig {
    pub frame_timeout: Duration,
    pub max_inflight_frames: usize,
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
}

struct FrameAssembly {
    packet_count: u16,
    packets: Vec<Option<Vec<u8>>>,
    received_count: u16,
    first_seen: Instant,
    flags: u16,
    timestamp_ms: u64,
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
    stats: ReassemblyStats,
}

impl H264Reassembler {
    pub fn new(config: ReassemblyConfig) -> Result<Self, String> {
        if config.frame_timeout.is_zero() {
            return Err("frame timeout must be greater than zero".to_string());
        }
        if config.max_inflight_frames == 0 {
            return Err("max inflight frames must be greater than zero".to_string());
        }
        Ok(Self {
            config,
            session_id: None,
            inflight: HashMap::new(),
            complete: BTreeMap::new(),
            skipped: BTreeSet::new(),
            next_frame: None,
            highest_seen_frame: None,
            blocked_since: None,
            stats: ReassemblyStats::default(),
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

        self.next_frame.get_or_insert(packet.frame_id);
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
        if entry.packets[index].is_none() {
            entry.packets[index] = Some(packet.payload);
            entry.received_count += 1;
        }

        if entry.received_count == entry.packet_count {
            if let Some(frame) = self.inflight.remove(&packet.frame_id) {
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
                    packet.frame_id,
                    EncodedFrame {
                        frame_id: packet.frame_id,
                        flags: frame.flags,
                        timestamp_ms: frame.timestamp_ms,
                        bytes,
                    },
                );
                self.stats.frames_complete += 1;
                self.stats.last_frame_id = Some(packet.frame_id);
            }
        }
    }

    fn expire_inflight_frame(&mut self, frame_id: u64) {
        if let Some(frame) = self.inflight.remove(&frame_id) {
            self.stats.frames_incomplete_expired += 1;
            self.stats.packets_lost_estimate +=
                u64::from(frame.packet_count.saturating_sub(frame.received_count));
            self.skipped.insert(frame_id);
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
                self.blocked_since = None;
                continue;
            }
            if self.skipped.remove(&next) {
                self.next_frame = next.checked_add(1);
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
            let blocked_since = self.blocked_since.get_or_insert(now);
            if !force && now.duration_since(*blocked_since) < self.config.frame_timeout {
                break;
            }
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
            self.stats.frames_incomplete_expired += skipped_count.saturating_sub(already_counted);
            self.stats.packets_lost_estimate += skipped_count.saturating_sub(already_counted);
            self.skipped.retain(|frame_id| *frame_id >= skip_to);
            self.next_frame = Some(skip_to);
            self.blocked_since = None;
        }
        ready
    }
}

pub fn run_self_test() -> Result<(), String> {
    let config = ReassemblyConfig {
        frame_timeout: Duration::from_millis(10),
        max_inflight_frames: 4,
    };
    let mut reassembler = H264Reassembler::new(config)?;
    let payload = vec![0x65; 3000];
    let packets = crate::packetize_media_payload(7, 0, 1234, &payload, FLAG_KEYFRAME)?;
    let now = Instant::now();
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
    Ok(())
}
