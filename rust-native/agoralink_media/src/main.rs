use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::net::UdpSocket;
use std::process;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod bgra_to_nv12;
mod capture_encode_probe;
mod capture_probe;
mod color_spec;
mod color_test_pattern;
mod decoded_frame_renderer;
mod encode_probe;
mod h264_annex_b;
mod h264_file_viewer;
mod h264_reassembly;
mod h264_recv_dump;
mod h264_recv_view;
mod h264_send_probe;
mod nv12_synthetic;
mod nv12_to_bgra;
mod wgc_latest_capture;
mod win32_gdi_viewer;
mod wmf_h264_decoder;
mod wmf_h264_encoder;
mod wmf_probe;

const MAGIC: &[u8; 4] = b"AGM1";
const VERSION: u8 = 1;
pub(crate) const STREAM_VIDEO: u8 = 1;
pub(crate) const FLAG_KEYFRAME: u16 = 1 << 0;
pub(crate) const FLAG_END_OF_FRAME: u16 = 1 << 1;
pub(crate) const FLAG_CONFIG: u16 = 1 << 2;
pub(crate) const FLAG_H264_ANNEX_B: u16 = 1 << 3;
const HEADER_LEN: usize = 38;
const MAX_UDP_PAYLOAD: usize = 1200;
pub(crate) const MAX_MEDIA_PAYLOAD: usize = MAX_UDP_PAYLOAD - HEADER_LEN;
const FRAME_TTL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone)]
pub(crate) struct MediaPacket {
    pub(crate) stream_id: u8,
    pub(crate) flags: u16,
    pub(crate) session_id: u64,
    pub(crate) frame_id: u64,
    pub(crate) packet_index: u16,
    pub(crate) packet_count: u16,
    pub(crate) timestamp_ms: u64,
    pub(crate) payload: Vec<u8>,
}

impl MediaPacket {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        if self.payload.len() > MAX_MEDIA_PAYLOAD {
            return Err(format!("payload too large: {}", self.payload.len()));
        }
        if self.payload.len() > u16::MAX as usize {
            return Err("payload length exceeds u16".to_string());
        }
        if self.packet_count == 0 {
            return Err("packet_count must be greater than zero".to_string());
        }
        if self.packet_index >= self.packet_count {
            return Err("packet_index must be less than packet_count".to_string());
        }

        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.stream_id);
        out.extend_from_slice(&self.flags.to_be_bytes());
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.frame_id.to_be_bytes());
        out.extend_from_slice(&self.packet_index.to_be_bytes());
        out.extend_from_slice(&self.packet_count.to_be_bytes());
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < HEADER_LEN {
            return Err("packet too short".to_string());
        }
        if &buf[0..4] != MAGIC {
            return Err("bad magic".to_string());
        }
        if buf[4] != VERSION {
            return Err(format!("unsupported version: {}", buf[4]));
        }

        let stream_id = buf[5];
        let flags = u16::from_be_bytes([buf[6], buf[7]]);
        let session_id = u64::from_be_bytes(buf[8..16].try_into().unwrap());
        let frame_id = u64::from_be_bytes(buf[16..24].try_into().unwrap());
        let packet_index = u16::from_be_bytes([buf[24], buf[25]]);
        let packet_count = u16::from_be_bytes([buf[26], buf[27]]);
        let timestamp_ms = u64::from_be_bytes(buf[28..36].try_into().unwrap());
        let payload_len = u16::from_be_bytes([buf[36], buf[37]]) as usize;

        if packet_count == 0 {
            return Err("packet_count is zero".to_string());
        }
        if packet_index >= packet_count {
            return Err("packet_index out of range".to_string());
        }
        if payload_len > MAX_MEDIA_PAYLOAD {
            return Err(format!("payload_len exceeds limit: {}", payload_len));
        }
        if HEADER_LEN + payload_len > buf.len() {
            return Err("payload_len exceeds datagram length".to_string());
        }

        Ok(Self {
            stream_id,
            flags,
            session_id,
            frame_id,
            packet_index,
            packet_count,
            timestamp_ms,
            payload: buf[HEADER_LEN..HEADER_LEN + payload_len].to_vec(),
        })
    }
}

#[derive(Debug)]
enum Command {
    SelfTest,
    Sender {
        host: String,
        port: u16,
        fps: u32,
        bitrate_mbps: f64,
    },
    Receiver {
        bind: String,
        port: u16,
    },
    CaptureProbe {
        duration_sec: u64,
        target_fps: u32,
    },
    WmfProbe,
    EncodeProbe(encode_probe::EncodeProbeConfig),
    CaptureEncodeProbe(capture_encode_probe::CaptureEncodeConfig),
    H264SendProbe(h264_send_probe::H264SendConfig),
    H264RecvDump(h264_recv_dump::H264RecvConfig),
    H264RecvView(h264_recv_view::H264RecvViewConfig),
    H264FileViewer(h264_file_viewer::H264FileViewerConfig),
    ColorTestPattern(color_test_pattern::ColorTestPatternConfig),
    Help,
}

