#[derive(Debug)]
pub struct H264RecvConfig {
    pub bind: String,
    pub port: u16,
    pub output: String,
    pub idle_timeout_sec: u64,
    pub drop_damaged_gop: bool,
    pub reorder_wait_ms: Option<u64>,
}

#[cfg(windows)]
mod platform {
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
    use crate::h264_reassembly::{
        DamagedGopTracker, EncodedFrame, H264Reassembler, ReassemblyConfig, ReorderWait,
    };

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

    pub fn run(config: H264RecvConfig) -> Result<(), String> {
        if config.output.trim().is_empty() {
            return Err("output path must not be empty".to_string());
        }
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let socket = UdpSocket::bind(format!("{}:{}", config.bind, config.port))
            .map_err(|err| format!("UDP bind failed: {err}"))?;
        let udp_recv_buffer_bytes = crate::udp_socket::configure_receive_buffer(
            &socket,
            crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES,
        )?;
        socket
            .set_read_timeout(Some(RECEIVE_POLL))
            .map_err(|err| format!("set UDP read timeout failed: {err}"))?;
        let mut output = File::create(Path::new(&config.output))
            .map_err(|err| format!("create output failed: {err}"))?;
        let mut reassembler = H264Reassembler::new(ReassemblyConfig {
            frame_timeout: Duration::from_millis(1000),
            reorder_wait: config
                .reorder_wait_ms
                .map_or(ReorderWait::Auto, |milliseconds| {
                    ReorderWait::Fixed(Duration::from_millis(milliseconds))
                }),
            max_inflight_frames: 120,
        })?;
        let mut damaged_gop = DamagedGopTracker::new(config.drop_damaged_gop);
        let mut previous_expired = 0u64;
        let mut buffer = [0u8; MAX_DATAGRAM_SIZE];
        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_packets = 0u64;
        let mut previous_frames = 0u64;
        let mut previous_bytes = 0u64;
        let mut frames_written = 0u64;
        let mut bytes_written = 0u64;
        let mut first_packet_at: Option<Instant> = None;
        let mut last_packet_at: Option<Instant> = None;
        let mut profile_change_sequence = 0u64;
        let mut profile_changes_received = 0u64;
        let mut stale_profile_packets_dropped = 0u64;
        let mut waiting_profile_idr = false;

        eprintln!(
            "h264-recv-dump bind={}:{} output={} idle_timeout_sec={} drop_damaged_gop={}",
            config.bind,
            config.port,
            config.output,
            config.idle_timeout_sec,
            config.drop_damaged_gop
        );
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((length, _peer)) => {
                    let received_at = Instant::now();
                    first_packet_at.get_or_insert(received_at);
                    last_packet_at = Some(received_at);
                    if crate::media_control::is_profile_change(&buffer[..length]) {
                        if let Ok(change) =
                            crate::media_control::ProfileChange::decode(&buffer[..length])
                        {
                            if change.change_sequence > profile_change_sequence
                                && reassembler.session_id() == Some(change.old_session_id)
                            {
                                reassembler.switch_session(change.new_session_id)?;
                                damaged_gop.reset_for_profile_change();
                                previous_expired = reassembler.stats().frames_incomplete_expired;
                                waiting_profile_idr = true;
                                profile_change_sequence = change.change_sequence;
                                profile_changes_received =
                                    profile_changes_received.saturating_add(1);
                            }
                        }
                        continue;
                    }
                    if crate::repair::media_packet_key(&buffer[..length]).is_some_and(
                        |(packet_session, _, _)| {
                            reassembler
                                .session_id()
                                .is_some_and(|session_id| session_id != packet_session)
                        },
                    ) {
                        stale_profile_packets_dropped =
                            stale_profile_packets_dropped.saturating_add(1);
                        continue;
                    }
                    match reassembler.accept_datagram(&buffer[..length], received_at) {
                        Ok(frames) => {
                            detect_damage(
                                &reassembler,
                                &mut previous_expired,
                                &mut damaged_gop,
                                received_at,
                            );
                            write_frames(
                                &mut output,
                                frames,
                                &mut damaged_gop,
                                received_at,
                                &mut frames_written,
                                &mut bytes_written,
                                &mut waiting_profile_idr,
                            )?
                        }
                        Err(_) => {}
                    }
                }
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::TimedOut => {}
                Err(err) => return Err(format!("UDP receive failed: {err}")),
            }

