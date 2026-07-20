use std::time::{Duration, Instant};

use crate::h264_reassembly::{H264Reassembler, ReassemblyConfig, ReorderWait};
use crate::{MediaPacket, FLAG_END_OF_FRAME, STREAM_VIDEO};

#[derive(Debug)]
pub struct BenchReassemblyConfig {
    pub frames: u64,
    pub packets_per_frame: u16,
    pub payload_size: usize,
    pub loss_rate: f64,
}

pub fn run(config: BenchReassemblyConfig) -> Result<(), String> {
    validate(&config)?;
    let mut reassembler = H264Reassembler::new(ReassemblyConfig {
        frame_timeout: Duration::from_millis(2),
        reorder_wait: ReorderWait::Fixed(Duration::from_millis(1)),
        max_inflight_frames: 256,
    })?;
    let payload = vec![0x5au8; config.payload_size];
    let mut datagrams = Vec::with_capacity(config.packets_per_frame as usize);
    for packet_index in 0..config.packets_per_frame {
        let flags = if packet_index + 1 == config.packets_per_frame {
            FLAG_END_OF_FRAME
        } else {
            0
        };
        datagrams.push(
            MediaPacket {
                stream_id: STREAM_VIDEO,
                flags,
                session_id: 0x4245_4e43_484d_4152,
                frame_id: 0,
                packet_index,
                packet_count: config.packets_per_frame,
                timestamp_ms: 0,
                payload: payload.clone(),
            }
            .encode_with_udp_payload_size(config.payload_size + crate::HEADER_LEN)?,
        );
    }
    let started = Instant::now();
    let mut packets_inserted = 0u64;
    let mut bytes_inserted = 0u64;
    let mut ordinal = 0u64;

    for frame_id in 0..config.frames {
        let now = Instant::now();
        for datagram in &mut datagrams {
            let drop_packet = deterministic_fraction(ordinal) < config.loss_rate;
            ordinal += 1;
            if drop_packet {
                continue;
            }
            datagram[16..24].copy_from_slice(&frame_id.to_be_bytes());
            datagram[28..36].copy_from_slice(&frame_id.saturating_mul(16).to_be_bytes());
            bytes_inserted += datagram.len() as u64;
            packets_inserted += 1;
            drop(reassembler.accept_datagram(datagram, now)?);
        }
        if config.loss_rate > 0.0 {
            drop(reassembler.expire(now + Duration::from_millis(3)));
        }
    }
    drop(reassembler.finish());
    let elapsed = started.elapsed().as_secs_f64().max(0.000_001);
    let stats = reassembler.stats();
    println!(
        r#"{{"type":"BENCH_REASSEMBLY","frames":{},"packets_per_frame":{},"payload_size":{},"loss_rate":{:.6},"packets_inserted":{},"packets_per_second":{:.2},"frames_completed":{},"reassembly_ms":{:.3},"mbps_equivalent":{:.3},"reassembly_frames_active":{},"reassembly_packets_active":{},"reassembly_packet_slots_reserved":{},"reassembly_payload_bytes_reserved":{},"reassembly_budget_rejected_frames":{},"reassembly_allocations_estimate":{},"reassembly_complete_scan_count":{}}}"#,
        config.frames,
        config.packets_per_frame,
        config.payload_size,
        config.loss_rate,
        packets_inserted,
        packets_inserted as f64 / elapsed,
        stats.frames_complete,
        elapsed * 1000.0,
        bytes_inserted as f64 * 8.0 / elapsed / 1_000_000.0,
        stats.reassembly_frames_active,
        stats.reassembly_packets_active,
        stats.reassembly_packet_slots_reserved,
        stats.reassembly_payload_bytes_reserved,
        stats.reassembly_budget_rejected_frames,
        stats.reassembly_allocations_estimate,
        stats.reassembly_complete_scan_count,
    );
    Ok(())
}

fn validate(config: &BenchReassemblyConfig) -> Result<(), String> {
    if config.frames == 0 {
        return Err("frames must be greater than zero".to_string());
    }
    if config.packets_per_frame == 0 {
        return Err("packets-per-frame must be greater than zero".to_string());
    }
    if config.payload_size == 0 || config.payload_size > crate::MAX_MEDIA_PAYLOAD {
        return Err(format!(
            "payload-size must be between 1 and {}",
            crate::MAX_MEDIA_PAYLOAD
        ));
    }
    if !config.loss_rate.is_finite() || !(0.0..=1.0).contains(&config.loss_rate) {
        return Err("loss-rate must be between 0 and 1".to_string());
    }
    Ok(())
}

fn deterministic_fraction(index: u64) -> f64 {
    let value = index
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (value >> 11) as f64 / ((1u64 << 53) as f64)
}

pub fn run_self_test() -> Result<(), String> {
    validate(&BenchReassemblyConfig {
        frames: 1,
        packets_per_frame: 1,
        payload_size: 100,
        loss_rate: 0.0,
    })?;
    if validate(&BenchReassemblyConfig {
        frames: 0,
        packets_per_frame: 1,
        payload_size: 100,
        loss_rate: 0.0,
    })
    .is_ok()
        || deterministic_fraction(1) >= 1.0
    {
        return Err("reassembly benchmark validation failed".to_string());
    }
    Ok(())
}