#[derive(Debug, Default)]
struct ReceiverStats {
    packets_received: u64,
    packets_dropped_or_lost_estimate: u64,
    frames_complete: u64,
    frames_incomplete_expired: u64,
    last_frame_id: u64,
}

#[derive(Debug)]
struct FrameAssembly {
    packet_count: u16,
    received: Vec<bool>,
    received_count: u16,
    first_seen: Instant,
    bytes: usize,
}

#[derive(Debug)]
struct Reassembler {
    frames: HashMap<u64, FrameAssembly>,
    ttl: Duration,
    stats: ReceiverStats,
}

impl Reassembler {
    fn new(ttl: Duration) -> Self {
        Self {
            frames: HashMap::new(),
            ttl,
            stats: ReceiverStats::default(),
        }
    }

    fn accept(&mut self, packet: MediaPacket, now: Instant) -> bool {
        self.stats.packets_received += 1;
        if packet.stream_id != STREAM_VIDEO || packet.packet_count == 0 {
            self.stats.packets_dropped_or_lost_estimate += 1;
            return false;
        }

        let entry = self
            .frames
            .entry(packet.frame_id)
            .or_insert_with(|| FrameAssembly {
                packet_count: packet.packet_count,
                received: vec![false; packet.packet_count as usize],
                received_count: 0,
                first_seen: now,
                bytes: 0,
            });

        if entry.packet_count != packet.packet_count {
            self.stats.packets_dropped_or_lost_estimate += 1;
            return false;
        }

        let index = packet.packet_index as usize;
        if index >= entry.received.len() {
            self.stats.packets_dropped_or_lost_estimate += 1;
            return false;
        }

        if !entry.received[index] {
            entry.received[index] = true;
            entry.received_count += 1;
            entry.bytes += packet.payload.len();
        }

        if entry.received_count == entry.packet_count {
            self.frames.remove(&packet.frame_id);
            self.stats.frames_complete += 1;
            self.stats.last_frame_id = packet.frame_id;
            return true;
        }
        false
    }

    fn expire(&mut self, now: Instant) {
        let ttl = self.ttl;
        let expired: Vec<u64> = self
            .frames
            .iter()
            .filter_map(|(frame_id, frame)| {
                if now.duration_since(frame.first_seen) > ttl {
                    Some(*frame_id)
                } else {
                    None
                }
            })
            .collect();

        for frame_id in expired {
            if let Some(frame) = self.frames.remove(&frame_id) {
                self.stats.frames_incomplete_expired += 1;
                self.stats.packets_dropped_or_lost_estimate +=
                    u64::from(frame.packet_count.saturating_sub(frame.received_count));
                self.stats.last_frame_id = frame_id;
            }
        }
    }
}

