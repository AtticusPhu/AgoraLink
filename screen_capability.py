#!/usr/bin/env python3
"""Screen casting capability probe for AgoraLink.

Run:
    python screen_capability.py

The script prints JSON to stdout. It does not touch AgoraLink protocol, GUI, or
runtime data files.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from typing import Dict, Iterable, List, Optional, Tuple


ENCODERS_OF_INTEREST = (
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
)

TEST_PROFILES = (
    {"name": "720p30 hevc_qsv", "width": 1280, "height": 720, "fps": 30, "encoder": "hevc_qsv"},
    {"name": "720p30 h264_qsv", "width": 1280, "height": 720, "fps": 30, "encoder": "h264_qsv"},
    {"name": "1080p30 h264_qsv", "width": 1920, "height": 1080, "fps": 30, "encoder": "h264_qsv"},
    {"name": "720p60 h264_qsv", "width": 1280, "height": 720, "fps": 60, "encoder": "h264_qsv"},
    {"name": "1080p60 h264_qsv", "width": 1920, "height": 1080, "fps": 60, "encoder": "h264_qsv"},
    {"name": "1080p60 hevc_qsv", "width": 1920, "height": 1080, "fps": 60, "encoder": "hevc_qsv"},
)


def _run_capture(args: List[str], timeout: float = 10.0) -> Tuple[int, str, str]:
    proc = subprocess.run(
        args,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
    )
    return int(proc.returncode), proc.stdout or "", proc.stderr or ""


def _tail(text: str, max_chars: int = 3000) -> str:
    clean = str(text or "").replace("\r", "\n")
    lines = [line.rstrip() for line in clean.splitlines() if line.strip()]
    joined = "\n".join(lines)
    if len(joined) <= max_chars:
        return joined
    return joined[-max_chars:]


def find_ffmpeg(explicit_path: str = "") -> Dict[str, object]:
    path = str(explicit_path or "").strip() or shutil.which("ffmpeg") or ""
    if not path:
        return {"ok": False, "path": "", "version": "", "error_tail": "ffmpeg_not_found"}
    try:
        rc, out, err = _run_capture([path, "-version"], timeout=8.0)
    except Exception as exc:
        return {"ok": False, "path": path, "version": "", "error_tail": str(exc)}
    text = out or err
    first_line = text.splitlines()[0].strip() if text.splitlines() else ""
    return {"ok": rc == 0, "path": path, "version": first_line, "error_tail": "" if rc == 0 else _tail(err or out)}


def read_gpu_names() -> Dict[str, object]:
    if os.name == "nt":
        commands = [
            [
                "powershell",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name }",
            ],
            ["wmic", "path", "win32_VideoController", "get", "name"],
        ]
    elif sys.platform == "darwin":
        commands = [["system_profiler", "SPDisplaysDataType"]]
    else:
        commands = [["lspci"]]

    errors: List[str] = []
    for cmd in commands:
        try:
            rc, out, err = _run_capture(cmd, timeout=8.0)
        except Exception as exc:
            errors.append(f"{cmd[0]}: {exc}")
            continue
        if rc != 0:
            errors.append(f"{cmd[0]}: {_tail(err or out, 500)}")
            continue
        names = _parse_gpu_names(out)
        if names:
            return {"ok": True, "names": names, "error_tail": ""}
    return {"ok": False, "names": [], "error_tail": _tail("\n".join(errors), 1000)}


def _parse_gpu_names(text: str) -> List[str]:
    names: List[str] = []
    for raw in str(text or "").splitlines():
        line = raw.strip()
        if not line or line.lower() == "name":
            continue
        lower = line.lower()
        if "vga compatible controller" in lower or "3d controller" in lower or "display controller" in lower:
            if ":" in line:
                line = line.split(":", 2)[-1].strip()
        elif "chipset model:" in lower:
            line = line.split(":", 1)[-1].strip()
        if line and line not in names:
            names.append(line)
    return names


def parse_ffmpeg_encoders(ffmpeg_path: str) -> Dict[str, object]:
    try:
        rc, out, err = _run_capture([ffmpeg_path, "-hide_banner", "-encoders"], timeout=15.0)
    except Exception as exc:
        return {
            "ok": False,
            "available": {name: False for name in ENCODERS_OF_INTEREST},
            "error_tail": str(exc),
        }
    text = (out or "") + "\n" + (err or "")
    found = set()
    for line in text.splitlines():
        match = re.match(r"^\s*[A-Z.]{6}\s+(\S+)\s+", line)
        if match:
            found.add(match.group(1))
    return {
        "ok": rc == 0,
        "available": {name: name in found for name in ENCODERS_OF_INTEREST},
        "error_tail": "" if rc == 0 else _tail(text),
    }


def _ffmpeg_test_command(ffmpeg_path: str, profile: Dict[str, object], seconds: float) -> List[str]:
    size = f"{int(profile['width'])}x{int(profile['height'])}"
    fps = int(profile["fps"])
    encoder = str(profile["encoder"])
    return [
        ffmpeg_path,
        "-hide_banner",
        "-nostdin",
        "-stats",
        "-loglevel",
        "info",
        "-f",
        "lavfi",
        "-i",
        f"testsrc2=size={size}:rate={fps}",
        "-t",
        f"{float(seconds):.3f}",
        "-an",
        "-c:v",
        encoder,
        "-f",
        "null",
        "-",
    ]


def _parse_encoded_frames(text: str) -> int:
    frames = 0
    for match in re.finditer(r"\bframe=\s*(\d+)", str(text or "")):
        try:
            frames = max(frames, int(match.group(1)))
        except Exception:
            pass
    return frames


def run_profile_test(
    ffmpeg_path: str,
    profile: Dict[str, object],
    encoders_available: Dict[str, bool],
    seconds: float = 5.0,
) -> Dict[str, object]:
    target_fps = int(profile["fps"])
    encoder = str(profile["encoder"])
    result = {
        "name": str(profile["name"]),
        "encoder": encoder,
        "width": int(profile["width"]),
        "height": int(profile["height"]),
        "duration_sec": float(seconds),
        "ok": False,
        "avg_fps": 0.0,
        "target_fps": target_fps,
        "recommended": False,
        "error_tail": "",
    }
    if not encoders_available.get(encoder, False):
        result["error_tail"] = "encoder_not_available"
        return result

    cmd = _ffmpeg_test_command(ffmpeg_path, profile, seconds)
    timeout = max(30.0, float(seconds) * 8.0)
    start = time.perf_counter()
    try:
        proc = subprocess.run(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout,
        )
        elapsed = max(time.perf_counter() - start, 1e-6)
        combined = (proc.stdout or "") + "\n" + (proc.stderr or "")
        frames = _parse_encoded_frames(combined)
        result["ok"] = int(proc.returncode) == 0
        result["avg_fps"] = round(float(frames) / elapsed, 2) if frames > 0 else 0.0
        if proc.returncode != 0:
            result["error_tail"] = _tail(combined)
    except subprocess.TimeoutExpired as exc:
        combined = (exc.stdout or "") + "\n" + (exc.stderr or "")
        result["error_tail"] = "timeout\n" + _tail(combined)
    except Exception as exc:
        result["error_tail"] = str(exc)
    return result


def choose_recommended(results: Iterable[Dict[str, object]]) -> Optional[str]:
    chosen: Optional[str] = None
    for result in results:
        if result.get("ok") and float(result.get("avg_fps") or 0.0) >= float(result.get("target_fps") or 0) * 0.95:
            chosen = str(result.get("name") or "")
    return chosen or None


def probe_screen_capability(ffmpeg_path: str = "", seconds: float = 5.0) -> Dict[str, object]:
    ffmpeg = find_ffmpeg(ffmpeg_path)
    gpu = read_gpu_names()
    encoders = {
        "ok": False,
        "available": {name: False for name in ENCODERS_OF_INTEREST},
        "error_tail": "ffmpeg_not_available",
    }
    tests: List[Dict[str, object]] = []

    if ffmpeg.get("ok"):
        encoders = parse_ffmpeg_encoders(str(ffmpeg.get("path") or "ffmpeg"))
        for profile in TEST_PROFILES:
            tests.append(run_profile_test(str(ffmpeg.get("path") or "ffmpeg"), profile, encoders["available"], seconds=seconds))
    else:
        for profile in TEST_PROFILES:
            tests.append({
                "name": str(profile["name"]),
                "encoder": str(profile["encoder"]),
                "width": int(profile["width"]),
                "height": int(profile["height"]),
                "duration_sec": float(seconds),
                "ok": False,
                "avg_fps": 0.0,
                "target_fps": int(profile["fps"]),
                "recommended": False,
                "error_tail": "ffmpeg_not_found",
            })

    recommended = choose_recommended(tests)
    for result in tests:
        result["recommended"] = bool(recommended and result.get("name") == recommended)

    return {
        "ok": bool(ffmpeg.get("ok") and encoders.get("ok")),
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "ffmpeg": ffmpeg,
        "gpu": gpu,
        "encoders": encoders,
        "recommended": recommended,
        "tests": tests,
    }


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Probe AgoraLink screen casting capability.")
    parser.add_argument("--ffmpeg", default="", help="Path to ffmpeg. Defaults to PATH lookup.")
    parser.add_argument("--seconds", type=float, default=5.0, help="Synthetic source duration per profile. Default: 5.")
    parser.add_argument("--indent", type=int, default=2, help="JSON indentation. Use 0 for compact JSON.")
    return parser


def main() -> int:
    args = build_argparser().parse_args()
    seconds = max(0.5, float(args.seconds or 5.0))
    data = probe_screen_capability(ffmpeg_path=str(args.ffmpeg or ""), seconds=seconds)
    indent = None if int(args.indent or 0) <= 0 else int(args.indent)
    print(json.dumps(data, ensure_ascii=False, indent=indent))
    return 0 if data.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
