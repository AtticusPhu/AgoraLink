from __future__ import annotations

import json
import queue
import subprocess
import threading
import time
import unittest
from pathlib import Path
from typing import Optional

from screen_runtime import STATE_SENDING, STATE_STOPPING, ScreenRuntime


class _QueuedTextStream:
    def __init__(self) -> None:
        self._lines: queue.Queue[Optional[str]] = queue.Queue()
        self._closed = False

    def push(self, line: str) -> None:
        if not self._closed:
            self._lines.put(line)

    def close(self) -> None:
        if not self._closed:
            self._closed = True
            self._lines.put(None)

    def readline(self) -> str:
        line = self._lines.get(timeout=2.0)
        return "" if line is None else line


class _FakeStdin:
    def __init__(self, process: "_FakeNativeProcess") -> None:
        self.process = process
        self.writes: list[str] = []
        self.closed = False

    def write(self, text: str) -> int:
        self.writes.append(text)
        return len(text)

    def flush(self) -> None:
        self.process.local_stop_requests += 1
        if self.process.graceful_exit:
            self.process.schedule_graceful_exit()

    def close(self) -> None:
        self.closed = True


class _FakeNativeProcess:
    _pid = 4000

    def __init__(
        self,
        *,
        graceful_exit: bool = True,
        terminate_exits: bool = True,
        graceful_delay: float = 0.0,
    ) -> None:
        type(self)._pid += 1
        self.pid = type(self)._pid
        self.graceful_exit = graceful_exit
        self.terminate_exits = terminate_exits
        self.graceful_delay = graceful_delay
        self.returncode: Optional[int] = None
        self.local_stop_requests = 0
        self.terminate_calls = 0
        self.kill_calls = 0
        self.stdout = _QueuedTextStream()
        self.stderr = _QueuedTextStream()
        self.stdin = _FakeStdin(self)
        self._finish_lock = threading.Lock()
        self._graceful_scheduled = False

    def poll(self) -> Optional[int]:
        return self.returncode

    def wait(self, timeout: Optional[float] = None) -> int:
        deadline = time.monotonic() + (2.0 if timeout is None else timeout)
        while self.returncode is None and time.monotonic() < deadline:
            time.sleep(0.001)
        if self.returncode is None:
            raise subprocess.TimeoutExpired("fake-native", timeout)
        return self.returncode

    def terminate(self) -> None:
        self.terminate_calls += 1
        if self.terminate_exits:
            self.force_exit(-15)

    def kill(self) -> None:
        self.kill_calls += 1
        self.force_exit(-9)

    def schedule_graceful_exit(self) -> None:
        with self._finish_lock:
            if self._graceful_scheduled:
                return
            self._graceful_scheduled = True

        def finish() -> None:
            if self.graceful_delay:
                time.sleep(self.graceful_delay)
            event = {
                "type": "NATIVE_SCREEN_STOPPED",
                "reason": "local_stop",
                "stream_close_sent": True,
                "stream_close_ack_received": True,
                "frames_sent": 7,
            }
            self.stdout.push(json.dumps(event) + "\n")
            self.force_exit(0)

        threading.Thread(target=finish, daemon=True).start()

    def force_exit(self, returncode: int) -> None:
        with self._finish_lock:
            if self.returncode is not None:
                return
            self.returncode = returncode
            self.stdout.close()
            self.stderr.close()