fn main() {
    match parse_args(env::args().skip(1).collect()) {
        Ok(Command::SelfTest) => {
            if let Err(err) = run_self_test() {
                eprintln!("self-test failed: {err}");
                println!(
                    r#"{{"type":"SELF_TEST","ok":false,"error":"{}"}}"#,
                    json_escape(&err)
                );
                process::exit(1);
            }
            println!(r#"{{"type":"SELF_TEST","ok":true,"packet_format":"AGM1"}}"#);
        }
        Ok(Command::Sender {
            host,
            port,
            fps,
            bitrate_mbps,
        }) => {
            if let Err(err) = run_sender(&host, port, fps, bitrate_mbps) {
                eprintln!("sender error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::Receiver { bind, port }) => {
            if let Err(err) = run_receiver(&bind, port) {
                eprintln!("receiver error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::CaptureProbe {
            duration_sec,
            target_fps,
        }) => {
            if let Err(err) = capture_probe::run(duration_sec, target_fps) {
                eprintln!("capture-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::WmfProbe) => {
            if let Err(err) = wmf_probe::run() {
                eprintln!("wmf-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::EncodeProbe(config)) => {
            if let Err(err) = encode_probe::run(config) {
                eprintln!("encode-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::CaptureEncodeProbe(config)) => {
            if let Err(err) = capture_encode_probe::run(config) {
                eprintln!("capture-encode-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264SendProbe(config)) => {
            if let Err(err) = h264_send_probe::run(config) {
                eprintln!("h264-send-probe error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264RecvDump(config)) => {
            if let Err(err) = h264_recv_dump::run(config) {
                eprintln!("h264-recv-dump error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264RecvView(config)) => {
            if let Err(err) = h264_recv_view::run(config) {
                eprintln!("h264-recv-view error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::H264FileViewer(config)) => {
            if let Err(err) = h264_file_viewer::run(config) {
                eprintln!("h264-file-viewer error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::ColorTestPattern(config)) => {
            if let Err(err) = color_test_pattern::run(config) {
                eprintln!("color-test-pattern error: {err}");
                process::exit(1);
            }
        }
        Ok(Command::Help) => {
            print_help();
        }
        Err(err) => {
            eprintln!("{err}");
            eprintln!();
            print_help();
            process::exit(2);
        }
    }
}

fn parse_args(args: Vec<String>) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Help);
    }

    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "self-test" => {
            if args.len() == 1 {
                Ok(Command::SelfTest)
            } else {
                Err("self-test does not accept extra arguments".to_string())
            }
        }
        "sender" => parse_sender_args(&args[1..]),
        "receiver" => parse_receiver_args(&args[1..]),
        "capture-probe" => parse_capture_probe_args(&args[1..]),
        "wmf-probe" => {
            if args.len() == 1 {
                Ok(Command::WmfProbe)
            } else {
                Err("wmf-probe does not accept extra arguments".to_string())
            }
        }
        "encode-probe" => parse_encode_probe_args(&args[1..]),
        "capture-encode-probe" => parse_capture_encode_probe_args(&args[1..]),
        "h264-send-probe" => parse_h264_send_probe_args(&args[1..]),
        "h264-recv-dump" => parse_h264_recv_dump_args(&args[1..]),
        "h264-recv-view" => parse_h264_recv_view_args(&args[1..]),
        "h264-file-viewer" => parse_h264_file_viewer_args(&args[1..]),
        "color-test-pattern" => parse_color_test_pattern_args(&args[1..]),
        other => Err(format!("unknown command: {other}")),
    }
}

fn parse_sender_args(args: &[String]) -> Result<Command, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 50120;
    let mut fps: u32 = 30;
    let mut bitrate_mbps: f64 = 4.0;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = required_value(args, i, "--host")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "--fps" => {
                i += 1;
                fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown sender argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::Sender {
        host,
        port,
        fps,
        bitrate_mbps,
    })
}

fn parse_receiver_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port: u16 = 50120;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown receiver argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::Receiver { bind, port })
}

fn parse_capture_probe_args(args: &[String]) -> Result<Command, String> {
    let mut duration_sec = 10u64;
    let mut target_fps = 30u32;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown capture-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::CaptureProbe {
        duration_sec,
        target_fps,
    })
}

fn parse_encode_probe_args(args: &[String]) -> Result<Command, String> {
    let mut width = 1280u32;
    let mut height = 720u32;
    let mut fps = 30u32;
    let mut duration_sec = 5u64;
    let mut bitrate_mbps = 4.0f64;
    let mut output = "synthetic_720p30.h264".to_string();
    let mut encoder = encode_probe::EncoderChoice::Auto;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--width" => {
                i += 1;
                width = parse_dimension(required_value(args, i, "--width")?, "width")?;
            }
            "--height" => {
                i += 1;
                height = parse_dimension(required_value(args, i, "--height")?, "height")?;
            }
            "--fps" => {
                i += 1;
                fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
                if output.trim().is_empty() {
                    return Err("output path must not be empty".to_string());
                }
            }
            "--encoder" => {
                i += 1;
                encoder = parse_encoder_choice(required_value(args, i, "--encoder")?)?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown encode-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::EncodeProbe(encode_probe::EncodeProbeConfig {
        width,
        height,
        fps,
        duration_sec,
        bitrate_mbps,
        output,
        encoder,
        color_spec,
    }))
}

fn parse_capture_encode_probe_args(args: &[String]) -> Result<Command, String> {
    let mut duration_sec = 5u64;
    let mut target_fps = 30u32;
    let mut bitrate_mbps = 4.0f64;
    let mut out_width = 1280u32;
    let mut out_height = 720u32;
    let mut output = "capture_720p30.h264".to_string();
    let mut color_spec = color_spec::ColorSpec::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "--out-width" => {
                i += 1;
                out_width = parse_dimension(required_value(args, i, "--out-width")?, "out-width")?;
            }
            "--out-height" => {
                i += 1;
                out_height =
                    parse_dimension(required_value(args, i, "--out-height")?, "out-height")?;
            }
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
                if output.trim().is_empty() {
                    return Err("output path must not be empty".to_string());
                }
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown capture-encode-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::CaptureEncodeProbe(
        capture_encode_probe::CaptureEncodeConfig {
            duration_sec,
            target_fps,
            bitrate_mbps,
            out_width,
            out_height,
            output,
            color_spec,
        },
    ))
}

fn parse_h264_send_probe_args(args: &[String]) -> Result<Command, String> {
    let mut host = "127.0.0.1".to_string();
    let mut port = 50130u16;
    let mut duration_sec = 10u64;
    let mut target_fps = 30u32;
    let mut bitrate_mbps = 4.0f64;
    let mut out_width = 1280u32;
    let mut out_height = 720u32;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = required_value(args, i, "--host")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--target-fps" => {
                i += 1;
                target_fps = parse_fps(required_value(args, i, "--target-fps")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "--out-width" => {
                i += 1;
                out_width = parse_dimension(required_value(args, i, "--out-width")?, "out-width")?;
            }
            "--out-height" => {
                i += 1;
                out_height =
                    parse_dimension(required_value(args, i, "--out-height")?, "out-height")?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown h264-send-probe argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::H264SendProbe(h264_send_probe::H264SendConfig {
        host,
        port,
        duration_sec,
        target_fps,
        bitrate_mbps,
        out_width,
        out_height,
        color_spec,
    }))
}

fn parse_color_test_pattern_args(args: &[String]) -> Result<Command, String> {
    let mut output = "color_test_1080p.h264".to_string();
    let mut width = 1920u32;
    let mut height = 1080u32;
    let mut fps = 30u32;
    let mut duration_sec = 3u64;
    let mut bitrate_mbps = 8.0f64;
    let mut color_spec = color_spec::ColorSpec::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
            }
            "--width" => {
                i += 1;
                width = parse_dimension(required_value(args, i, "--width")?, "width")?;
            }
            "--height" => {
                i += 1;
                height = parse_dimension(required_value(args, i, "--height")?, "height")?;
            }
            "--fps" => {
                i += 1;
                fps = parse_fps(required_value(args, i, "--fps")?)?;
            }
            "--duration-sec" => {
                i += 1;
                duration_sec = parse_duration_sec(required_value(args, i, "--duration-sec")?)?;
            }
            "--bitrate-mbps" => {
                i += 1;
                bitrate_mbps = parse_bitrate(required_value(args, i, "--bitrate-mbps")?)?;
            }
            "--color-matrix" => {
                i += 1;
                color_spec = color_spec::ColorSpec::with_matrix(color_spec::ColorMatrix::parse(
                    required_value(args, i, "--color-matrix")?,
                )?);
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown color-test-pattern argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::ColorTestPattern(
        color_test_pattern::ColorTestPatternConfig {
            output,
            width,
            height,
            fps,
            duration_sec,
            bitrate_mbps,
            color_spec,
        },
    ))
}

fn parse_h264_recv_dump_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port = 50130u16;
    let mut output = "received_capture.h264".to_string();
    let mut idle_timeout_sec = 3u64;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = parse_port(required_value(args, i, "--port")?)?;
            }
            "--output" => {
                i += 1;
                output = required_value(args, i, "--output")?.to_string();
                if output.trim().is_empty() {
                    return Err("output path must not be empty".to_string());
                }
            }
            "--idle-timeout-sec" => {
                i += 1;
                idle_timeout_sec =
                    parse_idle_timeout(required_value(args, i, "--idle-timeout-sec")?)?;
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown h264-recv-dump argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::H264RecvDump(h264_recv_dump::H264RecvConfig {
        bind,
        port,
        output,
        idle_timeout_sec,
    }))
}

fn parse_h264_file_viewer_args(args: &[String]) -> Result<Command, String> {
    let mut input = String::new();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--input" => {
                i += 1;
                input = required_value(args, i, "--input")?.to_string();
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown h264-file-viewer argument: {other}")),
        }
        i += 1;
    }
    if input.trim().is_empty() {
        return Err("h264-file-viewer requires --input <path>".to_string());
    }
    Ok(Command::H264FileViewer(
        h264_file_viewer::H264FileViewerConfig { input },
    ))
}

fn parse_h264_recv_view_args(args: &[String]) -> Result<Command, String> {
    let mut bind = "0.0.0.0".to_string();
    let mut port = None;
    let mut frame_timeout_ms = 300u64;
    let mut max_inflight_frames = 120usize;
    let mut max_decode_queue = 30usize;
    let mut strict_decode_order = true;
    let mut debug_dump_frames = None;
    let mut debug_dump_limit = 10usize;
    let mut json_interval_ms = 1000u64;
    let mut title = "AgoraLink Native Viewer".to_string();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                i += 1;
                bind = required_value(args, i, "--bind")?.to_string();
            }
            "--port" => {
                i += 1;
                port = Some(parse_port(required_value(args, i, "--port")?)?);
            }
            "--frame-timeout-ms" => {
                i += 1;
                frame_timeout_ms = parse_milliseconds(
                    required_value(args, i, "--frame-timeout-ms")?,
                    "frame-timeout-ms",
                    1,
                    60_000,
                )?;
            }
            "--max-inflight-frames" => {
                i += 1;
                max_inflight_frames = parse_count(
                    required_value(args, i, "--max-inflight-frames")?,
                    "max-inflight-frames",
                    1,
                    10_000,
                )?;
            }
            "--max-decode-queue" => {
                i += 1;
                max_decode_queue = parse_count(
                    required_value(args, i, "--max-decode-queue")?,
                    "max-decode-queue",
                    1,
                    1000,
                )?;
            }
            "--strict-decode-order" => {
                i += 1;
                strict_decode_order =
                    parse_bool(required_value(args, i, "--strict-decode-order")?)?;
            }
            "--debug-dump-frames" => {
                i += 1;
                debug_dump_frames =
                    Some(required_value(args, i, "--debug-dump-frames")?.to_string());
            }
            "--debug-dump-limit" => {
                i += 1;
                debug_dump_limit = parse_count(
                    required_value(args, i, "--debug-dump-limit")?,
                    "debug-dump-limit",
                    1,
                    10_000,
                )?;
            }
            "--json-interval-ms" => {
                i += 1;
                json_interval_ms = parse_milliseconds(
                    required_value(args, i, "--json-interval-ms")?,
                    "json-interval-ms",
                    100,
                    60_000,
                )?;
            }
            "--title" => {
                i += 1;
                title = required_value(args, i, "--title")?.to_string();
                if title.trim().is_empty() {
                    return Err("title must not be empty".to_string());
                }
            }
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown h264-recv-view argument: {other}")),
        }
        i += 1;
    }

    Ok(Command::H264RecvView(h264_recv_view::H264RecvViewConfig {
        bind,
        port: port.ok_or_else(|| "h264-recv-view requires --port <port>".to_string())?,
        frame_timeout_ms,
        max_inflight_frames,
        max_decode_queue,
        strict_decode_order,
        debug_dump_frames,
        debug_dump_limit,
        json_interval_ms,
        title,
    }))
}

fn required_value<'a>(args: &'a [String], index: usize, name: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing value for {name}"))
}

fn parse_port(text: &str) -> Result<u16, String> {
    let port: u16 = text.parse().map_err(|_| format!("invalid port: {text}"))?;
    if port == 0 {
        Err("port must be greater than zero".to_string())
    } else {
        Ok(port)
    }
}

fn parse_bool(text: &str) -> Result<bool, String> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!("invalid boolean value: {text}")),
    }
}

