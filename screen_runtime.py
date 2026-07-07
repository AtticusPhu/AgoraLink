#!/usr/bin/env python3
"""AgoraLink screen sharing runtime process manager.

This module only starts and stops FFmpeg/FFplay screen streaming processes. It
does not send chat messages, touch GUI code, or modify protocol/database
behavior.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Callable, Dict, List, Mapping, Optional, TextIO

from app_paths import debug_log_dir
from process_utils import popen_ffplay_windowed, popen_no_console, run_no_console
from screen_control import DEFAULT_SCREEN_PORT
from screen_profile import PROFILES_BY_NAME, ScreenProfile, profile_id_from_info


STATE_IDLE = "idle"
STATE_RECEIVING = "receiving"
STATE_SENDING = "sending"
STATE_STOPPING = "stopping"
STATE_ERROR = "error"

DEFAULT_SCREEN_PROFILE = "720p30_h264_qsv"
SCREEN_BACKEND_FFMPEG = "ffmpeg"
SCREEN_BACKEND_RUST = "rust"
SCREEN_BACKENDS = {SCREEN_BACKEND_FFMPEG, SCREEN_BACKEND_RUST}
FFMPEG_INSTALL_HINT = "winget install --id Gyan.FFmpeg -e"
FFMPEG_MISSING_MESSAGE = (
    "找不到 ffmpeg/ffplay。请安装 FFmpeg 或使用内置 tools/ffmpeg/bin。\n"
    f"安装命令：{FFMPEG_INSTALL_HINT}"
)
RUST_NATIVE_MISSING_MESSAGE = "Rust native media executable not found"
RUST_NATIVE_VIDEO_ONLY_MESSAGE = "Rust native backend currently supports video only"
NATIVE_LITE_FLAVOR = "native_lite"
FULL_PACKAGE_FLAVOR = "full"
SOURCE_PACKAGE_FLAVOR = "source"
NATIVE_LITE_FFMPEG_UNAVAILABLE_MESSAGE = (
    "FFmpeg backend is unavailable in Native Lite package. "
    "Switched to Rust native video backend."
)
NATIVE_LITE_VIDEO_ONLY_MESSAGE = (
    "Native Lite currently supports video-only screen sharing. "
    "System audio requires the Full package with FFmpeg backend."
)

class ScreenRuntime:
    def __init__(
        self,
        *,
        script_dir: Optional[Path] = None,
        popen_factory: Callable[..., subprocess.Popen[bytes]] = subprocess.Popen,
        taskkill_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
        tool_finder: Optional[Callable[[str], str]] = None,
        stop_timeout: float = 5.0,
    ) -> None:
        self.script_dir = (Path(script_dir) if script_dir is not None else Path(__file__).resolve().parent).resolve()
        self._popen_factory = popen_factory
        self._taskkill_runner = taskkill_runner
        self._tool_finder = tool_finder
        self.stop_timeout = float(stop_timeout)
        self._process: Optional[subprocess.Popen[bytes]] = None
        self._process_log_file = None
        self._state = STATE_IDLE
        self.last_error = ""
        self.last_returncode: Optional[int] = None
        self.last_command: List[str] = []
        self.current_backend = SCREEN_BACKEND_FFMPEG
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
        self.native_stats: Dict[str, object] = {}
        self.native_last_event: Dict[str, object] = {}
        self._process_log_path: Optional[Path] = None
        self._wasapi_support_cache: Dict[str, bool] = {}
        self._dshow_audio_devices_cache: Dict[str, List[Dict[str, str]]] = {}

    def start_receiver(
        self,
        port: int = DEFAULT_SCREEN_PORT,
        profile: object = "",
        *,
        peer_label: Optional[str] = None,
        selected_profile: object = None,
        screen_port: Optional[int] = None,
        audio: object = None,
        backend: object = SCREEN_BACKEND_FFMPEG,
    ) -> Dict[str, object]:
        if self._has_running_process():
            return self._already_running_result()
        try:
            backend_name = self._normalize_backend(backend)
            self.current_backend = backend_name
            if screen_port is not None:
                port = int(screen_port)
            if selected_profile:
                profile = selected_profile
            port = self._validate_port(port)
            profile_name = self._validate_profile(profile) if profile else ""
            peer_label_text = self._validate_peer_label(peer_label)
            audio_config = self._normalize_audio_config(audio, enabled_default=False)
            self.last_command = []
            self.native_stats = {}
            self.native_last_event = {}
            if backend_name == SCREEN_BACKEND_RUST:
                native_exe = self._find_native_media_exe()
                if not native_exe:
                    return self._set_error(RUST_NATIVE_MISSING_MESSAGE)
                cmd = self._build_native_receiver_command(port, native_exe=native_exe)
                self.last_command = list(cmd)
                self._process = self._start_native_process(cmd, "agoralink_media")
            else:
                deps = self.check_dependencies()
                ffplay = str(deps.get("ffplay_path") or "")
                if not ffplay:
                    return self._set_error(self._missing_tool_error(["ffplay"]))
                cmd = self._build_receiver_command(port, ffplay_path=ffplay, peer_label=peer_label_text)
                self.last_command = list(cmd)
                self._process = self._start_process_no_console(cmd, "ffplay")
        except Exception as exc:
            return self._set_error(str(exc))

        self._state = STATE_RECEIVING
        self.last_error = ""
        self.last_returncode = None
        self.current_backend = backend_name
        self.current_mode = STATE_RECEIVING
        self.current_host = None
        self.current_port = port
        self.current_profile = profile_name or None
        self.current_peer_label = peer_label_text or None
        self._set_audio_session(audio_config)
        self._schedule_startup_exit_check("agoralink_media" if backend_name == SCREEN_BACKEND_RUST else "ffplay", delay_sec=1.0)
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
        backend: object = SCREEN_BACKEND_FFMPEG,
    ) -> Dict[str, object]:
        if self._has_running_process():
            return self._already_running_result()
        try:
            backend_name = self._normalize_backend(backend)
            self.current_backend = backend_name
            if screen_port is not None:
                port = int(screen_port)
            if selected_profile:
                profile = selected_profile
            host = self._validate_host(host)
            port = self._validate_port(port)
            profile = self._validate_profile(profile)
            peer_label_text = self._validate_peer_label(peer_label) or host
            audio_config = self._normalize_audio_config(audio, enabled_default=bool(system_audio))
            self.last_command = []
            self.native_stats = {}
            self.native_last_event = {}
            audio_requested = bool(audio_config.get("enabled"))
            fallback_cmd = []
            audio_enabled = False
            audio_notice = ""
            if backend_name == SCREEN_BACKEND_RUST:
                native_exe = self._find_native_media_exe()
                if not native_exe:
                    return self._set_error(RUST_NATIVE_MISSING_MESSAGE)
                if audio_requested:
                    audio_notice = self.native_video_only_message()
                    audio_config = {
                        "enabled": False,
                        "mode": "none",
                        "state": "video_only",
                        "error": audio_notice,
                    }
                cmd = self._build_native_sender_command(host=host, port=port, native_exe=native_exe)
                self.last_command = list(cmd)
                self._process = self._start_native_process(cmd, "agoralink_media")
            else:
                deps = self.check_dependencies()
                ffmpeg = str(deps.get("ffmpeg_path") or "")
                if not ffmpeg:
                    return self._set_error(self._missing_tool_error(["ffmpeg"]))
                if audio_requested:
                    audio_config = self._resolve_system_audio_config(ffmpeg, audio_config)
                audio_enabled = bool(audio_config.get("enabled"))
                audio_notice = "System audio unavailable, continued video-only." if audio_requested and not audio_enabled else ""
                cmd = self._build_sender_command(
                    host=host,
                    port=port,
                    profile_name=profile,
                    ffmpeg_path=ffmpeg,
                    system_audio=audio_enabled,
                    audio=audio_config,
                )
                self.last_command = list(cmd)
                self._process = self._start_process_no_console(cmd, "ffmpeg")
                if audio_enabled:
                    fallback_cmd = self._build_sender_command(
                        host=host,
                        port=port,
                        profile_name=profile,
                        ffmpeg_path=ffmpeg,
                        system_audio=False,
                        audio={"enabled": False, "mode": "none"},
                    )
        except Exception as exc:
            return self._set_error(str(exc))

        self._state = STATE_SENDING
        self.last_error = audio_notice
        self.last_returncode = None
        self.current_backend = backend_name
        self.current_mode = STATE_SENDING
        self.current_host = host
        self.current_port = port
        self.current_profile = profile
        self.current_peer_label = peer_label_text or None
        self._set_audio_session(audio_config)
        if backend_name == SCREEN_BACKEND_FFMPEG and audio_requested:
            self._write_process_log_note(self._screen_audio_launch_note(audio_config))
        if backend_name == SCREEN_BACKEND_FFMPEG and audio_enabled:
            self._schedule_audio_fallback_check(self._process, fallback_cmd, delay_sec=1.0)
            self._schedule_audio_fallback_check(self._process, fallback_cmd, delay_sec=2.5)
        return self.get_state()

    def stop(self) -> Dict[str, object]:
        if not self._has_running_process():
            self._process = None
            self._close_process_log_file()
            self._state = STATE_IDLE
            self.last_error = ""
            self._clear_current_session()
            return self.get_state()

        self._state = STATE_STOPPING
        proc = self._process
        try:
            if os.name == "nt":
                self._stop_windows_process_tree(proc)
            else:
                self._stop_portable_process(proc)
        except Exception as exc:
            return self._set_error(str(exc))
        finally:
            self._process = None
            self._close_process_log_file()

        self._state = STATE_IDLE
        self.last_error = ""
        self._clear_current_session()
        return self.get_state()

    def is_running(self) -> bool:
        return self._has_running_process()

    def get_state(self) -> Dict[str, object]:
        self._refresh_process()
        return self._snapshot()

    def _debug_log_dir(self) -> Path:
        return debug_log_dir()

    def _open_process_log_file(self, tool_name: str):
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

    def _start_process_no_console(self, cmd: List[str], tool_name: str) -> subprocess.Popen[bytes]:
        self._close_process_log_file()
        log_file = self._open_process_log_file(tool_name)
        try:
            kwargs = {
                "cwd": str(self.script_dir),
                "stdin": subprocess.PIPE,
                "stdout": log_file if log_file is not None else subprocess.DEVNULL,
                "stderr": subprocess.STDOUT,
                "popen_factory": self._popen_factory,
            }
            if str(tool_name or "").lower() == "ffplay":
                proc = popen_ffplay_windowed(cmd, **kwargs)
            else:
                proc = popen_no_console(cmd, **kwargs)
        except Exception:
            if log_file is not None:
                try:
                    log_file.close()
                except Exception:
                    pass
            raise
        self._process_log_file = log_file
        return proc

    def _start_native_process(self, cmd: List[str], tool_name: str) -> subprocess.Popen[str]:
        self._close_process_log_file()
        log_file = self._open_process_log_file(tool_name)
        try:
            proc = self._popen_factory(
                cmd,
                cwd=str(self.script_dir),
                stdin=subprocess.DEVNULL,
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
        self._start_native_output_threads(proc, log_file)
        return proc

    def _start_native_output_threads(self, proc: subprocess.Popen[str], log_file: Optional[TextIO]) -> None:
        stdout = getattr(proc, "stdout", None)
        stderr = getattr(proc, "stderr", None)
        if stdout is not None:
            thread = threading.Thread(target=self._read_native_stdout, args=(proc, stdout), daemon=True)
            thread.start()
        if stderr is not None:
            thread = threading.Thread(target=self._read_native_stderr, args=(stderr, log_file), daemon=True)
            thread.start()

    def _read_native_stdout(self, proc: subprocess.Popen[str], stream: TextIO) -> None:
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
                    self._handle_native_event(dict(event), proc)
        except Exception as exc:
            self._write_process_log_note(f"native stdout reader failed: {exc}")

    def _read_native_stderr(self, stream: TextIO, log_file: Optional[TextIO]) -> None:
        try:
            for line in iter(stream.readline, ""):
                text = str(line or "")
                if not text:
                    continue
                if log_file is not None:
                    try:
                        log_file.write(text)
                        if not text.endswith("\n"):
                            log_file.write("\n")
                        log_file.flush()
                    except Exception:
                        pass
        except Exception as exc:
            self._write_process_log_note(f"native stderr reader failed: {exc}")

    def _handle_native_event(self, event: Dict[str, object], proc: Optional[subprocess.Popen[str]] = None) -> None:
        event_type = str(event.get("type") or "").strip()
        self.native_last_event = dict(event)
        if event_type == "NATIVE_SCREEN_STATS":
            self.native_stats = dict(event)
            return
        if event_type == "NATIVE_SCREEN_STARTED":
            if not self.last_error:
                self.last_error = ""
            return
        if event_type == "NATIVE_SCREEN_STOPPED":
            self.native_stats = dict(event)
            if proc is None or proc is self._process:
                self.last_error = ""
            return
        if event_type == "NATIVE_SCREEN_ERROR":
            message = str(event.get("error") or event.get("message") or "native screen error").strip()
            self.last_error = message or "native screen error"
            self._state = STATE_ERROR

    def _schedule_startup_exit_check(self, tool_name: str, delay_sec: float = 1.0) -> None:
        proc = self._process
        if proc is None:
            return
        timer = threading.Timer(max(0.0, float(delay_sec or 0.0)), lambda: self._handle_startup_exit_check(proc, tool_name))
        timer.daemon = True
        timer.start()

    def _schedule_audio_fallback_check(self, proc: Optional[subprocess.Popen[bytes]], fallback_cmd: List[str], delay_sec: float = 1.0) -> None:
        if proc is None or not fallback_cmd:
            return
        timer = threading.Timer(
            max(0.0, float(delay_sec or 0.0)),
            lambda: self._handle_audio_fallback_check(proc, list(fallback_cmd)),
        )
        timer.daemon = True
        timer.start()

    def _handle_audio_fallback_check(self, proc: subprocess.Popen[bytes], fallback_cmd: List[str]) -> None:
        if proc is None or proc is not self._process:
            return
        returncode = proc.poll()
        if returncode is None:
            return
        tail = self._read_process_log_tail("ffmpeg")
        self.last_returncode = int(returncode)
        self._process = None
        self._close_process_log_file()
        self.current_audio_enabled = False
        self.current_audio_mode = "none"
        self.current_audio_state = "fallback_video_only"
        self.current_audio_config = {"enabled": False, "mode": "none", "state": "fallback_video_only"}
        self.current_audio_error = f"system audio ffmpeg exited with code {returncode}"
        self.current_audio_input = ""
        message = "System audio unavailable, continued video-only."
        if tail:
            message = message + "\n" + tail
        self.last_error = message
        try:
            self.last_command = list(fallback_cmd)
            self._process = self._start_process_no_console(fallback_cmd, "ffmpeg")
            self._state = STATE_SENDING
            self._schedule_startup_exit_check("ffmpeg", delay_sec=1.0)
        except Exception as exc:
            self._state = STATE_ERROR
            self.current_audio_state = "audio_failed"
            self.current_audio_error = str(exc)
            self.last_error = f"System audio failed and video-only retry failed: {exc}"

    def _handle_startup_exit_check(self, proc: subprocess.Popen[bytes], tool_name: str) -> None:
        if proc is None or proc is not self._process:
            return
        returncode = proc.poll()
        if returncode is None:
            return
        self.last_returncode = int(returncode)
        self._process = None
        self._close_process_log_file()
        mode = self.current_mode or str(tool_name or "screen")
        tail = self._read_process_log_tail(tool_name)
        self._state = STATE_ERROR
        self.last_error = f"{mode} process exited with code {returncode}"
        if tail:
            self.last_error = self.last_error + "\n" + tail

    def _read_process_log_tail(self, tool_name: str = "", max_chars: int = 4000) -> str:
        path = self._process_log_path
        if path is None:
            candidate = self._debug_log_dir() / f"screen_{str(tool_name or 'process')}.log"
            path = candidate if candidate.exists() else None
        if path is None:
            return ""
        try:
            max_bytes = max(512, int(max_chars or 4000) * 4)
            size = path.stat().st_size
            with path.open("rb") as f:
                if size > max_bytes:
                    f.seek(-max_bytes, os.SEEK_END)
                raw = f.read(max_bytes)
            return raw.decode("utf-8", errors="replace")[-int(max_chars or 4000):].strip()
        except Exception:
            return ""

    def _has_running_process(self) -> bool:
        self._refresh_process()
        return self._process is not None and self._process.poll() is None

    def _refresh_process(self) -> None:
        if self._process is None:
            return
        returncode = self._process.poll()
        if returncode is None:
            return
        self.last_returncode = int(returncode)
        self._process = None
        self._close_process_log_file()
        if self._state == STATE_STOPPING or returncode == 0:
            self._state = STATE_IDLE
            self.last_error = ""
            self._clear_current_session()
        else:
            mode = self.current_mode or "screen"
            self._state = STATE_ERROR
            self.last_error = f"{mode} process exited with code {returncode}"

    def _snapshot(self, *, state: Optional[str] = None, ok: Optional[bool] = None) -> Dict[str, object]:
        running = self._process is not None and self._process.poll() is None
        actual_state = state or self._state
        return {
            "ok": bool(running or actual_state == STATE_IDLE) if ok is None else bool(ok),
            "state": actual_state,
            "running": running,
            "backend": self.current_backend,
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
            "pid": int(self._process.pid) if running and getattr(self._process, "pid", None) is not None else None,
            "returncode": self.last_returncode,
            "last_error": self.last_error,
            "command": list(self.last_command),
            "native_stats": dict(self.native_stats),
            "native_last_event": dict(self.native_last_event),
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

    def check_dependencies(self) -> Dict[str, object]:
        ffmpeg = self._find_media_tool("ffmpeg")
        ffplay = self._find_media_tool("ffplay")
        native = self._find_native_media_exe()
        package_info = self.screen_package_info()
        ffmpeg_ok = bool(ffmpeg)
        ffplay_ok = bool(ffplay)
        native_ok = bool(native)
        missing = []
        if not ffmpeg_ok:
            missing.append("ffmpeg")
        if not ffplay_ok:
            missing.append("ffplay")
        return {
            "ok": bool(ffmpeg_ok and ffplay_ok),
            "ffmpeg_ok": ffmpeg_ok,
            "ffplay_ok": ffplay_ok,
            "rust_native_ok": native_ok,
            "native_media_ok": native_ok,
            "ffmpeg_path": str(ffmpeg or ""),
            "ffplay_path": str(ffplay or ""),
            "rust_native_path": str(native or ""),
            "native_media_path": str(native or ""),
            "package_flavor": package_info["package_flavor"],
            "native_lite": package_info["native_lite"],
            "bundled_ffmpeg_available": package_info["bundled_ffmpeg_available"],
            "screen_backend_default": package_info["screen_backend_default"],
            "native_screen_video_only": package_info["native_screen_video_only"],
            "error": "" if not missing else self._missing_tool_error(missing),
            "rust_error": "" if native_ok else RUST_NATIVE_MISSING_MESSAGE,
            "install_hint": FFMPEG_INSTALL_HINT,
        }

    def screen_package_info(self) -> Dict[str, object]:
        native = self._find_native_media_exe()
        bundled_ffmpeg = self._find_bundled_media_tool("ffmpeg")
        bundled_ffplay = self._find_bundled_media_tool("ffplay")
        bundled_ffmpeg_available = bool(bundled_ffmpeg and bundled_ffplay)
        if bool(getattr(sys, "frozen", False)):
            package_flavor = NATIVE_LITE_FLAVOR if not bundled_ffmpeg_available else FULL_PACKAGE_FLAVOR
        else:
            package_flavor = SOURCE_PACKAGE_FLAVOR
        return {
            "package_flavor": package_flavor,
            "native_lite": package_flavor == NATIVE_LITE_FLAVOR,
            "rust_native_available": bool(native),
            "native_media_ok": bool(native),
            "rust_native_path": str(native or ""),
            "bundled_ffmpeg_available": bundled_ffmpeg_available,
            "bundled_ffmpeg_path": str(bundled_ffmpeg or ""),
            "bundled_ffplay_path": str(bundled_ffplay or ""),
            "screen_backend_default": SCREEN_BACKEND_RUST if package_flavor == NATIVE_LITE_FLAVOR else SCREEN_BACKEND_FFMPEG,
            "native_screen_video_only": True,
        }

    def native_video_only_message(self) -> str:
        if bool(self.screen_package_info().get("native_lite")):
            return NATIVE_LITE_VIDEO_ONLY_MESSAGE
        return RUST_NATIVE_VIDEO_ONLY_MESSAGE

    def _missing_tool_error(self, missing: List[str]) -> str:
        names = ", ".join(str(name or "").strip() for name in missing if str(name or "").strip())
        if names:
            return f"{FFMPEG_MISSING_MESSAGE}\n缺少：{names}"
        return FFMPEG_MISSING_MESSAGE

    def _build_native_receiver_command(self, port: int, native_exe: Optional[str] = None) -> List[str]:
        exe = str(native_exe or self._find_native_media_exe() or "")
        if not exe:
            raise FileNotFoundError(RUST_NATIVE_MISSING_MESSAGE)
        return [
            exe,
            "screen-recv",
            "--bind",
            "0.0.0.0",
            "--port",
            str(int(port)),
            "--title",
            "AgoraLink Native Viewer",
        ]

    def _build_native_sender_command(self, *, host: str, port: int, native_exe: Optional[str] = None) -> List[str]:
        exe = str(native_exe or self._find_native_media_exe() or "")
        if not exe:
            raise FileNotFoundError(RUST_NATIVE_MISSING_MESSAGE)
        return [
            exe,
            "screen-send",
            "--host",
            str(host),
            "--port",
            str(int(port)),
            "--width",
            "1280",
            "--height",
            "720",
            "--fps",
            "30",
            "--bitrate-mbps",
            "4",
        ]

    def _build_receiver_command(self, port: int, ffplay_path: Optional[str] = None, peer_label: str = "") -> List[str]:
        ffplay = str(ffplay_path or self._find_media_tool("ffplay") or "")
        if not ffplay:
            raise FileNotFoundError(self._missing_tool_error(["ffplay"]))
        window_title = self._receiver_window_title(peer_label)
        return [
            ffplay,
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            "-framedrop",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-window_title",
            window_title,
            f"udp://0.0.0.0:{int(port)}?fifo_size=1000000&overrun_nonfatal=1",
        ]

    def _build_sender_command(
        self,
        *,
        host: str,
        port: int,
        profile_name: str,
        ffmpeg_path: Optional[str] = None,
        system_audio: bool = False,
        audio: object = None,
    ) -> List[str]:
        ffmpeg = str(ffmpeg_path or self._find_media_tool("ffmpeg") or "")
        if not ffmpeg:
            raise FileNotFoundError(self._missing_tool_error(["ffmpeg"]))
        profile = self._profile_for_name(profile_name)
        audio_config = self._normalize_audio_config(audio, enabled_default=bool(system_audio))
        cmd = [
            ffmpeg,
            "-hide_banner",
            "-fflags",
            "+genpts",
            "-f",
            "gdigrab",
            "-framerate",
            str(profile.fps),
            "-i",
            "desktop",
        ]
        if audio_config.get("enabled"):
            audio_backend = str(audio_config.get("backend") or "wasapi").strip().lower()
            if audio_backend == "dshow":
                input_name = str(
                    audio_config.get("input_name")
                    or audio_config.get("alternative_name")
                    or audio_config.get("device_name")
                    or ""
                ).strip()
                if not input_name:
                    raise ValueError("dshow system audio input is missing")
                cmd.extend(
                    [
                        "-thread_queue_size",
                        "512",
                        "-f",
                        "dshow",
                        "-i",
                        f"audio={input_name}",
                    ]
                )
            else:
                cmd.extend(
                    [
                        "-thread_queue_size",
                        "512",
                        "-f",
                        "wasapi",
                        "-loopback",
                        "1",
                        "-i",
                        "default",
                    ]
                )
        cmd.extend(
            [
            "-vf",
            f"scale={profile.width}:{profile.height},format=nv12",
            "-c:v",
            profile.encoder,
            "-b:v",
            profile.bitrate,
            "-maxrate",
            profile.maxrate,
            "-bufsize",
            profile.bufsize,
            "-g",
            str(profile.fps),
            "-bf",
            "0",
            "-fps_mode",
            "cfr",
            ]
        )
        if audio_config.get("enabled"):
            cmd.extend(
                [
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:a",
                    "aac",
                    "-ar",
                    str(int(audio_config.get("sample_rate") or 48000)),
                    "-ac",
                    str(int(audio_config.get("channels") or 2)),
                    "-b:a",
                    self._audio_bitrate_arg(audio_config.get("bitrate")),
                ]
            )
        cmd.extend(
            [
            "-f",
            "mpegts",
            f"udp://{host}:{int(port)}?pkt_size=1316",
            ]
        )
        return cmd

    def _normalize_audio_config(self, audio: object = None, *, enabled_default: bool = False) -> Dict[str, object]:
        if isinstance(audio, Mapping):
            enabled = bool(audio.get("enabled", enabled_default))
            mode = str(audio.get("mode") or ("system" if enabled else "none")).strip().lower()
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
            for key in ("backend", "input_name", "device_name", "alternative_name", "state"):
                value = str(audio.get(key) or "").strip()
                if value:
                    result[key] = value
        return result

    def _set_audio_session(self, audio_config: Mapping[str, Any]) -> None:
        enabled = bool((audio_config or {}).get("enabled"))
        self.current_audio_enabled = enabled
        self.current_audio_mode = "system" if enabled else "none"
        self.current_audio_state = str((audio_config or {}).get("state") or ("system_audio_on" if enabled else "video_only"))
        self.current_audio_config = dict(audio_config or {"enabled": False, "mode": "none"})
        self.current_audio_error = str((audio_config or {}).get("error") or "")
        self.current_audio_input = str(
            (audio_config or {}).get("input_name")
            or (audio_config or {}).get("alternative_name")
            or (audio_config or {}).get("device_name")
            or ""
        )

    def _audio_bitrate_arg(self, bitrate: object) -> str:
        try:
            value = int(bitrate or 128000)
        except Exception:
            value = 128000
        if value % 1000 == 0:
            return f"{max(1, value // 1000)}k"
        return str(max(1, value))

    def _resolve_system_audio_config(self, ffmpeg_path: str, audio_config: Mapping[str, Any]) -> Dict[str, object]:
        base = dict(audio_config or {})
        if not bool(base.get("enabled")):
            return {"enabled": False, "mode": "none"}
        explicit_backend = str(base.get("backend") or "").strip().lower()
        if explicit_backend:
            return base
        if os.name == "nt" and self._ffmpeg_supports_wasapi(ffmpeg_path):
            base.update({"backend": "wasapi", "input_name": "default"})
            return base
        if os.name == "nt":
            device = self._find_dshow_system_mix_device(ffmpeg_path)
            if device:
                input_name = str(device.get("alternative_name") or device.get("name") or "").strip()
                base.update(
                    {
                        "backend": "dshow",
                        "input_name": input_name,
                        "device_name": str(device.get("name") or ""),
                        "alternative_name": str(device.get("alternative_name") or ""),
                    }
                )
                return base
        return {
            "enabled": False,
            "mode": "none",
            "state": "fallback_video_only",
            "error": "system audio unavailable",
        }

    def _ffmpeg_supports_wasapi(self, ffmpeg_path: str) -> bool:
        key = str(ffmpeg_path or "")
        if key in self._wasapi_support_cache:
            return bool(self._wasapi_support_cache[key])
        output = self._run_ffmpeg_probe([key, "-hide_banner", "-devices"], timeout_sec=2.5)
        supported = bool(re.search(r"(^|\s)wasapi(\s|$)", output, flags=re.IGNORECASE | re.MULTILINE))
        self._wasapi_support_cache[key] = supported
        return supported

    def _find_dshow_system_mix_device(self, ffmpeg_path: str) -> Dict[str, str]:
        devices = self._dshow_audio_devices(ffmpeg_path)
        return self._select_dshow_system_mix_device(devices)

    def _dshow_audio_devices(self, ffmpeg_path: str) -> List[Dict[str, str]]:
        key = str(ffmpeg_path or "")
        if key in self._dshow_audio_devices_cache:
            return [dict(item) for item in self._dshow_audio_devices_cache[key]]
        output = self._run_ffmpeg_probe(
            [key, "-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
            timeout_sec=4.0,
        )
        devices = self._parse_dshow_audio_devices(output)
        self._dshow_audio_devices_cache[key] = [dict(item) for item in devices]
        return devices

    def _run_ffmpeg_probe(self, args: List[str], timeout_sec: float = 3.0) -> str:
        if not args or not str(args[0] or "").strip():
            return ""
        try:
            result = run_no_console(
                args,
                cwd=str(self.script_dir),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=max(1.0, float(timeout_sec or 3.0)),
            )
            return (str(result.stdout or "") + "\n" + str(result.stderr or "")).strip()
        except Exception as exc:
            return str(exc)

    def _parse_dshow_audio_devices(self, text: str) -> List[Dict[str, str]]:
        devices: List[Dict[str, str]] = []
        in_audio_section = False
        current: Optional[Dict[str, str]] = None
        for raw_line in str(text or "").splitlines():
            line = raw_line.strip()
            lower = line.lower()
            if "directshow audio devices" in lower:
                in_audio_section = True
                current = None
                continue
            if "directshow video devices" in lower:
                in_audio_section = False
                current = None
                continue
            alt_match = re.search(r'Alternative name\s+"([^"]+)"', line, flags=re.IGNORECASE)
            if alt_match and current is not None:
                current["alternative_name"] = alt_match.group(1).strip()
                continue
            if "alternative name" in lower:
                continue
            explicit_audio = "(audio)" in lower
            if not in_audio_section and not explicit_audio:
                continue
            name_match = re.search(r'"([^"]+)"(?:\s+\(audio\))?\s*$', line)
            if name_match:
                current = {"name": name_match.group(1).strip(), "alternative_name": "", "raw": raw_line}
                devices.append(current)
        return devices

    def _select_dshow_system_mix_device(self, devices: List[Dict[str, str]]) -> Dict[str, str]:
        for item in devices or []:
            name = str((item or {}).get("name") or "")
            alternative = str((item or {}).get("alternative_name") or "")
            haystack = f"{name}\n{alternative}"
            if self._is_excluded_audio_capture_name(haystack):
                continue
            if self._is_system_mix_audio_name(haystack):
                return dict(item)
        return {}

    def _is_system_mix_audio_name(self, text: str) -> bool:
        lower = str(text or "").lower()
        keywords = (
            "stereo mix",
            "立体声混音",
            "立體聲混音",
            "what u hear",
            "wave out mix",
            "wave mix",
            "绔嬩綋澹版贩闊",
        )
        return any(keyword.lower() in lower for keyword in keywords)

    def _is_excluded_audio_capture_name(self, text: str) -> bool:
        lower = str(text or "").lower()
        blocked = ("microphone", "麦克风", "麥克風", "麦克风阵列", "插孔麦克风")
        if any(item.lower() in lower for item in blocked):
            return True
        return bool(re.search(r"(^|[\s(_-])mic($|[\s)_-])", lower))

    def _screen_audio_launch_note(self, audio_config: Mapping[str, Any]) -> str:
        if not bool((audio_config or {}).get("enabled")):
            return "system audio unavailable; using video-only screen stream"
        backend = str((audio_config or {}).get("backend") or "wasapi")
        input_name = str((audio_config or {}).get("input_name") or "default")
        if backend == "dshow":
            return f'system audio input selected: -f dshow -i audio="{input_name}"'
        return f"system audio input selected: -f wasapi -loopback 1 -i {input_name}"

    def _profile_for_name(self, profile_name: object) -> ScreenProfile:
        normalized = profile_id_from_info(profile_name)
        profile = PROFILES_BY_NAME.get(normalized)
        if profile is None:
            available = ", ".join(sorted(PROFILES_BY_NAME))
            raise ValueError(f"unknown screen profile {profile_name!r}; available: {available}")
        return profile

    def _find_media_tool(self, name: str) -> str:
        if self._tool_finder is not None:
            return str(self._tool_finder(name) or "")
        exe_names = self._tool_executable_names(name)

        for base in self._env_ffmpeg_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found

        for base in self._pyinstaller_internal_ffmpeg_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found

        for base in self._pyinstaller_meipass_ffmpeg_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found

        for base in self._exe_sibling_ffmpeg_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found

        for base in self._source_ffmpeg_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found

        for exe in exe_names:
            found = shutil.which(exe)
            if found:
                return str(Path(found).resolve())

        for base in self._winget_ffmpeg_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found
        return ""

    def _find_bundled_media_tool(self, name: str) -> str:
        exe_names = self._tool_executable_names(name)
        dirs: List[Path] = []
        if bool(getattr(sys, "frozen", False)):
            dirs.extend(self._pyinstaller_internal_ffmpeg_dirs())
            dirs.extend(self._pyinstaller_meipass_ffmpeg_dirs())
            dirs.extend(self._exe_sibling_ffmpeg_dirs())
        else:
            dirs.extend(self._source_ffmpeg_dirs())
        for base in dirs:
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found
        return ""

    def _find_native_media_exe(self) -> str:
        if self._tool_finder is not None:
            found = str(self._tool_finder("agoralink_media") or self._tool_finder("agoralink_media.exe") or "")
            if found:
                return found
        exe_names = self._tool_executable_names("agoralink_media")
        for base in self._native_media_dirs():
            found = self._find_tool_in_dir(base, exe_names)
            if found:
                return found
        for exe in exe_names:
            found = shutil.which(exe)
            if found:
                return str(Path(found).resolve())
        return ""

    def _native_media_dirs(self) -> List[Path]:
        dirs: List[Path] = []
        exe_dir = Path(sys.executable).resolve().parent
        dirs.append(exe_dir / "_internal" / "tools" / "agoralink_media")
        meipass = str(getattr(sys, "_MEIPASS", "") or "").strip()
        if meipass:
            dirs.append(Path(meipass) / "tools" / "agoralink_media")
        dirs.append(exe_dir / "tools" / "agoralink_media")
        dirs.append(self.script_dir / "tools" / "agoralink_media")
        dirs.append(self.script_dir / "rust-native" / "agoralink_media" / "target" / "release")
        return dirs

    @staticmethod
    def _tool_executable_names(name: str) -> List[str]:
        base = str(name or "").strip()
        if not base:
            return []
        stem = base[:-4] if base.lower().endswith(".exe") else base
        names = [stem + ".exe", stem] if os.name == "nt" else [stem, stem + ".exe"]
        result = []
        for item in names:
            if item and item not in result:
                result.append(item)
        return result

    def _env_ffmpeg_dirs(self) -> List[Path]:
        raw = str(os.environ.get("AGORALINK_FFMPEG_DIR") or "").strip()
        if not raw:
            return []
        return [Path(raw), Path(raw) / "bin"]

    def _source_ffmpeg_dirs(self) -> List[Path]:
        return [self.script_dir / "tools" / "ffmpeg" / "bin"]

    def _pyinstaller_meipass_ffmpeg_dirs(self) -> List[Path]:
        meipass = str(getattr(sys, "_MEIPASS", "") or "").strip()
        if not meipass:
            return []
        return [Path(meipass) / "tools" / "ffmpeg" / "bin"]

    def _pyinstaller_internal_ffmpeg_dirs(self) -> List[Path]:
        exe_dir = Path(sys.executable).resolve().parent
        return [exe_dir / "_internal" / "tools" / "ffmpeg" / "bin"]

    def _exe_sibling_ffmpeg_dirs(self) -> List[Path]:
        exe_dir = Path(sys.executable).resolve().parent
        return [exe_dir / "tools" / "ffmpeg" / "bin"]

    def _winget_ffmpeg_dirs(self) -> List[Path]:
        if os.name != "nt":
            return []
        dirs: List[Path] = []
        local_appdata = os.environ.get("LOCALAPPDATA")
        if local_appdata:
            dirs.append(Path(local_appdata) / "Microsoft" / "WinGet" / "Links")
            packages = Path(local_appdata) / "Microsoft" / "WinGet" / "Packages"
            try:
                for pkg in packages.glob("Gyan.FFmpeg*"):
                    dirs.append(pkg)
                    dirs.extend(pkg.glob("ffmpeg*\\bin"))
                    dirs.extend(pkg.glob("*\\bin"))
            except Exception:
                pass
        program_files = os.environ.get("ProgramFiles")
        if program_files:
            dirs.append(Path(program_files) / "ffmpeg" / "bin")
        return dirs

    @staticmethod
    def _find_tool_in_dir(base: Path, exe_names: List[str]) -> str:
        try:
            base_path = Path(base)
            if base_path.exists() and base_path.is_file() and base_path.name in exe_names:
                return str(base_path.resolve())
            for exe in exe_names:
                direct = base_path / exe
                if direct.exists() and direct.is_file():
                    return str(direct.resolve())
        except Exception:
            pass
        return ""

    @staticmethod
    def _normalize_backend(backend: object) -> str:
        value = str(backend or SCREEN_BACKEND_FFMPEG).strip().lower()
        if not value:
            return SCREEN_BACKEND_FFMPEG
        if value not in SCREEN_BACKENDS:
            raise ValueError("screen backend must be ffmpeg or rust")
        return value

    def _stop_windows_process_tree(self, proc: subprocess.Popen[bytes]) -> None:
        pid = getattr(proc, "pid", None)
        if pid is None:
            self._stop_portable_process(proc)
            return
        try:
            completed = run_no_console(
                ["taskkill", "/PID", str(int(pid)), "/T", "/F"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=max(5.0, self.stop_timeout),
                run_factory=self._taskkill_runner,
            )
            if int(getattr(completed, "returncode", 0) or 0) != 0:
                stderr = str(getattr(completed, "stderr", "") or "").strip()
                stdout = str(getattr(completed, "stdout", "") or "").strip()
                self.last_error = stderr or stdout or f"taskkill failed for pid {pid}"
                self._stop_portable_process(proc)
                return
        except Exception as exc:
            self.last_error = f"taskkill failed for pid {pid}: {exc}"
            self._stop_portable_process(proc)
            return

        try:
            self.last_returncode = int(proc.wait(timeout=max(1.0, self.stop_timeout)))
        except subprocess.TimeoutExpired:
            try:
                proc.kill()
                self.last_returncode = int(proc.wait(timeout=max(1.0, self.stop_timeout)))
            except Exception:
                polled = proc.poll()
                self.last_returncode = int(polled) if polled is not None else -9

    def _stop_portable_process(self, proc: subprocess.Popen[bytes]) -> None:
        if proc.poll() is not None:
            self.last_returncode = int(proc.poll())
            return
        proc.terminate()
        try:
            self.last_returncode = int(proc.wait(timeout=self.stop_timeout))
        except subprocess.TimeoutExpired:
            proc.kill()
            self.last_returncode = int(proc.wait(timeout=self.stop_timeout))

    @staticmethod
    def _validate_port(port: int) -> int:
        value = int(port)
        if value < 1 or value > 65535:
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

    @classmethod
    def _receiver_window_title(cls, peer_label: str = "") -> str:
        label = cls._validate_peer_label(peer_label) or "Remote"
        return f"AgoraLink Screen Viewer - {label}"


class _FakeProcess:
    _next_pid = 1000

    def __init__(self, cmd: List[str], cwd: Optional[str] = None, **kwargs) -> None:
        type(self)._next_pid += 1
        self.pid = type(self)._next_pid
        self.cmd = list(cmd)
        self.cwd = cwd
        self.kwargs = dict(kwargs)
        self.returncode: Optional[int] = None
        self.terminated = False
        self.killed = False

    def poll(self) -> Optional[int]:
        return self.returncode

    def terminate(self) -> None:
        self.terminated = True
        self.returncode = 0

    def kill(self) -> None:
        self.killed = True
        self.returncode = -9

    def wait(self, timeout: Optional[float] = None) -> int:
        if self.returncode is None:
            self.returncode = 0
        return int(self.returncode)


def _run_self_test() -> Dict[str, object]:
    commands: List[List[str]] = []
    cwd_values: List[Optional[str]] = []
    popen_kwargs: List[Dict[str, object]] = []
    taskkill_commands: List[List[str]] = []

    expected_ffmpeg = str(Path("C:/AgoraLinkTools/ffmpeg/bin/ffmpeg.exe"))
    expected_ffplay = str(Path("C:/AgoraLinkTools/ffmpeg/bin/ffplay.exe"))

    def fake_tool_finder(name: str) -> str:
        if name == "ffmpeg":
            return expected_ffmpeg
        if name == "ffplay":
            return expected_ffplay
        return ""

    def fake_popen(cmd: List[str], cwd: Optional[str] = None, **kwargs) -> _FakeProcess:
        commands.append(list(cmd))
        cwd_values.append(cwd)
        popen_kwargs.append(dict(kwargs))
        return _FakeProcess(cmd, cwd=cwd, **kwargs)

    def fake_taskkill(cmd: List[str], **_kwargs) -> subprocess.CompletedProcess[str]:
        taskkill_commands.append(list(cmd))
        return subprocess.CompletedProcess(cmd, 0, "", "")

    runtime = ScreenRuntime(popen_factory=fake_popen, taskkill_runner=fake_taskkill, tool_finder=fake_tool_finder)
    initial_state = runtime.get_state()
    receiver_state = runtime.start_receiver()
    duplicate_state = runtime.start_sender("127.0.0.1")
    commands_after_duplicate = len(commands)
    stop_state = runtime.stop()
    sender_state = runtime.start_sender("127.0.0.1")
    sender_proc = runtime._process
    if isinstance(sender_proc, _FakeProcess):
        sender_proc.returncode = 42
    crashed_state = runtime.get_state()
    commands_after_crashed_state = len(commands)
    expected_script_dir = str(Path(__file__).resolve().parent)

    def fast_exit_popen(cmd: List[str], cwd: Optional[str] = None, **kwargs) -> _FakeProcess:
        commands.append(list(cmd))
        cwd_values.append(cwd)
        popen_kwargs.append(dict(kwargs))
        proc = _FakeProcess(cmd, cwd=cwd, **kwargs)
        proc.returncode = 7
        return proc

    fast_exit_runtime = ScreenRuntime(popen_factory=fast_exit_popen, taskkill_runner=fake_taskkill, tool_finder=fake_tool_finder)
    fast_exit_state = fast_exit_runtime.start_receiver()
    fast_exit_runtime._handle_startup_exit_check(fast_exit_runtime._process, "ffplay")
    fast_exit_state = fast_exit_runtime.get_state()
    missing_runtime = ScreenRuntime(popen_factory=fake_popen, taskkill_runner=fake_taskkill, tool_finder=lambda _name: "")
    missing_state = missing_runtime.start_sender("127.0.0.1")
    sample_dshow = r'''
[dshow @ 000001] DirectShow audio devices
[dshow @ 000001]  "Microphone (Realtek(R) Audio)" (audio)
[dshow @ 000001]     Alternative name "@device_cm_{MIC}\wave_{MIC}"
[dshow @ 000001]  "绔嬩綋澹版贩闊?(Realtek(R) Audio)" (audio)
[dshow @ 000001]     Alternative name "@device_cm_{33D9A762-90C8-11D0-BD43-00A0C911CE86}\wave_{9801B1F7-24E7-4D18-A739-D6DD3DB6444E}"
'''
    parsed_devices = runtime._parse_dshow_audio_devices(sample_dshow)
    selected_mix = runtime._select_dshow_system_mix_device(parsed_devices)
    expected_mix_alt = r"@device_cm_{33D9A762-90C8-11D0-BD43-00A0C911CE86}\wave_{9801B1F7-24E7-4D18-A739-D6DD3DB6444E}"
    sample_dshow_inline = rf'''
[in#0 @ 000001] "Integrated Webcam" (video)
[in#0 @ 000001]   Alternative name "@device_pnp_\\?\usb#camera"
[in#0 @ 000001] "插孔麦克风 (Realtek(R) Audio)" (audio)
[in#0 @ 000001]   Alternative name "@device_cm_{{MIC}}\wave_{{MIC}}"
[in#0 @ 000001] "立体声混音 (Realtek(R) Audio)" (audio)
[in#0 @ 000001]   Alternative name "{expected_mix_alt}"
'''
    parsed_inline_devices = runtime._parse_dshow_audio_devices(sample_dshow_inline)
    selected_inline_mix = runtime._select_dshow_system_mix_device(parsed_inline_devices)
    dshow_cmd = runtime._build_sender_command(
        host="127.0.0.1",
        port=DEFAULT_SCREEN_PORT,
        profile_name=DEFAULT_SCREEN_PROFILE,
        ffmpeg_path=expected_ffmpeg,
        audio={
            "enabled": True,
            "mode": "system",
            "backend": "dshow",
            "input_name": expected_mix_alt,
        },
    )
    checks = [
        initial_state["state"] == STATE_IDLE,
        receiver_state["state"] == STATE_RECEIVING,
        receiver_state["mode"] == STATE_RECEIVING,
        receiver_state["command"][0] == expected_ffplay,
        "udp://0.0.0.0:50020" in str(receiver_state["command"][-1]),
        duplicate_state["last_error"] == "already running",
        duplicate_state["ok"] is False,
        duplicate_state["error"] == "already running",
        duplicate_state["state"] == STATE_RECEIVING,
        duplicate_state["mode"] == STATE_RECEIVING,
        commands_after_duplicate == 1,
        stop_state["state"] == STATE_IDLE,
        not runtime.is_running(),
        sender_state["state"] == STATE_SENDING,
        sender_state["profile"] == DEFAULT_SCREEN_PROFILE,
        sender_state["command"][0] == expected_ffmpeg,
        "-f" in sender_state["command"],
        "gdigrab" in sender_state["command"],
        "720p30_h264_qsv" == sender_state["profile"],
        str(sender_state["command"][-1]).startswith("udp://127.0.0.1:50020?pkt_size=1316"),
        commands_after_crashed_state == 2,
        crashed_state["state"] == STATE_ERROR,
        crashed_state["returncode"] == 42,
        crashed_state["mode"] == STATE_SENDING,
        not runtime.is_running(),
        cwd_values[0] == expected_script_dir,
        os.name != "nt" or "startupinfo" not in popen_kwargs[0],
        os.name != "nt" or "creationflags" not in popen_kwargs[0],
        os.name != "nt" or "startupinfo" in popen_kwargs[1],
        os.name != "nt" or (bool(taskkill_commands) and taskkill_commands[0][0] == "taskkill" and "/T" in taskkill_commands[0] and "/F" in taskkill_commands[0]),
        fast_exit_state["state"] == STATE_ERROR,
        fast_exit_state["returncode"] == 7,
        "exited with code 7" in str(fast_exit_state["last_error"]),
        fast_exit_state["command"][0] == expected_ffplay,
        missing_state["state"] == STATE_ERROR,
        "winget install --id Gyan.FFmpeg -e" in str(missing_state["last_error"]),
        len(parsed_devices) == 2,
        selected_mix.get("alternative_name") == expected_mix_alt,
        len(parsed_inline_devices) == 2,
        selected_inline_mix.get("alternative_name") == expected_mix_alt,
        not runtime._is_system_mix_audio_name("Microphone (Realtek(R) Audio)"),
        runtime._is_excluded_audio_capture_name("Mic Array"),
        "-f" in dshow_cmd and "dshow" in dshow_cmd,
        f"audio={expected_mix_alt}" in dshow_cmd,
    ]
    return {
        "ok": all(checks),
        "checks": checks,
        "commands": commands,
        "state": runtime.get_state(),
    }


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="AgoraLink screen runtime process manager.")
    parser.add_argument("--self-test", action="store_true", help="Run state-machine smoke test without launching FFmpeg.")
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
