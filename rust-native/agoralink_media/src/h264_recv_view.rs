#[derive(Debug)]
pub struct H264RecvViewConfig {
    pub bind: String,
    pub port: u16,
    pub duration_sec: Option<u64>,
    pub frame_timeout_ms: u64,
    pub reorder_wait_ms: Option<u64>,
    pub playout_delay_ms: u64,
    pub max_inflight_frames: usize,
    pub max_decode_queue: usize,
    pub strict_decode_order: bool,
    pub drop_damaged_gop: bool,
    pub debug_dump_frames: Option<String>,
    pub debug_dump_limit: usize,
    pub json_interval_ms: u64,
    pub title: String,
    pub render_scale: crate::win32_gdi_viewer::RenderScaleMode,
    pub mode: H264RecvViewMode,
    pub verbose: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum H264RecvViewMode {
    Probe,
    Screen,
}

#[cfg(windows)]
mod platform {
    use std::collections::VecDeque;
    use std::io::{self, Write};
    use std::net::UdpSocket;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    use super::{H264RecvViewConfig, H264RecvViewMode};
    use crate::color_spec::{ColorSpec, MediaColorMetadata};
    use crate::decoded_frame_renderer::OwnedBgraFrame;
    use crate::h264_annex_b::{dimensions_from_sps, VideoDimensions};
    use crate::h264_reassembly::{
        DamagedGopStats, DamagedGopTracker, EncodedFrame, H264Reassembler, ReassemblyConfig,
        ReassemblyStats, ReorderWait,
    };
    use crate::playout_buffer::{PlayoutBuffer, PlayoutStats};
    use crate::win32_gdi_viewer::{GdiRenderStats, GdiViewerWindow};
    use crate::wmf_h264_decoder::{WmfH264Decoder, DECODER_NAME};

    const MAX_DATAGRAM_SIZE: usize = 2048;
    const MAX_DATAGRAMS_PER_TICK: usize = 1024;
    const DECODER_FPS: u32 = 30;

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

    struct DecodeQueue {
        items: VecDeque<DecodeQueueItem>,
        waiting_for_keyframe: bool,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        decode_queue_peak: usize,
        last_keyframe_id: Option<u64>,
        damaged_gop: DamagedGopTracker,
    }

    impl DecodeQueue {
        fn new(capacity: usize, drop_damaged_gop: bool) -> Self {
            Self {
                items: VecDeque::with_capacity(capacity + 1),
                waiting_for_keyframe: false,
                frames_predecode_dropped: 0,
                frames_waiting_keyframe_dropped: 0,
                keyframe_recovery_count: 0,
                decode_queue_peak: 0,
                last_keyframe_id: None,
                damaged_gop: DamagedGopTracker::new(drop_damaged_gop),
            }
        }

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

        fn begin_damaged_gop_recovery(&mut self, now: Instant, damaged_frame_id: Option<u64>) {
            if !self.damaged_gop.mark_damaged(now, damaged_frame_id) {
                return;
            }
            let dropped = self.frame_len() as u64;
            self.frames_predecode_dropped += dropped;
            self.frames_waiting_keyframe_dropped += dropped;
            self.damaged_gop.discard_queued_frames(dropped);
            self.items.clear();
            self.items.push_back(DecodeQueueItem::Reset);
            self.waiting_for_keyframe = true;
            self.keyframe_recovery_count += 1;
        }

