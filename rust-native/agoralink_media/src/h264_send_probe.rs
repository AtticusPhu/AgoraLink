#[derive(Debug)]
pub struct H264SendConfig {
    pub host: String,
    pub port: u16,
    pub duration_sec: Option<u64>,
    pub target_fps: u32,
    pub bitrate_mbps: f64,
    pub bitrate_selection: crate::bitrate::BitrateSelection,
    pub out_width: u32,
    pub out_height: u32,
    pub color_spec: crate::color_spec::ColorSpec,
    pub encoder: crate::wmf_h264_encoder::EncoderChoice,
    pub convert_backend: crate::capture_encode_probe::ConvertBackend,
    pub packet_pacing: PacketPacing,
    pub fec_mode: crate::fec::FecMode,
    pub udp_payload_size: usize,
    pub keyframe_interval_sec: f64,
    pub mode: H264SendMode,
    pub verbose: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketPacing {
    Auto,
    Off,
}

impl PacketPacing {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum H264SendMode {
    Probe,
    Screen,
}

#[cfg(windows)]
mod platform {
    use std::io::{self, Write};
    use std::net::UdpSocket;
    use std::sync::mpsc::{self, SyncSender};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use super::{H264SendConfig, H264SendMode, PacketPacing};
    use crate::capture_encode_probe::{
        self, CaptureEncodeObserver, CapturePipelineDone, CapturePipelineStarted,
        CapturePipelineStats,
    };
    use crate::fec::{packetize_frame, FecMode};
    use crate::h264_annex_b::{
        nal_types, parameter_set_presence, summarize_nals, AnnexBParameterSets,
    };
    use crate::wmf_h264_encoder::EncodedSample;
    use crate::{make_session_id, now_millis, FLAG_CONFIG, FLAG_H264_ANNEX_B, FLAG_KEYFRAME};

    const SEND_QUEUE_DEPTH: usize = 4;

    struct PacedFrame {
        packets: Vec<Vec<u8>>,
        data_packet_count: usize,
        fec_packet_count: usize,
        fec_bytes: usize,
    }

    #[derive(Clone, Default)]
    struct SendCounters {
        packets_sent: u64,
        frames_sent: u64,
        bytes_sent: u64,
        data_packets_sent: u64,
        fec_packets_sent: u64,
        fec_bytes_sent: u64,
        max_packets_per_frame: u64,
        pacing_sleep_ms_total: f64,
        pacing_overrun_frames: u64,
        send_errors: u64,
        error: Option<String>,
    }

    struct UdpObserver {
        sender: Option<SyncSender<PacedFrame>>,
        worker: Option<JoinHandle<()>>,
        send_counters: Arc<Mutex<SendCounters>>,
        target: String,
        host: String,
        port: u16,
        session_id: u64,
        mode: H264SendMode,
        duration_sec: Option<u64>,
        next_frame_id: u64,
        h264_bytes: u64,
        keyframes: u64,
        idr_frames: u64,
        sps_pps_repeated: u64,
        last_idr_frame_id: Option<u64>,
        idr_interval_frames_total: u64,
        idr_interval_count: u64,
        config_frames: u64,
        previous_packets: u64,
        previous_frames: u64,
        previous_bytes: u64,
        packetize_send_ms_total: f64,
        packet_pacing: PacketPacing,
        fec_mode: FecMode,
        udp_payload_size: usize,
        data_payload_size: usize,
        keyframe_interval_sec: f64,
        keyframe_control_configured: bool,
        keyframe_interval_target_frames: u32,
        keyframe_force_supported: bool,
        keyframe_force_requests: u64,
        keyframe_force_failures: u64,
        udp_send_buffer_bytes: i32,
        parameter_sets: AnnexBParameterSets,
    }

