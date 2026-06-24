#[derive(Debug)]
pub struct H264SendConfig {
    pub host: String,
    pub port: u16,
    pub duration_sec: u64,
    pub target_fps: u32,
    pub bitrate_mbps: f64,
}

#[cfg(windows)]
mod platform {
    use std::io::{self, Write};
    use std::net::UdpSocket;
    use std::thread;

    use super::H264SendConfig;
    use crate::capture_encode_probe::{
        self, CaptureEncodeObserver, CapturePipelineDone, CapturePipelineStats,
    };
    use crate::wmf_h264_encoder::{EncodedSample, ENCODER_NAME};
    use crate::{
        make_session_id, now_millis, packetize_media_payload, FLAG_CONFIG, FLAG_H264_ANNEX_B,
        FLAG_KEYFRAME,
    };

    struct UdpObserver {
        socket: UdpSocket,
        target: String,
        session_id: u64,
        next_frame_id: u64,
        packets_sent: u64,
        frames_sent: u64,
        bytes_sent: u64,
        h264_bytes: u64,
        keyframes: u64,
        config_frames: u64,
        previous_packets: u64,
        previous_frames: u64,
        previous_bytes: u64,
    }

    impl UdpObserver {
        fn new(host: &str, port: u16) -> Result<Self, String> {
            let target = format!("{host}:{port}");
            let socket =
                UdpSocket::bind("0.0.0.0:0").map_err(|err| format!("UDP bind failed: {err}"))?;
            socket
                .connect(&target)
                .map_err(|err| format!("UDP connect to {target} failed: {err}"))?;
            Ok(Self {
                socket,
                target,
                session_id: make_session_id(),
                next_frame_id: 0,
                packets_sent: 0,
                frames_sent: 0,
                bytes_sent: 0,
                h264_bytes: 0,
                keyframes: 0,
                config_frames: 0,
                previous_packets: 0,
                previous_frames: 0,
                previous_bytes: 0,
            })
        }

        fn print_done(&self, done: CapturePipelineDone) {
            println!(
                r#"{{"type":"H264_SEND_DONE","mode":"h264_send_probe","encoder":"{}","target":"{}","session_id":{},"packets_sent":{},"frames_sent":{},"bytes_sent":{},"h264_bytes":{},"keyframes":{},"config_frames":{},"raw_frames":{},"accepted_frames":{},"skipped_frames":{},"frames_in":{},"samples_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{:.2},"mbps":{:.3},"width":{},"height":{},"dropped":{}}}"#,
                ENCODER_NAME,
                json_escape(&self.target),
                self.session_id,
                self.packets_sent,
                self.frames_sent,
                self.bytes_sent,
                self.h264_bytes,
                self.keyframes,
                self.config_frames,
                done.raw_frames,
                done.accepted_frames,
                done.skipped_frames,
                done.encoder.frames_in,
                done.encoder.samples_out,
                done.media_duration_sec,
                done.wall_time_sec,
                self.frames_sent as f64 / done.wall_time_sec.max(0.001),
                self.bytes_sent as f64 * 8.0 / done.wall_time_sec.max(0.001) / 1_000_000.0,
                done.width,
                done.height,
                done.dropped
            );
            io::stdout().flush().ok();
        }
    }

    impl CaptureEncodeObserver for UdpObserver {
        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String> {
            let mut flags = 0;
            if sample.keyframe == Some(true) {
                flags |= FLAG_KEYFRAME;
                self.keyframes += 1;
            }
            let nal_types: Vec<u8> = annex_b_nal_types(&sample.bytes).collect();
            if !nal_types.is_empty() {
                flags |= FLAG_H264_ANNEX_B;
            }
            if nal_types
                .iter()
                .any(|nal_type| *nal_type == 7 || *nal_type == 8)
            {
                flags |= FLAG_CONFIG;
                self.config_frames += 1;
            }

            let packets = packetize_media_payload(
                self.session_id,
                self.next_frame_id,
                now_millis(),
                &sample.bytes,
                flags,
            )?;
            for (index, packet) in packets.iter().enumerate() {
                let sent = self
                    .socket
                    .send(packet)
                    .map_err(|err| format!("UDP send to {} failed: {err}", self.target))?;
                if sent != packet.len() {
                    return Err(format!(
                        "UDP short send to {}: expected {}, sent {}",
                        self.target,
                        packet.len(),
                        sent
                    ));
                }
                self.packets_sent += 1;
                self.bytes_sent += sent as u64;
                if index != 0 && index % 32 == 0 {
                    thread::yield_now();
                }
            }
            self.frames_sent += 1;
            self.h264_bytes += sample.bytes.len() as u64;
            self.next_frame_id += 1;
            Ok(())
        }

        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String> {
            let packets_delta = self.packets_sent.saturating_sub(self.previous_packets);
            let frames_delta = self.frames_sent.saturating_sub(self.previous_frames);
            let bytes_delta = self.bytes_sent.saturating_sub(self.previous_bytes);
            println!(
                r#"{{"type":"H264_SEND_STATS","mode":"h264_send_probe","encoder":"{}","target":"{}","session_id":{},"packets_sent":{},"frames_sent":{},"bytes_sent":{},"packets_per_sec":{},"fps":{},"mbps":{:.3},"raw_fps":{:.2},"accepted_fps":{:.2},"encode_fps":{:.2},"target_fps":{},"width":{},"height":{},"dropped":{}}}"#,
                ENCODER_NAME,
                json_escape(&self.target),
                self.session_id,
                self.packets_sent,
                self.frames_sent,
                self.bytes_sent,
                packets_delta,
                frames_delta,
                bytes_delta as f64 * 8.0 / 1_000_000.0,
                stats.raw_fps,
                stats.accepted_fps,
                stats.encode_fps,
                stats.target_fps,
                stats.width,
                stats.height,
                stats.dropped
            );
            io::stdout().flush().ok();
            self.previous_packets = self.packets_sent;
            self.previous_frames = self.frames_sent;
            self.previous_bytes = self.bytes_sent;
            Ok(())
        }
    }

    pub fn run(config: H264SendConfig) -> Result<(), String> {
        let mut observer = UdpObserver::new(&config.host, config.port)?;
        eprintln!(
            "h264-send-probe target={} duration_sec={} target_fps={} bitrate_mbps={} packet_payload_max=1200",
            observer.target, config.duration_sec, config.target_fps, config.bitrate_mbps
        );
        let pipeline_config = capture_encode_probe::CaptureEncodeConfig {
            duration_sec: config.duration_sec,
            target_fps: config.target_fps,
            bitrate_mbps: config.bitrate_mbps,
            output: String::new(),
        };
        let done = capture_encode_probe::run_with_observer(&pipeline_config, &mut observer)?;
        observer.print_done(done);
        Ok(())
    }

    fn annex_b_nal_types(bytes: &[u8]) -> impl Iterator<Item = u8> + '_ {
        (0..bytes.len()).filter_map(move |index| {
            let nal_start = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
                index + 4
            } else if bytes[index..].starts_with(&[0, 0, 1]) {
                index + 3
            } else {
                return None;
            };
            bytes.get(nal_start).map(|value| value & 0x1f)
        })
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }
}

#[cfg(windows)]
pub use platform::run;

#[cfg(not(windows))]
pub fn run(_config: H264SendConfig) -> Result<(), String> {
    Err("h264-send-probe is only supported on Windows".to_string())
}