            let now = Instant::now();
            let expired_frames = reassembler.expire(now);
            detect_damage(&reassembler, &mut previous_expired, &mut damaged_gop, now);
            write_frames(
                &mut output,
                expired_frames,
                &mut damaged_gop,
                now,
                &mut frames_written,
                &mut bytes_written,
                &mut waiting_profile_idr,
            )?;

            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let elapsed = now.duration_since(report_at).as_secs_f64().max(0.001);
                let stats = reassembler.stats();
                let packet_delta = stats.packets_received.saturating_sub(previous_packets);
                let frame_delta = frames_written.saturating_sub(previous_frames);
                let byte_delta = stats.bytes_received.saturating_sub(previous_bytes);
                println!(
                    r#"{{"type":"H264_RECV_STATS","mode":"h264_recv_dump","session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"frames_written":{},"bytes_received":{},"bytes_written":{},"packets_per_sec":{:.2},"fps":{:.2},"mbps":{:.3},"last_frame_id":{},"inflight_frames":{},"completed_waiting":{},"udp_recv_buffer_bytes":{},"profile_changes_received":{},"profile_change_sequence":{},"stale_profile_packets_dropped":{},"waiting_profile_idr":{},{} }}"#,
                    optional_u64_json(reassembler.session_id()),
                    stats.packets_received,
                    stats.packets_invalid,
                    stats.packets_lost_estimate,
                    stats.frames_complete,
                    stats.frames_incomplete_expired,
                    frames_written,
                    stats.bytes_received,
                    bytes_written,
                    packet_delta as f64 / elapsed,
                    frame_delta as f64 / elapsed,
                    byte_delta as f64 * 8.0 / elapsed / 1_000_000.0,
                    optional_u64_json(stats.last_frame_id),
                    reassembler.inflight_len(),
                    reassembler.completed_waiting_len(),
                    udp_recv_buffer_bytes,
                    profile_changes_received,
                    profile_change_sequence,
                    stale_profile_packets_dropped,
                    waiting_profile_idr,
                    recovery_fragment(&damaged_gop, &reassembler)
                );
                io::stdout().flush().ok();
                previous_packets = stats.packets_received;
                previous_frames = frames_written;
                previous_bytes = stats.bytes_received;
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