    impl UdpObserver {
        fn new(config: &H264SendConfig) -> Result<Self, String> {
            let target = format!("{}:{}", config.host, config.port);
            let socket =
                UdpSocket::bind("0.0.0.0:0").map_err(|err| format!("UDP bind failed: {err}"))?;
            socket
                .connect(&target)
                .map_err(|err| format!("UDP connect to {target} failed: {err}"))?;
            let udp_send_buffer_bytes = crate::udp_socket::configure_send_buffer(
                &socket,
                crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES,
            )?;
            let (sender, receiver) = mpsc::sync_channel::<PacedFrame>(SEND_QUEUE_DEPTH);
            let send_counters = Arc::new(Mutex::new(SendCounters::default()));
            let worker_counters = Arc::clone(&send_counters);
            let worker_target = target.clone();
            let packet_pacing = config.packet_pacing;
            let frame_interval =
                Duration::from_nanos(1_000_000_000u64 / u64::from(config.target_fps));
            let worker = thread::Builder::new()
                .name("agoralink-udp-pacer".to_string())
                .spawn(move || {
                    let mut next_frame_start = Instant::now();
                    while let Ok(frame) = receiver.recv() {
                        let frame_started = Instant::now().max(next_frame_start);
                        let packet_count = frame.packets.len().max(1);
                        let mut bytes_sent = 0u64;
                        let mut packets_sent = 0u64;
                        let mut sleep_ms = 0.0;
                        let mut failure = None;
                        if packet_pacing == PacketPacing::Auto {
                            sleep_ms += wait_until(frame_started);
                        }
                        for (index, packet) in frame.packets.iter().enumerate() {
                            if packet_pacing == PacketPacing::Auto && index != 0 {
                                let offset = frame_interval.mul_f64(index as f64 / packet_count as f64);
                                sleep_ms += wait_until(frame_started + offset);
                            }
                            match socket.send(packet) {
                                Ok(sent) if sent == packet.len() => {
                                    packets_sent += 1;
                                    bytes_sent += sent as u64;
                                }
                                Ok(sent) => {
                                    failure = Some(format!(
                                        "UDP short send to {worker_target}: expected {}, sent {sent}",
                                        packet.len()
                                    ));
                                    break;
                                }
                                Err(err) => {
                                    failure = Some(format!(
                                        "UDP send to {worker_target} failed: {err}"
                                    ));
                                    break;
                                }
                            }
                        }
                        let elapsed = frame_started.elapsed();
                        let mut counters = match worker_counters.lock() {
                            Ok(counters) => counters,
                            Err(_) => break,
                        };
                        counters.packets_sent += packets_sent;
                        counters.bytes_sent += bytes_sent;
                        counters.data_packets_sent +=
                            packets_sent.min(frame.data_packet_count as u64);
                        if packets_sent > frame.data_packet_count as u64 {
                            counters.fec_packets_sent += frame.fec_packet_count as u64;
                            counters.fec_bytes_sent += frame.fec_bytes as u64;
                        }
                        counters.max_packets_per_frame = counters
                            .max_packets_per_frame
                            .max(frame.packets.len() as u64);
                        counters.pacing_sleep_ms_total += sleep_ms;
                        if elapsed > frame_interval {
                            counters.pacing_overrun_frames += 1;
                        }
                        if let Some(error) = failure {
                            counters.send_errors += 1;
                            counters.error = Some(error);
                            break;
                        }
                        counters.frames_sent += 1;
                        drop(counters);
                        next_frame_start = if elapsed > frame_interval.saturating_mul(2) {
                            Instant::now() + frame_interval
                        } else {
                            frame_started + frame_interval
                        };
                    }
                })
                .map_err(|err| format!("spawn UDP pacing worker failed: {err}"))?;
            Ok(Self {
                sender: Some(sender),
                worker: Some(worker),
                send_counters,
                target,
                host: config.host.clone(),
                port: config.port,
                session_id: make_session_id(),
                mode: config.mode,
                duration_sec: config.duration_sec,
                next_frame_id: 0,
                h264_bytes: 0,
                keyframes: 0,
                idr_frames: 0,
                sps_pps_repeated: 0,
                last_idr_frame_id: None,
                idr_interval_frames_total: 0,
                idr_interval_count: 0,
                config_frames: 0,
                previous_packets: 0,
                previous_frames: 0,
                previous_bytes: 0,
                packetize_send_ms_total: 0.0,
                packet_pacing: config.packet_pacing,
                fec_mode: config.fec_mode,
                udp_payload_size: config.udp_payload_size,
                data_payload_size: config.udp_payload_size
                    - crate::HEADER_LEN
                    - if config.fec_mode == FecMode::SingleXor {
                        crate::fec::FEC_METADATA_LEN
                    } else {
                        0
                    },
                keyframe_interval_sec: config.keyframe_interval_sec,
                keyframe_control_configured: false,
                keyframe_interval_target_frames: (config.keyframe_interval_sec
                    * f64::from(config.target_fps))
                .round()
                .max(1.0) as u32,
                keyframe_force_supported: false,
                keyframe_force_requests: 0,
                keyframe_force_failures: 0,
                udp_send_buffer_bytes,
                parameter_sets: AnnexBParameterSets::default(),
            })
        }

