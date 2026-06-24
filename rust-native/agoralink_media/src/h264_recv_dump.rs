#[derive(Debug)]
pub struct H264RecvConfig {
    pub bind: String,
    pub port: u16,
    pub output: String,
    pub idle_timeout_sec: u64,
}

#[cfg(windows)]
mod platform {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::fs::File;
    use std::io::{self, Write};
    use std::net::UdpSocket;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::H264RecvConfig;
    use crate::{MediaPacket, STREAM_VIDEO};

    const FRAME_TTL: Duration = Duration::from_millis(1000);
    const RECEIVE_POLL: Duration = Duration::from_millis(50);
    const MAX_DATAGRAM_SIZE: usize = 2048;

    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

    struct ConsoleCtrlGuard;

    impl ConsoleCtrlGuard {
        fn install() -> Result<Self, String> {
            STOP_REQUESTED.store(false, Ordering::SeqCst);
            unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), true) }
                .map_err(|err| format!("SetConsoleCtrlHandler failed: {err}"))?;
            Ok(Self)
        }
    }

    impl Drop for ConsoleCtrlGuard {
        fn drop(&mut self) {
            let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
        }
    }

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows::core::BOOL {
        if matches!(
            ctrl_type,
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT
        ) {
            STOP_REQUESTED.store(true, Ordering::SeqCst);
            true.into()
        } else {
            false.into()
        }
    }

    #[derive(Default)]
    struct ReceiverStats {
        packets_received: u64,
        packets_invalid: u64,
        packets_lost_estimate: u64,
        frames_complete: u64,
        frames_incomplete_expired: u64,
        frames_written: u64,
        bytes_received: u64,
        bytes_written: u64,
        last_frame_id: Option<u64>,
    }

    struct FrameAssembly {
        packet_count: u16,
        packets: Vec<Option<Vec<u8>>>,
        received_count: u16,
        first_seen: Instant,
        flags: u16,
        timestamp_ms: u64,
    }

    struct CompleteFrame {
        bytes: Vec<u8>,
        _flags: u16,
        _timestamp_ms: u64,
    }

    struct H264Reassembler {
        session_id: Option<u64>,
        inflight: HashMap<u64, FrameAssembly>,
        complete: BTreeMap<u64, CompleteFrame>,
        skipped: BTreeSet<u64>,
        next_write_frame: Option<u64>,
        highest_seen_frame: Option<u64>,
        blocked_since: Option<Instant>,
        stats: ReceiverStats,
    }

    impl H264Reassembler {
        fn new() -> Self {
            Self {
                session_id: None,
                inflight: HashMap::new(),
                complete: BTreeMap::new(),
                skipped: BTreeSet::new(),
                next_write_frame: None,
                highest_seen_frame: None,
                blocked_since: None,
                stats: ReceiverStats::default(),
            }
        }

        fn accept(&mut self, packet: MediaPacket, now: Instant) {
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

            self.next_write_frame.get_or_insert(packet.frame_id);
            self.highest_seen_frame = Some(
                self.highest_seen_frame
                    .map_or(packet.frame_id, |current| current.max(packet.frame_id)),
            );
            if self
                .next_write_frame
                .is_some_and(|next| packet.frame_id < next)
            {
                return;
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
                        CompleteFrame {
                            bytes,
                            _flags: frame.flags,
                            _timestamp_ms: frame.timestamp_ms,
                        },
                    );
                    self.stats.frames_complete += 1;
                    self.stats.last_frame_id = Some(packet.frame_id);
                }
            }
        }

        fn expire(&mut self, now: Instant) {
            let expired: Vec<u64> = self
                .inflight
                .iter()
                .filter_map(|(frame_id, frame)| {
                    (now.duration_since(frame.first_seen) >= FRAME_TTL).then_some(*frame_id)
                })
                .collect();
            for frame_id in expired {
                if let Some(frame) = self.inflight.remove(&frame_id) {
                    self.stats.frames_incomplete_expired += 1;
                    self.stats.packets_lost_estimate +=
                        u64::from(frame.packet_count.saturating_sub(frame.received_count));
                    self.skipped.insert(frame_id);
                }
            }
        }

        fn flush_ready(&mut self, output: &mut File, now: Instant) -> Result<(), String> {
            loop {
                let Some(next) = self.next_write_frame else {
                    return Ok(());
                };
                if let Some(frame) = self.complete.remove(&next) {
                    output
                        .write_all(&frame.bytes)
                        .map_err(|err| format!("write H.264 dump failed: {err}"))?;
                    self.stats.frames_written += 1;
                    self.stats.bytes_written += frame.bytes.len() as u64;
                    self.next_write_frame = next.checked_add(1);
                    self.blocked_since = None;
                    continue;
                }
                if self.skipped.remove(&next) {
                    self.next_write_frame = next.checked_add(1);
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
                    return Ok(());
                }
                let blocked_since = self.blocked_since.get_or_insert(now);
                if now.duration_since(*blocked_since) < FRAME_TTL {
                    return Ok(());
                }
                self.stats.frames_incomplete_expired += 1;
                self.stats.packets_lost_estimate += 1;
                self.next_write_frame = next.checked_add(1);
                self.blocked_since = None;
            }
        }

        fn finish(&mut self, output: &mut File) -> Result<(), String> {
            for (_, frame) in self.inflight.drain() {
                self.stats.frames_incomplete_expired += 1;
                self.stats.packets_lost_estimate +=
                    u64::from(frame.packet_count.saturating_sub(frame.received_count));
            }
            while let Some((&frame_id, _)) = self.complete.first_key_value() {
                if let Some(next) = self.next_write_frame {
                    if frame_id > next {
                        self.stats.frames_incomplete_expired += frame_id - next;
                        self.stats.packets_lost_estimate += frame_id - next;
                    }
                }
                self.next_write_frame = Some(frame_id);
                self.flush_ready(output, Instant::now() + FRAME_TTL)?;
            }
            output
                .flush()
                .map_err(|err| format!("flush H.264 dump failed: {err}"))
        }
    }

    pub fn run(config: H264RecvConfig) -> Result<(), String> {
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let socket = UdpSocket::bind(format!("{}:{}", config.bind, config.port))
            .map_err(|err| format!("UDP bind failed: {err}"))?;
        socket
            .set_read_timeout(Some(RECEIVE_POLL))
            .map_err(|err| format!("set UDP read timeout failed: {err}"))?;
        let mut output = File::create(Path::new(&config.output))
            .map_err(|err| format!("create output failed: {err}"))?;
        let mut reassembler = H264Reassembler::new();
        let mut buffer = [0u8; MAX_DATAGRAM_SIZE];
        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_packets = 0u64;
        let mut previous_frames = 0u64;
        let mut previous_bytes = 0u64;
        let mut first_packet_at: Option<Instant> = None;
        let mut last_packet_at: Option<Instant> = None;

        eprintln!(
            "h264-recv-dump bind={}:{} output={} idle_timeout_sec={}",
            config.bind, config.port, config.output, config.idle_timeout_sec
        );
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((length, _peer)) => {
                    let received_at = Instant::now();
                    first_packet_at.get_or_insert(received_at);
                    last_packet_at = Some(received_at);
                    reassembler.stats.bytes_received += length as u64;
                    match MediaPacket::decode(&buffer[..length]) {
                        Ok(packet) => reassembler.accept(packet, received_at),
                        Err(err) => {
                            reassembler.stats.packets_invalid += 1;
                            eprintln!("discarding invalid AGM1 packet: {err}");
                        }
                    }
                }
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::TimedOut => {}
                Err(err) => return Err(format!("UDP receive failed: {err}")),
            }

            let now = Instant::now();
            reassembler.expire(now);
            reassembler.flush_ready(&mut output, now)?;

            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let elapsed = now.duration_since(report_at).as_secs_f64().max(0.001);
                let packet_delta = reassembler
                    .stats
                    .packets_received
                    .saturating_sub(previous_packets);
                let frame_delta = reassembler
                    .stats
                    .frames_written
                    .saturating_sub(previous_frames);
                let byte_delta = reassembler
                    .stats
                    .bytes_received
                    .saturating_sub(previous_bytes);
                println!(
                    r#"{{"type":"H264_RECV_STATS","mode":"h264_recv_dump","session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"frames_written":{},"bytes_received":{},"bytes_written":{},"packets_per_sec":{:.2},"fps":{:.2},"mbps":{:.3},"last_frame_id":{},"inflight_frames":{},"completed_waiting":{}}}"#,
                    optional_u64_json(reassembler.session_id),
                    reassembler.stats.packets_received,
                    reassembler.stats.packets_invalid,
                    reassembler.stats.packets_lost_estimate,
                    reassembler.stats.frames_complete,
                    reassembler.stats.frames_incomplete_expired,
                    reassembler.stats.frames_written,
                    reassembler.stats.bytes_received,
                    reassembler.stats.bytes_written,
                    packet_delta as f64 / elapsed,
                    frame_delta as f64 / elapsed,
                    byte_delta as f64 * 8.0 / elapsed / 1_000_000.0,
                    optional_u64_json(reassembler.stats.last_frame_id),
                    reassembler.inflight.len(),
                    reassembler.complete.len()
                );
                io::stdout().flush().ok();
                previous_packets = reassembler.stats.packets_received;
                previous_frames = reassembler.stats.frames_written;
                previous_bytes = reassembler.stats.bytes_received;
                report_at = now;
            }

            let idle_complete = config.idle_timeout_sec > 0
                && last_packet_at.is_some_and(|last| {
                    now.duration_since(last) >= Duration::from_secs(config.idle_timeout_sec)
                });
            if STOP_REQUESTED.load(Ordering::SeqCst) || idle_complete {
                break;
            }
        }

        reassembler.finish(&mut output)?;
        let wall_time_sec = started_at.elapsed().as_secs_f64();
        let active_duration_sec = match (first_packet_at, last_packet_at) {
            (Some(first), Some(last)) => last.duration_since(first).as_secs_f64().max(0.001),
            _ => 0.0,
        };
        println!(
            r#"{{"type":"H264_RECV_DONE","mode":"h264_recv_dump","session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"frames_written":{},"bytes_received":{},"bytes_written":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{:.2},"mbps":{:.3},"last_frame_id":{},"output":"{}"}}"#,
            optional_u64_json(reassembler.session_id),
            reassembler.stats.packets_received,
            reassembler.stats.packets_invalid,
            reassembler.stats.packets_lost_estimate,
            reassembler.stats.frames_complete,
            reassembler.stats.frames_incomplete_expired,
            reassembler.stats.frames_written,
            reassembler.stats.bytes_received,
            reassembler.stats.bytes_written,
            active_duration_sec,
            wall_time_sec,
            reassembler.stats.frames_written as f64 / active_duration_sec.max(0.001),
            reassembler.stats.bytes_received as f64 * 8.0
                / active_duration_sec.max(0.001)
                / 1_000_000.0,
            optional_u64_json(reassembler.stats.last_frame_id),
            json_escape(&config.output)
        );
        io::stdout().flush().ok();
        eprintln!(
            "h264-recv-dump stopped reason={}",
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                "console-control"
            } else {
                "idle-timeout"
            }
        );
        Ok(())
    }

    pub fn run_self_test() -> Result<(), String> {
        let mut reassembler = H264Reassembler::new();
        let now = Instant::now();
        let payload = vec![0x65; 3000];
        let packets = crate::packetize_media_payload(7, 0, 1234, &payload, 0)?;
        for packet in packets.iter().rev() {
            reassembler.accept(MediaPacket::decode(packet)?, now);
        }
        if reassembler.stats.frames_complete != 1 {
            return Err("H.264 reassembler did not complete out-of-order frame".to_string());
        }
        let output_path = std::env::temp_dir().join(format!(
            "agoralink_media_reassembly_{}_{}.h264",
            std::process::id(),
            crate::now_millis()
        ));
        let mut output = File::create(&output_path)
            .map_err(|err| format!("create reassembly self-test output failed: {err}"))?;
        reassembler.flush_ready(&mut output, now)?;
        reassembler.finish(&mut output)?;
        let bytes = std::fs::read(&output_path)
            .map_err(|err| format!("read reassembly self-test output failed: {err}"))?;
        let _ = std::fs::remove_file(&output_path);
        if bytes != payload {
            return Err("H.264 reassembler output mismatch".to_string());
        }
        Ok(())
    }

    fn optional_u64_json(value: Option<u64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| value.to_string())
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }
}

#[cfg(windows)]
pub use platform::{run, run_self_test};

#[cfg(not(windows))]
pub fn run(_config: H264RecvConfig) -> Result<(), String> {
    Err("h264-recv-dump is only supported on Windows".to_string())
}

#[cfg(not(windows))]
pub fn run_self_test() -> Result<(), String> {
    Ok(())
}