        let finished_frames = reassembler.finish();
        let finished_at = Instant::now();
        detect_damage(
            &reassembler,
            &mut previous_expired,
            &mut damaged_gop,
            finished_at,
        );
        write_frames(
            &mut output,
            finished_frames,
            &mut damaged_gop,
            finished_at,
            &mut frames_written,
            &mut bytes_written,
            &mut waiting_profile_idr,
        )?;
        output
            .flush()
            .map_err(|err| format!("flush H.264 dump failed: {err}"))?;
        let stats = reassembler.stats();
        let wall_time_sec = started_at.elapsed().as_secs_f64();
        let active_duration_sec = match (first_packet_at, last_packet_at) {
            (Some(first), Some(last)) => last.duration_since(first).as_secs_f64().max(0.001),
            _ => 0.0,
        };
        println!(
            r#"{{"type":"H264_RECV_DONE","mode":"h264_recv_dump","session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"frames_written":{},"bytes_received":{},"bytes_written":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{:.2},"mbps":{:.3},"last_frame_id":{},"output":"{}","udp_recv_buffer_bytes":{},"profile_changes_received":{},"profile_change_sequence":{},"stale_profile_packets_dropped":{},"waiting_profile_idr":{},{} }}"#,
            optional_u64_json(reassembler.session_id()),
            stats.packets_received,
            stats.packets_invalid,
            stats.packets_lost_estimate,
            stats.frames_complete,
            stats.frames_incomplete_expired,
            frames_written,
            stats.bytes_received,
            bytes_written,
            active_duration_sec,
            wall_time_sec,
            frames_written as f64 / active_duration_sec.max(0.001),
            stats.bytes_received as f64 * 8.0 / active_duration_sec.max(0.001) / 1_000_000.0,
            optional_u64_json(stats.last_frame_id),
            json_escape(&config.output),
            udp_recv_buffer_bytes,
            profile_changes_received,
            profile_change_sequence,
            stale_profile_packets_dropped,
            waiting_profile_idr,
            recovery_fragment(&damaged_gop, &reassembler)
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

    fn write_frames(
        output: &mut File,
        frames: Vec<EncodedFrame>,
        damaged_gop: &mut DamagedGopTracker,
        now: Instant,
        frames_written: &mut u64,
        bytes_written: &mut u64,
        waiting_profile_idr: &mut bool,
    ) -> Result<(), String> {
        for frame in frames {
            let Some(frame) = damaged_gop.prepare_frame(frame, now) else {
                continue;
            };
            if *waiting_profile_idr {
                let summary = crate::h264_annex_b::summarize_nals(&frame.bytes);
                if !summary.has_idr_slice || !summary.has_sps || !summary.has_pps {
                    continue;
                }
                *waiting_profile_idr = false;
            }
            output
                .write_all(&frame.bytes)
                .map_err(|err| format!("write H.264 dump failed: {err}"))?;
            *frames_written += 1;
            *bytes_written += frame.bytes.len() as u64;
        }
        Ok(())
    }

    fn detect_damage(
        reassembler: &H264Reassembler,
        previous_expired: &mut u64,
        damaged_gop: &mut DamagedGopTracker,
        now: Instant,
    ) {
        let current_expired = reassembler.stats().frames_incomplete_expired;
        if current_expired > *previous_expired {
            damaged_gop.mark_damaged(now, reassembler.stats().last_damaged_frame_id);
            *previous_expired = current_expired;
        }
    }

    fn recovery_fragment(tracker: &DamagedGopTracker, reassembler: &H264Reassembler) -> String {
        let stats = tracker.stats();
        let reassembly = reassembler.stats();
        format!(
            r#""reassembly_frames_active":{},"reassembly_packets_active":{},"reassembly_fast_path_enabled":true,"reassembly_allocations_estimate":{},"reassembly_complete_scan_count":{},"fec_mode":"{}","fec_packets_received":{},"fec_frames_recovered":{},"fec_packets_recovered":{},"fec_recovery_failed_multi_missing":{},"fec_recovery_failed_no_parity":{},"fec_recovery_failed_invalid":{},"frames_missing_after_fec":{},"frames_dropped_after_fec":{},"damaged_gop_count":{},"frames_discarded_damaged_gop":{},"frames_discarded_waiting_keyframe":{},"waiting_keyframe":{},"waiting_keyframe_entries":{},"waiting_keyframe_exits":{},"idr_frames_received":{},"idr_frames_used_for_recovery":{},"non_idr_frames_discarded_waiting":{},"recovery_wait_ms_avg":{:.3},"recovery_wait_ms_max":{:.3},"recovery_wait_frames_avg":{:.3},"recovery_wait_frames_max":{},"next_decode_frame_id":{},"decode_gate_stalls":{},"decode_gate_gap_events":{},"decode_gate_gap_to_damage_ms_avg":{:.3},"decode_gate_gap_to_damage_ms_max":{:.3},"frames_buffered_waiting_order":{},"frames_discarded_decode_gate":{},"reorder_wait_ms":{}"#,
            reassembly.reassembly_frames_active,
            reassembly.reassembly_packets_active,
            reassembly.reassembly_allocations_estimate,
            reassembly.reassembly_complete_scan_count,
            if reassembly.fec_packets_received > 0
                || reassembly.fec_protected_data_packets_received > 0
            {
                "single-xor"
            } else {
                "off"
            },
            reassembly.fec_packets_received,
            reassembly.fec_frames_recovered,
            reassembly.fec_packets_recovered,
            reassembly.fec_recovery_failed_multi_missing,
            reassembly.fec_recovery_failed_no_parity,
            reassembly.fec_recovery_failed_invalid,
            reassembly.frames_missing_after_fec,
            reassembly.frames_dropped_after_fec,
            stats.damaged_gop_count,
            stats.frames_discarded_damaged_gop,
            stats.frames_discarded_waiting_keyframe,
            tracker.waiting_keyframe(),
            stats.waiting_keyframe_entries,
            stats.waiting_keyframe_exits,
            stats.idr_frames_received,
            stats.idr_frames_used_for_recovery,
            stats.non_idr_frames_discarded_waiting,
            stats.recovery_wait_ms_avg(),
            stats.recovery_wait_ms_max,
            stats.recovery_wait_frames_avg(),
            stats.recovery_wait_frames_max,
            optional_u64_json(reassembly.next_decode_frame_id),
            reassembly.decode_gate_stalls,
            reassembly.decode_gate_gap_events,
            reassembly.decode_gate_gap_to_damage_ms_avg(),
            reassembly.decode_gate_gap_to_damage_ms_max,
            reassembler.completed_waiting_len(),
            reassembly.frames_discarded_decode_gate,
            reassembly.reorder_wait_ms,
        )
    }

    pub fn run_self_test() -> Result<(), String> {
        crate::h264_reassembly::run_self_test()
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