        fn send_snapshot(&self) -> SendCounters {
            self.send_counters
                .lock()
                .map(|counters| counters.clone())
                .unwrap_or_default()
        }

        fn observed_idr_interval_frames(&self) -> Option<f64> {
            (self.idr_interval_count > 0)
                .then(|| self.idr_interval_frames_total as f64 / self.idr_interval_count as f64)
        }

        fn keyframe_interval_warning(&self) -> Option<&'static str> {
            evaluate_keyframe_interval(
                self.keyframe_control_configured,
                self.keyframe_interval_target_frames,
                self.observed_idr_interval_frames(),
                self.keyframe_force_failures,
            )
        }

        fn keyframe_interval_applied(&self) -> bool {
            self.keyframe_interval_warning().is_none()
        }

        fn keyframe_metrics_fragment(&self) -> String {
            let observed_frames = self.observed_idr_interval_frames();
            let observed_sec = observed_frames.map(|frames| {
                frames / f64::from(self.keyframe_interval_target_frames)
                    * self.keyframe_interval_sec
            });
            format!(
                r#""keyframe_interval_requested_sec":{:.3},"keyframe_interval_target_frames":{},"keyframe_interval_observed_sec":{},"keyframe_interval_observed_frames_avg":{},"keyframe_interval_warning":{},"keyframe_force_supported":{},"keyframe_force_requests":{},"keyframe_force_failures":{},"idr_frames_sent":{},"sps_pps_repeated":{}"#,
                self.keyframe_interval_sec,
                self.keyframe_interval_target_frames,
                optional_f64_json(observed_sec),
                optional_f64_json(observed_frames),
                optional_json_string(self.keyframe_interval_warning()),
                self.keyframe_force_supported,
                self.keyframe_force_requests,
                self.keyframe_force_failures,
                self.idr_frames,
                self.sps_pps_repeated,
            )
        }

        fn finish_worker(&mut self) -> Result<(), String> {
            self.sender.take();
            if let Some(worker) = self.worker.take() {
                worker
                    .join()
                    .map_err(|_| "UDP pacing worker panicked".to_string())?;
            }
            if let Some(error) = self.send_snapshot().error {
                Err(error)
            } else {
                Ok(())
            }
        }

        fn print_started(&self, started: &CapturePipelineStarted) {
            if self.mode != H264SendMode::Screen {
                return;
            }
            println!(
                r#"{{"type":"NATIVE_SCREEN_STARTED","role":"sender","mode":"screen-send","host":"{}","port":{},"width":{},"height":{},"fps":{},"bitrate_mbps":{:.3},{},{},{},{},{},{}}}"#,
                json_escape(&self.host),
                self.port,
                started.width,
                started.height,
                started.target_fps,
                started.bitrate_mbps,
                started.bitrate_selection.json_fragment(None),
                started.conversion_selection.json_fragment(),
                started.encoder_selection.json_fragment(),
                started.color_spec.json_fragment(),
                started
                    .encoder_input_color_metadata
                    .json_fragment("encoder_input"),
                started
                    .encoder_output_color_metadata
                    .json_fragment("encoder_output")
            );
            io::stdout().flush().ok();
        }

