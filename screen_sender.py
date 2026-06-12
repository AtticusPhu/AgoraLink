#!/usr/bin/env python3
"""AgoraLink screen sender CLI prototype.

Video is sent with FFmpeg over UDP. It does not use the RUDP file transfer
queue.
"""

from __future__ import annotations

import argparse
import signal
import subprocess
import sys
from typing import List, Optional

from screen_profile import PROFILES_BY_NAME, ScreenProfile, normalize_profile_name


_ACTIVE_PROC: Optional[subprocess.Popen[bytes]] = None
_STOPPING = False


def _valid_port(value: str) -> int:
    port = int(value)
    if port < 1 or port > 65535:
        raise argparse.ArgumentTypeError("port must be in 1..65535")
    return port


def get_profile(name: str) -> ScreenProfile:
    normalized = normalize_profile_name(name)
    profile = PROFILES_BY_NAME.get(normalized)
    if profile is None:
        available = ", ".join(sorted(PROFILES_BY_NAME))
        raise ValueError(f"unknown profile {name!r}; available: {available}")
    return profile


def build_ffmpeg_command(host: str, port: int, profile: ScreenProfile) -> List[str]:
    return [
        "ffmpeg",
        "-hide_banner",
        "-fflags",
        "+genpts",
        "-f",
        "gdigrab",
        "-framerate",
        str(profile.fps),
        "-i",
        "desktop",
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
        "-f",
        "mpegts",
        f"udp://{host}:{port}?pkt_size=1316",
    ]


def _write_ffmpeg_quit(proc: subprocess.Popen[bytes]) -> None:
    try:
        if proc.stdin is not None and not proc.stdin.closed:
            proc.stdin.write(b"q\n")
            proc.stdin.flush()
    except Exception:
        pass


def _terminate_process(proc: Optional[subprocess.Popen[bytes]]) -> None:
    global _STOPPING
    if proc is None or proc.poll() is not None:
        return
    if _STOPPING:
        return
    _STOPPING = True
    _write_ffmpeg_quit(proc)
    try:
        proc.wait(timeout=3.0)
        return
    except subprocess.TimeoutExpired:
        pass
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5.0)
        return
    except subprocess.TimeoutExpired:
        pass
    if proc.poll() is not None:
        return
    proc.kill()
    try:
        proc.wait(timeout=5.0)
    except Exception:
        pass


def _handle_stop_signal(signum, _frame) -> None:
    print(f"Stopping ffmpeg, signal={signum}...", file=sys.stderr)
    _terminate_process(_ACTIVE_PROC)
    raise SystemExit(128 + int(signum))


def _install_signal_handlers() -> None:
    for sig in (signal.SIGTERM, getattr(signal, "SIGBREAK", None)):
        if sig is None:
            continue
        try:
            signal.signal(sig, _handle_stop_signal)
        except Exception:
            pass


def run_command(cmd: List[str]) -> int:
    global _ACTIVE_PROC, _STOPPING
    print(subprocess.list2cmdline(cmd), flush=True)
    _STOPPING = False
    _install_signal_handlers()
    try:
        proc = subprocess.Popen(cmd, stdin=subprocess.PIPE)
        _ACTIVE_PROC = proc
    except FileNotFoundError:
        print("ffmpeg not found in PATH", file=sys.stderr)
        return 127

    try:
        return int(proc.wait())
    except KeyboardInterrupt:
        print("Stopping ffmpeg...", file=sys.stderr)
        _terminate_process(proc)
        return 130
    finally:
        try:
            if proc.poll() is None:
                _terminate_process(proc)
        finally:
            _ACTIVE_PROC = None


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Send desktop screen video with FFmpeg/UDP.")
    parser.add_argument("--host", required=True, help="Receiver IP address.")
    parser.add_argument("--port", type=_valid_port, default=50020, help="Receiver UDP port. Default: 50020.")
    parser.add_argument("--profile", default="720p30_h264_qsv", help="Screen profile name. Default: 720p30_h264_qsv.")
    return parser


def main() -> int:
    parser = build_argparser()
    args = parser.parse_args()
    try:
        profile = get_profile(args.profile)
    except ValueError as exc:
        parser.error(str(exc))
    cmd = build_ffmpeg_command(str(args.host), int(args.port), profile)
    return run_command(cmd)


if __name__ == "__main__":
    raise SystemExit(main())
