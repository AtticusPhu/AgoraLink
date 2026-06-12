#!/usr/bin/env python3
"""AgoraLink screen sharing runtime process manager.

This module only starts and stops the screen sender/receiver CLI prototypes.
It does not send chat messages, touch GUI code, or modify protocol/database
behavior.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Callable, Dict, List, Optional

from screen_control import DEFAULT_SCREEN_PORT


STATE_IDLE = "idle"
STATE_RECEIVING = "receiving"
STATE_SENDING = "sending"
STATE_STOPPING = "stopping"
STATE_ERROR = "error"

DEFAULT_SCREEN_PROFILE = "720p30_h264_qsv"


class ScreenRuntime:
    def __init__(
        self,
        *,
        python_executable: Optional[str] = None,
        script_dir: Optional[Path] = None,
        popen_factory: Callable[..., subprocess.Popen[bytes]] = subprocess.Popen,
        taskkill_runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
        stop_timeout: float = 5.0,
    ) -> None:
        self.python_executable = str(python_executable or sys.executable)
        self.script_dir = (Path(script_dir) if script_dir is not None else Path(__file__).resolve().parent).resolve()
        self._popen_factory = popen_factory
        self._taskkill_runner = taskkill_runner
        self.stop_timeout = float(stop_timeout)
        self._process: Optional[subprocess.Popen[bytes]] = None
        self._state = STATE_IDLE
        self.last_error = ""
        self.last_returncode: Optional[int] = None
        self.last_command: List[str] = []
        self.current_mode: Optional[str] = None
        self.current_host: Optional[str] = None
        self.current_port: Optional[int] = None
        self.current_profile: Optional[str] = None

    def start_receiver(self, port: int = DEFAULT_SCREEN_PORT) -> Dict[str, object]:
        if self._has_running_process():
            return self._already_running_result()
        try:
            port = self._validate_port(port)
            cmd = [
                self.python_executable,
                "-B",
                str(self._script_path("screen_receiver.py")),
                "--port",
                str(port),
            ]
            self.last_command = list(cmd)
            self._process = self._popen_factory(cmd, cwd=str(self.script_dir))
        except Exception as exc:
            return self._set_error(str(exc))

        self._state = STATE_RECEIVING
        self.last_error = ""
        self.last_returncode = None
        self.current_mode = STATE_RECEIVING
        self.current_host = None
        self.current_port = port
        self.current_profile = None
        return self.get_state()

    def start_sender(
        self,
        host: str,
        port: int = DEFAULT_SCREEN_PORT,
        profile: str = DEFAULT_SCREEN_PROFILE,
    ) -> Dict[str, object]:
        if self._has_running_process():
            return self._already_running_result()
        try:
            host = self._validate_host(host)
            port = self._validate_port(port)
            profile = self._validate_profile(profile)
            cmd = [
                self.python_executable,
                "-B",
                str(self._script_path("screen_sender.py")),
                "--host",
                host,
                "--port",
                str(port),
                "--profile",
                profile,
            ]
            self.last_command = list(cmd)
            self._process = self._popen_factory(cmd, cwd=str(self.script_dir))
        except Exception as exc:
            return self._set_error(str(exc))

        self._state = STATE_SENDING
        self.last_error = ""
        self.last_returncode = None
        self.current_mode = STATE_SENDING
        self.current_host = host
        self.current_port = port
        self.current_profile = profile
        return self.get_state()

    def stop(self) -> Dict[str, object]:
        if not self._has_running_process():
            self._process = None
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

        self._state = STATE_IDLE
        self.last_error = ""
        self._clear_current_session()
        return self.get_state()

    def is_running(self) -> bool:
        return self._has_running_process()

    def get_state(self) -> Dict[str, object]:
        self._refresh_process()
        return self._snapshot()

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
            "mode": self.current_mode,
            "host": self.current_host,
            "port": self.current_port,
            "profile": self.current_profile,
            "pid": int(self._process.pid) if running and getattr(self._process, "pid", None) is not None else None,
            "returncode": self.last_returncode,
            "last_error": self.last_error,
            "command": list(self.last_command),
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

    def _script_path(self, name: str) -> Path:
        return (self.script_dir / name).resolve()

    def _stop_windows_process_tree(self, proc: subprocess.Popen[bytes]) -> None:
        pid = getattr(proc, "pid", None)
        if pid is None:
            self._stop_portable_process(proc)
            return
        try:
            completed = self._taskkill_runner(
                ["taskkill", "/PID", str(int(pid)), "/T", "/F"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=max(5.0, self.stop_timeout),
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
    def _validate_profile(profile: str) -> str:
        value = str(profile or "").strip()
        if not value:
            raise ValueError("profile is required")
        return value


class _FakeProcess:
    _next_pid = 1000

    def __init__(self, cmd: List[str], cwd: Optional[str] = None) -> None:
        type(self)._next_pid += 1
        self.pid = type(self)._next_pid
        self.cmd = list(cmd)
        self.cwd = cwd
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
    taskkill_commands: List[List[str]] = []

    def fake_popen(cmd: List[str], cwd: Optional[str] = None) -> _FakeProcess:
        commands.append(list(cmd))
        cwd_values.append(cwd)
        return _FakeProcess(cmd, cwd=cwd)

    def fake_taskkill(cmd: List[str], **_kwargs) -> subprocess.CompletedProcess[str]:
        taskkill_commands.append(list(cmd))
        return subprocess.CompletedProcess(cmd, 0, "", "")

    runtime = ScreenRuntime(popen_factory=fake_popen, taskkill_runner=fake_taskkill)
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
    expected_receiver_script = str(Path(__file__).resolve().parent / "screen_receiver.py")
    expected_sender_script = str(Path(__file__).resolve().parent / "screen_sender.py")

    def fast_exit_popen(cmd: List[str], cwd: Optional[str] = None) -> _FakeProcess:
        commands.append(list(cmd))
        cwd_values.append(cwd)
        proc = _FakeProcess(cmd, cwd=cwd)
        proc.returncode = 7
        return proc

    fast_exit_runtime = ScreenRuntime(popen_factory=fast_exit_popen, taskkill_runner=fake_taskkill)
    fast_exit_state = fast_exit_runtime.start_receiver()
    checks = [
        initial_state["state"] == STATE_IDLE,
        receiver_state["state"] == STATE_RECEIVING,
        receiver_state["mode"] == STATE_RECEIVING,
        receiver_state["command"][0] == sys.executable,
        receiver_state["command"][2] == expected_receiver_script,
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
        sender_state["command"][0] == sys.executable,
        sender_state["command"][2] == expected_sender_script,
        commands_after_crashed_state == 2,
        crashed_state["state"] == STATE_ERROR,
        crashed_state["returncode"] == 42,
        crashed_state["mode"] == STATE_SENDING,
        not runtime.is_running(),
        cwd_values[0] == expected_script_dir,
        os.name != "nt" or (bool(taskkill_commands) and taskkill_commands[0][0] == "taskkill" and "/T" in taskkill_commands[0] and "/F" in taskkill_commands[0]),
        fast_exit_state["state"] == STATE_ERROR,
        fast_exit_state["returncode"] == 7,
        "exited with code 7" in str(fast_exit_state["last_error"]),
        fast_exit_state["command"][2] == expected_receiver_script,
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