        fn print_done(&self, done: CapturePipelineDone) {
            let sent = self.send_snapshot();
            match self.mode {
                H264SendMode::Probe => {
                    println!(
                        r#"{{"type":"H264_SEND_DONE","mode":"h264_send_probe","encoder":"{}","target":"{}","session_id":{},"packets_sent":{},"frames_sent":{},"bytes_sent":{},"h264_bytes":{},"keyframes":{},"config_frames":{},"capture_raw_frames":{},"capture_latest_updates":{},"capture_callback_skipped":{},"capture_dropped":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"samples_out":{},"duration_sec":{:.3},"wall_time_sec":{:.3},"fps":{:.2},"mbps":{:.3},"target_bitrate_mbps":{:.3},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},{},{},{},{},{},{},{}}}"#,
                        json_escape(&done.encoder_selection.selected_name),
                        json_escape(&self.target),
                        self.session_id,
                        sent.packets_sent,
                        sent.frames_sent,
                        sent.bytes_sent,
                        self.h264_bytes,
                        self.keyframes,
                        self.config_frames,
                        done.capture_raw_frames,
                        done.capture_latest_updates,
                        done.capture_callback_skipped,
                        done.capture_dropped,
                        done.encode_ticks,
                        done.no_new_frame_skipped,
                        done.no_new_frame_reused,
                        done.frames_encoded,
                        done.encode_lag_skips,
                        done.encoder.samples_out,
                        done.media_duration_sec,
                        done.wall_time_sec,
                        sent.frames_sent as f64 / done.wall_time_sec.max(0.001),
                        sent.bytes_sent as f64 * 8.0 / done.wall_time_sec.max(0.001) / 1_000_000.0,
                        done.bitrate_mbps,
                        done.width,
                        done.height,
                        done.copy_ms_avg,
                        done.convert_ms_avg,
                        done.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        done.bitrate_selection.json_fragment(Some(
                            sent.bytes_sent as f64 * 8.0
                                / done.wall_time_sec.max(0.001)
                                / 1_000_000.0,
                        )),
                        conversion_metrics_fragment(
                            &done.conversion_selection,
                            done.gpu_convert_ms_avg,
                            done.cpu_convert_ms_avg,
                        ),
                        done.encoder_selection.json_fragment(),
                        done.color_spec.json_fragment(),
                        done.encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        done.encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(
                            self,
                            &sent,
                            sent.packets_sent as f64 / done.wall_time_sec.max(0.001),
                        )
                    );
                }
                H264SendMode::Screen => {
                    println!(
                        r#"{{"type":"NATIVE_SCREEN_STOPPED","role":"sender","mode":"screen-send","reason":"{}","host":"{}","port":{},"frames_sent":{},"packets_sent":{},"bytes_sent":{},"duration_sec":{:.3},"fps":{:.2},"mbps":{:.3},"target_bitrate_mbps":{:.3},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},{},{},{},{},{},{},{}}}"#,
                        stop_reason(done.stopped_by_console, self.duration_sec),
                        json_escape(&self.host),
                        self.port,
                        sent.frames_sent,
                        sent.packets_sent,
                        sent.bytes_sent,
                        done.wall_time_sec,
                        sent.frames_sent as f64 / done.wall_time_sec.max(0.001),
                        sent.bytes_sent as f64 * 8.0 / done.wall_time_sec.max(0.001) / 1_000_000.0,
                        done.bitrate_mbps,
                        done.width,
                        done.height,
                        done.copy_ms_avg,
                        done.convert_ms_avg,
                        done.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        done.bitrate_selection.json_fragment(Some(
                            sent.bytes_sent as f64 * 8.0
                                / done.wall_time_sec.max(0.001)
                                / 1_000_000.0,
                        )),
                        conversion_metrics_fragment(
                            &done.conversion_selection,
                            done.gpu_convert_ms_avg,
                            done.cpu_convert_ms_avg,
                        ),
                        done.encoder_selection.json_fragment(),
                        done.color_spec.json_fragment(),
                        done.encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        done.encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(
                            self,
                            &sent,
                            sent.packets_sent as f64 / done.wall_time_sec.max(0.001),
                        )
                    );
                }
            }
            io::stdout().flush().ok();
        }
    }

