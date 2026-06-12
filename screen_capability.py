#!/usr/bin/env python3
"""Screen casting capability probe for AgoraLink.

Run:
    python screen_capability.py

The probe uses real desktop capture by default. FFmpeg encoders compiled into a
binary are reported separately from encoders that actually run on this machine.
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
    {
        "name": "720p30 hevc_qsv",
        "mode": "auto",
        "width": 1280,
        "height": 720,
        "fps": 30,
        "encoder": "hevc_qsv",
        "experimental": False,
    },
    {
        "name": "720p30 h264_qsv",
        "mode": "auto",
        "width": 1280,
        "height": 720,
        "fps": 30,
        "encoder": "h264_qsv",
        "experimental": False,
    },
    {
        "name": "1080p30 h264_qsv",
        "mode": "auto",
        "width": 1920,
        "height": 1080,
        "fps": 30,
        "encoder": "h264_qsv",
        "experimental": False,
    },
    {
        "name": "720p60 h264_qsv",
        "mode": "high_fps",
        "width": 1280,
        "height": 720,
        "fps": 60,
        "encoder": "h264_qsv",
        "experimental": False,
    },
    {
        "name": "1080p60 h264_qsv",
        "mode": "experimental",
        "width": 1920,
        "height": 1080,
        "fps": 60,
        "encoder": "h264_qsv",
        "experimental": True,
    },
    {
        "name": "1080p60 hevc_qsv",
        "mode": "experimental",
        "width": 1920,
        "height": 1080,
        "fps": 60,
        "encoder": "hevc_qsv",
        "experimental": True,
    },
)

AUTO_RECOMMENDATION_ORDER = (
    "720p30 hevc_qsv",
    "720p30 h264_qsv",
    "1080p30 h264_qsv",
    "1080p30 hevc_qsv",
)

FATAL_ENCODER_PATTERNS = (
    "could not open encoder",
    "unsupported",
    "no device",
    "error initializing",
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


def _output_text(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return str(value)


def _has_fatal_encoder_error(text: str) -> bool:
    lower = str(text or "").lower()
    return any(pattern in lower for pattern in FATAL_ENCODER_PATTERNS)


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
            "compiled": {name: False for name in ENCODERS_OF_INTEREST},
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
        "compiled": {name: name in found for name in ENCODERS_OF_INTEREST},
        "error_tail": "" if rc == 0 else _tail(text),
    }


def _desktop_capture_command(
    ffmpeg_path: str,
    *,
    width: int,
    height: int,
    fps: int,
    encoder: str,
    seconds: float,
) -> List[str]:
    return [
        ffmpeg_path,
        "-hide_banner",
        "-nostdin",
        "-stats",
        "-loglevel",
        "info",
        "-f",
        "gdigrab",
        "-framerate",
        str(int(fps)),
        "-i",
        "desktop",
        "-t",
        f"{float(seconds):.3f}",
        "-vf",
        f"scale={int(width)}:{int(height)}",
        "-an",
        "-c:v",
        str(encoder),
        "-f",
        "null",
        "-",
    ]


def _parse_last_status_metrics(text: str) -> Dict[str, object]:
    normalized = str(text or "").replace("\r", "\n")
    fps_values: List[float] = []
    frames = 0
    for match in re.finditer(r"\bfps=\s*([0-9]+(?:\.[0-9]+)?)", normalized):
        try:
            fps_values.append(float(match.group(1)))
        except Exception:
            pass
    for match in re.finditer(r"\bframe=\s*(\d+)", normalized):
        try:
            frames = max(frames, int(match.group(1)))
        except Exception:
            pass
    return {
        "last_fps": fps_values[-1] if fps_values else None,
        "frames": frames,
    }


def _profile_result_base(profile: Dict[str, object], *, seconds: float, compiled: bool) -> Dict[str, object]:
    return {
        "name": str(profile["name"]),
        "mode": str(profile.get("mode") or "auto"),
        "experimental": bool(profile.get("experimental", False)),
        "encoder": str(profile["encoder"]),
        "width": int(profile["width"]),
        "height": int(profile["height"]),
        "duration_sec": float(seconds),
        "compiled": bool(compiled),
        "runtime_ok": False,
        "ok": False,
        "avg_fps": 0.0,
        "target_fps": int(profile["fps"]),
        "recommended": False,
        "reason": "",
        "error_tail": "",
    }


def _reason_for_failure(returncode: Optional[int], combined: str, avg_fps: float, target_fps: int) -> str:
    if _has_fatal_encoder_error(combined):
        return "encoder_runtime_error"
    if returncode is None:
        return "timeout"
    if int(returncode) != 0:
        return f"ffmpeg_exit_{int(returncode)}"
    if avg_fps <= 0:
        return "no_fps_observed"
    if avg_fps < float(target_fps) * 0.90:
        return "below_realtime_threshold"
    return "ok"


def run_profile_test(
    ffmpeg_path: str,
    profile: Dict[str, object],
    encoders_compiled: Dict[str, bool],
    seconds: float = 5.0,
) -> Dict[str, object]:
    target_fps = int(profile["fps"])
    encoder = str(profile["encoder"])
    result = _profile_result_base(profile, seconds=seconds, compiled=bool(encoders_compiled.get(encoder, False)))
    if not result["compiled"]:
        result["reason"] = "encoder_not_compiled"
        result["error_tail"] = "encoder_not_compiled"
        return result

    cmd = _desktop_capture_command(
        ffmpeg_path,
        width=int(profile["width"]),
        height=int(profile["height"]),
        fps=target_fps,
        encoder=encoder,
        seconds=seconds,
    )
    timeout = max(20.0, float(seconds) + 15.0)
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
        metrics = _parse_last_status_metrics(combined)
        fps_value = metrics.get("last_fps")
        if fps_value is None:
            fps_value = float(metrics.get("frames") or 0) / elapsed if metrics.get("frames") else 0.0
        avg_fps = round(float(fps_value or 0.0), 2)
        runtime_ok = int(proc.returncode) == 0 and not _has_fatal_encoder_error(combined)
        reason = _reason_for_failure(int(proc.returncode), combined, avg_fps, target_fps)
        result.update({
            "runtime_ok": bool(runtime_ok),
            "ok": bool(runtime_ok and avg_fps >= float(target_fps) * 0.90),
            "avg_fps": avg_fps,
            "reason": reason,
            "error_tail": "" if reason == "ok" else _tail(combined),
        })
    except subprocess.TimeoutExpired as exc:
        combined = _output_text(exc.stdout) + "\n" + _output_text(exc.stderr)
        result["reason"] = "timeout"
        result["error_tail"] = "timeout\n" + _tail(combined)
    except Exception as exc:
        result["reason"] = "probe_exception"
        result["error_tail"] = str(exc)
    return result


def run_encoder_runtime_test(
    ffmpeg_path: str,
    encoder: str,
    compiled: bool,
    seconds: float = 1.5,
) -> Dict[str, object]:
    result = {
        "compiled": bool(compiled),
        "runtime_test_ok": False,
        "reason": "",
        "error_tail": "",
    }
    if not compiled:
        result["reason"] = "encoder_not_compiled"
        return result
    profile = {
        "name": f"runtime {encoder}",
        "mode": "runtime",
        "width": 640,
        "height": 360,
        "fps": 15,
        "encoder": encoder,
        "experimental": False,
    }
    probe = run_profile_test(ffmpeg_path, profile, {encoder: True}, seconds=max(0.5, float(seconds)))
    result["runtime_test_ok"] = bool(probe.get("runtime_ok"))
    result["reason"] = str(probe.get("reason") or ("ok" if probe.get("runtime_ok") else "runtime_failed"))
    result["error_tail"] = str(probe.get("error_tail") or "")
    return result


def choose_recommended(results: Iterable[Dict[str, object]]) -> Optional[str]:
    by_name = {str(result.get("name") or ""): result for result in results}
    for name in AUTO_RECOMMENDATION_ORDER:
        result = by_name.get(name)
        if result and result.get("ok") and not result.get("experimental"):
            return name
    return None


def probe_screen_capability(ffmpeg_path: str = "", seconds: float = 5.0) -> Dict[str, object]:
    ffmpeg = find_ffmpeg(ffmpeg_path)
    gpu = read_gpu_names()
    encoders = {
        "ok": False,
        "compiled": {name: False for name in ENCODERS_OF_INTEREST},
        "runtime_test_ok": {name: False for name in ENCODERS_OF_INTEREST},
        "usable": {name: False for name in ENCODERS_OF_INTEREST},
        "runtime_reason": {name: "ffmpeg_not_available" for name in ENCODERS_OF_INTEREST},
        "error_tail": "ffmpeg_not_available",
    }
    tests: List[Dict[str, object]] = []

    if ffmpeg.get("ok"):
        parsed = parse_ffmpeg_encoders(str(ffmpeg.get("path") or "ffmpeg"))
        encoders["ok"] = bool(parsed.get("ok"))
        encoders["compiled"] = dict(parsed.get("compiled") or {})
        encoders["error_tail"] = str(parsed.get("error_tail") or "")

        for profile in TEST_PROFILES:
            test = run_profile_test(str(ffmpeg.get("path") or "ffmpeg"), profile, encoders["compiled"], seconds=seconds)
            tests.append(test)
            encoder = str(test.get("encoder") or "")
            if encoder:
                encoders["runtime_test_ok"][encoder] = bool(encoders["runtime_test_ok"].get(encoder) or test.get("runtime_ok"))
                if test.get("runtime_ok"):
                    encoders["runtime_reason"][encoder] = "ok"
                elif encoders["runtime_reason"].get(encoder) != "ok":
                    encoders["runtime_reason"][encoder] = str(test.get("reason") or "runtime_failed")

        covered = {str(profile["encoder"]) for profile in TEST_PROFILES}
        runtime_seconds = min(max(0.5, float(seconds)), 1.5)
        for encoder in ENCODERS_OF_INTEREST:
            if encoder in covered:
                if not encoders["compiled"].get(encoder, False):
                    encoders["runtime_reason"][encoder] = "encoder_not_compiled"
                continue
            runtime = run_encoder_runtime_test(
                str(ffmpeg.get("path") or "ffmpeg"),
                encoder,
                bool(encoders["compiled"].get(encoder, False)),
                seconds=runtime_seconds,
            )
            encoders["runtime_test_ok"][encoder] = bool(runtime.get("runtime_test_ok"))
            encoders["runtime_reason"][encoder] = str(runtime.get("reason") or "")

        encoders["usable"] = {
            name: bool(encoders["compiled"].get(name, False) and encoders["runtime_test_ok"].get(name, False))
            for name in ENCODERS_OF_INTEREST
        }
    else:
        for profile in TEST_PROFILES:
            tests.append({
                "name": str(profile["name"]),
                "mode": str(profile.get("mode") or "auto"),
                "experimental": bool(profile.get("experimental", False)),
                "encoder": str(profile["encoder"]),
                "width": int(profile["width"]),
                "height": int(profile["height"]),
                "duration_sec": float(seconds),
                "compiled": False,
                "runtime_ok": False,
                "ok": False,
                "avg_fps": 0.0,
                "target_fps": int(profile["fps"]),
                "recommended": False,
                "reason": "ffmpeg_not_found",
                "error_tail": "ffmpeg_not_found",
            })

    recommended = choose_recommended(tests)
    for result in tests:
        result["recommended"] = bool(recommended and result.get("name") == recommended)

    experimental_profiles = [str(result.get("name") or "") for result in tests if result.get("experimental") and result.get("ok")]
    reason = "ok" if recommended else "no_stable_realtime_qsv_profile_ok; use software fallback or show unavailable"

    return {
        "ok": bool(recommended),
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "capture_source": "gdigrab:desktop",
        "ffmpeg": ffmpeg,
        "gpu": gpu,
        "encoders": encoders,
        "recommended": recommended,
        "reason": reason,
        "experimental_profiles": experimental_profiles,
        "tests": tests,
    }


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Probe AgoraLink real desktop screen casting capability.")
    parser.add_argument("--ffmpeg", default="", help="Path to ffmpeg. Defaults to PATH lookup.")
    parser.add_argument("--seconds", type=float, default=5.0, help="Real desktop capture duration per profile. Default: 5.")
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