fn parse_fps(text: &str) -> Result<u32, String> {
    let fps: u32 = text.parse().map_err(|_| format!("invalid fps: {text}"))?;
    if (1..=240).contains(&fps) {
        Ok(fps)
    } else {
        Err("fps must be between 1 and 240".to_string())
    }
}

fn parse_bitrate(text: &str) -> Result<f64, String> {
    let bitrate: f64 = text
        .parse()
        .map_err(|_| format!("invalid bitrate-mbps: {text}"))?;
    if bitrate.is_finite() && bitrate > 0.0 && bitrate <= 1000.0 {
        Ok(bitrate)
    } else {
        Err("bitrate-mbps must be > 0 and <= 1000".to_string())
    }
}

fn parse_duration_sec(text: &str) -> Result<u64, String> {
    let duration: u64 = text
        .parse()
        .map_err(|_| format!("invalid duration-sec: {text}"))?;
    if (1..=3600).contains(&duration) {
        Ok(duration)
    } else {
        Err("duration-sec must be between 1 and 3600".to_string())
    }
}

fn parse_idle_timeout(text: &str) -> Result<u64, String> {
    let duration: u64 = text
        .parse()
        .map_err(|_| format!("invalid idle-timeout-sec: {text}"))?;
    if duration <= 3600 {
        Ok(duration)
    } else {
        Err("idle-timeout-sec must be between 0 and 3600".to_string())
    }
}