    impl CaptureEncodeObserver for UdpObserver {
        fn on_started(&mut self, started: &CapturePipelineStarted) -> Result<(), String> {
            self.keyframe_control_configured = started.keyframe_interval_applied;
            self.keyframe_interval_target_frames = started
                .keyframe_interval_target_frames
                .unwrap_or(self.keyframe_interval_target_frames);
            self.keyframe_force_supported = started.keyframe_force_supported;
            self.print_started(started);
            Ok(())
        }

        fn on_sample(&mut self, sample: EncodedSample) -> Result<(), String> {
            let packetize_started = Instant::now();
            let mut flags = 0;
            let mut encoded_bytes = sample.bytes;
            if sample.keyframe == Some(true) {
                self.keyframes += 1;
            }
            let summary = summarize_nals(&encoded_bytes);
            let is_idr = summary.has_idr_slice;
            self.parameter_sets.update_from(&encoded_bytes);
            if is_idr {
                if let Some(last_idr) = self.last_idr_frame_id {
                    self.idr_interval_frames_total += self.next_frame_id.saturating_sub(last_idr);
                    self.idr_interval_count += 1;
                }
                self.last_idr_frame_id = Some(self.next_frame_id);
                self.idr_frames += 1;
                if !summary.has_sps || !summary.has_pps {
                    self.sps_pps_repeated += 1;
                }
                encoded_bytes = self
                    .parameter_sets
                    .prepend_missing_to_keyframe(&encoded_bytes)?;
                flags |= FLAG_KEYFRAME;
            }
            let nal_types = nal_types(&encoded_bytes);
            if !nal_types.is_empty() {
                flags |= FLAG_H264_ANNEX_B;
            }
            let (has_sps, has_pps) = parameter_set_presence(&encoded_bytes);
            if has_sps || has_pps {
                flags |= FLAG_CONFIG;
                self.config_frames += 1;
            }

            let packetized = packetize_frame(
                self.session_id,
                self.next_frame_id,
                now_millis(),
                &encoded_bytes,
                flags,
                self.fec_mode,
                self.udp_payload_size,
            )?;
            if let Some(error) = self.send_snapshot().error {
                return Err(error);
            }
            self.sender
                .as_ref()
                .ok_or_else(|| "UDP pacing worker is stopped".to_string())?
                .send(PacedFrame {
                    packets: packetized.packets,
                    data_packet_count: packetized.data_packet_count,
                    fec_packet_count: packetized.fec_packet_count,
                    fec_bytes: packetized.fec_bytes,
                })
                .map_err(|_| "UDP pacing worker stopped before frame enqueue".to_string())?;
            self.h264_bytes += encoded_bytes.len() as u64;
            self.next_frame_id += 1;
            self.packetize_send_ms_total += packetize_started.elapsed().as_secs_f64() * 1000.0;
            Ok(())
        }

