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
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple


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
    priority: int = 0
    label: str = ""

    def to_dict(self) -> Dict[str, object]:
        data = asdict(self)
        data["id"] = self.name
        if not data.get("label"):
            data["label"] = _profile_label(self)
        return data


BUILTIN_PROFILES: Tuple[ScreenProfile, ...] = (
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
        priority=100,
        label="720p30 H.264 QSV",
    ),
    ScreenProfile(
        name="720p30_h264_nvenc",
        codec="h264",
        encoder="h264_nvenc",
        width=1280,
        height=720,
        fps=30,
        bitrate="6M",
        maxrate="10M",
        bufsize="2M",
        experimental=False,
        mode="auto",
        priority=95,
        label="720p30 H.264 NVENC",
    ),
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
        priority=90,
        label="720p30 HEVC QSV",
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
        priority=80,
        label="1080p30 H.264 QSV",
    ),
    ScreenProfile(
        name="1080p30_h264_nvenc",
        codec="h264",
        encoder="h264_nvenc",
        width=1920,
        height=1080,
        fps=30,
        bitrate="12M",
        maxrate="16M",
        bufsize="4M",
        experimental=False,
        mode="auto",
        priority=75,
        label="1080p30 H.264 NVENC",
    ),
    ScreenProfile(
        name="720p30_h264_software",
        codec="h264",
        encoder="libx264",
        width=1280,
        height=720,
        fps=30,
        bitrate="5M",
        maxrate="8M",
        bufsize="2M",
        experimental=False,
        mode="fallback",
        priority=10,
        label="720p30 H.264 Software",
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
        priority=40,
        label="720p60 H.264 QSV",
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
        priority=0,
        label="1080p60 H.264 QSV",
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
        priority=0,
        label="1080p60 HEVC QSV",
    ),
)

PROFILES_BY_NAME: Dict[str, ScreenProfile] = {profile.name: profile for profile in BUILTIN_PROFILES}
DEFAULT_SCREEN_PROFILE = "720p30_h264_qsv"