fn parse_milliseconds(text: &str, name: &str, minimum: u64, maximum: u64) -> Result<u64, String> {
    let value: u64 = text
        .parse()
        .map_err(|_| format!("invalid {name}: {text}"))?;
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{name} must be between {minimum} and {maximum}"))
    }
}

fn parse_count(text: &str, name: &str, minimum: usize, maximum: usize) -> Result<usize, String> {
    let value: usize = text
        .parse()
        .map_err(|_| format!("invalid {name}: {text}"))?;
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{name} must be between {minimum} and {maximum}"))
    }
}

fn parse_dimension(text: &str, name: &str) -> Result<u32, String> {
    let value: u32 = text
        .parse()
        .map_err(|_| format!("invalid {name}: {text}"))?;
    if value < 16 || value > 8192 {
        return Err(format!("{name} must be between 16 and 8192"));
    }
    if value % 2 != 0 {
        return Err(format!("{name} must be even for NV12"));
    }
    Ok(value)
}

fn parse_encoder_choice(text: &str) -> Result<encode_probe::EncoderChoice, String> {
    match text {
        "auto" => Ok(encode_probe::EncoderChoice::Auto),
        "software" => Ok(encode_probe::EncoderChoice::Software),
        "hardware" => Ok(encode_probe::EncoderChoice::Hardware),
        _ => Err("encoder must be auto, software, or hardware".to_string()),
    }
}