        fn on_stats(&mut self, stats: &CapturePipelineStats) -> Result<(), String> {
            self.keyframe_force_requests = stats.keyframe_force_requests;
            self.keyframe_force_failures = stats.keyframe_force_failures;
            let sent = self.send_snapshot();
            if let Some(error) = sent.error.clone() {
                return Err(error);
            }
            let packets_delta = sent.packets_sent.saturating_sub(self.previous_packets);
            let frames_delta = sent.frames_sent.saturating_sub(self.previous_frames);
            let bytes_delta = sent.bytes_sent.saturating_sub(self.previous_bytes);
            match self.mode {
                H264SendMode::Probe => {
                    println!(
                        r#"{{"type":"H264_SEND_STATS","mode":"h264_send_probe","encoder":"{}","target":"{}","session_id":{},"packets_sent":{},"frames_sent":{},"bytes_sent":{},"packets_per_sec":{},"fps":{},"mbps":{:.3},"target_bitrate_mbps":{:.3},"capture_raw_frames":{},"capture_latest_updates":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"raw_fps":{:.2},"accepted_fps":{:.2},"encode_fps":{:.2},"target_fps":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},"capture_dropped":{},{},{},{},{},{},{},{}}}"#,
                        json_escape(&stats.encoder_selection.selected_name),
                        json_escape(&self.target),
                        self.session_id,
                        sent.packets_sent,
                        sent.frames_sent,
                        sent.bytes_sent,
                        packets_delta,
                        frames_delta,
                        bytes_delta as f64 * 8.0 / 1_000_000.0,
                        stats.target_bitrate_mbps,
                        stats.capture_raw_frames,
                        stats.capture_latest_updates,
                        stats.encode_ticks,
                        stats.no_new_frame_skipped,
                        stats.no_new_frame_reused,
                        stats.frames_encoded,
                        stats.encode_lag_skips,
                        stats.raw_fps,
                        stats.accepted_fps,
                        stats.encode_fps,
                        stats.target_fps,
                        stats.width,
                        stats.height,
                        stats.copy_ms_avg,
                        stats.convert_ms_avg,
                        stats.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        stats.capture_dropped,
                        stats
                            .bitrate_selection
                            .json_fragment(Some(bytes_delta as f64 * 8.0 / 1_000_000.0)),
                        conversion_metrics_fragment(
                            &stats.conversion_selection,
                            stats.gpu_convert_ms_avg,
                            stats.cpu_convert_ms_avg,
                        ),
                        stats.encoder_selection.json_fragment(),
                        stats.color_spec.json_fragment(),
                        stats
                            .encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        stats
                            .encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(self, &sent, packets_delta as f64)
                    );
                }
                H264SendMode::Screen => {
                    println!(
                        r#"{{"type":"NATIVE_SCREEN_STATS","role":"sender","mode":"screen-send","host":"{}","port":{},"session_id":{},"frames_sent":{},"packets_sent":{},"bytes_sent":{},"packets_per_sec":{},"fps":{},"mbps":{:.3},"target_bitrate_mbps":{:.3},"capture_raw_frames":{},"capture_latest_updates":{},"encode_ticks":{},"no_new_frame_skipped":{},"no_new_frame_reused":{},"frames_encoded":{},"encode_lag_skips":{},"target_fps":{},"width":{},"height":{},"copy_ms_avg":{:.3},"convert_ms_avg":{:.3},"encode_ms_avg":{:.3},"packetize_send_ms_avg":{:.3},"capture_dropped":{},{},{},{},{},{},{},{}}}"#,
                        json_escape(&self.host),
                        self.port,
                        self.session_id,
                        sent.frames_sent,
                        sent.packets_sent,
                        sent.bytes_sent,
                        packets_delta,
                        frames_delta,
                        bytes_delta as f64 * 8.0 / 1_000_000.0,
                        stats.target_bitrate_mbps,
                        stats.capture_raw_frames,
                        stats.capture_latest_updates,
                        stats.encode_ticks,
                        stats.no_new_frame_skipped,
                        stats.no_new_frame_reused,
                        stats.frames_encoded,
                        stats.encode_lag_skips,
                        stats.target_fps,
                        stats.width,
                        stats.height,
                        stats.copy_ms_avg,
                        stats.convert_ms_avg,
                        stats.encode_ms_avg,
                        average_ms(self.packetize_send_ms_total, sent.frames_sent),
                        stats.capture_dropped,
                        stats
                            .bitrate_selection
                            .json_fragment(Some(bytes_delta as f64 * 8.0 / 1_000_000.0)),
                        conversion_metrics_fragment(
                            &stats.conversion_selection,
                            stats.gpu_convert_ms_avg,
                            stats.cpu_convert_ms_avg,
                        ),
                        stats.encoder_selection.json_fragment(),
                        stats.color_spec.json_fragment(),
                        stats
                            .encoder_input_color_metadata
                            .json_fragment("encoder_input"),
                        stats
                            .encoder_output_color_metadata
                            .json_fragment("encoder_output"),
                        transport_metrics_fragment(self, &sent, packets_delta as f64)
                    );
                }
            }
            io::stdout().flush().ok();
            self.previous_packets = sent.packets_sent;
            self.previous_frames = sent.frames_sent;
            self.previous_bytes = sent.bytes_sent;
            Ok(())
        }
    }

