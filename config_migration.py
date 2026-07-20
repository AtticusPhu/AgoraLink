#!/usr/bin/env python3
"""One-way migration for media settings removed in v0.0.12."""

from __future__ import annotations

from typing import Dict, Mapping, Tuple


NATIVE_SCREEN_BACKEND = "rust"
LEGACY_SCREEN_BACKENDS = frozenset({"ffmpeg", "external"})
LEGACY_MEDIA_CONFIG_KEYS = frozenset(
    {
        "backend",
        "ffmpeg_path",
        "ffprobe_path",
        "ffplay_path",
        "source_mode",
        "external_media_backend",
    }
)
MIGRATION_WARNING = "legacy_ffmpeg_config_migrated_to_native"


def migrate_legacy_media_config(
    value: Mapping[str, object] | None,
) -> Tuple[Dict[str, object], bool]:
    """Return a native-only config and whether obsolete fields were removed."""
    migrated = False
    result = dict(value or {})
    has_legacy_fields = any(key in result for key in LEGACY_MEDIA_CONFIG_KEYS)
    backend = str(
        result.get("screen_backend") or result.get("backend") or ""
    ).strip().lower()
    if (
        backend in LEGACY_SCREEN_BACKENDS
        or backend not in ("", NATIVE_SCREEN_BACKEND)
        or has_legacy_fields
    ):
        migrated = True
        result["screen_backend"] = NATIVE_SCREEN_BACKEND
    for key in LEGACY_MEDIA_CONFIG_KEYS:
        if key in result:
            result.pop(key, None)
            migrated = True
    return result, migrated