fn print_help() {
    println!(
        "AgoraLink Native Media prototype\n\n\
Usage:\n\
  agoralink_media self-test\n\
  agoralink_media sender --host <ip> --port <port> --fps <fps> --bitrate-mbps <mbps>\n\
  agoralink_media receiver --bind <ip> --port <port>\n\
  agoralink_media capture-probe --duration-sec <seconds> --target-fps <fps>\n\
  agoralink_media wmf-probe\n\
  agoralink_media encode-probe --width <pixels> --height <pixels> --fps <fps> --duration-sec <seconds> --bitrate-mbps <mbps> --output <path> [--encoder auto|software|hardware] [--color-matrix bt601|bt709]\n\
  agoralink_media capture-encode-probe --duration-sec <seconds> --target-fps <fps> --bitrate-mbps <mbps> --out-width <pixels> --out-height <pixels> --output <path> [--color-matrix bt601|bt709]\n\
  agoralink_media h264-send-probe --host <ip> --port <port> --duration-sec <seconds> --target-fps <fps> --bitrate-mbps <mbps> --out-width <pixels> --out-height <pixels> [--color-matrix bt601|bt709]\n\
  agoralink_media h264-recv-dump --bind <ip> --port <port> --output <path> [--idle-timeout-sec <seconds>]\n\
  agoralink_media h264-recv-view --bind <ip> --port <port> [--frame-timeout-ms <ms>] [--max-inflight-frames <n>] [--max-decode-queue <n>] [--strict-decode-order <true|false>] [--debug-dump-frames <dir>] [--debug-dump-limit <n>] [--json-interval-ms <ms>] [--title <text>]\n\
  agoralink_media h264-file-viewer --input <path>\n\n\
  agoralink_media color-test-pattern --output <path> --width <pixels> --height <pixels> --duration-sec <seconds> [--fps <fps>] [--bitrate-mbps <mbps>] [--color-matrix bt601|bt709]\n\n\
Defaults:\n\
  sender: --host 127.0.0.1 --port 50120 --fps 30 --bitrate-mbps 4\n\
  receiver: --bind 0.0.0.0 --port 50120\n\
  capture-probe: --duration-sec 10 --target-fps 30\n\
  encode-probe: --width 1280 --height 720 --fps 30 --duration-sec 5 --bitrate-mbps 4 --output synthetic_720p30.h264 --encoder auto --color-matrix bt709\n\
  capture-encode-probe: --duration-sec 5 --target-fps 30 --bitrate-mbps 4 --out-width 1280 --out-height 720 --output capture_720p30.h264 --color-matrix bt709\n\
  h264-send-probe: --host 127.0.0.1 --port 50130 --duration-sec 10 --target-fps 30 --bitrate-mbps 4 --out-width 1280 --out-height 720 --color-matrix bt709\n\
  h264-recv-dump: --bind 0.0.0.0 --port 50130 --output received_capture.h264 --idle-timeout-sec 3\n\
  h264-recv-view: --bind 0.0.0.0 --port required --frame-timeout-ms 300 --max-inflight-frames 120 --max-decode-queue 30 --strict-decode-order true --debug-dump-limit 10 --json-interval-ms 1000 --title \"AgoraLink Native Viewer\"\n\
  h264-file-viewer: --input received_capture_lan.h264\n\
  color-test-pattern: --output color_test_1080p.h264 --width 1920 --height 1080 --fps 30 --duration-sec 3 --bitrate-mbps 8 --color-matrix bt709"
    );
}

fn run_sender(host: &str, port: u16, fps: u32, bitrate_mbps: f64) -> io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let target = format!("{host}:{port}");
    socket.connect(&target)?;

    eprintln!(
        "agoralink_media sender target={} fps={} bitrate_mbps={}",
        target, fps, bitrate_mbps
    );

    let session_id = make_session_id();
    let frame_interval = Duration::from_nanos(1_000_000_000u64 / u64::from(fps));
    let bytes_per_frame = estimate_frame_payload_bytes(fps, bitrate_mbps);
    let mut frame_id = 0u64;
    let mut packets_sent = 0u64;
    let mut frames_sent = 0u64;
    let mut window_packets = 0u64;
    let mut window_frames = 0u64;
    let mut window_bytes = 0u64;
    let mut next_frame_at = Instant::now();
    let mut stats_at = Instant::now();

    loop {
        let now = Instant::now();
        if now < next_frame_at {
            thread::sleep(next_frame_at.duration_since(now));
        }

        let keyframe = frame_id % u64::from(fps) == 0;
        let timestamp_ms = now_millis();
        let packets = build_frame_packets(
            session_id,
            frame_id,
            timestamp_ms,
            bytes_per_frame,
            keyframe,
        )
        .map_err(io::Error::other)?;

        for packet in packets {
            let sent = socket.send(&packet)?;
            packets_sent += 1;
            window_packets += 1;
            window_bytes += sent as u64;
        }

        frames_sent += 1;
        window_frames += 1;
        frame_id += 1;

        let elapsed = stats_at.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let elapsed_sec = elapsed.as_secs_f64().max(0.001);
            let mbps = (window_bytes as f64 * 8.0) / elapsed_sec / 1_000_000.0;
            let measured_fps = window_frames as f64 / elapsed_sec;
            println!(
                r#"{{"type":"MEDIA_STATS","mode":"sender","packets_sent":{},"frames_sent":{},"mbps":{:.3},"fps":{:.2},"target_bitrate_mbps":{:.3}}}"#,
                packets_sent, frames_sent, mbps, measured_fps, bitrate_mbps
            );
            io::stdout().flush().ok();
            window_packets = 0;
            window_frames = 0;
            window_bytes = 0;
            stats_at = Instant::now();
        }

        let after_send = Instant::now();
        next_frame_at += frame_interval;
        if next_frame_at < after_send {
            next_frame_at = after_send + frame_interval;
        }

        let _ = window_packets;
    }
}

