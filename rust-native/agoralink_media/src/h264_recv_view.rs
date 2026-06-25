#[derive(Debug)]
pub struct H264RecvViewConfig {
    pub bind: String,
    pub port: u16,
    pub frame_timeout_ms: u64,
    pub max_inflight_frames: usize,
    pub max_decode_queue: usize,
    pub strict_decode_order: bool,
    pub json_interval_ms: u64,
    pub title: String,
}

#[cfg(windows)]
mod platform {
    use std::collections::VecDeque;
    use std::io::{self, Write};
    use std::net::UdpSocket;
    use std::os::windows::io::AsRawSocket;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::Win32::Networking::WinSock::{
        setsockopt, SOCKET, SOCKET_ERROR, SOL_SOCKET, SO_RCVBUF,
    };
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::H264RecvViewConfig;
    use crate::h264_annex_b::{dimensions_from_sps, VideoDimensions};
    use crate::h264_reassembly::{
        EncodedFrame, H264Reassembler, ReassemblyConfig, ReassemblyStats,
    };
    use crate::nv12_to_bgra;
    use crate::win32_gdi_viewer::GdiViewerWindow;
    use crate::wmf_h264_decoder::{WmfH264Decoder, DECODER_NAME};

    const MAX_DATAGRAM_SIZE: usize = 2048;
    const MAX_DATAGRAMS_PER_TICK: usize = 1024;
    const DECODER_FPS: u32 = 30;
    const UDP_RECEIVE_BUFFER_BYTES: i32 = 8 * 1024 * 1024;

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

    enum DecodeQueueItem {
        Reset,
        Frame(EncodedFrame),
    }

    #[derive(Default)]
    struct DecodeQueue {
        items: VecDeque<DecodeQueueItem>,
        waiting_for_keyframe: bool,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        decode_queue_peak: usize,
        last_keyframe_id: Option<u64>,
    }

    impl DecodeQueue {
        fn frame_len(&self) -> usize {
            self.items
                .iter()
                .filter(|item| matches!(item, DecodeQueueItem::Frame(_)))
                .count()
        }

        fn begin_keyframe_recovery(&mut self) {
            let dropped = self.frame_len() as u64;
            self.frames_predecode_dropped += dropped;
            self.items.clear();
            self.items.push_back(DecodeQueueItem::Reset);
            self.waiting_for_keyframe = true;
            self.keyframe_recovery_count += 1;
        }

        fn enqueue_frame(&mut self, frame: EncodedFrame, max_decode_queue: usize) {
            if self.waiting_for_keyframe {
                if !frame.is_keyframe() {
                    self.frames_predecode_dropped += 1;
                    self.frames_waiting_keyframe_dropped += 1;
                    return;
                }
                self.last_keyframe_id = Some(frame.frame_id);
                self.waiting_for_keyframe = false;
                self.items.push_back(DecodeQueueItem::Frame(frame));
                self.decode_queue_peak = self.decode_queue_peak.max(self.frame_len());
                return;
            }

            if self.frame_len() >= max_decode_queue {
                self.begin_keyframe_recovery();
                if !frame.is_keyframe() {
                    self.frames_predecode_dropped += 1;
                    self.frames_waiting_keyframe_dropped += 1;
                    return;
                }
            }
            if frame.is_keyframe() {
                self.last_keyframe_id = Some(frame.frame_id);
                self.waiting_for_keyframe = false;
            }
            self.items.push_back(DecodeQueueItem::Frame(frame));
            self.decode_queue_peak = self.decode_queue_peak.max(self.frame_len());
        }
    }

    #[derive(Clone, Copy, Default)]
    struct NetworkSnapshot {
        reassembly: ReassemblyStats,
        session_id: Option<u64>,
        inflight_frames: usize,
        completed_waiting: usize,
        decode_queue: usize,
        decode_queue_peak: usize,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        last_keyframe_id: Option<u64>,
        waiting_keyframe: bool,
    }

    #[derive(Default)]
    struct SharedNetworkState {
        snapshot: NetworkSnapshot,
        error: Option<String>,
    }

    #[derive(Default)]
    struct ViewerStats {
        frames_decoded: u64,
        frames_rendered: u64,
        frames_render_skipped: u64,
        frames_decoded_not_rendered: u64,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        decoder_errors: u64,
        decoder_resets: u64,
        render_queue_peak: usize,
        decode_ms_total: f64,
        render_ms_total: f64,
    }

    struct PendingRender {
        frame_id: u64,
        nv12: Vec<u8>,
    }

