#!/usr/bin/env python3
"""Process launch helpers shared by AgoraLink GUI and screen runtime."""

from __future__ import annotations

import os
import subprocess
from typing import Callable, Dict, Optional


def make_no_window_startupinfo() -> Optional[subprocess.STARTUPINFO]:
    """Build Windows startupinfo that hides console windows only."""
    if os.name != "nt":
        return None
    startupinfo = subprocess.STARTUPINFO()
    startupinfo.dwFlags |= getattr(subprocess, "STARTF_USESHOWWINDOW", 1)
    startupinfo.wShowWindow = getattr(subprocess, "SW_HIDE", 0)
    return startupinfo


def get_no_window_creationflags() -> int:
    if os.name != "nt":
        return 0
    return int(getattr(subprocess, "CREATE_NO_WINDOW", 0) or 0)


def get_detached_process_creationflags() -> int:
    if os.name != "nt":
        return 0
    return int(getattr(subprocess, "DETACHED_PROCESS", 0) or 0)


def _apply_no_window_kwargs(kwargs: Dict[str, object]) -> Dict[str, object]:
    if os.name != "nt":
        return kwargs
    fixed = dict(kwargs)
    fixed["creationflags"] = int(fixed.get("creationflags") or 0) | get_no_window_creationflags()
    startupinfo = fixed.get("startupinfo") or make_no_window_startupinfo()
    if startupinfo is not None:
        try:
            startupinfo.dwFlags |= getattr(subprocess, "STARTF_USESHOWWINDOW", 1)
            startupinfo.wShowWindow = getattr(subprocess, "SW_HIDE", 0)
        except Exception:
            pass
        fixed["startupinfo"] = startupinfo
    return fixed


def _remove_window_hiding_kwargs(kwargs: Dict[str, object]) -> Dict[str, object]:
    fixed = dict(kwargs)
    fixed.pop("startupinfo", None)
    if os.name == "nt":
        fixed.pop("creationflags", None)
    return fixed


def _apply_ffplay_windowed_kwargs(kwargs: Dict[str, object], launch_mode: Optional[str] = None) -> Dict[str, object]:
    """Prepare ffplay kwargs without hiding the SDL video window."""
    fixed = _remove_window_hiding_kwargs(kwargs)
    if os.name != "nt":
        return fixed
    mode = str(launch_mode or os.environ.get("AGORALINK_FFPLAY_LAUNCH_MODE") or "normal").strip().lower()
    if mode == "detached":
        flags = get_detached_process_creationflags()
        if flags:
            fixed["creationflags"] = int(fixed.get("creationflags") or 0) | flags
    elif mode == "no_window":
        flags = get_no_window_creationflags()
        if flags:
            fixed["creationflags"] = int(fixed.get("creationflags") or 0) | flags
    return fixed


def popen_no_console(
    args,
    *popen_args,
    popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    **kwargs,
):
    """Start a process without creating a console window on Windows."""
    return popen_factory(args, *popen_args, **_apply_no_window_kwargs(kwargs))


def popen_ffplay_windowed(
    args,
    *popen_args,
    popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    launch_mode: Optional[str] = None,
    **kwargs,
):
    """Start ffplay without hiding its SDL video window.

    ffplay is special: STARTF_USESHOWWINDOW/SW_HIDE can hide the SDL video
    window along with the console. The default launch mode passes no window
    hiding flags. Set AGORALINK_FFPLAY_LAUNCH_MODE to detached or no_window only
    for local diagnostics.
    """
    return popen_factory(args, *popen_args, **_apply_ffplay_windowed_kwargs(kwargs, launch_mode=launch_mode))


def popen_ffplay_visible(
    args,
    *popen_args,
    popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    **kwargs,
):
    """Backward-compatible alias for the ffplay windowed launcher."""
    return popen_ffplay_windowed(args, *popen_args, popen_factory=popen_factory, **kwargs)


def popen_ffplay_visible_fallback(
    args,
    *popen_args,
    popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    **kwargs,
):
    """Start ffplay with no Windows window-hiding flags at all."""
    return popen_factory(args, *popen_args, **_remove_window_hiding_kwargs(kwargs))


def run_no_console(
    args,
    *run_args,
    run_factory: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    **kwargs,
):
    """Run a process without creating a console window on Windows."""
    return run_factory(args, *run_args, **_apply_no_window_kwargs(kwargs))