fn run_receiver(bind: &str, port: u16) -> io::Result<()> {
    let socket = UdpSocket::bind(format!("{bind}:{port}"))?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    eprintln!("agoralink_media receiver bind={bind}:{port}");

    let mut buf = [0u8; 2048];
    let mut reassembler = Reassembler::new(FRAME_TTL);
    let mut stats_at = Instant::now();
    let mut window_bytes = 0u64;
    let mut window_frames = 0u64;

    loop {
        match socket.recv_from(&mut buf) {
            Ok((len, _addr)) => match MediaPacket::decode(&buf[..len]) {
                Ok(packet) => {
                    let now = Instant::now();
                    window_bytes += len as u64;
                    if reassembler.accept(packet, now) {
                        window_frames += 1;
                    }
                }
                Err(_err) => {
                    reassembler.stats.packets_dropped_or_lost_estimate += 1;
                }
            },
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => return Err(err),
        }

        let now = Instant::now();
        reassembler.expire(now);

        let elapsed = stats_at.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let elapsed_sec = elapsed.as_secs_f64().max(0.001);
            let mbps = (window_bytes as f64 * 8.0) / elapsed_sec / 1_000_000.0;
            let fps = window_frames as f64 / elapsed_sec;
            let stats = &reassembler.stats;
            println!(
                r#"{{"type":"MEDIA_STATS","mode":"receiver","packets_received":{},"packets_dropped_or_lost_estimate":{},"frames_complete":{},"frames_incomplete_expired":{},"mbps":{:.3},"fps":{:.2},"last_frame_id":{},"inflight_frames":{}}}"#,
                stats.packets_received,
                stats.packets_dropped_or_lost_estimate,
                stats.frames_complete,
                stats.frames_incomplete_expired,
                mbps,
                fps,
                stats.last_frame_id,
                reassembler.frames.len()
            );
            io::stdout().flush().ok();
            stats_at = Instant::now();
            window_bytes = 0;
            window_frames = 0;
        }
    }
}

fn build_frame_packets(
    session_id: u64,
    frame_id: u64,
    timestamp_ms: u64,
    payload_size: usize,
    keyframe: bool,
) -> Result<Vec<Vec<u8>>, String> {
    let packet_count = payload_size.div_ceil(MAX_MEDIA_PAYLOAD).max(1);
    if packet_count > u16::MAX as usize {
        return Err(format!("frame too large: {} packets", packet_count));
    }

    let mut packets = Vec::with_capacity(packet_count);
    let mut remaining = payload_size;
    for packet_index in 0..packet_count {
        let chunk_len = remaining.min(MAX_MEDIA_PAYLOAD);
        remaining = remaining.saturating_sub(chunk_len);
        let mut flags = 0u16;
        if keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if packet_index + 1 == packet_count {
            flags |= FLAG_END_OF_FRAME;
        }
        let fill = (frame_id as u8).wrapping_add(packet_index as u8);
        let packet = MediaPacket {
            stream_id: STREAM_VIDEO,
            flags,
            session_id,
            frame_id,
            packet_index: packet_index as u16,
            packet_count: packet_count as u16,
            timestamp_ms,
            payload: vec![fill; chunk_len],
        };
        packets.push(packet.encode()?);
    }

    Ok(packets)
}

pub(crate) fn packetize_media_payload(
    session_id: u64,
    frame_id: u64,
    timestamp_ms: u64,
    payload: &[u8],
    frame_flags: u16,
) -> Result<Vec<Vec<u8>>, String> {
    let packet_count = payload.len().div_ceil(MAX_MEDIA_PAYLOAD).max(1);
    if packet_count > u16::MAX as usize {
        return Err(format!("encoded frame too large: {packet_count} packets"));
    }

    let mut packets = Vec::with_capacity(packet_count);
    for packet_index in 0..packet_count {
        let start = packet_index * MAX_MEDIA_PAYLOAD;
        let end = (start + MAX_MEDIA_PAYLOAD).min(payload.len());
        let mut flags = if packet_index == 0 { frame_flags } else { 0 };
        if packet_index + 1 == packet_count {
            flags |= FLAG_END_OF_FRAME;
        }
        packets.push(
            MediaPacket {
                stream_id: STREAM_VIDEO,
                flags,
                session_id,
                frame_id,
                packet_index: packet_index as u16,
                packet_count: packet_count as u16,
                timestamp_ms,
                payload: payload[start..end].to_vec(),
            }
            .encode()?,
        );
    }
    Ok(packets)
}