    pub fn run(config: H264SendConfig) -> Result<(), String> {
        validate_config(&config)?;
        let mut observer = UdpObserver::new(&config)?;
        if config.verbose {
            eprintln!(
                "h264-send-probe target={} duration_sec={} target_fps={} bitrate_mbps={} output={}x{} encoder={} convert_backend={} packet_pacing={} keyframe_interval_sec={:.3} udp_send_buffer_bytes={} color_matrix={} range={} packet_payload_max=1200",
                observer.target,
                optional_duration_text(config.duration_sec),
                config.target_fps,
                config.bitrate_mbps,
                config.out_width,
                config.out_height,
                config.encoder.name(),
                config.convert_backend.name(),
                config.packet_pacing.name(),
                config.keyframe_interval_sec,
                observer.udp_send_buffer_bytes,
                config.color_spec.yuv_matrix(),
                config.color_spec.color_range()
            );
        }
        let pipeline_config = capture_encode_probe::CaptureEncodeConfig {
            duration_sec: config.duration_sec,
            target_fps: config.target_fps,
            bitrate_mbps: config.bitrate_mbps,
            bitrate_selection: config.bitrate_selection.clone(),
            out_width: config.out_width,
            out_height: config.out_height,
            output: String::new(),
            color_spec: config.color_spec,
            encoder: config.encoder,
            convert_backend: config.convert_backend,
            keyframe_interval_sec: Some(config.keyframe_interval_sec),
            verbose: config.verbose,
        };
        let pipeline_result =
            capture_encode_probe::run_with_observer(&pipeline_config, &mut observer);
        let worker_result = observer.finish_worker();
        let done = pipeline_result?;
        worker_result?;
        observer.keyframe_force_requests = done.keyframe_force_requests;
        observer.keyframe_force_failures = done.keyframe_force_failures;
        observer.print_done(done);
        Ok(())
    }

    fn validate_config(config: &H264SendConfig) -> Result<(), String> {
        if config.host.trim().is_empty() {
            return Err("host must not be empty".to_string());
        }
        if config.port == 0 {
            return Err("port must be greater than zero".to_string());
        }
        crate::validate_udp_payload_size(config.udp_payload_size)?;
        Ok(())
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }

