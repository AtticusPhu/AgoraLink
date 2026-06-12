#!/usr/bin/env python3
"""AgoraLink screen sender CLI prototype.

Video is sent with FFmpeg over UDP. It does not use the RUDP file transfer
queue.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from typing import List

from screen_profile import PROFILES_BY_NAME, ScreenProfile, normalize_profile_name


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


def _terminate_process(proc: subprocess.Popen[bytes]) -> None:
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5.0)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5.0)


def run_command(cmd: List[str]) -> int:
    print(subprocess.list2cmdline(cmd), flush=True)
    try:
        proc = subprocess.Popen(cmd)
    except FileNotFoundError:
        print("ffmpeg not found in PATH", file=sys.stderr)
        return 127

    try:
        return int(proc.wait())
    except KeyboardInterrupt:
        print("Stopping ffmpeg...", file=sys.stderr)
        _terminate_process(proc)
        return 130


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