fn estimate_frame_payload_bytes(fps: u32, bitrate_mbps: f64) -> usize {
    let bytes_per_second = bitrate_mbps * 1_000_000.0 / 8.0;
    (bytes_per_second / fps as f64).round().max(1.0) as usize
}

pub(crate) fn make_session_id() -> u64 {
    now_millis() ^ ((process::id() as u64) << 32)
}

pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn run_self_test() -> Result<(), String> {
    bgra_to_nv12::run_self_test()?;
    color_spec::run_self_test()?;
    h264_annex_b::run_self_test()?;
    h264_recv_dump::run_self_test()?;
    h264_recv_view::run_self_test()?;
    nv12_to_bgra::run_self_test()?;
    let nv12_size = nv12_synthetic::buffer_size(16, 16)?;
    if nv12_size != 16 * 16 * 3 / 2 {
        return Err("NV12 buffer size mismatch".to_string());
    }
    let mut first_nv12 = vec![0u8; nv12_size];
    let mut second_nv12 = vec![0u8; nv12_size];
    nv12_synthetic::fill_frame(&mut first_nv12, 16, 16, 0)?;
    nv12_synthetic::fill_frame(&mut second_nv12, 16, 16, 1)?;
    if first_nv12 == second_nv12 {
        return Err("synthetic NV12 frames did not change over time".to_string());
    }

    let packet = MediaPacket {
        stream_id: STREAM_VIDEO,
        flags: FLAG_KEYFRAME | FLAG_END_OF_FRAME,
        session_id: 123,
        frame_id: 456,
        packet_index: 0,
        packet_count: 1,
        timestamp_ms: 789,
        payload: vec![7; 32],
    };
    let encoded = packet.encode()?;
    if encoded.len() != HEADER_LEN + 32 {
        return Err("encoded length mismatch".to_string());
    }
    let decoded = MediaPacket::decode(&encoded)?;
    if decoded.session_id != packet.session_id
        || decoded.frame_id != packet.frame_id
        || decoded.flags != packet.flags
        || decoded.payload != packet.payload
    {
        return Err("packet roundtrip mismatch".to_string());
    }

    let frame_packets = build_frame_packets(1, 9, 1000, 5000, true)?;
    if frame_packets.len() != 5 {
        return Err(format!("expected 5 packets, got {}", frame_packets.len()));
    }
    if frame_packets
        .iter()
        .any(|item| item.len() > MAX_UDP_PAYLOAD)
    {
        return Err("packet exceeded UDP payload limit".to_string());
    }

    let encoded_payload = vec![0x55; 5000];
    let encoded_packets = packetize_media_payload(
        3,
        11,
        3000,
        &encoded_payload,
        FLAG_KEYFRAME | FLAG_CONFIG | FLAG_H264_ANNEX_B,
    )?;
    if encoded_packets.len() != 5 {
        return Err(format!(
            "expected 5 encoded media packets, got {}",
            encoded_packets.len()
        ));
    }
    let first_encoded = MediaPacket::decode(&encoded_packets[0])?;
    let last_encoded = MediaPacket::decode(encoded_packets.last().unwrap())?;
    if first_encoded.flags & (FLAG_KEYFRAME | FLAG_CONFIG | FLAG_H264_ANNEX_B)
        != FLAG_KEYFRAME | FLAG_CONFIG | FLAG_H264_ANNEX_B
        || last_encoded.flags & FLAG_END_OF_FRAME == 0
    {
        return Err("encoded media packet flags mismatch".to_string());
    }

    let mut reassembler = Reassembler::new(FRAME_TTL);
    let now = Instant::now();
    for raw in frame_packets.iter().rev() {
        let packet = MediaPacket::decode(raw)?;
        reassembler.accept(packet, now);
    }
    if reassembler.stats.frames_complete != 1 {
        return Err("reassembler did not complete frame".to_string());
    }

    let mut expiring = Reassembler::new(Duration::from_millis(1));
    let incomplete = build_frame_packets(2, 10, 2000, 3000, false)?;
    let first = MediaPacket::decode(&incomplete[0])?;
    expiring.accept(first, Instant::now() - Duration::from_millis(10));
    expiring.expire(Instant::now());
    if expiring.stats.frames_incomplete_expired != 1 {
        return Err("expired incomplete frame was not counted".to_string());
    }
    if expiring.stats.packets_dropped_or_lost_estimate == 0 {
        return Err("lost packet estimate was not updated".to_string());
    }

    Ok(())
}

fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