    fn average_ms(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn wait_until(target: Instant) -> f64 {
        const PACING_GRANULARITY: Duration = Duration::from_micros(500);
        let started = Instant::now();
        loop {
            let now = Instant::now();
            if now >= target {
                break;
            }
            let remaining = target.duration_since(now);
            if remaining <= PACING_GRANULARITY {
                break;
            }
            thread::sleep(remaining.saturating_sub(PACING_GRANULARITY));
        }
        started.elapsed().as_secs_f64() * 1000.0
    }

    fn transport_metrics_fragment(
        observer: &UdpObserver,
        sent: &SendCounters,
        packets_per_second: f64,
    ) -> String {
        format!(
            r#""udp_send_buffer_bytes":{},"udp_payload_size":{},"packets_per_second":{:.2},"avg_packets_per_frame":{:.3},"max_packets_per_frame":{},"packet_pacing":"{}","packet_pacing_sleep_ms_avg":{:.3},"packet_pacing_overrun_frames":{},"send_errors":{},"fec_mode":"{}","fec_packets_sent":{},"fec_overhead_packets":{},"fec_overhead_bytes":{},"fec_overhead_ratio":{:.6},"data_payload_size":{},"keyframe_interval_sec":{:.3},"keyframe_interval_applied":{},"keyframes_sent":{},{}"#,
            observer.udp_send_buffer_bytes,
            observer.udp_payload_size,
            packets_per_second,
            if sent.frames_sent == 0 {
                0.0
            } else {
                sent.packets_sent as f64 / sent.frames_sent as f64
            },
            sent.max_packets_per_frame,
            observer.packet_pacing.name(),
            average_ms(sent.pacing_sleep_ms_total, sent.frames_sent),
            sent.pacing_overrun_frames,
            sent.send_errors,
            observer.fec_mode.name(),
            sent.fec_packets_sent,
            sent.fec_packets_sent,
            sent.fec_bytes_sent,
            if sent.data_packets_sent == 0 {
                0.0
            } else {
                sent.fec_packets_sent as f64 / sent.data_packets_sent as f64
            },
            observer.data_payload_size,
            observer.keyframe_interval_sec,
            observer.keyframe_interval_applied(),
            observer.keyframes,
            observer.keyframe_metrics_fragment(),
        )
    }

    fn conversion_metrics_fragment(
        selection: &capture_encode_probe::ConversionSelection,
        gpu_convert_ms_avg: f64,
        cpu_convert_ms_avg: f64,
    ) -> String {
        format!(
            r#"{},"gpu_convert_ms_avg":{:.3},"cpu_convert_ms_avg":{:.3}"#,
            selection.json_fragment(),
            gpu_convert_ms_avg,
            cpu_convert_ms_avg,
        )
    }

    fn optional_duration_text(duration_sec: Option<u64>) -> String {
        duration_sec.map_or_else(|| "unlimited".to_string(), |seconds| seconds.to_string())
    }

    fn evaluate_keyframe_interval(
        configured: bool,
        target_frames: u32,
        observed_frames: Option<f64>,
        force_failures: u64,
    ) -> Option<&'static str> {
        if !configured {
            return Some("encoder-keyframe-control-unavailable");
        }
        if force_failures > 0 {
            return Some("forced-idr-request-failed");
        }
        let Some(observed) = observed_frames else {
            return Some("insufficient-idr-observations");
        };
        let target = f64::from(target_frames);
        let tolerance = (target * 0.25).max(2.0);
        ((observed - target).abs() > tolerance).then_some("observed-idr-interval-deviates")
    }

    fn optional_f64_json(value: Option<f64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| format!("{value:.3}"))
    }

    fn optional_json_string(value: Option<&str>) -> String {
        value.map_or_else(
            || "null".to_string(),
            |value| format!(r#""{}""#, json_escape(value)),
        )
    }

    fn stop_reason(stopped_by_console: bool, duration_sec: Option<u64>) -> &'static str {
        if stopped_by_console {
            "ctrl_c"
        } else if duration_sec.is_some() {
            "duration"
        } else {
            "stopped"
        }
    }

    pub fn run_self_test() -> Result<(), String> {
        if evaluate_keyframe_interval(true, 30, Some(30.0), 0).is_some() {
            return Err("matching IDR interval was reported as a warning".to_string());
        }
        if evaluate_keyframe_interval(true, 30, Some(120.0), 0)
            != Some("observed-idr-interval-deviates")
        {
            return Err("IDR interval deviation was not detected".to_string());
        }
        if evaluate_keyframe_interval(true, 30, None, 0) != Some("insufficient-idr-observations") {
            return Err("missing IDR observations were not detected".to_string());
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use platform::{run, run_self_test};

#[cfg(not(windows))]
pub fn run(_config: H264SendConfig) -> Result<(), String> {
    Err("h264-send-probe is only supported on Windows".to_string())
}

#[cfg(not(windows))]
pub fn run_self_test() -> Result<(), String> {
    Ok(())
}
