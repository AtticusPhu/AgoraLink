#[derive(Debug)]
pub struct H264RecvConfig {
    pub bind: String,
    pub port: u16,
    pub output: String,
    pub idle_timeout_sec: u64,
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
    use crate::h264_reassembly::{EncodedFrame, H264Reassembler, ReassemblyConfig};

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
        socket
            .set_read_timeout(Some(RECEIVE_POLL))
            .map_err(|err| format!("set UDP read timeout failed: {err}"))?;
        let mut output = File::create(Path::new(&config.output))
            .map_err(|err| format!("create output failed: {err}"))?;
        let mut reassembler = H264Reassembler::new(ReassemblyConfig {
            frame_timeout: Duration::from_millis(1000),
            max_inflight_frames: 120,
        })?;
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
                    match reassembler.accept_datagram(&buffer[..length], received_at) {
                        Ok(frames) => write_frames(
                            &mut output,
                            frames,
                            &mut frames_written,
                            &mut bytes_written,
                        )?,
                        Err(err) => eprintln!("discarding invalid AGM1 packet: {err}"),
                    }
                }
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::TimedOut => {}
                Err(err) => return Err(format!("UDP receive failed: {err}")),
            }

            let now = Instant::now();
            write_frames(
                &mut output,
                reassembler.expire(now),
                &mut frames_written,
                &mut bytes_written,
            )?;

            if now.duration_since(report_at) >= Duration::from_secs(1) {
                let elapsed = now.duration_since(report_at).as_secs_f64().max(0.001);
                let stats = reassembler.stats();
                let packet_delta = stats.packets_received.saturating_sub(previous_packets);
                let frame_delta = frames_written.saturating_sub(previous_frames);
                let byte_delta = stats.bytes_received.saturating_sub(previous_bytes);
                println!(
                    r#"{{"type":"H264_RECV_STATS","mode":"h264_recv_dump","session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"frames_written":{},"bytes_received":{},"bytes_written":{},"packets_per_sec":{:.2},"fps":{:.2},"mbps":{:.3},"last_frame_id":{},"inflight_frames":{},"completed_waiting":{}}}"#,
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
                    reassembler.completed_waiting_len()
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

        write_frames(
            &mut output,
            reassembler.finish(),
            &mut frames_written,
            &mut bytes_written,
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
            r#"{{"type":"H264_RECV_DONE","mode":"h264_recv_dump","session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"frames_written":{},"bytes_received":{},"bytes_written":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{:.2},"mbps":{:.3},"last_frame_id":{},"output":"{}"}}"#,
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

    fn write_frames(
        output: &mut File,
        frames: Vec<EncodedFrame>,
        frames_written: &mut u64,
        bytes_written: &mut u64,
    ) -> Result<(), String> {
        for frame in frames {
            output
                .write_all(&frame.bytes)
                .map_err(|err| format!("write H.264 dump failed: {err}"))?;
            *frames_written += 1;
            *bytes_written += frame.bytes.len() as u64;
        }
        Ok(())
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
