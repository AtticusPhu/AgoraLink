#!/usr/bin/env python3
"""Screen casting profile negotiation for AgoraLink.

This module only chooses a screen casting profile from local and remote
capability JSON. AgoraLink chat/RUDP should carry invitations, accept/reject,
stop, parameter negotiation, and status. The actual video stream belongs on
FFmpeg/UDP, may drop frames, and must not enter the reliable file queue.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple


@dataclass(frozen=True)
class ScreenProfile:
    name: str
    codec: str
    encoder: str
    width: int
    height: int
    fps: int
    bitrate: str
    maxrate: str
    bufsize: str
    experimental: bool
    mode: str

    def to_dict(self) -> Dict[str, object]:
        return asdict(self)


BUILTIN_PROFILES: Tuple[ScreenProfile, ...] = (
    ScreenProfile(
        name="720p30_hevc_qsv",
        codec="hevc",
        encoder="hevc_qsv",
        width=1280,
        height=720,
        fps=30,
        bitrate="5M",
        maxrate="8M",
        bufsize="2M",
        experimental=False,
        mode="auto",
    ),
    ScreenProfile(
        name="720p30_h264_qsv",
        codec="h264",
        encoder="h264_qsv",
        width=1280,
        height=720,
        fps=30,
        bitrate="6M",
        maxrate="10M",
        bufsize="2M",
        experimental=False,
        mode="auto",
    ),
    ScreenProfile(
        name="1080p30_h264_qsv",
        codec="h264",
        encoder="h264_qsv",
        width=1920,
        height=1080,
        fps=30,
        bitrate="12M",
        maxrate="16M",
        bufsize="4M",
        experimental=False,
        mode="auto",
    ),
    ScreenProfile(
        name="720p60_h264_qsv",
        codec="h264",
        encoder="h264_qsv",
        width=1280,
        height=720,
        fps=60,
        bitrate="12M",
        maxrate="16M",
        bufsize="4M",
        experimental=False,
        mode="high_fps",
    ),
    ScreenProfile(
        name="1080p60_h264_qsv",
        codec="h264",
        encoder="h264_qsv",
        width=1920,
        height=1080,
        fps=60,
        bitrate="24M",
        maxrate="32M",
        bufsize="8M",
        experimental=True,
        mode="experimental",
    ),
    ScreenProfile(
        name="1080p60_hevc_qsv",
        codec="hevc",
        encoder="hevc_qsv",
        width=1920,
        height=1080,
        fps=60,
        bitrate="18M",
        maxrate="24M",
        bufsize="6M",
        experimental=True,
        mode="experimental",
    ),
)

PROFILES_BY_NAME: Dict[str, ScreenProfile] = {profile.name: profile for profile in BUILTIN_PROFILES}

AUTO_PROFILE_ORDER: Tuple[str, ...] = (
    "720p30_hevc_qsv",
    "720p30_h264_qsv",
    "1080p30_h264_qsv",
)

HIGH_FPS_PROFILE_ORDER: Tuple[str, ...] = (
    "720p60_h264_qsv",
    *AUTO_PROFILE_ORDER,
)

EXPERIMENTAL_PROFILE_ORDER: Tuple[str, ...] = (
    "1080p60_hevc_qsv",
    "1080p60_h264_qsv",
    "720p60_h264_qsv",
    *AUTO_PROFILE_ORDER,
)


def normalize_profile_name(value: object) -> str:
    text = str(value or "").strip().lower()
    text = re.sub(r"[^a-z0-9]+", "_", text)
    text = re.sub(r"_+", "_", text).strip("_")
    return text


def _profile_display_name(profile: ScreenProfile) -> str:
    return profile.name.replace("_", " ")


def _as_mapping(value: object) -> Mapping[str, Any]:
    return value if isinstance(value, Mapping) else {}


def _as_sequence(value: object) -> Sequence[Any]:
    return value if isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)) else ()


def _bool_at(mapping: Mapping[str, Any], key: str) -> bool:
    return bool(mapping.get(key, False))


def _find_profile_test(caps: Mapping[str, Any], profile: ScreenProfile) -> Optional[Mapping[str, Any]]:
    wanted = normalize_profile_name(profile.name)
    display = normalize_profile_name(_profile_display_name(profile))
    for item in _as_sequence(caps.get("tests")):
        test = _as_mapping(item)
        name = normalize_profile_name(test.get("name"))
        if name in {wanted, display}:
            return test
    return None


def _profile_ok_from_test(test: Mapping[str, Any]) -> bool:
    return bool(test.get("ok")) and not bool(test.get("experimental", False))


def _experimental_profile_ok_from_test(test: Mapping[str, Any], profile: ScreenProfile) -> bool:
    if profile.experimental:
        return bool(test.get("ok"))
    return _profile_ok_from_test(test)


def _encoder_status(caps: Mapping[str, Any], encoder: str) -> Tuple[bool, str]:
    encoders = _as_mapping(caps.get("encoders"))
    compiled = _as_mapping(encoders.get("compiled"))
    runtime = _as_mapping(encoders.get("runtime_test_ok"))
    usable = _as_mapping(encoders.get("usable"))

    if not _bool_at(compiled, encoder):
        return False, "encoder_not_compiled"
    if _bool_at(runtime, encoder) or _bool_at(usable, encoder):
        return True, "encoder_runtime_ok"
    reason = _as_mapping(encoders.get("runtime_reason")).get(encoder)
    return False, str(reason or "encoder_runtime_not_ok")


def _caps_support_profile(caps: Mapping[str, Any], profile: ScreenProfile) -> Tuple[bool, str]:
    if not caps:
        return False, "missing_capabilities"

    test = _find_profile_test(caps, profile)
    if test is not None:
        ok = _experimental_profile_ok_from_test(test, profile)
        if ok:
            return True, "profile_test_ok"
        return False, str(test.get("reason") or "profile_test_not_ok")

    ok, reason = _encoder_status(caps, profile.encoder)
    if ok:
        return True, reason
    return False, reason


def _candidate_names_for_mode(mode: str) -> Tuple[str, ...]:
    normalized = normalize_profile_name(mode or "auto")
    if normalized == "auto":
        return AUTO_PROFILE_ORDER
    if normalized in {"high_fps", "fps60"}:
        return HIGH_FPS_PROFILE_ORDER
    if normalized in {"experimental", "1080p60"}:
        return EXPERIMENTAL_PROFILE_ORDER
    if normalized in PROFILES_BY_NAME:
        return (normalized,)
    return AUTO_PROFILE_ORDER


def _candidate_reason(local_ok: bool, remote_ok: bool, local_reason: str, remote_reason: str) -> str:
    if local_ok and remote_ok:
        return "ok"
    if not local_ok and not remote_ok:
        return f"local:{local_reason}; remote:{remote_reason}"
    if not local_ok:
        return f"local:{local_reason}"
    return f"remote:{remote_reason}"


def choose_profile(local_caps: Mapping[str, Any], remote_caps: Mapping[str, Any], mode: str = "auto") -> Dict[str, object]:
    """Choose a profile from local and remote capability JSON.

    The default auto mode is intentionally stability-first and never promotes
    1080p60 experimental profiles.
    """

    candidates: List[Dict[str, object]] = []
    selected: Optional[ScreenProfile] = None

    for name in _candidate_names_for_mode(mode):
        profile = PROFILES_BY_NAME.get(name)
        if profile is None:
            continue
        local_ok, local_reason = _caps_support_profile(local_caps, profile)
        remote_ok, remote_reason = _caps_support_profile(remote_caps, profile)
        ok = bool(local_ok and remote_ok)
        candidate = {
            "name": profile.name,
            "mode": profile.mode,
            "experimental": profile.experimental,
            "local_ok": local_ok,
            "remote_ok": remote_ok,
            "ok": ok,
            "reason": _candidate_reason(local_ok, remote_ok, local_reason, remote_reason),
        }
        candidates.append(candidate)
        if selected is None and ok:
            selected = profile

    for candidate in candidates:
        candidate["selected"] = bool(selected and candidate.get("name") == selected.name)

    reason = "ok" if selected else "no_common_realtime_screen_profile"
    return {
        "ok": selected is not None,
        "mode": normalize_profile_name(mode or "auto"),
        "recommended": selected.name if selected else None,
        "profile": selected.to_dict() if selected else None,
        "reason": reason,
        "candidates": candidates,
    }


def _load_json_file(path: str) -> Mapping[str, Any]:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, Mapping):
        raise ValueError(f"{path} must contain a JSON object")
    return data


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Choose an AgoraLink screen casting profile.")
    parser.add_argument("--local", required=True, help="Local screen_capability.py JSON output.")
    parser.add_argument("--remote", required=True, help="Remote screen_capability.py JSON output.")
    parser.add_argument("--mode", default="auto", help="auto, high_fps, experimental, or an explicit profile name.")
    parser.add_argument("--indent", type=int, default=2, help="JSON indentation. Use 0 for compact JSON.")
    return parser


def main() -> int:
    parser = build_argparser()
    args = parser.parse_args()
    try:
        local_caps = _load_json_file(args.local)
        remote_caps = _load_json_file(args.remote)
        result = choose_profile(local_caps, remote_caps, mode=str(args.mode or "auto"))
    except Exception as exc:
        result = {
            "ok": False,
            "mode": normalize_profile_name(getattr(args, "mode", "auto")),
            "recommended": None,
            "profile": None,
            "reason": "input_error",
            "error": str(exc),
        }
        indent = None if int(args.indent or 0) <= 0 else int(args.indent)
        print(json.dumps(result, ensure_ascii=False, indent=indent))
        return 2

    indent = None if int(args.indent or 0) <= 0 else int(args.indent)
    print(json.dumps(result, ensure_ascii=False, indent=indent))
    return 0 if result.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