AUTO_PROFILE_ORDER: Tuple[str, ...] = (
    "720p30_h264_qsv",
    "720p30_h264_nvenc",
    "720p30_hevc_qsv",
    "1080p30_h264_qsv",
    "1080p30_h264_nvenc",
    "720p30_h264_software",
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

NEGOTIATION_PROFILE_ORDER: Tuple[str, ...] = AUTO_PROFILE_ORDER


def normalize_profile_name(value: object) -> str:
    text = str(value or "").strip().lower()
    text = re.sub(r"[^a-z0-9]+", "_", text)
    text = re.sub(r"_+", "_", text).strip("_")
    return text


def _profile_label(profile: ScreenProfile) -> str:
    if profile.label:
        return profile.label
    codec = profile.codec.upper()
    if codec == "H264":
        codec = "H.264"
    resolution = f"{profile.width}x{profile.height}"
    return f"{resolution} {profile.fps} FPS {codec} {profile.encoder}"


def _profile_display_name(profile: ScreenProfile) -> str:
    return profile.name.replace("_", " ")


def profile_id_from_info(value: object, default: str = DEFAULT_SCREEN_PROFILE) -> str:
    if isinstance(value, ScreenProfile):
        return value.name
    if isinstance(value, Mapping):
        for key in ("id", "name", "profile_id", "profile_name", "selected_profile"):
            raw = value.get(key)
            if raw:
                normalized = normalize_profile_name(raw)
                if normalized:
                    return normalized
    normalized = normalize_profile_name(value)
    return normalized or default


def profile_info(profile_id: object) -> Dict[str, object]:
    name = profile_id_from_info(profile_id)
    profile = PROFILES_BY_NAME.get(name) or PROFILES_BY_NAME[DEFAULT_SCREEN_PROFILE]
    return profile.to_dict()


def _profile_advertisement(profile: ScreenProfile) -> Dict[str, object]:
    data = profile.to_dict()
    return {
        "id": profile.name,
        "name": profile.name,
        "codec": profile.codec,
        "encoder": profile.encoder,
        "width": profile.width,
        "height": profile.height,
        "fps": profile.fps,
        "bitrate": profile.bitrate,
        "maxrate": profile.maxrate,
        "bufsize": profile.bufsize,
        "priority": int(profile.priority or 0),
        "label": str(data.get("label") or _profile_label(profile)),
        "experimental": bool(profile.experimental),
        "mode": profile.mode,
    }


def _encoder_runtime_map_from_caps(caps: Mapping[str, Any]) -> Dict[str, bool]:
    encoders = _as_mapping(caps.get("encoders"))
    compiled = _as_mapping(encoders.get("compiled"))
    runtime = _as_mapping(encoders.get("runtime_test_ok"))
    usable = _as_mapping(encoders.get("usable"))
    result: Dict[str, bool] = {}
    for profile in BUILTIN_PROFILES:
        encoder = profile.encoder
        result[encoder] = bool(
            usable.get(encoder)
            or runtime.get(encoder)
            or (encoder == "libx264" and compiled.get(encoder))
        )
    return result


def _probe_encoder_runtime(ffmpeg_path: str, encoders: Iterable[str], runtime_seconds: float) -> Dict[str, bool]:
    try:
        from screen_capability import find_ffmpeg, parse_ffmpeg_encoders, run_encoder_runtime_test
    except Exception:
        return {}

    ffmpeg = find_ffmpeg(ffmpeg_path)
    if not ffmpeg.get("ok"):
        return {}
    path = str(ffmpeg.get("path") or "ffmpeg")
    parsed = parse_ffmpeg_encoders(path)
    compiled = dict(parsed.get("compiled") or {})
    result: Dict[str, bool] = {}
    for encoder in sorted({str(item or "") for item in encoders if str(item or "")}):
        is_compiled = bool(compiled.get(encoder, False))
        if not is_compiled:
            result[encoder] = False
            continue
        try:
            runtime = run_encoder_runtime_test(path, encoder, is_compiled, seconds=max(0.5, min(float(runtime_seconds), 1.5)))
            result[encoder] = bool(runtime.get("runtime_test_ok"))
        except Exception:
            result[encoder] = False
    return result


def get_advertised_profiles(
    capabilities: Optional[Mapping[str, Any]] = None,
    *,
    ffmpeg_path: str = "",
    runtime_seconds: float = 0.75,
    include_experimental: bool = False,
) -> List[Dict[str, object]]:
    """Return sender profiles this machine should advertise in an OFFER.

    Hardware profiles are included only when the corresponding encoder can run
    on this machine. Software H.264 is kept as the lowest-priority fallback.
    """

    caps = _as_mapping(capabilities)
    if caps:
        encoder_ok = _encoder_runtime_map_from_caps(caps)
    else:
        wanted_encoders = {
            profile.encoder
            for profile in BUILTIN_PROFILES
            if include_experimental or not profile.experimental
        }
        encoder_ok = _probe_encoder_runtime(ffmpeg_path, wanted_encoders, runtime_seconds)

    profiles: List[Dict[str, object]] = []
    for name in NEGOTIATION_PROFILE_ORDER:
        profile = PROFILES_BY_NAME.get(name)
        if profile is None:
            continue
        if profile.experimental and not include_experimental:
            continue
        if bool(encoder_ok.get(profile.encoder, False)):
            profiles.append(_profile_advertisement(profile))
    return profiles


def _advertised_profile_id(item: object) -> str:
    return profile_id_from_info(item, default="")


def choose_advertised_profile(
    offered_profiles: Iterable[object],
    local_profiles: Iterable[object],
    *,
    preferred_profile: object = "",
) -> Optional[Dict[str, object]]:
    offered_by_id = {
        _advertised_profile_id(item): dict(item)
        for item in offered_profiles or []
        if isinstance(item, Mapping) and _advertised_profile_id(item)
    }
    local_by_id = {
        _advertised_profile_id(item): dict(item)
        for item in local_profiles or []
        if isinstance(item, Mapping) and _advertised_profile_id(item)
    }
    common = set(offered_by_id).intersection(local_by_id)
    if not common:
        return None

    preferred = profile_id_from_info(preferred_profile, default="")
    if preferred in common:
        profile = PROFILES_BY_NAME.get(preferred)
        if profile and not profile.experimental:
            return _profile_advertisement(profile)

    for name in NEGOTIATION_PROFILE_ORDER:
        profile = PROFILES_BY_NAME.get(name)
        if profile is not None and name in common and not profile.experimental:
            return _profile_advertisement(profile)

    return None


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
