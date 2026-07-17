#!/usr/bin/env python3
"""Shared AgoraLink application paths and app metadata helpers."""

from __future__ import annotations

import os
import platform
import sys
import time
from pathlib import Path
from typing import Dict, Optional


APP_NAME = "AgoraLink"
APP_VERSION = "v0.0.11"
IS_WINDOWS = os.name == "nt"
FROZEN = bool(getattr(sys, "frozen", False))
APP_DIR = Path(sys.executable).resolve().parent if FROZEN else Path(__file__).resolve().parent
RESOURCE_DIR = Path(getattr(sys, "_MEIPASS", APP_DIR))


def user_data_dir() -> Path:
    if IS_WINDOWS:
        base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
        path = Path(base) / APP_NAME
    elif sys.platform == "darwin":
        path = Path.home() / "Library" / "Application Support" / APP_NAME
    else:
        path = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local" / "share"))) / APP_NAME
    path.mkdir(parents=True, exist_ok=True)
    return path


def debug_log_dir() -> Path:
    path = user_data_dir() / "debug"
    path.mkdir(parents=True, exist_ok=True)
    return path


def diagnostics_dir() -> Path:
    path = debug_log_dir() / "diagnostics"
    path.mkdir(parents=True, exist_ok=True)
    return path


def temp_dir() -> Path:
    path = user_data_dir() / "temp"
    path.mkdir(parents=True, exist_ok=True)
    return path


def get_app_info(*, version: str = APP_VERSION, git_commit: Optional[str] = None) -> Dict[str, object]:
    return {
        "app": APP_NAME,
        "version": str(version or APP_VERSION),
        "timestamp": time.strftime("%Y-%m-%d %H:%M:%S"),
        "python": sys.version,
        "python_executable": sys.executable,
        "platform": sys.platform,
        "platform_detail": platform.platform(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "frozen": FROZEN,
        "app_dir": str(APP_DIR),
        "resource_dir": str(RESOURCE_DIR),
        "user_data_dir": str(user_data_dir()),
        "debug_log_dir": str(debug_log_dir()),
        "diagnostics_dir": str(diagnostics_dir()),
        "temp_dir": str(temp_dir()),
        "cwd": os.getcwd(),
        "git_commit": git_commit,
    }
