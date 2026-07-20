#!/usr/bin/env python3
"""Native screen profile negotiation for AgoraLink control messages."""

from __future__ import annotations

import argparse
import json
import re
from collections.abc import Mapping
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
        name="r4_default",
        codec="h264",
        encoder="auto",
        width=1920,
        height=1080,
        fps=60,
        bitrate="22M",
        maxrate="22M",
        bufsize="44M",
        experimental=False,
        mode="fixed",
        priority=100,
        label="R4 Default 1080p60 / 22 Mbps",
    ),
    ScreenProfile(
        name="stable",
        codec="h264",
        encoder="auto",
        width=1280,
        height=720,
        fps=30,
        bitrate="20M",
        maxrate="20M",
        bufsize="40M",
        experimental=False,
        mode="stable",
        priority=90,
        label="Stable 720p30 / 20 Mbps",
    ),
    ScreenProfile(
        name="recommended",
        codec="h264",
        encoder="auto",
        width=1920,
        height=1080,
        fps=60,
        bitrate="50M",
        maxrate="50M",
        bufsize="100M",
        experimental=False,
        mode="high_bandwidth",
        priority=80,
        label="Recommended 1080p60 / 50 Mbps",
    ),
    ScreenProfile(
        name="high_quality",
        codec="h264",
        encoder="auto",
        width=1920,
        height=1080,
        fps=60,
        bitrate="80M",
        maxrate="80M",
        bufsize="160M",
        experimental=True,
        mode="experimental",
        priority=10,
        label="High Quality 1080p60 / 80 Mbps",
    ),
)

PROFILES_BY_NAME: Dict[str, ScreenProfile] = {
    profile.name: profile for profile in BUILTIN_PROFILES
}
DEFAULT_SCREEN_PROFILE = "r4_default"
NEGOTIATION_PROFILE_ORDER: Tuple[str, ...] = (
    "r4_default",
    "stable",
    "recommended",
)


def normalize_profile_name(value: object) -> str:
    text = str(value or "").strip().lower()
    text = re.sub(r"[^a-z0-9]+", "_", text)
    return re.sub(r"_+", "_", text).strip("_")


def _profile_label(profile: ScreenProfile) -> str:
    if profile.label:
        return profile.label
    return (
        f"{profile.width}x{profile.height} {profile.fps} FPS "
        f"{profile.codec.upper()}"
    )


def profile_id_from_info(
    value: object,
    default: str = DEFAULT_SCREEN_PROFILE,
) -> str:
    if isinstance(value, ScreenProfile):
        return value.name
    if isinstance(value, Mapping):
        for key in ("id", "name", "profile_id", "profile_name", "selected_profile"):
            raw = value.get(key)
            if raw:
                normalized = normalize_profile_name(raw)
                if normalized:
                    return normalized
    return normalize_profile_name(value) or default


def profile_info(profile_id: object) -> Dict[str, object]:
    name = profile_id_from_info(profile_id)
    profile = PROFILES_BY_NAME.get(name) or PROFILES_BY_NAME[DEFAULT_SCREEN_PROFILE]
    return profile.to_dict()


def _profile_advertisement(profile: ScreenProfile) -> Dict[str, object]:
    return profile.to_dict()


def get_advertised_profiles(
    capabilities: Optional[Mapping[str, Any]] = None,
    *,
    include_experimental: bool = False,
) -> List[Dict[str, object]]:
    """Return the built-in native profiles advertised in an OFFER.

    ``capabilities`` is accepted for API compatibility. Native runtime startup
    performs the actual WGC/D3D11/WMF capability check and reports a clear error
    if the device cannot start the selected profile.
    """
    del capabilities
    profiles = [
        _profile_advertisement(PROFILES_BY_NAME[name])
        for name in NEGOTIATION_PROFILE_ORDER
    ]
    if include_experimental:
        profiles.append(_profile_advertisement(PROFILES_BY_NAME["high_quality"]))
    return profiles


def _advertised_profile_id(item: object) -> str:
    return profile_id_from_info(item, default="")


def choose_advertised_profile(
    offered_profiles: Iterable[object],
    local_profiles: Iterable[object],
    *,
    preferred_profile: object = "",
) -> Optional[Dict[str, object]]:
    offered = {
        _advertised_profile_id(item)
        for item in offered_profiles or []
        if isinstance(item, Mapping) and _advertised_profile_id(item)
    }
    local = {
        _advertised_profile_id(item)
        for item in local_profiles or []
        if isinstance(item, Mapping) and _advertised_profile_id(item)
    }
    common = offered.intersection(local)
    preferred = profile_id_from_info(preferred_profile, default="")
    if preferred in common and preferred in PROFILES_BY_NAME:
        return _profile_advertisement(PROFILES_BY_NAME[preferred])
    for name in NEGOTIATION_PROFILE_ORDER:
        if name in common:
            return _profile_advertisement(PROFILES_BY_NAME[name])
    return None


def _capability_profile_ids(capabilities: Mapping[str, Any]) -> set[str]:
    values = capabilities.get("profiles") if isinstance(capabilities, Mapping) else None
    if not isinstance(values, (list, tuple)):
        return set(NEGOTIATION_PROFILE_ORDER)
    return {
        profile_id_from_info(item, default="")
        for item in values
        if profile_id_from_info(item, default="")
    }


def choose_profile(
    local_caps: Mapping[str, Any],
    remote_caps: Mapping[str, Any],
    mode: str = "auto",
) -> Dict[str, object]:
    local = _capability_profile_ids(local_caps)
    remote = _capability_profile_ids(remote_caps)
    common = local.intersection(remote)
    requested = normalize_profile_name(mode or "auto")
    order = (
        (requested,)
        if requested in PROFILES_BY_NAME
        else NEGOTIATION_PROFILE_ORDER
    )
    selected = next((PROFILES_BY_NAME[name] for name in order if name in common), None)
    candidates = [
        {
            "name": name,
            "ok": name in common,
            "selected": bool(selected and selected.name == name),
            "reason": "ok" if name in common else "not_advertised_by_both_peers",
        }
        for name in order
    ]
    return {
        "ok": selected is not None,
        "mode": requested or "auto",
        "recommended": selected.name if selected else None,
        "profile": selected.to_dict() if selected else None,
        "reason": "ok" if selected else "no_common_native_screen_profile",
        "candidates": candidates,
    }


def _load_json_file(path: str) -> Mapping[str, Any]:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, Mapping):
        raise ValueError(f"{path} must contain a JSON object")
    return data


def build_argparser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Choose an AgoraLink native screen profile."
    )
    parser.add_argument("--local", required=True, help="Local native capability JSON.")
    parser.add_argument("--remote", required=True, help="Remote native capability JSON.")
    parser.add_argument("--mode", default="auto")
    parser.add_argument("--indent", type=int, default=2)
    return parser


def main() -> int:
    args = build_argparser().parse_args()
    try:
        result = choose_profile(
            _load_json_file(args.local),
            _load_json_file(args.remote),
            mode=str(args.mode or "auto"),
        )
    except Exception as exc:
        result = {
            "ok": False,
            "mode": normalize_profile_name(args.mode or "auto"),
            "recommended": None,
            "profile": None,
            "reason": "input_error",
            "error": str(exc),
        }
    indent = None if int(args.indent or 0) <= 0 else int(args.indent)
    print(json.dumps(result, ensure_ascii=False, indent=indent))
    return 0 if result.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
