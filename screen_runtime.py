#!/usr/bin/env python3
"""Native Windows screen-sharing process manager for AgoraLink.

The runtime owns only the bundled ``agoralink_media`` process. Chat control
messages, file transfer, and database behavior remain outside this module.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import logging
import os
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Mapping, Optional, TextIO

from app_paths import FROZEN, NATIVE_MEDIA_EXPECTED_SHA256, PACKAGE_FLAVOR, debug_log_dir
from process_utils import run_no_console
from screen_control import DEFAULT_SCREEN_PORT, SCREEN_BACKEND_RUST
from screen_profile import DEFAULT_SCREEN_PROFILE, profile_id_from_info


STATE_IDLE = "idle"
STATE_RECEIVING = "receiving"
STATE_SENDING = "sending"
STATE_STOP_REQUESTED = "stop_requested"
STATE_STOPPING = "stopping"
STATE_NATIVE_STOPPED = "native_stopped"
STATE_ERROR = "error"

LOCAL_STOP_VERSION = 1
GRACEFUL_STOP_TIMEOUT_SEC = 3.0
TERMINATE_STOP_TIMEOUT_SEC = 2.0
FORCED_STOP_TIMEOUT_SEC = 2.0

DEFAULT_NATIVE_SCREEN_PRESET = "r4_default"
NATIVE_SCREEN_PRESETS: Dict[str, Dict[str, object]] = {
    "r4_default": {
        "id": "r4_default",
        "label": "R4 Default: 1920x1080 60fps 22Mbps NACK adaptive off",
        "width": 1920,
        "height": 1080,
        "fps": 60,
        "bitrate_mbps": 22,
        "playout_delay_ms": 250,
        "repair": "nack",
        "adaptive_quality": "off",
        "encoder": "auto",
        "convert_backend": "auto",
        "render_backend": "d3d11",
    },
    "stable": {
        "id": "stable",
        "label": "Stable 720p30 / 20 Mbps",
        "width": 1280,
        "height": 720,
        "fps": 30,
        "bitrate_mbps": 20,
        "playout_delay_ms": 120,
        "repair": "off",
        "adaptive_quality": "off",
        "encoder": "auto",
        "convert_backend": "auto",
        "render_backend": "d3d11",
    },
    "recommended": {
        "id": "recommended",
        "label": "Recommended 1080p60 / 50 Mbps",
        "width": 1920,
        "height": 1080,
        "fps": 60,
        "bitrate_mbps": 50,
        "playout_delay_ms": 250,
        "repair": "nack",
        "adaptive_quality": "off",
        "encoder": "auto",
        "convert_backend": "auto",
        "render_backend": "d3d11",
    },
    "high_quality": {
        "id": "high_quality",
        "label": "High Quality 1080p60 / 80 Mbps",
        "width": 1920,
        "height": 1080,
        "fps": 60,
        "bitrate_mbps": 80,
        "playout_delay_ms": 300,
        "repair": "nack",
        "adaptive_quality": "off",
        "encoder": "auto",
        "convert_backend": "auto",
        "render_backend": "d3d11",
    },
}

RUST_NATIVE_MISSING_MESSAGE = "Rust native media executable not found"
RUST_NATIVE_UNAVAILABLE_MESSAGE = "Native screen sharing is unavailable on this device."
RUST_NATIVE_AUDIO_UNAVAILABLE_MESSAGE = (
    "Rust native system audio is unavailable; continuing video-only."
)

_LOGGER = logging.getLogger(__name__)
_INVALID_NATIVE_PRESET_WARNED: set[str] = set()
_INVALID_NATIVE_PRESET_WARNED_LOCK = threading.Lock()


def _is_frozen_runtime() -> bool:
    return bool(getattr(sys, "frozen", FROZEN))


def resolve_native_screen_preset_id(
    value: object = None,
    *,
    warn_invalid: bool = True,
) -> tuple[str, bool]:
    if value is None or str(value).strip() == "":
        return DEFAULT_NATIVE_SCREEN_PRESET, False
    raw = str(value.get("id") if isinstance(value, Mapping) else value or "").strip()
    key = raw.lower().replace("-", "_").replace(" ", "_")
    if key in NATIVE_SCREEN_PRESETS:
        return key, False
    for preset_key, preset in NATIVE_SCREEN_PRESETS.items():
        if raw == str(preset.get("label") or ""):
            return preset_key, False
    if warn_invalid:
        _warn_invalid_native_screen_preset(value)
    return DEFAULT_NATIVE_SCREEN_PRESET, True


def _warn_invalid_native_screen_preset(value: object) -> None:
    raw = str(value or "").strip() or "<empty>"
    with _INVALID_NATIVE_PRESET_WARNED_LOCK:
        if raw in _INVALID_NATIVE_PRESET_WARNED:
            return
        _INVALID_NATIVE_PRESET_WARNED.add(raw)
    _LOGGER.warning(
        "Unknown native screen preset %r; falling back to %s",
        raw,
        DEFAULT_NATIVE_SCREEN_PRESET,
    )


def native_screen_preset_info(value: object = None) -> Dict[str, object]:
    if isinstance(value, Mapping) and not str(value.get("id") or "").strip():
        merged = dict(NATIVE_SCREEN_PRESETS[DEFAULT_NATIVE_SCREEN_PRESET])
        merged.update(dict(value))
        merged["id"] = DEFAULT_NATIVE_SCREEN_PRESET
        return merged
    key, _invalid = resolve_native_screen_preset_id(value)
    return dict(NATIVE_SCREEN_PRESETS[key])


def native_media_file_identity(
    path: object,
    *,
    expected_sha256: str = NATIVE_MEDIA_EXPECTED_SHA256,
) -> Dict[str, object]:
    candidate = Path(str(path or "")).expanduser()
    expected = str(expected_sha256 or "").strip().upper()
    result: Dict[str, object] = {
        "path": str(candidate),
        "exists": False,
        "size_bytes": 0,
        "sha256": "",
        "expected_sha256": expected,
        "hash_matches": False,
        "error": "",
    }
    if not str(path or "").strip() or not candidate.is_file():
        result["error"] = RUST_NATIVE_MISSING_MESSAGE
        return result
    try:
        resolved = candidate.resolve()
        digest = hashlib.sha256()
        with resolved.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        actual = digest.hexdigest().upper()
        result.update(
            {
                "path": str(resolved),
                "exists": True,
                "size_bytes": int(resolved.stat().st_size),
                "sha256": actual,
                "hash_matches": bool(expected and actual == expected),
            }
        )
        if expected and actual != expected:
            result["error"] = (
                "Rust native media executable hash mismatch: "
                f"expected {expected}, got {actual}"
            )
    except Exception as exc:
        result["error"] = f"Rust native media executable verification failed: {exc}"
    return result


class ScreenRuntime:
    def __init__(
        self,
        *,
        script_dir: Optional[Path] = None,
        popen_factory: Callable[..., subprocess.Popen[str]] = subprocess.Popen,
        taskkill_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
        tool_finder: Optional[Callable[[str], str]] = None,
        stop_timeout: float = 5.0,
        graceful_stop_timeout: float = GRACEFUL_STOP_TIMEOUT_SEC,
        terminate_stop_timeout: float = TERMINATE_STOP_TIMEOUT_SEC,
        forced_stop_timeout: float = FORCED_STOP_TIMEOUT_SEC,
    ) -> None:
        self.script_dir = (
            Path(script_dir) if script_dir is not None else Path(__file__).resolve().parent
        ).resolve()
        self._popen_factory = popen_factory
        self._taskkill_runner = taskkill_runner
        self._tool_finder = tool_finder
        self.stop_timeout = float(stop_timeout)
        self.graceful_stop_timeout = max(0.01, float(graceful_stop_timeout))
        self.terminate_stop_timeout = max(0.01, float(terminate_stop_timeout))
        self.forced_stop_timeout = max(0.01, float(forced_stop_timeout))
        self._process: Optional[subprocess.Popen[str]] = None
        self._lifecycle_lock = threading.RLock()
        self._stop_complete = threading.Event()
        self._stop_complete.set()
        self._stop_in_progress = False
        self._async_stop_thread: Optional[threading.Thread] = None
        self._reader_threads: List[threading.Thread] = []
        self._native_stopped_event = threading.Event()
        self._native_stopped_payload: Dict[str, object] = {}
        self._process_log_file: Optional[TextIO] = None
        self._process_log_path: Optional[Path] = None
        self._state = STATE_IDLE
        self.last_error = ""
        self.last_returncode: Optional[int] = None
        self.last_command: List[str] = []
        self.current_backend = SCREEN_BACKEND_RUST
        self.current_mode: Optional[str] = None
        self.current_host: Optional[str] = None
        self.current_port: Optional[int] = None
        self.current_profile: Optional[str] = None
        self.current_peer_label: Optional[str] = None
        self.current_audio_enabled = False
        self.current_audio_mode = "none"
        self.current_audio_state = "video_only"
        self.current_audio_config: Dict[str, object] = {"enabled": False, "mode": "none"}
        self.current_audio_error = ""
        self.current_audio_input = ""
        self.current_native_preset = DEFAULT_NATIVE_SCREEN_PRESET
        self.native_stats: Dict[str, object] = {}
        self.native_last_event: Dict[str, object] = {}
        self.stop_telemetry: Dict[str, object] = self._new_stop_telemetry()
        self._native_audio_capabilities_cache: Dict[str, Dict[str, object]] = {}
        self._native_identity_cache: Dict[tuple[str, int, int], Dict[str, object]] = {}

    def start_receiver(
        self,
        port: int = DEFAULT_SCREEN_PORT,
        profile: object = "",
        *,
        peer_label: Optional[str] = None,
        selected_profile: object = None,
        screen_port: Optional[int] = None,
        audio: object = None,
        backend: object = SCREEN_BACKEND_RUST,
        native_preset: object = None,
    ) -> Dict[str, object]:
        if self._stop_is_active():
            return self._stopping_result()
        if self._has_running_process():
            return self._already_running_result()
        try:
            self._normalize_backend(backend)
            if screen_port is not None:
                port = int(screen_port)
            if selected_profile:
                profile = selected_profile
            port = self._validate_port(port)
            profile_name = self._validate_profile(profile) if profile else ""
            peer_label_text = self._validate_peer_label(peer_label)
            audio_config = self._normalize_audio_config(audio, enabled_default=False)
            audio_config, audio_notice = self._resolve_native_audio_request(
                audio_config,
                role="playback",
            )
            native_exe = self._find_native_media_exe()
            if not native_exe:
                return self._set_error(RUST_NATIVE_MISSING_MESSAGE)
            preset = native_screen_preset_info(native_preset)
            command = self._build_native_receiver_command(
                port,
                native_exe=native_exe,
                native_preset=preset,
                audio=audio_config,
            )
            self.last_command = list(command)
            self.native_stats = {}
            self.native_last_event = {}
            self.current_native_preset = str(
                preset.get("id") or DEFAULT_NATIVE_SCREEN_PRESET
            )
            self._process = self._start_native_process(command, "agoralink_media")
        except Exception as exc:
            return self._set_error(str(exc))

        self._state = STATE_RECEIVING
        self.last_error = audio_notice
        self.last_returncode = None
        self.current_backend = SCREEN_BACKEND_RUST
        self.current_mode = STATE_RECEIVING
        self.current_host = None
        self.current_port = port
        self.current_profile = profile_name or None
        self.current_peer_label = peer_label_text or None
        self._set_audio_session(audio_config)
        self._schedule_startup_exit_check("agoralink_media")
        return self.get_state()

    def start_sender(
        self,
        host: str,
        port: int = DEFAULT_SCREEN_PORT,
        profile: object = DEFAULT_SCREEN_PROFILE,
        *,
        peer_label: Optional[str] = None,
        selected_profile: object = None,
        screen_port: Optional[int] = None,
        system_audio: bool = False,
        audio: object = None,
        backend: object = SCREEN_BACKEND_RUST,
        native_preset: object = None,
    ) -> Dict[str, object]:
        if self._stop_is_active():
            return self._stopping_result()
        if self._has_running_process():
            return self._already_running_result()
        try:
            self._normalize_backend(backend)
            if screen_port is not None:
                port = int(screen_port)
            if selected_profile:
                profile = selected_profile
            host = self._validate_host(host)
            port = self._validate_port(port)
            profile_name = self._validate_profile(profile)
            peer_label_text = self._validate_peer_label(peer_label) or host
            audio_config = self._normalize_audio_config(
                audio,
                enabled_default=bool(system_audio),
            )
            audio_config, audio_notice = self._resolve_native_audio_request(
                audio_config,
                role="capture",
            )
            native_exe = self._find_native_media_exe()
            if not native_exe:
                return self._set_error(RUST_NATIVE_MISSING_MESSAGE)
            preset = native_screen_preset_info(native_preset)
            command = self._build_native_sender_command(
                host=host,
                port=port,
                native_exe=native_exe,
                native_preset=preset,
                audio=audio_config,
            )
            self.last_command = list(command)
            self.native_stats = {}
            self.native_last_event = {}
            self.current_native_preset = str(
                preset.get("id") or DEFAULT_NATIVE_SCREEN_PRESET
            )
            self._process = self._start_native_process(command, "agoralink_media")
        except Exception as exc:
            return self._set_error(str(exc))

        self._state = STATE_SENDING
        self.last_error = audio_notice
        self.last_returncode = None
        self.current_backend = SCREEN_BACKEND_RUST
        self.current_mode = STATE_SENDING
        self.current_host = host
        self.current_port = port
        self.current_profile = profile_name
        self.current_peer_label = peer_label_text or None
        self._set_audio_session(audio_config)
        self._schedule_startup_exit_check("agoralink_media")
        return self.get_state()

    def stop(self, *, reason: str = "gui_stop") -> Dict[str, object]:
        with self._lifecycle_lock:
            if self._stop_in_progress:
                wait_for_existing = True
            else:
                wait_for_existing = False
                self._refresh_process_locked()
                process = self._process
                if process is None or process.poll() is not None:
                    if process is not None:
                        self.last_returncode = self._process_returncode(process)
                    self._process = None
                    self._join_reader_threads(self.forced_stop_timeout)
                    self._close_process_log_file()
                    self._state = STATE_IDLE
                    self.last_error = ""
                    self._clear_current_session()
                    return self._snapshot()
                self._stop_in_progress = True
                self._stop_complete.clear()
                self._native_stopped_event.clear()
                self._native_stopped_payload = {}
                self.stop_telemetry = self._new_stop_telemetry()
                self._state = STATE_STOP_REQUESTED
                self._record_stop_state(STATE_STOP_REQUESTED)

        if wait_for_existing:
            self._stop_complete.wait(
                self.graceful_stop_timeout
                + self.terminate_stop_timeout
                + self.forced_stop_timeout
            )
            return self.get_state()

        started_at = time.monotonic()
        stop_error = ""
        try:
            self.stop_telemetry["graceful_stop_requested"] = self._send_local_stop(
                process,
                reason=reason,
            )
            self._state = STATE_STOPPING
            self._record_stop_state(STATE_STOPPING)
            exited = self._wait_for_native_stop(process, self.graceful_stop_timeout)
            if not exited:
                self.stop_telemetry["forced_terminate_used"] = True
                process.terminate()
                exited = self._wait_for_process_exit(
                    process,
                    self.terminate_stop_timeout,
                )
            if not exited:
                self.stop_telemetry["forced_kill_used"] = True
                self._force_kill_process_tree(process)
                exited = self._wait_for_process_exit(process, self.forced_stop_timeout)
            if not exited:
                stop_error = "native screen process did not exit after forced termination"
            self.last_returncode = self._process_returncode(process)
            self.stop_telemetry["exit_code"] = self.last_returncode
        except Exception as exc:
            stop_error = str(exc)
            if process.poll() is None:
                try:
                    process.kill()
                    self._wait_for_process_exit(process, self.forced_stop_timeout)
                except Exception as kill_exc:
                    stop_error = f"{stop_error}; final kill failed: {kill_exc}"
        finally:
            self._close_process_stdin(process)
            self._join_reader_threads(self.forced_stop_timeout)
            self.stop_telemetry["native_stopped_received"] = bool(
                self._native_stopped_event.is_set()
            )
            final_event = dict(self._native_stopped_payload)
            self.stop_telemetry["stream_close_sent"] = bool(
                final_event.get("stream_close_sent") or final_event.get("close_sent")
            )
            self.stop_telemetry["stream_close_ack_received"] = bool(
                final_event.get("stream_close_ack_received")
            )
            self.stop_telemetry["stop_elapsed_ms"] = round(
                (time.monotonic() - started_at) * 1000.0,
                3,
            )
            with self._lifecycle_lock:
                process_alive = process.poll() is None
                if self._process is process and not process_alive:
                    self._process = None
                    self._close_process_log_file()
                self._state = STATE_NATIVE_STOPPED
                self._record_stop_state(STATE_NATIVE_STOPPED)
                if stop_error or process_alive:
                    self.last_error = stop_error or "native screen process is still running"
                    self._state = STATE_ERROR
                    self._record_stop_state(STATE_ERROR)
                else:
                    self.last_error = ""
                    self._state = STATE_IDLE
                    self._record_stop_state(STATE_IDLE)
                self._clear_current_session()
                self._stop_in_progress = False
                self._stop_complete.set()
        return self._snapshot(ok=not bool(stop_error) and process.poll() is not None)

    def stop_async(self, *, reason: str = "app_close") -> threading.Thread:
        with self._lifecycle_lock:
            existing = self._async_stop_thread
            if existing is not None and existing.is_alive():
                return existing
            thread = threading.Thread(
                target=self.stop,
                kwargs={"reason": reason},
                name="agoralink-screen-stop",
                daemon=False,
            )
            self._async_stop_thread = thread
            thread.start()
            return thread

    def is_running(self) -> bool:
        return self._has_running_process()

    def get_state(self) -> Dict[str, object]:
        self._refresh_process()
        return self._snapshot()

    def _debug_log_dir(self) -> Path:
        return debug_log_dir()

    def _open_process_log_file(self, tool_name: str) -> Optional[TextIO]:
        self._process_log_path = None
        if self._popen_factory is not subprocess.Popen:
            return None
        path = self._debug_log_dir() / f"screen_{str(tool_name or 'process')}.log"
        self._process_log_path = path
        return path.open("a", encoding="utf-8", errors="replace")

    def _close_process_log_file(self) -> None:
        handle = self._process_log_file
        self._process_log_file = None
        if handle is not None:
            try:
                handle.close()
            except Exception:
                pass

    def _write_process_log_note(self, text: str) -> None:
        handle = self._process_log_file
        if handle is None:
            return
        try:
            handle.write(f"[AgoraLink] {str(text or '').strip()}\n")
            handle.flush()
        except Exception:
            pass

    def _start_native_process(
        self,
        command: List[str],
        tool_name: str,
    ) -> subprocess.Popen[str]:
        self._close_process_log_file()
        log_file = self._open_process_log_file(tool_name)
        try:
            identity = self._validated_native_media_identity(command[0] if command else "")
            if log_file is not None:
                log_file.write(f"[AgoraLink] native executable: {identity['path']}\n")
                log_file.write(f"[AgoraLink] native executable sha256: {identity['sha256']}\n")
                log_file.flush()
            process = self._popen_factory(
                command,
                cwd=str(self.script_dir),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
                shell=False,
            )
        except Exception:
            if log_file is not None:
                try:
                    log_file.close()
                except Exception:
                    pass
            raise
        self._process_log_file = log_file
        self._native_stopped_event.clear()
        self._native_stopped_payload = {}
        self._start_native_output_threads(process, log_file)
        return process

    def _start_native_output_threads(
        self,
        process: subprocess.Popen[str],
        log_file: Optional[TextIO],
    ) -> None:
        threads: List[threading.Thread] = []
        stdout = getattr(process, "stdout", None)
        stderr = getattr(process, "stderr", None)
        if stdout is not None:
            thread = threading.Thread(
                target=self._read_native_stdout,
                args=(process, stdout),
                daemon=True,
            )
            thread.start()
            threads.append(thread)
        if stderr is not None:
            thread = threading.Thread(
                target=self._read_native_stderr,
                args=(stderr, log_file),
                daemon=True,
            )
            thread.start()
            threads.append(thread)
        with self._lifecycle_lock:
            self._reader_threads = threads

    def _read_native_stdout(
        self,
        process: subprocess.Popen[str],
        stream: TextIO,
    ) -> None:
        try:
            for line in iter(stream.readline, ""):
                text = str(line or "").strip()
                if not text:
                    continue
                try:
                    event = json.loads(text)
                except Exception:
                    self._write_process_log_note(f"native stdout non-json: {text}")
                    continue
                if isinstance(event, Mapping):
                    self._handle_native_event(dict(event), process)
        except Exception as exc:
            self._write_process_log_note(f"native stdout reader failed: {exc}")

    def _read_native_stderr(
        self,
        stream: TextIO,
        log_file: Optional[TextIO],
    ) -> None:
        try:
            for line in iter(stream.readline, ""):
                text = str(line or "")
                if text and log_file is not None:
                    try:
                        log_file.write(text)
                        if not text.endswith("\n"):
                            log_file.write("\n")
                        log_file.flush()
                    except Exception:
                        pass
        except Exception as exc:
            self._write_process_log_note(f"native stderr reader failed: {exc}")

    def _handle_native_event(
        self,
        event: Dict[str, object],
        process: Optional[subprocess.Popen[str]] = None,
    ) -> None:
        event_type = str(event.get("type") or "").strip()
        self.native_last_event = dict(event)
        if event_type == "NATIVE_SCREEN_STATS":
            self.native_stats = dict(event)
            self._update_native_audio_state(event)
        elif event_type == "NATIVE_SCREEN_STARTED":
            self._update_native_audio_state(event)
        elif event_type in {"NATIVE_SCREEN_STOPPED", "NATIVE_SCREEN_SHUTDOWN_FAILED"}:
            self.native_stats = dict(event)
            self._native_stopped_payload = dict(event)
            self._native_stopped_event.set()
            self._update_native_audio_state(event)
            if event_type == "NATIVE_SCREEN_STOPPED" and (
                process is None or process is self._process
            ):
                self.last_error = ""
        elif event_type == "NATIVE_SCREEN_ERROR":
            message = str(
                event.get("error") or event.get("message") or "native screen error"
            ).strip()
            self.last_error = message or "native screen error"
            self._state = STATE_ERROR

    def _schedule_startup_exit_check(
        self,
        tool_name: str,
        delay_sec: float = 1.0,
    ) -> None:
        process = self._process
        if process is None:
            return
        timer = threading.Timer(
            max(0.0, float(delay_sec or 0.0)),
            lambda: self._handle_startup_exit_check(process, tool_name),
        )
        timer.daemon = True
        timer.start()

    def _handle_startup_exit_check(
        self,
        process: subprocess.Popen[str],
        tool_name: str,
    ) -> None:
        with self._lifecycle_lock:
            if process is not self._process:
                return
            returncode = process.poll()
            if returncode is None:
                return
            self.last_returncode = int(returncode)
            if self._stop_in_progress:
                return
            self._process = None
            self._join_reader_threads(self.forced_stop_timeout)
            self._close_process_log_file()
            if returncode == 0:
                self._state = STATE_IDLE
                self.last_error = ""
                self._clear_current_session()
                return
            tail = self._read_process_log_tail(tool_name)
            self._state = STATE_ERROR
            self.last_error = f"{tool_name} exited with code {returncode}"
            if tail:
                self.last_error = f"{self.last_error}\n{tail}"

    def _read_process_log_tail(
        self,
        tool_name: str = "",
        max_chars: int = 4000,
    ) -> str:
        path = self._process_log_path
        if path is None:
            candidate = self._debug_log_dir() / f"screen_{str(tool_name or 'process')}.log"
            path = candidate if candidate.exists() else None
        if path is None:
            return ""
        try:
            max_bytes = max(512, int(max_chars or 4000) * 4)
            size = path.stat().st_size
            with path.open("rb") as handle:
                if size > max_bytes:
                    handle.seek(-max_bytes, os.SEEK_END)
                raw = handle.read(max_bytes)
            return raw.decode("utf-8", errors="replace")[-int(max_chars or 4000) :].strip()
        except Exception:
            return ""

    def _has_running_process(self) -> bool:
        self._refresh_process()
        return self._process is not None and self._process.poll() is None

    def _refresh_process(self) -> None:
        with self._lifecycle_lock:
            self._refresh_process_locked()

    def _refresh_process_locked(self) -> None:
        if self._process is None:
            return
        returncode = self._process.poll()
        if returncode is None:
            return
        self.last_returncode = int(returncode)
        if self._stop_in_progress:
            return
        self._process = None
        self._join_reader_threads(FORCED_STOP_TIMEOUT_SEC)
        self._close_process_log_file()
        if self._state == STATE_STOPPING or returncode == 0:
            self._state = STATE_IDLE
            self.last_error = ""
            self._clear_current_session()
        else:
            mode = self.current_mode or "screen"
            self._state = STATE_ERROR
            self.last_error = f"{mode} process exited with code {returncode}"

    def _snapshot(
        self,
        *,
        state: Optional[str] = None,
        ok: Optional[bool] = None,
    ) -> Dict[str, object]:
        running = self._process is not None and self._process.poll() is None
        actual_state = state or self._state
        return {
            "ok": bool(running or actual_state == STATE_IDLE) if ok is None else bool(ok),
            "state": actual_state,
            "running": running,
            "backend": SCREEN_BACKEND_RUST,
            "mode": self.current_mode,
            "host": self.current_host,
            "port": self.current_port,
            "profile": self.current_profile,
            "peer_label": self.current_peer_label,
            "audio_enabled": self.current_audio_enabled,
            "audio_mode": self.current_audio_mode,
            "audio_state": self.current_audio_state,
            "audio_config": dict(self.current_audio_config),
            "audio_error": self.current_audio_error,
            "audio_input": self.current_audio_input,
            "pid": (
                int(self._process.pid)
                if running and getattr(self._process, "pid", None) is not None
                else None
            ),
            "returncode": self.last_returncode,
            "last_error": self.last_error,
            "command": list(self.last_command),
            "native_preset": self.current_native_preset,
            "native_stats": dict(self.native_stats),
            "native_last_event": dict(self.native_last_event),
            "stop_telemetry": dict(self.stop_telemetry),
        }

    def _set_error(self, message: str) -> Dict[str, object]:
        self._state = STATE_ERROR
        self.last_error = str(message or "screen runtime error")
        return self._snapshot(ok=False)

    def _already_running_result(self) -> Dict[str, object]:
        self.last_error = "already running"
        result = self._snapshot(ok=False)
        result["error"] = "already running"
        return result

    def _stopping_result(self) -> Dict[str, object]:
        self.last_error = "screen runtime is stopping"
        result = self._snapshot(ok=False)
        result["error"] = self.last_error
        return result

    def _stop_is_active(self) -> bool:
        with self._lifecycle_lock:
            return self._stop_in_progress or self._state in {
                STATE_STOP_REQUESTED,
                STATE_STOPPING,
                STATE_NATIVE_STOPPED,
            }

    def _clear_current_session(self) -> None:
        self.current_mode = None
        self.current_host = None
        self.current_port = None
        self.current_profile = None
        self.current_peer_label = None
        self.current_audio_enabled = False
        self.current_audio_mode = "none"
        self.current_audio_state = "video_only"
        self.current_audio_config = {"enabled": False, "mode": "none"}
        self.current_audio_error = ""
        self.current_audio_input = ""
        self.current_native_preset = DEFAULT_NATIVE_SCREEN_PRESET

    def native_audio_capabilities(
        self,
        native_exe: Optional[str] = None,
    ) -> Dict[str, object]:
        """Return compile-time native media capability after a structured self-test."""
        executable = str(native_exe or self._find_native_media_exe() or "").strip()
        unavailable: Dict[str, object] = {
            "rust_audio_capture_available": False,
            "rust_audio_playback_available": False,
            "native_screen_av_sync_supported": False,
            "rust_audio_capability_checked": False,
            "rust_audio_capability_error": "",
            "native_self_test_ok": False,
        }
        if not executable:
            unavailable["rust_audio_capability_error"] = RUST_NATIVE_MISSING_MESSAGE
            return unavailable
        identity = self._native_media_identity(executable)
        if not self._native_identity_usable(identity):
            unavailable["rust_audio_capability_error"] = str(
                identity.get("error") or RUST_NATIVE_UNAVAILABLE_MESSAGE
            )
            return unavailable
        if os.name != "nt":
            unavailable["rust_audio_capability_error"] = (
                "Rust native system audio is only supported on Windows"
            )
            return unavailable
        cached = self._native_audio_capabilities_cache.get(executable)
        if cached is not None:
            return dict(cached)

        result = dict(unavailable)
        result["rust_audio_capability_checked"] = True
        try:
            completed = run_no_console(
                [executable, "self-test"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=5.0,
                shell=False,
            )
            events = []
            for line in str(completed.stdout or "").splitlines():
                try:
                    value = json.loads(line)
                except Exception:
                    continue
                if isinstance(value, Mapping):
                    events.append(dict(value))
            self_test_ok = bool(
                completed.returncode == 0
                and any(event.get("type") == "SELF_TEST" and event.get("ok") for event in events)
            )
            result.update(
                {
                    "native_self_test_ok": self_test_ok,
                    "rust_audio_capture_available": self_test_ok,
                    "rust_audio_playback_available": self_test_ok,
                    "native_screen_av_sync_supported": self_test_ok,
                }
            )
            if not self_test_ok:
                detail = str(completed.stderr or "").strip()
                result["rust_audio_capability_error"] = (
                    detail or "Rust native media self-test did not report success"
                )
        except Exception as exc:
            result["rust_audio_capability_error"] = (
                f"Rust native media self-test failed: {exc}"
            )
        self._native_audio_capabilities_cache[executable] = dict(result)
        return result

    def check_dependencies(self) -> Dict[str, object]:
        native = self._find_native_media_exe()
        identity = self._native_media_identity(native)
        native_ok = self._native_identity_usable(identity)
        package = self.screen_package_info()
        return {
            "ok": native_ok,
            "rust_native_ok": native_ok,
            "native_media_ok": native_ok,
            "rust_native_path": str(native or ""),
            "native_media_path": str(native or ""),
            "native_media_sha256": str(identity.get("sha256") or ""),
            "native_media_expected_sha256": NATIVE_MEDIA_EXPECTED_SHA256,
            "native_media_hash_matches": bool(identity.get("hash_matches")),
            "package_flavor": package["package_flavor"],
            "screen_backend_default": SCREEN_BACKEND_RUST,
            "native_screen_video_only": package["native_screen_video_only"],
            "rust_audio_capture_available": package["rust_audio_capture_available"],
            "rust_audio_playback_available": package["rust_audio_playback_available"],
            "native_screen_av_sync_supported": package[
                "native_screen_av_sync_supported"
            ],
            "rust_audio_capability_error": package["rust_audio_capability_error"],
            "error": "" if native_ok else RUST_NATIVE_UNAVAILABLE_MESSAGE,
            "rust_error": "" if native_ok else str(
                identity.get("error") or RUST_NATIVE_MISSING_MESSAGE
            ),
        }

    def screen_package_info(self) -> Dict[str, object]:
        native = self._find_native_media_exe()
        identity = self._native_media_identity(native)
        native_ok = self._native_identity_usable(identity)
        native_audio = self.native_audio_capabilities(native) if native_ok else {}
        return {
            "package_flavor": PACKAGE_FLAVOR,
            "rust_native_available": native_ok,
            "native_media_ok": native_ok,
            "rust_native_path": str(native or ""),
            "rust_native_sha256": str(identity.get("sha256") or ""),
            "rust_native_expected_sha256": NATIVE_MEDIA_EXPECTED_SHA256,
            "rust_native_hash_matches": bool(identity.get("hash_matches")),
            "rust_audio_capture_available": bool(
                native_audio.get("rust_audio_capture_available")
            ),
            "rust_audio_playback_available": bool(
                native_audio.get("rust_audio_playback_available")
            ),
            "native_screen_av_sync_supported": bool(
                native_audio.get("native_screen_av_sync_supported")
            ),
            "rust_audio_capability_checked": bool(
                native_audio.get("rust_audio_capability_checked")
            ),
            "rust_audio_capability_error": str(
                native_audio.get("rust_audio_capability_error") or ""
            ),
            "screen_backend_default": SCREEN_BACKEND_RUST,
            "native_screen_video_only": not bool(
                native_audio.get("native_screen_av_sync_supported")
            ),
        }

    def native_video_only_message(self) -> str:
        capabilities = self.native_audio_capabilities()
        if bool(capabilities.get("rust_audio_capture_available")):
            return (
                "Rust native system audio is available when enabled in "
                "Screen sharing settings"
            )
        return RUST_NATIVE_AUDIO_UNAVAILABLE_MESSAGE

    def _build_native_receiver_command(
        self,
        port: int,
        native_exe: Optional[str] = None,
        native_preset: object = None,
        audio: object = None,
    ) -> List[str]:
        executable = str(native_exe or self._find_native_media_exe() or "")
        if not executable:
            raise FileNotFoundError(RUST_NATIVE_MISSING_MESSAGE)
        preset = native_screen_preset_info(native_preset)
        command = [
            executable,
            "screen-recv",
            "--bind",
            "0.0.0.0",
            "--port",
            str(int(port)),
            "--title",
            "AgoraLink Native Viewer",
            "--playout-delay-ms",
            str(int(preset.get("playout_delay_ms") or 120)),
            "--repair",
            str(preset.get("repair") or "off"),
            "--render-backend",
            str(preset.get("render_backend") or "d3d11"),
        ]
        if bool(self._normalize_audio_config(audio).get("enabled")):
            command.extend(["--audio", "on"])
        return command

    def _build_native_sender_command(
        self,
        *,
        host: str,
        port: int,
        native_exe: Optional[str] = None,
        native_preset: object = None,
        audio: object = None,
    ) -> List[str]:
        executable = str(native_exe or self._find_native_media_exe() or "")
        if not executable:
            raise FileNotFoundError(RUST_NATIVE_MISSING_MESSAGE)
        preset = native_screen_preset_info(native_preset)
        command = [
            executable,
            "screen-send",
            "--host",
            str(host),
            "--port",
            str(int(port)),
            "--width",
            str(int(preset.get("width") or 1920)),
            "--height",
            str(int(preset.get("height") or 1080)),
            "--fps",
            str(int(preset.get("fps") or 60)),
            "--bitrate-mbps",
            str(int(preset.get("bitrate_mbps") or 22)),
            "--repair",
            str(preset.get("repair") or "nack"),
            "--adaptive-quality",
            str(preset.get("adaptive_quality") or "off"),
            "--encoder",
            str(preset.get("encoder") or "auto"),
            "--convert-backend",
            str(preset.get("convert_backend") or "auto"),
        ]
        if bool(self._normalize_audio_config(audio).get("enabled")):
            command.extend(["--audio", "system"])
        return command

    def _normalize_audio_config(
        self,
        audio: object = None,
        *,
        enabled_default: bool = False,
    ) -> Dict[str, object]:
        if isinstance(audio, Mapping):
            enabled = bool(audio.get("enabled", enabled_default))
            mode = str(audio.get("mode") or ("system" if enabled else "none")).lower()
            sample_rate = audio.get("sample_rate", 48000)
            channels = audio.get("channels", 2)
            bitrate = audio.get("bitrate", 128000)
        else:
            enabled = bool(enabled_default)
            mode = "system" if enabled else "none"
            sample_rate = 48000
            channels = 2
            bitrate = 128000
        if not enabled:
            return {"enabled": False, "mode": "none"}
        if mode != "system":
            raise ValueError("only system audio is supported")
        result: Dict[str, object] = {
            "enabled": True,
            "mode": "system",
            "codec": "aac",
            "sample_rate": max(8000, int(sample_rate or 48000)),
            "channels": max(1, int(channels or 2)),
            "bitrate": max(32000, int(bitrate or 128000)),
        }
        if isinstance(audio, Mapping):
            for key in ("state", "role"):
                value = str(audio.get(key) or "").strip()
                if value:
                    result[key] = value
            if "requested" in audio:
                result["requested"] = bool(audio.get("requested"))
        return result

    def _resolve_native_audio_request(
        self,
        audio_config: Mapping[str, Any],
        *,
        role: str,
    ) -> tuple[Dict[str, object], str]:
        if not bool((audio_config or {}).get("enabled")):
            return {"enabled": False, "mode": "none"}, ""
        capabilities = self.native_audio_capabilities()
        key = (
            "rust_audio_capture_available"
            if role == "capture"
            else "rust_audio_playback_available"
        )
        if bool(capabilities.get(key)):
            return self._native_audio_session_config(audio_config, role), ""
        message = self._native_audio_unavailable_message(capabilities, role)
        return self._native_audio_fallback_config(role, message), message

    @staticmethod
    def _native_audio_session_config(
        audio_config: Mapping[str, Any],
        role: str,
    ) -> Dict[str, object]:
        result = dict(audio_config or {})
        result.update(
            {
                "enabled": True,
                "mode": "system",
                "backend": "rust",
                "role": role,
                "requested": True,
                "state": "capturing" if role == "capture" else "playing",
                "input_name": (
                    "WASAPI loopback" if role == "capture" else "WASAPI render"
                ),
            }
        )
        return result

    @staticmethod
    def _native_audio_fallback_config(role: str, reason: str) -> Dict[str, object]:
        return {
            "enabled": False,
            "mode": "none",
            "backend": "rust",
            "role": role,
            "requested": True,
            "state": "fallback_video_only",
            "error": str(reason or RUST_NATIVE_AUDIO_UNAVAILABLE_MESSAGE),
        }

    @staticmethod
    def _native_audio_unavailable_message(
        capabilities: Mapping[str, object],
        role: str,
    ) -> str:
        detail = str((capabilities or {}).get("rust_audio_capability_error") or "").strip()
        action = "capture" if role == "capture" else "playback"
        message = (
            f"Rust native system audio {action} is unavailable; continuing video-only."
        )
        return f"{message} {detail}".strip()

    def _update_native_audio_state(self, event: Mapping[str, object]) -> None:
        current = dict(self.current_audio_config or {})
        if not bool(current.get("requested") or current.get("enabled")):
            return
        unavailable_reason = str(event.get("audio_unavailable_reason") or "").strip()
        role = str(
            current.get("role")
            or ("capture" if self.current_mode == STATE_SENDING else "playback")
        )
        if unavailable_reason:
            message = (
                f"System audio unavailable, continued video-only: {unavailable_reason}"
            )
            self._set_audio_session(self._native_audio_fallback_config(role, message))
            self.last_error = message
        elif bool(event.get("audio_enabled")):
            self._set_audio_session(self._native_audio_session_config(current, role))

    def _set_audio_session(self, audio_config: Mapping[str, Any]) -> None:
        enabled = bool((audio_config or {}).get("enabled"))
        self.current_audio_enabled = enabled
        self.current_audio_mode = "system" if enabled else "none"
        self.current_audio_state = str(
            (audio_config or {}).get("state")
            or ("system_audio_on" if enabled else "video_only")
        )
        self.current_audio_config = dict(
            audio_config or {"enabled": False, "mode": "none"}
        )
        self.current_audio_error = str((audio_config or {}).get("error") or "")
        self.current_audio_input = str((audio_config or {}).get("input_name") or "")

    def _find_native_media_exe(self) -> str:
        if self._tool_finder is not None:
            found = str(
                self._tool_finder("agoralink_media")
                or self._tool_finder("agoralink_media.exe")
                or ""
            )
            if found:
                return found
        executable_names = self._tool_executable_names("agoralink_media")
        for base in self._native_media_dirs():
            found = self._find_tool_in_dir(base, executable_names)
            if found:
                return found
        return ""

    def _native_media_dirs(self) -> List[Path]:
        directories: List[Path] = []
        executable_dir = Path(sys.executable).resolve().parent
        if _is_frozen_runtime():
            directories.append(
                executable_dir / "_internal" / "tools" / "agoralink_media"
            )
            meipass = str(getattr(sys, "_MEIPASS", "") or "").strip()
            if meipass:
                directories.append(Path(meipass) / "tools" / "agoralink_media")
            directories.append(executable_dir / "tools" / "agoralink_media")
        else:
            directories.append(
                self.script_dir
                / "rust-native"
                / "agoralink_media"
                / "target"
                / "release"
            )
        directories.append(self.script_dir / "tools" / "agoralink_media")
        return directories

    def _native_media_identity(self, path: object) -> Dict[str, object]:
        raw = str(path or "").strip()
        if not raw:
            return native_media_file_identity("")
        candidate = Path(raw)
        try:
            stat = candidate.stat()
            key = (str(candidate.resolve()), int(stat.st_size), int(stat.st_mtime_ns))
        except Exception:
            return native_media_file_identity(candidate)
        cached = self._native_identity_cache.get(key)
        if cached is None:
            cached = native_media_file_identity(candidate)
            self._native_identity_cache = {key: dict(cached)}
        return dict(cached)

    @staticmethod
    def _native_identity_usable(identity: Mapping[str, object]) -> bool:
        return bool(
            identity.get("exists")
            and (identity.get("hash_matches") or not _is_frozen_runtime())
        )

    def _validated_native_media_identity(self, path: object) -> Dict[str, object]:
        identity = self._native_media_identity(path)
        if not identity.get("exists"):
            raise FileNotFoundError(
                str(identity.get("error") or RUST_NATIVE_MISSING_MESSAGE)
            )
        if _is_frozen_runtime() and not identity.get("hash_matches"):
            raise RuntimeError(
                str(
                    identity.get("error")
                    or "Rust native media executable hash mismatch"
                )
            )
        return identity

    @staticmethod
    def _tool_executable_names(name: str) -> List[str]:
        base = str(name or "").strip()
        if not base:
            return []
        stem = base[:-4] if base.lower().endswith(".exe") else base
        names = [stem + ".exe", stem] if os.name == "nt" else [stem, stem + ".exe"]
        return list(dict.fromkeys(item for item in names if item))

    @staticmethod
    def _find_tool_in_dir(base: Path, executable_names: List[str]) -> str:
        try:
            path = Path(base)
            if path.is_file() and path.name in executable_names:
                return str(path.resolve())
            for executable in executable_names:
                candidate = path / executable
                if candidate.is_file():
                    return str(candidate.resolve())
        except Exception:
            pass
        return ""

    @staticmethod
    def _normalize_backend(backend: object) -> str:
        value = str(backend or SCREEN_BACKEND_RUST).strip().lower()
        if value in ("", "native", SCREEN_BACKEND_RUST):
            return SCREEN_BACKEND_RUST
        raise ValueError("only the Rust native screen backend is supported")

    @staticmethod
    def _new_stop_telemetry() -> Dict[str, object]:
        return {
            "graceful_stop_requested": False,
            "native_stopped_received": False,
            "stream_close_sent": False,
            "stream_close_ack_received": False,
            "forced_terminate_used": False,
            "forced_kill_used": False,
            "reader_threads_joined": True,
            "stop_elapsed_ms": 0.0,
            "exit_code": None,
            "state_history": [],
        }

    def _record_stop_state(self, state: str) -> None:
        history = self.stop_telemetry.setdefault("state_history", [])
        if isinstance(history, list) and (not history or history[-1] != state):
            history.append(state)

    @staticmethod
    def _process_returncode(process: subprocess.Popen[str]) -> Optional[int]:
        returncode = process.poll()
        return None if returncode is None else int(returncode)

    def _send_local_stop(
        self,
        process: subprocess.Popen[str],
        *,
        reason: str,
    ) -> bool:
        if process.poll() is not None:
            return False
        stdin = getattr(process, "stdin", None)
        if stdin is None:
            return False
        command = {
            "type": "LOCAL_STOP",
            "reason": str(reason or "gui_stop"),
            "version": LOCAL_STOP_VERSION,
        }
        try:
            stdin.write(json.dumps(command, separators=(",", ":")) + "\n")
            stdin.flush()
            return True
        except (BrokenPipeError, OSError, ValueError) as exc:
            self._write_process_log_note(f"native local stop channel failed: {exc}")
            return False

    @staticmethod
    def _close_process_stdin(process: subprocess.Popen[str]) -> None:
        stdin = getattr(process, "stdin", None)
        if stdin is None:
            return
        try:
            stdin.close()
        except Exception:
            pass

    def _wait_for_native_stop(
        self,
        process: subprocess.Popen[str],
        timeout_sec: float,
    ) -> bool:
        deadline = time.monotonic() + max(0.0, float(timeout_sec))
        while time.monotonic() < deadline:
            if process.poll() is not None:
                return True
            remaining = deadline - time.monotonic()
            delay = min(0.02, max(0.0, remaining))
            if self._native_stopped_event.is_set():
                time.sleep(delay)
            else:
                self._native_stopped_event.wait(delay)
        return process.poll() is not None

    @staticmethod
    def _wait_for_process_exit(
        process: subprocess.Popen[str],
        timeout_sec: float,
    ) -> bool:
        deadline = time.monotonic() + max(0.0, float(timeout_sec))
        while time.monotonic() < deadline:
            if process.poll() is not None:
                return True
            time.sleep(min(0.02, max(0.0, deadline - time.monotonic())))
        return process.poll() is not None

    def _join_reader_threads(self, timeout_sec: float) -> None:
        with self._lifecycle_lock:
            threads = list(self._reader_threads)
            self._reader_threads = []
        deadline = time.monotonic() + max(0.0, float(timeout_sec))
        all_joined = True
        still_running: List[threading.Thread] = []
        current = threading.current_thread()
        for thread in threads:
            if thread is current:
                all_joined = False
                still_running.append(thread)
                continue
            thread.join(max(0.0, deadline - time.monotonic()))
            if thread.is_alive():
                all_joined = False
                still_running.append(thread)
        with self._lifecycle_lock:
            self._reader_threads.extend(still_running)
        self.stop_telemetry["reader_threads_joined"] = all_joined

    def _force_kill_process_tree(self, process: subprocess.Popen[str]) -> None:
        if process.poll() is not None:
            return
        if os.name == "nt":
            self._force_kill_windows_process_tree(process)
        else:
            process.kill()

    def _force_kill_windows_process_tree(self, process: subprocess.Popen[str]) -> None:
        pid = getattr(process, "pid", None)
        if pid is None:
            process.kill()
            return
        try:
            completed = run_no_console(
                ["taskkill", "/PID", str(int(pid)), "/T", "/F"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=max(1.0, self.stop_timeout),
                run_factory=self._taskkill_runner,
            )
            if int(getattr(completed, "returncode", 0) or 0) != 0:
                detail = str(
                    getattr(completed, "stderr", "")
                    or getattr(completed, "stdout", "")
                    or f"taskkill failed for pid {pid}"
                ).strip()
                raise RuntimeError(detail)
        except Exception as exc:
            process.kill()
            self._write_process_log_note(f"taskkill failed for pid {pid}: {exc}")

    @staticmethod
    def _validate_port(port: int) -> int:
        value = int(port)
        if not 1 <= value <= 65535:
            raise ValueError("port must be in 1..65535")
        return value

    @staticmethod
    def _validate_host(host: str) -> str:
        value = str(host or "").strip()
        if not value:
            raise ValueError("host is required")
        return value

    @staticmethod
    def _validate_profile(profile: object) -> str:
        value = profile_id_from_info(profile, default="")
        if not value:
            raise ValueError("profile is required")
        return value

    @staticmethod
    def _validate_peer_label(peer_label: Optional[str]) -> str:
        return str(peer_label or "").strip()


class _FakeProcess:
    _next_pid = 1000

    def __init__(self, command: List[str], **_kwargs: object) -> None:
        type(self)._next_pid += 1
        self.pid = type(self)._next_pid
        self.command = list(command)
        self.returncode: Optional[int] = None
        self.stdout = None
        self.stderr = None
        self.stdin = self
        self.stdin_text = ""
        self.stdin_closed = False

    def poll(self) -> Optional[int]:
        return self.returncode

    def terminate(self) -> None:
        self.returncode = 0

    def kill(self) -> None:
        self.returncode = -9

    def wait(self, timeout: Optional[float] = None) -> int:
        del timeout
        if self.returncode is None:
            self.returncode = 0
        return self.returncode

    def write(self, text: str) -> int:
        self.stdin_text += str(text)
        return len(text)

    def flush(self) -> None:
        if "LOCAL_STOP" in self.stdin_text:
            self.returncode = 0

    def close(self) -> None:
        self.stdin_closed = True


def _run_self_test() -> Dict[str, object]:
    commands: List[List[str]] = []
    taskkill_commands: List[List[str]] = []

    with tempfile.TemporaryDirectory() as raw_dir:
        executable = Path(raw_dir) / "agoralink_media.exe"
        executable.write_bytes(b"native-runtime-self-test")

        def fake_tool_finder(name: str) -> str:
            return str(executable) if name.startswith("agoralink_media") else ""

        def fake_popen(
            command: List[str],
            **kwargs: object,
        ) -> _FakeProcess:
            del kwargs
            commands.append(list(command))
            return _FakeProcess(command)

        def fake_taskkill(
            command: List[str],
            **_kwargs: object,
        ) -> subprocess.CompletedProcess[str]:
            taskkill_commands.append(list(command))
            return subprocess.CompletedProcess(command, 0, "", "")

        runtime = ScreenRuntime(
            script_dir=Path(raw_dir),
            popen_factory=fake_popen,
            taskkill_runner=fake_taskkill,
            tool_finder=fake_tool_finder,
        )
        initial = runtime.get_state()
        receiver = runtime.start_receiver(
            port=55134,
            backend=SCREEN_BACKEND_RUST,
            native_preset="r4_default",
        )
        duplicate = runtime.start_sender("127.0.0.1")
        stopped = runtime.stop()
        sender = runtime.start_sender(
            "127.0.0.1",
            port=55134,
            backend=SCREEN_BACKEND_RUST,
            native_preset="r4_default",
        )
        sender_process = runtime._process
        if isinstance(sender_process, _FakeProcess):
            sender_process.returncode = 42
        crashed = runtime.get_state()
        missing = ScreenRuntime(
            script_dir=Path(raw_dir) / "missing",
            tool_finder=lambda _name: "",
        ).start_sender("127.0.0.1")

    checks = [
        initial["state"] == STATE_IDLE,
        receiver["state"] == STATE_RECEIVING,
        receiver["backend"] == SCREEN_BACKEND_RUST,
        receiver["command"][1] == "screen-recv",
        "--playout-delay-ms" in receiver["command"],
        duplicate["error"] == "already running",
        stopped["state"] == STATE_IDLE,
        sender["state"] == STATE_SENDING,
        sender["command"][1] == "screen-send",
        "--width" in sender["command"],
        "--adaptive-quality" in sender["command"],
        crashed["state"] == STATE_ERROR,
        crashed["returncode"] == 42,
        missing["state"] == STATE_ERROR,
        RUST_NATIVE_MISSING_MESSAGE in str(missing["last_error"]),
        not taskkill_commands,
        bool(stopped["stop_telemetry"]["graceful_stop_requested"]),
        not bool(stopped["stop_telemetry"]["forced_kill_used"]),
    ]
    return {
        "ok": all(checks),
        "checks": checks,
        "commands": commands,
        "backend": SCREEN_BACKEND_RUST,
    }


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="AgoraLink native screen runtime process manager."
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="Run a native command/state-machine smoke test.",
    )
    return parser


def main() -> int:
    args = build_argparser().parse_args()
    if not args.self_test:
        build_argparser().print_help()
        return 0
    result = _run_self_test()
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
