#!/usr/bin/env python3
"""AgoraLink screen receiver CLI prototype.

Video is received with ffplay over UDP. This path is intentionally separate
from AgoraLink's reliable file transfer queue.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from typing import List


def _valid_port(value: str) -> int:
    port = int(value)
    if port < 1 or port > 65535:
        raise argparse.ArgumentTypeError("port must be in 1..65535")
    return port


def build_ffplay_command(port: int) -> List[str]:
    return [
        "ffplay",
        "-fflags",
        "nobuffer",
        "-flags",
        "low_delay",
        "-framedrop",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        f"udp://0.0.0.0:{port}?fifo_size=1000000&overrun_nonfatal=1",
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
        print("ffplay not found in PATH", file=sys.stderr)
        return 127

    try:
        return int(proc.wait())
    except KeyboardInterrupt:
        print("Stopping ffplay...", file=sys.stderr)
        _terminate_process(proc)
        return 130


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Receive desktop screen video with ffplay/UDP.")
    parser.add_argument("--port", type=_valid_port, default=50020, help="Listen UDP port. Default: 50020.")
    return parser


def main() -> int:
    args = build_argparser().parse_args()
    cmd = build_ffplay_command(int(args.port))
    return run_command(cmd)


if __name__ == "__main__":
    raise SystemExit(main())
