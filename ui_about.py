#!/usr/bin/env python3
"""About-page data helpers kept separate from the Kivy application root."""

from __future__ import annotations

import platform
import sys
from pathlib import Path
from typing import Dict, Mapping


def package_label(flavor: object, lang: str = "en") -> str:
    value = str(flavor or "").strip().lower()
    labels = {
        "native": ("Native", "Native"),
        "native_lite": ("Native Lite", "Native Lite"),
        "full": ("完整包", "Full"),
        "source": ("源码模式", "Source"),
    }
    zh, en = labels.get(value, ("未知", "Unknown"))
    return en if str(lang).lower().startswith("en") else zh


def build_about_context(
    *,
    app_version: str,
    build_label: str,
    build_date: str,
    package_flavor: object,
    capabilities: Mapping[str, object],
    app_dir: object,
    user_data_dir: object,
    debug_log_dir: object,
    git_commit: str = "",
    lang: str = "en",
) -> Dict[str, object]:
    native_available = bool(capabilities.get("rust_native_available"))
    capture_available = bool(capabilities.get("rust_audio_capture_available"))
    playback_available = bool(capabilities.get("rust_audio_playback_available"))
    av_sync = bool(capabilities.get("native_screen_av_sync_supported"))
    short_commit = str(git_commit or "").strip()[:12] or "-"
    component_summary = (
        f"Python {sys.version.split()[0]} · Kivy · Rust native media "
        f"({'available' if native_available else 'unavailable'})"
    )
    technical = "\n".join(
        (
            f"Version: {app_version}",
            f"Build: {build_label}",
            f"Build date: {build_date}",
            f"Commit: {git_commit or '-'}",
            f"Package: {package_label(package_flavor, lang)}",
            f"Python: {sys.version}",
            f"Executable: {sys.executable}",
            f"Platform: {platform.platform()}",
            f"Application directory: {Path(app_dir)}",
            f"User data directory: {Path(user_data_dir)}",
            f"Log directory: {Path(debug_log_dir)}",
            f"Rust native available: {native_available}",
            f"Audio capture available: {capture_available}",
            f"Audio playback available: {playback_available}",
            f"Native A/V sync supported: {av_sync}",
        )
    )
    return {
        "about_version": str(app_version),
        "about_build_commit": short_commit,
        "about_build_date": str(build_date),
        "about_package": package_label(package_flavor, lang),
        "about_project": "AtticusPhu/AgoraLink",
        "about_license": "See repository",
        "about_components": component_summary,
        "about_full_technical_info": technical,
    }