    struct DecodeState {
        decoder: Option<WmfH264Decoder>,
        dimensions: Option<VideoDimensions>,
        waiting_for_keyframe: bool,
        input_index: u64,
        last_keyframe_id: Option<u64>,
        pending_render: Option<PendingRender>,
        bgra: Vec<u8>,
    }

    impl DecodeState {
        fn new() -> Self {
            Self {
                decoder: None,
                dimensions: None,
                waiting_for_keyframe: true,
                input_index: 0,
                last_keyframe_id: None,
                pending_render: None,
                bgra: Vec::new(),
            }
        }

        fn mark_discontinuity(&mut self, stats: &mut ViewerStats, count_recovery: bool) {
            if self.decoder.take().is_some() {
                stats.decoder_resets += 1;
            }
            if count_recovery {
                stats.keyframe_recovery_count += 1;
            }
            self.waiting_for_keyframe = true;
            self.input_index = 0;
            if self.pending_render.take().is_some() {
                stats.frames_decoded_not_rendered += 1;
                stats.frames_render_skipped += 1;
            }
        }
    }

    pub fn run(config: H264RecvViewConfig) -> Result<(), String> {
        validate_config(&config)?;
        let _console_ctrl = ConsoleCtrlGuard::install()?;
        let socket = UdpSocket::bind(format!("{}:{}", config.bind, config.port))
            .map_err(|err| format!("UDP bind failed: {err}"))?;
        set_receive_buffer_size(&socket, UDP_RECEIVE_BUFFER_BYTES)?;
        socket
            .set_nonblocking(true)
            .map_err(|err| format!("set UDP nonblocking failed: {err}"))?;
        let mut window = GdiViewerWindow::create(&config.title)?;
        let queue = Arc::new(Mutex::new(DecodeQueue {
            items: VecDeque::with_capacity(config.max_decode_queue + 1),
            ..DecodeQueue::default()
        }));
        let network_state = Arc::new(Mutex::new(SharedNetworkState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let network_thread = spawn_network_thread(
            socket,
            Arc::clone(&queue),
            Arc::clone(&network_state),
            Arc::clone(&stop),
            config.frame_timeout_ms,
            config.max_inflight_frames,
            config.max_decode_queue,
        )?;

        eprintln!(
            "h264-recv-view bind={}:{} frame_timeout_ms={} max_inflight_frames={} max_decode_queue={} strict_decode_order={} udp_receive_buffer={} decoder=\"{}\" output=NV12 render=GDI title=\"{}\"",
            config.bind,
            config.port,
            config.frame_timeout_ms,
            config.max_inflight_frames,
            config.max_decode_queue,
            config.strict_decode_order,
            UDP_RECEIVE_BUFFER_BYTES,
            DECODER_NAME,
            config.title
        );

        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_network = ReassemblyStats::default();
        let mut previous_decoded = 0u64;
        let mut previous_rendered = 0u64;
        let mut decode_state = DecodeState::new();
        let mut stats = ViewerStats::default();
        let mut closed_by_user = false;

        loop {
            if STOP_REQUESTED.load(Ordering::SeqCst) || stop.load(Ordering::SeqCst) {
                break;
            }
            if !window.pump_messages() {
                closed_by_user = true;
                break;
            }

            let items = queue.lock().map_or_else(
                |_| Vec::new(),
                |mut queue| queue.items.drain(..).collect::<Vec<_>>(),
            );
            if items.is_empty() {
                render_latest_decoded(&mut decode_state, &mut window, &mut stats);
                thread::sleep(Duration::from_millis(1));
            } else {
                let mut remaining_frames = items
                    .iter()
                    .filter(|item| matches!(item, DecodeQueueItem::Frame(_)))
                    .count();
                let skip_stale_renders = remaining_frames > (config.max_decode_queue / 2).max(2);
                for item in items {
                    match item {
                        DecodeQueueItem::Reset => {
                            decode_state.mark_discontinuity(&mut stats, false);
                        }
                        DecodeQueueItem::Frame(frame) => {
                            remaining_frames = remaining_frames.saturating_sub(1);
                            process_encoded_frame(
                                frame,
                                !skip_stale_renders || remaining_frames == 0,
                                &mut decode_state,
                                &mut window,
                                &mut stats,
                            );
                        }
                    }
                }
            }

            let now = Instant::now();
            if now.duration_since(report_at) >= Duration::from_millis(config.json_interval_ms) {
                let snapshot = network_snapshot(&network_state);
                print_stats(
                    snapshot,
                    &stats,
                    decode_state.dimensions,
                    decode_state.waiting_for_keyframe,
                    decode_state.last_keyframe_id,
                    config.strict_decode_order,
                    previous_network,
                    previous_decoded,
                    previous_rendered,
                    now.duration_since(report_at),
                );
                previous_network = snapshot.reassembly;
                previous_decoded = stats.frames_decoded;
                previous_rendered = stats.frames_rendered;
                report_at = now;
            }
        }

        stop.store(true, Ordering::SeqCst);
        network_thread
            .join()
            .map_err(|_| "H.264 receive thread panicked".to_string())?;
        let final_state = network_state
            .lock()
            .map_err(|_| "network state lock was poisoned".to_string())?;
        let snapshot = final_state.snapshot;
        let network_error = final_state.error.clone();
        drop(final_state);
        let dimensions = decode_state.dimensions.unwrap_or(VideoDimensions {
            width: 0,
            height: 0,
        });
        println!(
            r#"{{"type":"H264_RECV_VIEW_DONE","mode":"h264_recv_view","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_render_skipped":{},"frames_decoded_not_rendered":{},"frames_incomplete_expired":{},"frames_predecode_dropped":{},"frames_queue_dropped":{},"frames_waiting_keyframe_dropped":{},"keyframe_recovery_count":{},"decoder_errors":{},"decoder_resets":{},"decode_queue_peak":{},"render_queue_peak":{},"last_keyframe_id":{},"waiting_keyframe":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},"last_frame_id":{},"closed_by_user":{},"stopped_by_console":{},"duration_sec":{:.3}}}"#,
            config.strict_decode_order,
            optional_u64_json(snapshot.session_id),
            snapshot.reassembly.packets_received,
            snapshot.reassembly.packets_invalid,
            snapshot.reassembly.packets_lost_estimate,
            snapshot.reassembly.frames_complete,
            stats.frames_decoded,
            stats.frames_rendered,
            stats.frames_render_skipped,
            stats.frames_decoded_not_rendered,
            snapshot.reassembly.frames_incomplete_expired,
            snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
            snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
            snapshot.frames_waiting_keyframe_dropped + stats.frames_waiting_keyframe_dropped,
            snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
            stats.decoder_errors,
            stats.decoder_resets,
            snapshot.decode_queue_peak,
            stats.render_queue_peak,
            optional_u64_json(decode_state.last_keyframe_id.or(snapshot.last_keyframe_id)),
            decode_state.waiting_for_keyframe || snapshot.waiting_keyframe,
            average(stats.decode_ms_total, stats.frames_decoded),
            average(stats.render_ms_total, stats.frames_rendered),
            dimensions.width,
            dimensions.height,
            optional_u64_json(snapshot.reassembly.last_frame_id),
            closed_by_user,
            STOP_REQUESTED.load(Ordering::SeqCst),
            started_at.elapsed().as_secs_f64()
        );
        io::stdout().flush().ok();
        eprintln!(
            "h264-recv-view stopped reason={}",
            if closed_by_user {
                "window-closed"
            } else if network_error.is_some() {
                "network-error"
            } else {
                "console-control"
            }
        );
        if let Some(error) = network_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn spawn_network_thread(
        socket: UdpSocket,
        queue: Arc<Mutex<DecodeQueue>>,
        state: Arc<Mutex<SharedNetworkState>>,
        stop: Arc<AtomicBool>,
        frame_timeout_ms: u64,
        max_inflight_frames: usize,
        max_decode_queue: usize,
    ) -> Result<thread::JoinHandle<()>, String> {
        thread::Builder::new()
            .name("agoralink-h264-recv".to_string())
            .spawn(move || {
                if let Err(err) = receive_loop(
                    socket,
                    &queue,
                    &state,
                    &stop,
                    frame_timeout_ms,
                    max_inflight_frames,
                    max_decode_queue,
                ) {
                    if let Ok(mut shared) = state.lock() {
                        shared.error = Some(err);
                    }
                    stop.store(true, Ordering::SeqCst);
                }
            })
            .map_err(|err| format!("spawn H.264 receive thread failed: {err}"))
    }

    fn receive_loop(
        socket: UdpSocket,
        queue: &Arc<Mutex<DecodeQueue>>,
        state: &Arc<Mutex<SharedNetworkState>>,
        stop: &Arc<AtomicBool>,
        frame_timeout_ms: u64,
        max_inflight_frames: usize,
        max_decode_queue: usize,
    ) -> Result<(), String> {
        let mut reassembler = H264Reassembler::new(ReassemblyConfig {
            frame_timeout: Duration::from_millis(frame_timeout_ms),
            max_inflight_frames,
        })?;
        let mut datagram = [0u8; MAX_DATAGRAM_SIZE];
        let mut previous_expired = 0u64;

        while !stop.load(Ordering::SeqCst) {
            let mut did_work = false;
            for _ in 0..MAX_DATAGRAMS_PER_TICK {
                match socket.recv_from(&mut datagram) {
                    Ok((length, _peer)) => {
                        did_work = true;
                        match reassembler.accept_datagram(&datagram[..length], Instant::now()) {
                            Ok(frames) => {
                                let current_expired = reassembler.stats().frames_incomplete_expired;
                                if current_expired > previous_expired {
                                    begin_queue_recovery(queue)?;
                                    previous_expired = current_expired;
                                }
                                enqueue_network_frames(frames, queue, max_decode_queue)?;
                            }
                            Err(err) => eprintln!("discarding invalid AGM1 packet: {err}"),
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) => return Err(format!("UDP receive failed: {err}")),
                }
            }

            let frames = reassembler.expire(Instant::now());
            let current_expired = reassembler.stats().frames_incomplete_expired;
            if current_expired > previous_expired {
                begin_queue_recovery(queue)?;
                previous_expired = current_expired;
            }
            enqueue_network_frames(frames, queue, max_decode_queue)?;
            update_network_snapshot(state, &reassembler, queue)?;
            if !did_work {
                thread::sleep(Duration::from_millis(1));
            }
        }
        update_network_snapshot(state, &reassembler, queue)
    }

    fn enqueue_network_frames(
        frames: Vec<EncodedFrame>,
        queue: &Arc<Mutex<DecodeQueue>>,
        max_decode_queue: usize,
    ) -> Result<(), String> {
        if frames.is_empty() {
            return Ok(());
        }
        let mut queue = queue
            .lock()
            .map_err(|_| "decode queue lock was poisoned".to_string())?;
        for frame in frames {
            queue.enqueue_frame(frame, max_decode_queue);
        }
        Ok(())
    }

    fn begin_queue_recovery(queue: &Arc<Mutex<DecodeQueue>>) -> Result<(), String> {
        queue
            .lock()
            .map_err(|_| "decode queue lock was poisoned".to_string())?
            .begin_keyframe_recovery();
        Ok(())
    }

    fn update_network_snapshot(
        state: &Arc<Mutex<SharedNetworkState>>,
        reassembler: &H264Reassembler,
        queue: &Arc<Mutex<DecodeQueue>>,
    ) -> Result<(), String> {
        let queue = queue
            .lock()
            .map_err(|_| "decode queue lock was poisoned".to_string())?;
        let mut state = state
            .lock()
            .map_err(|_| "network state lock was poisoned".to_string())?;
        state.snapshot = NetworkSnapshot {
            reassembly: reassembler.stats(),
            session_id: reassembler.session_id(),
            inflight_frames: reassembler.inflight_len(),
            completed_waiting: reassembler.completed_waiting_len(),
            decode_queue: queue.frame_len(),
            decode_queue_peak: queue.decode_queue_peak,
            frames_predecode_dropped: queue.frames_predecode_dropped,
            frames_waiting_keyframe_dropped: queue.frames_waiting_keyframe_dropped,
            keyframe_recovery_count: queue.keyframe_recovery_count,
            last_keyframe_id: queue.last_keyframe_id,
            waiting_keyframe: queue.waiting_for_keyframe,
        };
        Ok(())
    }

    fn process_encoded_frame(
        frame: EncodedFrame,
        render_output: bool,
        decode_state: &mut DecodeState,
        window: &mut GdiViewerWindow,
        stats: &mut ViewerStats,
    ) {
        if frame.is_keyframe() {
            decode_state.last_keyframe_id = Some(frame.frame_id);
        }
        if decode_state.waiting_for_keyframe {
            if !frame.is_keyframe() {
                stats.frames_waiting_keyframe_dropped += 1;
                return;
            }
            let dimensions = match dimensions_from_sps(&frame.bytes) {
                Ok(dimensions) => dimensions,
                Err(err) => {
                    stats.frames_waiting_keyframe_dropped += 1;
                    eprintln!(
                        "keyframe {} has no usable SPS; waiting for next keyframe: {err}",
                        frame.frame_id
                    );
                    return;
                }
            };
            match WmfH264Decoder::new(dimensions.width, dimensions.height, DECODER_FPS) {
                Ok(decoder) => {
                    decode_state.decoder = Some(decoder);
                    decode_state.dimensions = Some(dimensions);
                    decode_state.waiting_for_keyframe = false;
                    decode_state.input_index = 0;
                    decode_state.last_keyframe_id = Some(frame.frame_id);
                }
                Err(err) => {
                    stats.decoder_errors += 1;
                    eprintln!(
                        "decoder initialization failed at frame {}: {err}",
                        frame.frame_id
                    );
                    return;
                }
            }
        }

        let Some(decoder) = decode_state.decoder.as_mut() else {
            stats.frames_waiting_keyframe_dropped += 1;
            decode_state.waiting_for_keyframe = true;
            return;
        };
        let decode_started = Instant::now();
        let decoded = match decoder.decode_access_unit(&frame.bytes, decode_state.input_index) {
            Ok(decoded) => decoded,
            Err(err) => {
                stats.decoder_errors += 1;
                eprintln!(
                    "decoder rejected frame {} timestamp_ms={}: {err}; waiting for next keyframe",
                    frame.frame_id, frame.timestamp_ms
                );
                decode_state.mark_discontinuity(stats, true);
                return;
            }
        };
        decode_state.input_index += 1;
        if !decoded.is_empty() {
            stats.decode_ms_total += decode_started.elapsed().as_secs_f64() * 1000.0;
            stats.frames_decoded += decoded.len() as u64;
        }

        for decoded_frame in decoded {
            if decode_state.pending_render.is_some() {
                stats.frames_decoded_not_rendered += 1;
                stats.frames_render_skipped += 1;
            }
            decode_state.pending_render = Some(PendingRender {
                frame_id: frame.frame_id,
                nv12: decoded_frame.nv12,
            });
            stats.render_queue_peak = stats.render_queue_peak.max(1);
            if render_output {
                render_latest_decoded(decode_state, window, stats);
            }
        }
    }

    fn render_latest_decoded(
        decode_state: &mut DecodeState,
        window: &mut GdiViewerWindow,
        stats: &mut ViewerStats,
    ) {
        let Some(pending) = decode_state.pending_render.take() else {
            return;
        };
        let Some(dimensions) = decode_state.dimensions else {
            return;
        };
        let render_started = Instant::now();
        if let Err(err) = nv12_to_bgra::convert(
            &pending.nv12,
            dimensions.width,
            dimensions.height,
            &mut decode_state.bgra,
        )
        .and_then(|_| window.render_bgra(&decode_state.bgra, dimensions.width, dimensions.height))
        {
            eprintln!("render frame {} failed: {err}", pending.frame_id);
            return;
        }
        stats.render_ms_total += render_started.elapsed().as_secs_f64() * 1000.0;
        stats.frames_rendered += 1;
    }

    #[allow(clippy::too_many_arguments)]
    fn print_stats(
        snapshot: NetworkSnapshot,
        stats: &ViewerStats,
        dimensions: Option<VideoDimensions>,
        decoder_waiting_keyframe: bool,
        decoder_last_keyframe_id: Option<u64>,
        strict_decode_order: bool,
        previous_network: ReassemblyStats,
        previous_decoded: u64,
        previous_rendered: u64,
        elapsed: Duration,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let dimensions = dimensions.unwrap_or(VideoDimensions {
            width: 0,
            height: 0,
        });
        println!(
            r#"{{"type":"H264_RECV_VIEW_STATS","mode":"h264_recv_view","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_render_skipped":{},"frames_decoded_not_rendered":{},"frames_incomplete_expired":{},"frames_predecode_dropped":{},"frames_queue_dropped":{},"frames_waiting_keyframe_dropped":{},"keyframe_recovery_count":{},"decoder_errors":{},"decoder_resets":{},"decode_queue":{},"decode_queue_peak":{},"render_queue_peak":{},"fps_decode":{:.2},"fps_render":{:.2},"mbps":{:.3},"last_frame_id":{},"last_keyframe_id":{},"waiting_keyframe":{},"inflight_frames":{},"completed_waiting":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{}}}"#,
            strict_decode_order,
            optional_u64_json(snapshot.session_id),
            snapshot.reassembly.packets_received,
            snapshot.reassembly.packets_invalid,
            snapshot.reassembly.packets_lost_estimate,
            snapshot.reassembly.frames_complete,
            stats.frames_decoded,
            stats.frames_rendered,
            stats.frames_render_skipped,
            stats.frames_decoded_not_rendered,
            snapshot.reassembly.frames_incomplete_expired,
            snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
            snapshot.frames_predecode_dropped + stats.frames_predecode_dropped,
            snapshot.frames_waiting_keyframe_dropped + stats.frames_waiting_keyframe_dropped,
            snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
            stats.decoder_errors,
            stats.decoder_resets,
            snapshot.decode_queue,
            snapshot.decode_queue_peak,
            stats.render_queue_peak,
            stats.frames_decoded.saturating_sub(previous_decoded) as f64 / elapsed_sec,
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec,
            snapshot
                .reassembly
                .bytes_received
                .saturating_sub(previous_network.bytes_received) as f64
                * 8.0
                / elapsed_sec
                / 1_000_000.0,
            optional_u64_json(snapshot.reassembly.last_frame_id),
            optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
            decoder_waiting_keyframe || snapshot.waiting_keyframe,
            snapshot.inflight_frames,
            snapshot.completed_waiting,
            average(stats.decode_ms_total, stats.frames_decoded),
            average(stats.render_ms_total, stats.frames_rendered),
            dimensions.width,
            dimensions.height
        );
        io::stdout().flush().ok();
    }

    fn network_snapshot(state: &Arc<Mutex<SharedNetworkState>>) -> NetworkSnapshot {
        state
            .lock()
            .map_or_else(|_| NetworkSnapshot::default(), |state| state.snapshot)
    }

    fn validate_config(config: &H264RecvViewConfig) -> Result<(), String> {
        if config.port == 0 {
            return Err("port must be greater than zero".to_string());
        }
        if config.frame_timeout_ms == 0 {
            return Err("frame-timeout-ms must be greater than zero".to_string());
        }
        if config.max_inflight_frames == 0 || config.max_decode_queue == 0 {
            return Err(
                "max-inflight-frames and max-decode-queue must be greater than zero".to_string(),
            );
        }
        if config.json_interval_ms == 0 {
            return Err("json-interval-ms must be greater than zero".to_string());
        }
        if config.title.trim().is_empty() {
            return Err("title must not be empty".to_string());
        }
        Ok(())
    }

    fn set_receive_buffer_size(socket: &UdpSocket, bytes: i32) -> Result<(), String> {
        let raw = SOCKET(socket.as_raw_socket() as usize);
        let value = bytes.to_ne_bytes();
        let result = unsafe { setsockopt(raw, SOL_SOCKET, SO_RCVBUF, Some(&value)) };
        if result == SOCKET_ERROR {
            Err(format!(
                "set UDP receive buffer to {bytes} bytes failed: {}",
                io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }

    fn average(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn optional_u64_json(value: Option<u64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| value.to_string())
    }

    pub fn run_self_test() -> Result<(), String> {
        fn frame(frame_id: u64, keyframe: bool) -> EncodedFrame {
            EncodedFrame {
                frame_id,
                flags: if keyframe { crate::FLAG_KEYFRAME } else { 0 },
                timestamp_ms: frame_id * 33,
                bytes: vec![frame_id as u8],
            }
        }

        let mut queue = DecodeQueue::default();
        queue.enqueue_frame(frame(0, true), 2);
        queue.enqueue_frame(frame(1, false), 2);
        queue.enqueue_frame(frame(2, false), 2);
        if !queue.waiting_for_keyframe
            || queue.keyframe_recovery_count != 1
            || !matches!(queue.items.front(), Some(DecodeQueueItem::Reset))
        {
            return Err("decode queue overflow did not enter keyframe recovery".to_string());
        }
        queue.enqueue_frame(frame(3, false), 2);
        queue.enqueue_frame(frame(4, true), 2);
        if queue.waiting_for_keyframe
            || queue.frames_predecode_dropped != 4
            || queue.items.len() != 2
            || !matches!(queue.items.front(), Some(DecodeQueueItem::Reset))
            || !matches!(
                queue.items.back(),
                Some(DecodeQueueItem::Frame(frame)) if frame.frame_id == 4
            )
        {
            return Err("decode queue keyframe recovery ordering failed".to_string());
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use platform::{run, run_self_test};

#[cfg(not(windows))]
pub fn run(_config: H264RecvViewConfig) -> Result<(), String> {
    Err("h264-recv-view is only supported on Windows".to_string())
}

#[cfg(not(windows))]
pub fn run_self_test() -> Result<(), String> {
    Ok(())
}
