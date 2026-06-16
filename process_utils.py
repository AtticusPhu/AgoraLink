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


def popen_no_console(
    args,
    *popen_args,
    popen_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    **kwargs,
):
    """Start a process without creating a console window on Windows."""
    return popen_factory(args, *popen_args, **_apply_no_window_kwargs(kwargs))


def run_no_console(
    args,
    *run_args,
    run_factory: Callable[..., subprocess.CompletedProcess] = subprocess.run,
    **kwargs,
):
    """Run a process without creating a console window on Windows."""
    return run_factory(args, *run_args, **_apply_no_window_kwargs(kwargs))