        fn enqueue_frame(&mut self, frame: EncodedFrame, max_decode_queue: usize) {
            let was_damaged = self.damaged_gop.waiting_keyframe();
            let Some(frame) = self.damaged_gop.prepare_frame(frame, Instant::now()) else {
                self.frames_predecode_dropped += 1;
                self.frames_waiting_keyframe_dropped += 1;
                return;
            };
            if was_damaged && !self.damaged_gop.waiting_keyframe() {
                self.waiting_for_keyframe = false;
            }
            if self.waiting_for_keyframe {
                if !frame.is_idr() {
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
                if !frame.is_idr() {
                    self.frames_predecode_dropped += 1;
                    self.frames_waiting_keyframe_dropped += 1;
                    return;
                }
            }
            if frame.is_idr() {
                self.last_keyframe_id = Some(frame.frame_id);
                self.waiting_for_keyframe = false;
            }
            self.items.push_back(DecodeQueueItem::Frame(frame));
            self.decode_queue_peak = self.decode_queue_peak.max(self.frame_len());
        }

        fn damaged_gop_stats(&self) -> DamagedGopStats {
            self.damaged_gop.stats()
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
        damaged_gop: DamagedGopStats,
        drop_damaged_gop: bool,
        udp_recv_buffer_bytes: i32,
        playout: PlayoutStats,
        playout_buffer_frames: usize,
        playout_delay_ms: u64,
    }

    #[derive(Default)]
    struct SharedNetworkState {
        snapshot: NetworkSnapshot,
        error: Option<String>,
    }

    #[derive(Default)]
    struct ViewerStats {
        frames_decoded: u64,
        frames_decoder_input: u64,
        frames_rendered: u64,
        frames_render_skipped: u64,
        frames_decoded_not_rendered: u64,
        frames_predecode_dropped: u64,
        frames_waiting_keyframe_dropped: u64,
        keyframe_recovery_count: u64,
        decoder_errors: u64,
        decoder_resets: u64,
        render_queue_peak: usize,
        render_frame_copies: u64,
        render_buffer_reused: u64,
        render_buffer_generation: u64,
        nv12_y_stride: usize,
        nv12_uv_stride: usize,
        nv12_uv_offset: usize,
        nv12_allocated_height: usize,
        nv12_buffer_len: usize,
        expected_tight_len: usize,
        decoder_used_2d_buffer: bool,
        color_spec: ColorSpec,
        decoder_color_metadata: MediaColorMetadata,
        decode_ms_total: f64,
        render_ms_total: f64,
        render_state: GdiRenderStats,
    }

    struct PendingRender {
        frame_id: u64,
        frame: OwnedBgraFrame,
    }

    struct DebugFrameDumper {
        directory: Option<PathBuf>,
        limit: usize,
        dumped: usize,
    }

    impl DebugFrameDumper {
        fn new(directory: Option<String>, limit: usize) -> Self {
            Self {
                directory: directory.map(PathBuf::from),
                limit,
                dumped: 0,
            }
        }

        fn maybe_dump(&mut self, frame: &OwnedBgraFrame) -> Result<(), String> {
            let Some(directory) = self.directory.as_deref() else {
                return Ok(());
            };
            if self.dumped >= self.limit {
                return Ok(());
            }
            self.dumped += 1;
            frame.dump_raw(directory, self.dumped as u64)
        }
    }

    struct DecodeState {
        decoder: Option<WmfH264Decoder>,
        dimensions: Option<VideoDimensions>,
        waiting_for_keyframe: bool,
        input_index: u64,
        last_keyframe_id: Option<u64>,
        pending_render: Option<PendingRender>,
        next_render_generation: u64,
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
                next_render_generation: 0,
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
        let udp_recv_buffer_bytes = crate::udp_socket::configure_receive_buffer(
            &socket,
            crate::udp_socket::DEFAULT_UDP_BUFFER_BYTES,
        )?;
        socket
            .set_nonblocking(true)
            .map_err(|err| format!("set UDP nonblocking failed: {err}"))?;
        let mut window = GdiViewerWindow::create(&config.title, config.render_scale)?;
        let queue = Arc::new(Mutex::new(DecodeQueue::new(
            config.max_decode_queue,
            config.drop_damaged_gop,
        )));
        let network_state = Arc::new(Mutex::new(SharedNetworkState::default()));
        if let Ok(mut state) = network_state.lock() {
            state.snapshot.udp_recv_buffer_bytes = udp_recv_buffer_bytes;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let network_thread = spawn_network_thread(
            socket,
            Arc::clone(&queue),
            Arc::clone(&network_state),
            Arc::clone(&stop),
            config.frame_timeout_ms,
            config.reorder_wait_ms,
            config.playout_delay_ms,
            config.max_inflight_frames,
            config.max_decode_queue,
            config.drop_damaged_gop,
            config.verbose,
        )?;

        if config.verbose {
            eprintln!(
                "h264-recv-view bind={}:{} frame_timeout_ms={} reorder_wait_ms={} playout_delay_ms={} max_inflight_frames={} max_decode_queue={} strict_decode_order={} drop_damaged_gop={} debug_dump_frames={:?} debug_dump_limit={} udp_receive_buffer={} decoder=\"{}\" output=NV12 render=GDI render_scale={} title=\"{}\" duration_sec={}",
                config.bind,
                config.port,
                config.frame_timeout_ms,
                config
                    .reorder_wait_ms
                    .map_or_else(|| "auto".to_string(), |value| value.to_string()),
                config.playout_delay_ms,
                config.max_inflight_frames,
                config.max_decode_queue,
                config.strict_decode_order,
                config.drop_damaged_gop,
                config.debug_dump_frames,
                config.debug_dump_limit,
                udp_recv_buffer_bytes,
                DECODER_NAME,
                config.render_scale.name(),
                config.title,
                optional_duration_text(config.duration_sec)
            );
        }
        print_started(&config, window.render_stats());

        let started_at = Instant::now();
        let mut report_at = started_at;
        let mut previous_network = ReassemblyStats::default();
        let mut previous_decoded = 0u64;
        let mut previous_decoder_input = 0u64;
        let mut previous_rendered = 0u64;
        let mut decode_state = DecodeState::new();
        let mut stats = ViewerStats::default();
        stats.render_state = window.render_stats();
        let mut debug_dumper =
            DebugFrameDumper::new(config.debug_dump_frames.clone(), config.debug_dump_limit);
        let mut closed_by_user = false;

        loop {
            if STOP_REQUESTED.load(Ordering::SeqCst) || stop.load(Ordering::SeqCst) {
                break;
            }
            if duration_elapsed(started_at, config.duration_sec) {
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
                render_latest_decoded(
                    &mut decode_state,
                    &mut window,
                    &mut debug_dumper,
                    &mut stats,
                );
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
                                &mut debug_dumper,
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
                    previous_decoder_input,
                    previous_rendered,
                    now.duration_since(report_at),
                    config.mode,
                );
                previous_network = snapshot.reassembly;
                previous_decoded = stats.frames_decoded;
                previous_decoder_input = stats.frames_decoder_input;
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
        print_done(
            &config,
            snapshot,
            &stats,
            dimensions,
            decode_state.last_keyframe_id,
            decode_state.waiting_for_keyframe,
            closed_by_user,
            network_error.is_some(),
            started_at.elapsed().as_secs_f64(),
        );
        io::stdout().flush().ok();
        if config.verbose {
            eprintln!(
                "h264-recv-view stopped reason={}",
                human_stop_reason(closed_by_user, network_error.is_some(), config.duration_sec)
            );
        }
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
        reorder_wait_ms: Option<u64>,
        playout_delay_ms: u64,
        max_inflight_frames: usize,
        max_decode_queue: usize,
        drop_damaged_gop: bool,
        verbose: bool,
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
                    reorder_wait_ms,
                    playout_delay_ms,
                    max_inflight_frames,
                    max_decode_queue,
                    drop_damaged_gop,
                    verbose,
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
        reorder_wait_ms: Option<u64>,
        playout_delay_ms: u64,
        max_inflight_frames: usize,
        max_decode_queue: usize,
        drop_damaged_gop: bool,
        verbose: bool,
    ) -> Result<(), String> {
        let mut reassembler = H264Reassembler::new(ReassemblyConfig {
            frame_timeout: Duration::from_millis(frame_timeout_ms),
            reorder_wait: reorder_wait_ms.map_or(ReorderWait::Auto, |milliseconds| {
                ReorderWait::Fixed(Duration::from_millis(milliseconds))
            }),
            max_inflight_frames,
        })?;
        let mut datagram = [0u8; MAX_DATAGRAM_SIZE];
        let mut previous_expired = 0u64;
        let mut playout = PlayoutBuffer::new(playout_delay_ms)?;

        while !stop.load(Ordering::SeqCst) {
            let mut did_work = false;
            for _ in 0..MAX_DATAGRAMS_PER_TICK {
                match socket.recv_from(&mut datagram) {
                    Ok((length, _peer)) => {
                        did_work = true;
                        let received_at = Instant::now();
                        match reassembler.accept_datagram(&datagram[..length], received_at) {
                            Ok(frames) => {
                                let reassembly_stats = reassembler.stats();
                                let current_expired = reassembly_stats.frames_incomplete_expired;
                                if current_expired > previous_expired {
                                    playout.clear_for_discontinuity();
                                    begin_queue_recovery(
                                        queue,
                                        drop_damaged_gop,
                                        Instant::now(),
                                        reassembly_stats.last_damaged_frame_id,
                                    )?;
                                    previous_expired = current_expired;
                                }
                                playout.push_frames(frames, received_at);
                            }
                            Err(err) => {
                                if verbose {
                                    eprintln!("discarding invalid AGM1 packet: {err}");
                                }
                            }
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) => return Err(format!("UDP receive failed: {err}")),
                }
            }

            let now = Instant::now();
            let frames = reassembler.expire(now);
            let reassembly_stats = reassembler.stats();
            let current_expired = reassembly_stats.frames_incomplete_expired;
            if current_expired > previous_expired {
                playout.clear_for_discontinuity();
                begin_queue_recovery(
                    queue,
                    drop_damaged_gop,
                    Instant::now(),
                    reassembly_stats.last_damaged_frame_id,
                )?;
                previous_expired = current_expired;
            }
            playout.push_frames(frames, now);
            enqueue_network_frames(playout.pop_due(now), queue, max_decode_queue)?;
            update_network_snapshot(state, &reassembler, queue, &playout)?;
            if !did_work {
                thread::sleep(Duration::from_millis(1));
            }
        }
        update_network_snapshot(state, &reassembler, queue, &playout)
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

    fn begin_queue_recovery(
        queue: &Arc<Mutex<DecodeQueue>>,
        drop_damaged_gop: bool,
        now: Instant,
        damaged_frame_id: Option<u64>,
    ) -> Result<(), String> {
        if drop_damaged_gop {
            queue
                .lock()
                .map_err(|_| "decode queue lock was poisoned".to_string())?
                .begin_damaged_gop_recovery(now, damaged_frame_id);
        }
        Ok(())
    }

    fn update_network_snapshot(
        state: &Arc<Mutex<SharedNetworkState>>,
        reassembler: &H264Reassembler,
        queue: &Arc<Mutex<DecodeQueue>>,
        playout: &PlayoutBuffer,
    ) -> Result<(), String> {
        let queue = queue
            .lock()
            .map_err(|_| "decode queue lock was poisoned".to_string())?;
        let mut state = state
            .lock()
            .map_err(|_| "network state lock was poisoned".to_string())?;
        let udp_recv_buffer_bytes = state.snapshot.udp_recv_buffer_bytes;
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
            waiting_keyframe: queue.waiting_for_keyframe || queue.damaged_gop.waiting_keyframe(),
            damaged_gop: queue.damaged_gop_stats(),
            drop_damaged_gop: queue.damaged_gop.enabled(),
            udp_recv_buffer_bytes,
            playout: playout.stats(),
            playout_buffer_frames: playout.len(),
            playout_delay_ms: playout.delay_ms(),
        };
        Ok(())
    }

    fn process_encoded_frame(
        frame: EncodedFrame,
        render_output: bool,
        decode_state: &mut DecodeState,
        window: &mut GdiViewerWindow,
        debug_dumper: &mut DebugFrameDumper,
        stats: &mut ViewerStats,
    ) {
        if frame.is_idr() {
            decode_state.last_keyframe_id = Some(frame.frame_id);
        }
        if decode_state.waiting_for_keyframe {
            if !frame.is_idr() {
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
        stats.frames_decoder_input += 1;
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

        let Some(dimensions) = decode_state.dimensions else {
            return;
        };
        for decoded_frame in decoded {
            decode_state.next_render_generation += 1;
            let owned_frame = match OwnedBgraFrame::from_decoded(
                &decoded_frame,
                dimensions.width,
                dimensions.height,
                decode_state.next_render_generation,
            ) {
                Ok(frame) => frame,
                Err(err) => {
                    eprintln!("convert decoded frame {} failed: {err}", frame.frame_id);
                    continue;
                }
            };
            stats.render_frame_copies += 1;
            stats.render_buffer_generation = owned_frame.generation;
            stats.nv12_y_stride = owned_frame.nv12_y_stride;
            stats.nv12_uv_stride = owned_frame.nv12_uv_stride;
            stats.nv12_uv_offset = owned_frame.nv12_uv_offset;
            stats.nv12_allocated_height = owned_frame.nv12_allocated_height;
            stats.nv12_buffer_len = owned_frame.nv12_buffer_len;
            stats.expected_tight_len = owned_frame.expected_tight_len;
            stats.decoder_used_2d_buffer = owned_frame.decoder_used_2d_buffer;
            stats.color_spec = owned_frame.color_spec;
            stats.decoder_color_metadata = owned_frame.color_metadata;
            if decode_state.pending_render.is_some() {
                stats.frames_decoded_not_rendered += 1;
                stats.frames_render_skipped += 1;
            }
            decode_state.pending_render = Some(PendingRender {
                frame_id: frame.frame_id,
                frame: owned_frame,
            });
            stats.render_queue_peak = stats.render_queue_peak.max(1);
            if render_output {
                render_latest_decoded(decode_state, window, debug_dumper, stats);
            }
        }
    }

    fn render_latest_decoded(
        decode_state: &mut DecodeState,
        window: &mut GdiViewerWindow,
        debug_dumper: &mut DebugFrameDumper,
        stats: &mut ViewerStats,
    ) {
        let Some(pending) = decode_state.pending_render.take() else {
            return;
        };
        let render_started = Instant::now();
        if let Err(err) = debug_dumper
            .maybe_dump(&pending.frame)
            .and_then(|_| pending.frame.render(window))
        {
            eprintln!("render frame {} failed: {err}", pending.frame_id);
            return;
        }
        stats.render_ms_total += render_started.elapsed().as_secs_f64() * 1000.0;
        stats.frames_rendered += 1;
        stats.render_state = window.render_stats();
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
        previous_decoder_input: u64,
        previous_rendered: u64,
        elapsed: Duration,
        mode: H264RecvViewMode,
    ) {
        let elapsed_sec = elapsed.as_secs_f64().max(0.001);
        let dimensions = dimensions.unwrap_or(VideoDimensions {
            width: 0,
            height: 0,
        });
        let fps_decode = stats.frames_decoded.saturating_sub(previous_decoded) as f64 / elapsed_sec;
        let fps_render =
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec;
        let mbps = snapshot
            .reassembly
            .bytes_received
            .saturating_sub(previous_network.bytes_received) as f64
            * 8.0
            / elapsed_sec
            / 1_000_000.0;
        match mode {
            H264RecvViewMode::Probe => {
                println!(
                    r#"{{"type":"H264_RECV_VIEW_STATS","mode":"h264_recv_view","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_render_skipped":{},"frames_decoded_not_rendered":{},"frames_incomplete_expired":{},"frames_predecode_dropped":{},"frames_queue_dropped":{},"frames_waiting_keyframe_dropped":{},"keyframe_recovery_count":{},"decoder_errors":{},"decoder_resets":{},"decode_queue":{},"decode_queue_peak":{},"render_queue_peak":{},"render_frame_copies":{},"render_buffer_reused":{},"render_buffer_generation":{},"nv12_y_stride":{},"nv12_uv_stride":{},"nv12_uv_offset":{},"nv12_allocated_height":{},"nv12_buffer_len":{},"expected_tight_len":{},"decoder_used_2d_buffer":{},"fps_decode":{:.2},"fps_render":{:.2},"mbps":{:.3},"last_frame_id":{},"last_keyframe_id":{},"waiting_keyframe":{},"inflight_frames":{},"completed_waiting":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},{},{},{}}}"#,
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
                    snapshot.frames_waiting_keyframe_dropped
                        + stats.frames_waiting_keyframe_dropped,
                    snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    snapshot.decode_queue,
                    snapshot.decode_queue_peak,
                    stats.render_queue_peak,
                    stats.render_frame_copies,
                    stats.render_buffer_reused,
                    stats.render_buffer_generation,
                    stats.nv12_y_stride,
                    stats.nv12_uv_stride,
                    stats.nv12_uv_offset,
                    stats.nv12_allocated_height,
                    stats.nv12_buffer_len,
                    stats.expected_tight_len,
                    stats.decoder_used_2d_buffer,
                    fps_decode,
                    fps_render,
                    mbps,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    snapshot.inflight_frames,
                    snapshot.completed_waiting,
                    average(stats.decode_ms_total, stats.frames_decoded),
                    average(stats.render_ms_total, stats.frames_rendered),
                    dimensions.width,
                    dimensions.height,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    receiver_transport_fragment(
                        snapshot,
                        stats,
                        previous_network,
                        elapsed_sec,
                        decoder_last_keyframe_id,
                        previous_decoder_input,
                        previous_rendered,
                    )
                );
            }
            H264RecvViewMode::Screen => {
                println!(
                    r#"{{"type":"NATIVE_SCREEN_STATS","role":"receiver","mode":"screen-recv","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_incomplete_expired":{},"decoder_errors":{},"decoder_resets":{},"decode_queue":{},"decode_queue_peak":{},"fps_decode":{:.2},"fps_render":{:.2},"mbps":{:.3},"last_frame_id":{},"waiting_keyframe":{},"inflight_frames":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},{},{},{}}}"#,
                    strict_decode_order,
                    optional_u64_json(snapshot.session_id),
                    snapshot.reassembly.packets_received,
                    snapshot.reassembly.packets_lost_estimate,
                    snapshot.reassembly.frames_complete,
                    stats.frames_decoded,
                    stats.frames_rendered,
                    snapshot.reassembly.frames_incomplete_expired,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    snapshot.decode_queue,
                    snapshot.decode_queue_peak,
                    fps_decode,
                    fps_render,
                    mbps,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    snapshot.inflight_frames,
                    average(stats.decode_ms_total, stats.frames_decoded),
                    average(stats.render_ms_total, stats.frames_rendered),
                    dimensions.width,
                    dimensions.height,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    receiver_transport_fragment(
                        snapshot,
                        stats,
                        previous_network,
                        elapsed_sec,
                        decoder_last_keyframe_id,
                        previous_decoder_input,
                        previous_rendered,
                    )
                );
            }
        }
        io::stdout().flush().ok();
    }

    fn print_started(config: &H264RecvViewConfig, render: GdiRenderStats) {
        if config.mode != H264RecvViewMode::Screen {
            return;
        }
        println!(
            r#"{{"type":"NATIVE_SCREEN_STARTED","role":"receiver","mode":"screen-recv","bind":"{}","port":{},"strict_decode_order":{},"drop_damaged_gop":{},"playout_delay_ms":{},"render_scale_mode":"{}","dpi_awareness_set":{},"dpi_awareness_mode":"{}","title":"{}"}}"#,
            json_escape(&config.bind),
            config.port,
            config.strict_decode_order,
            config.drop_damaged_gop,
            config.playout_delay_ms,
            config.render_scale.name(),
            render.dpi.set,
            render.dpi.mode,
            json_escape(&config.title)
        );
        io::stdout().flush().ok();
    }

    #[allow(clippy::too_many_arguments)]
    fn print_done(
        config: &H264RecvViewConfig,
        snapshot: NetworkSnapshot,
        stats: &ViewerStats,
        dimensions: VideoDimensions,
        decoder_last_keyframe_id: Option<u64>,
        decoder_waiting_keyframe: bool,
        closed_by_user: bool,
        network_error: bool,
        duration_sec: f64,
    ) {
        match config.mode {
            H264RecvViewMode::Probe => {
                println!(
                    r#"{{"type":"H264_RECV_VIEW_DONE","mode":"h264_recv_view","strict_decode_order":{},"session_id":{},"packets_received":{},"packets_invalid":{},"packets_lost_estimate":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"frames_render_skipped":{},"frames_decoded_not_rendered":{},"frames_incomplete_expired":{},"frames_predecode_dropped":{},"frames_queue_dropped":{},"frames_waiting_keyframe_dropped":{},"keyframe_recovery_count":{},"decoder_errors":{},"decoder_resets":{},"decode_queue_peak":{},"render_queue_peak":{},"render_frame_copies":{},"render_buffer_reused":{},"render_buffer_generation":{},"nv12_y_stride":{},"nv12_uv_stride":{},"nv12_uv_offset":{},"nv12_allocated_height":{},"nv12_buffer_len":{},"expected_tight_len":{},"decoder_used_2d_buffer":{},"last_keyframe_id":{},"waiting_keyframe":{},"decode_ms_avg":{:.3},"render_ms_avg":{:.3},"width":{},"height":{},"last_frame_id":{},"closed_by_user":{},"stopped_by_console":{},"duration_sec":{:.3},{},{},{}}}"#,
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
                    snapshot.frames_waiting_keyframe_dropped
                        + stats.frames_waiting_keyframe_dropped,
                    snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    snapshot.decode_queue_peak,
                    stats.render_queue_peak,
                    stats.render_frame_copies,
                    stats.render_buffer_reused,
                    stats.render_buffer_generation,
                    stats.nv12_y_stride,
                    stats.nv12_uv_stride,
                    stats.nv12_uv_offset,
                    stats.nv12_allocated_height,
                    stats.nv12_buffer_len,
                    stats.expected_tight_len,
                    stats.decoder_used_2d_buffer,
                    optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    average(stats.decode_ms_total, stats.frames_decoded),
                    average(stats.render_ms_total, stats.frames_rendered),
                    dimensions.width,
                    dimensions.height,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    closed_by_user,
                    STOP_REQUESTED.load(Ordering::SeqCst),
                    duration_sec,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    receiver_transport_fragment(
                        snapshot,
                        stats,
                        ReassemblyStats::default(),
                        duration_sec.max(0.001),
                        decoder_last_keyframe_id,
                        0,
                        0,
                    )
                );
            }
            H264RecvViewMode::Screen => {
                println!(
                    r#"{{"type":"NATIVE_SCREEN_STOPPED","role":"receiver","mode":"screen-recv","reason":"{}","bind":"{}","port":{},"frames_complete":{},"frames_decoded":{},"frames_rendered":{},"packets_received":{},"packets_lost_estimate":{},"decoder_errors":{},"decoder_resets":{},"duration_sec":{:.3},"width":{},"height":{},"last_frame_id":{},"waiting_keyframe":{},{},{},{}}}"#,
                    stop_reason(closed_by_user, network_error, config.duration_sec),
                    json_escape(&config.bind),
                    config.port,
                    snapshot.reassembly.frames_complete,
                    stats.frames_decoded,
                    stats.frames_rendered,
                    snapshot.reassembly.packets_received,
                    snapshot.reassembly.packets_lost_estimate,
                    stats.decoder_errors,
                    stats.decoder_resets,
                    duration_sec,
                    dimensions.width,
                    dimensions.height,
                    optional_u64_json(snapshot.reassembly.last_frame_id),
                    decoder_waiting_keyframe || snapshot.waiting_keyframe,
                    stats.color_spec.json_fragment(),
                    stats.decoder_color_metadata.json_fragment("decoder_output"),
                    receiver_transport_fragment(
                        snapshot,
                        stats,
                        ReassemblyStats::default(),
                        duration_sec.max(0.001),
                        decoder_last_keyframe_id,
                        0,
                        0,
                    )
                );
            }
        }
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
        PlayoutBuffer::new(config.playout_delay_ms)?;
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
        if config
            .debug_dump_frames
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err("debug-dump-frames must not be empty".to_string());
        }
        if config.debug_dump_limit == 0 {
            return Err("debug-dump-limit must be greater than zero".to_string());
        }
        Ok(())
    }

    fn average(total: f64, count: u64) -> f64 {
        if count == 0 {
            0.0
        } else {
            total / count as f64
        }
    }

    fn receiver_transport_fragment(
        snapshot: NetworkSnapshot,
        stats: &ViewerStats,
        previous: ReassemblyStats,
        elapsed_sec: f64,
        decoder_last_keyframe_id: Option<u64>,
        previous_decoder_input: u64,
        previous_rendered: u64,
    ) -> String {
        let packets_per_second = snapshot
            .reassembly
            .packets_received
            .saturating_sub(previous.packets_received) as f64
            / elapsed_sec.max(0.001);
        format!(
            r#""udp_recv_buffer_bytes":{},"packets_per_second":{:.2},"playout_delay_ms":{},"playout_buffer_frames":{},"playout_buffer_peak_frames":{},"playout_late_frames":{},"playout_dropped_late_frames":{},"playout_dropped_discontinuity_frames":{},"playout_delay_actual_ms_avg":{:.3},"playout_delay_actual_ms_max":{:.3},"decoder_input_fps":{:.2},"render_output_fps":{:.2},"frames_missing_packets":{},"frames_dropped_incomplete":{},"fec_mode":"{}","fec_packets_received":{},"fec_frames_recovered":{},"fec_packets_recovered":{},"fec_recovery_failed_multi_missing":{},"fec_recovery_failed_no_parity":{},"fec_recovery_failed_invalid":{},"frames_missing_after_fec":{},"frames_dropped_after_fec":{},"keyframe_recovery_count":{},"last_keyframe_id":{},"decoder_resets":{},"drop_damaged_gop":{},"damaged_gop_count":{},"frames_discarded_damaged_gop":{},"frames_discarded_waiting_keyframe":{},"waiting_keyframe_entries":{},"waiting_keyframe_exits":{},"idr_frames_received":{},"idr_frames_used_for_recovery":{},"non_idr_frames_discarded_waiting":{},"recovery_wait_ms_avg":{:.3},"recovery_wait_ms_max":{:.3},"recovery_wait_frames_avg":{:.3},"recovery_wait_frames_max":{},"next_decode_frame_id":{},"decode_gate_stalls":{},"decode_gate_gap_events":{},"decode_gate_gap_to_damage_ms_avg":{:.3},"decode_gate_gap_to_damage_ms_max":{:.3},"frames_buffered_waiting_order":{},"frames_discarded_decode_gate":{},"reorder_wait_ms":{},{}"#,
            snapshot.udp_recv_buffer_bytes,
            packets_per_second,
            snapshot.playout_delay_ms,
            snapshot.playout_buffer_frames,
            snapshot.playout.buffer_peak_frames,
            snapshot.playout.late_frames,
            snapshot.playout.dropped_late_frames,
            snapshot.playout.dropped_discontinuity_frames,
            snapshot.playout.delay_actual_ms_avg(),
            snapshot.playout.delay_actual_ms_max,
            stats
                .frames_decoder_input
                .saturating_sub(previous_decoder_input) as f64
                / elapsed_sec.max(0.001),
            stats.frames_rendered.saturating_sub(previous_rendered) as f64 / elapsed_sec.max(0.001),
            snapshot.reassembly.frames_incomplete_expired,
            snapshot.reassembly.frames_incomplete_expired,
            if snapshot.reassembly.fec_packets_received > 0
                || snapshot.reassembly.fec_protected_data_packets_received > 0
            {
                "single-xor"
            } else {
                "off"
            },
            snapshot.reassembly.fec_packets_received,
            snapshot.reassembly.fec_frames_recovered,
            snapshot.reassembly.fec_packets_recovered,
            snapshot.reassembly.fec_recovery_failed_multi_missing,
            snapshot.reassembly.fec_recovery_failed_no_parity,
            snapshot.reassembly.fec_recovery_failed_invalid,
            snapshot.reassembly.frames_missing_after_fec,
            snapshot.reassembly.frames_dropped_after_fec,
            snapshot.keyframe_recovery_count + stats.keyframe_recovery_count,
            optional_u64_json(decoder_last_keyframe_id.or(snapshot.last_keyframe_id)),
            stats.decoder_resets,
            snapshot.drop_damaged_gop,
            snapshot.damaged_gop.damaged_gop_count,
            snapshot.damaged_gop.frames_discarded_damaged_gop,
            snapshot.damaged_gop.frames_discarded_waiting_keyframe,
            snapshot.damaged_gop.waiting_keyframe_entries,
            snapshot.damaged_gop.waiting_keyframe_exits,
            snapshot.damaged_gop.idr_frames_received,
            snapshot.damaged_gop.idr_frames_used_for_recovery,
            snapshot.damaged_gop.non_idr_frames_discarded_waiting,
            snapshot.damaged_gop.recovery_wait_ms_avg(),
            snapshot.damaged_gop.recovery_wait_ms_max,
            snapshot.damaged_gop.recovery_wait_frames_avg(),
            snapshot.damaged_gop.recovery_wait_frames_max,
            optional_u64_json(snapshot.reassembly.next_decode_frame_id),
            snapshot.reassembly.decode_gate_stalls,
            snapshot.reassembly.decode_gate_gap_events,
            snapshot.reassembly.decode_gate_gap_to_damage_ms_avg(),
            snapshot.reassembly.decode_gate_gap_to_damage_ms_max,
            snapshot.completed_waiting,
            snapshot.reassembly.frames_discarded_decode_gate,
            snapshot.reassembly.reorder_wait_ms,
            stats.render_state.json_fragment(),
        )
    }

    fn optional_u64_json(value: Option<u64>) -> String {
        value.map_or_else(|| "null".to_string(), |value| value.to_string())
    }

    fn duration_elapsed(started_at: Instant, duration_sec: Option<u64>) -> bool {
        duration_sec
            .map(|seconds| started_at.elapsed() >= Duration::from_secs(seconds))
            .unwrap_or(false)
    }

    fn optional_duration_text(duration_sec: Option<u64>) -> String {
        duration_sec.map_or_else(|| "unlimited".to_string(), |seconds| seconds.to_string())
    }

    fn json_escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }

    fn stop_reason(
        closed_by_user: bool,
        network_error: bool,
        duration_sec: Option<u64>,
    ) -> &'static str {
        if closed_by_user {
            "window_closed"
        } else if STOP_REQUESTED.load(Ordering::SeqCst) {
            "ctrl_c"
        } else if duration_sec.is_some() && !network_error {
            "duration"
        } else {
            "stopped"
        }
    }

    fn human_stop_reason(
        closed_by_user: bool,
        network_error: bool,
        duration_sec: Option<u64>,
    ) -> &'static str {
        if closed_by_user {
            "window-closed"
        } else if STOP_REQUESTED.load(Ordering::SeqCst) {
            "console-control"
        } else if duration_sec.is_some() && !network_error {
            "duration-complete"
        } else if network_error {
            "network-error"
        } else {
            "stopped"
        }
    }

    pub fn run_self_test() -> Result<(), String> {
        fn frame(frame_id: u64, keyframe: bool) -> EncodedFrame {
            EncodedFrame {
                frame_id,
                flags: if keyframe { crate::FLAG_KEYFRAME } else { 0 },
                timestamp_ms: frame_id * 33,
                bytes: if keyframe {
                    vec![
                        0,
                        0,
                        0,
                        1,
                        7,
                        0x64,
                        0,
                        0,
                        0,
                        1,
                        8,
                        0xee,
                        0,
                        0,
                        0,
                        1,
                        5,
                        frame_id as u8,
                    ]
                } else {
                    vec![0, 0, 0, 1, 1, frame_id as u8]
                },
            }
        }

        let mut queue = DecodeQueue::new(2, true);
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

        let now = Instant::now();
        let mut damaged_queue = DecodeQueue::new(4, true);
        damaged_queue.enqueue_frame(frame(10, true), 4);
        damaged_queue.begin_damaged_gop_recovery(now, Some(10));
        damaged_queue.enqueue_frame(frame(11, false), 4);
        damaged_queue.enqueue_frame(frame(12, true), 4);
        let damaged_stats = damaged_queue.damaged_gop_stats();
        if damaged_queue.waiting_for_keyframe
            || damaged_stats.damaged_gop_count != 1
            || damaged_stats.frames_discarded_damaged_gop != 2
            || damaged_stats.recovery_completed != 1
            || !matches!(damaged_queue.items.front(), Some(DecodeQueueItem::Reset))
            || !matches!(
                damaged_queue.items.back(),
                Some(DecodeQueueItem::Frame(frame)) if frame.frame_id == 12
            )
        {
            return Err("damaged GOP keyframe recovery failed".to_string());
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