class ScreenRuntimeShutdownTests(unittest.TestCase):
    def make_runtime(
        self,
        process: _FakeNativeProcess,
    ) -> tuple[ScreenRuntime, list[list[str]]]:
        taskkill_calls: list[list[str]] = []

        def taskkill(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            taskkill_calls.append(list(command))
            process.force_exit(-9)
            return subprocess.CompletedProcess(command, 0, "", "")

        runtime = ScreenRuntime(
            script_dir=Path(__file__).resolve().parents[1],
            taskkill_runner=taskkill,
            graceful_stop_timeout=0.08,
            terminate_stop_timeout=0.05,
            forced_stop_timeout=0.05,
        )
        runtime._process = process
        runtime._state = STATE_SENDING
        runtime.current_mode = STATE_SENDING
        runtime._start_native_output_threads(process, None)
        return runtime, taskkill_calls

    def test_native_stop_uses_graceful_channel_first(self) -> None:
        process = _FakeNativeProcess()
        runtime, taskkill = self.make_runtime(process)
        result = runtime.stop()
        command = json.loads(process.stdin.writes[0])
        self.assertEqual(command["type"], "LOCAL_STOP")
        self.assertEqual(command["version"], 1)
        self.assertTrue(result["stop_telemetry"]["graceful_stop_requested"])
        self.assertFalse(result["stop_telemetry"]["forced_terminate_used"])
        self.assertFalse(taskkill)

    def test_native_stop_waits_for_stopped_event(self) -> None:
        process = _FakeNativeProcess(graceful_delay=0.03)
        runtime, _ = self.make_runtime(process)
        started = time.monotonic()
        result = runtime.stop()
        self.assertGreaterEqual(time.monotonic() - started, 0.02)
        self.assertTrue(result["stop_telemetry"]["native_stopped_received"])

    def test_double_stop_is_idempotent(self) -> None:
        process = _FakeNativeProcess(graceful_delay=0.03)
        runtime, taskkill = self.make_runtime(process)
        results: list[dict[str, object]] = []
        first_thread = threading.Thread(target=lambda: results.append(runtime.stop()))
        second_thread = threading.Thread(target=lambda: results.append(runtime.stop()))
        first_thread.start()
        time.sleep(0.005)
        second_thread.start()
        first_thread.join(1.0)
        second_thread.join(1.0)
        self.assertEqual(process.local_stop_requests, 1)
        self.assertEqual(len(results), 2)
        self.assertEqual(results[0]["stop_telemetry"], results[1]["stop_telemetry"])
        self.assertFalse(taskkill)

    def test_app_close_uses_same_stop_path(self) -> None:
        process = _FakeNativeProcess()
        runtime, _ = self.make_runtime(process)
        thread = runtime.stop_async(reason="app_close")
        thread.join(1.0)
        self.assertFalse(thread.is_alive())
        self.assertEqual(json.loads(process.stdin.writes[0])["reason"], "app_close")
        self.assertEqual(process.local_stop_requests, 1)

    def test_start_while_stopping_rejected(self) -> None:
        process = _FakeNativeProcess(graceful_exit=False)
        runtime, _ = self.make_runtime(process)
        runtime._stop_in_progress = True
        runtime._state = STATE_STOPPING
        result = runtime.start_sender("127.0.0.1")
        self.assertFalse(result["ok"])
        self.assertEqual(result["error"], "screen runtime is stopping")

    def test_already_exited_process_does_not_taskkill(self) -> None:
        process = _FakeNativeProcess()
        runtime, taskkill = self.make_runtime(process)
        process.force_exit(0)
        result = runtime.stop()
        self.assertEqual(result["state"], "idle")
        self.assertFalse(taskkill)
        self.assertEqual(process.kill_calls, 0)

    def test_graceful_timeout_escalates_to_terminate(self) -> None:
        process = _FakeNativeProcess(graceful_exit=False, terminate_exits=True)
        runtime, taskkill = self.make_runtime(process)
        result = runtime.stop()
        self.assertTrue(result["stop_telemetry"]["forced_terminate_used"])
        self.assertFalse(result["stop_telemetry"]["forced_kill_used"])
        self.assertEqual(process.terminate_calls, 1)
        self.assertFalse(taskkill)

    def test_terminate_timeout_escalates_to_force_kill(self) -> None:
        process = _FakeNativeProcess(graceful_exit=False, terminate_exits=False)
        runtime, taskkill = self.make_runtime(process)
        result = runtime.stop()
        self.assertTrue(result["stop_telemetry"]["forced_terminate_used"])
        self.assertTrue(result["stop_telemetry"]["forced_kill_used"])
        self.assertEqual(len(taskkill), 1)

    def test_reader_threads_joined(self) -> None:
        process = _FakeNativeProcess()
        runtime, _ = self.make_runtime(process)
        result = runtime.stop()
        self.assertTrue(result["stop_telemetry"]["reader_threads_joined"])
        self.assertFalse(any(thread.is_alive() for thread in runtime._reader_threads))

    def test_final_json_event_collected(self) -> None:
        process = _FakeNativeProcess()
        runtime, _ = self.make_runtime(process)
        result = runtime.stop()
        self.assertEqual(result["native_last_event"]["type"], "NATIVE_SCREEN_STOPPED")
        self.assertEqual(result["native_stats"]["frames_sent"], 7)
        self.assertTrue(result["stop_telemetry"]["stream_close_sent"])
        self.assertTrue(result["stop_telemetry"]["stream_close_ack_received"])


if __name__ == "__main__":
    unittest.main()
