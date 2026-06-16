#!/usr/bin/env python3
"""File packaging helpers for AgoraLink UI.

This module prepares a multi-file selection as one ZIP so the existing single
file transfer path can be reused without touching the RUDP protocol.
"""

from __future__ import annotations

import os
import time
import zipfile
from pathlib import Path
from typing import Dict, Iterable, List


def _default_output_dir() -> Path:
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
        path = Path(base) / "AgoraLink" / "temp"
    else:
        path = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "AgoraLink" / "temp"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _unique_arcname(used: set[str], name: str) -> str:
    arc = str(name or "unnamed").replace("\\", "/").strip("/") or "unnamed"
    if arc not in used:
        used.add(arc)
        return arc
    base, ext = os.path.splitext(arc)
    idx = 2
    while True:
        candidate = f"{base}_{idx}{ext}"
        if candidate not in used:
            used.add(candidate)
            return candidate
        idx += 1


def _unique_output_path(output_dir: Path) -> Path:
    stamp = time.strftime("%Y%m%d_%H%M%S")
    path = output_dir / f"AgoraLink_files_{stamp}.zip"
    if not path.exists():
        return path
    idx = 2
    while True:
        candidate = output_dir / f"AgoraLink_files_{stamp}_{idx}.zip"
        if not candidate.exists():
            return candidate
        idx += 1


def _normalize_files(paths: Iterable[str]) -> List[Path]:
    files: List[Path] = []
    seen: set[str] = set()
    for raw in paths or []:
        try:
            path = Path(str(raw or "")).expanduser().resolve()
        except Exception:
            continue
        key = os.path.normcase(str(path))
        if key in seen:
            continue
        seen.add(key)
        if path.is_dir():
            raise IsADirectoryError("不支持文件夹，请选择文件")
        if not path.is_file():
            raise FileNotFoundError(f"文件不存在: {path}")
        files.append(path)
    return files


def package_files_to_zip(paths: list[str], output_dir: str | None = None) -> Dict[str, object]:
    """Package files into one ZIP and return transfer-ready metadata."""
    try:
        files = _normalize_files(paths)
        if not files:
            return {"ok": False, "zip_path": "", "file_count": 0, "total_bytes": 0, "error": "未选择文件"}
        out_dir = Path(output_dir).expanduser().resolve() if output_dir else _default_output_dir()
        out_dir.mkdir(parents=True, exist_ok=True)
        zip_path = _unique_output_path(out_dir)
        total = 0
        used: set[str] = set()
        with zipfile.ZipFile(zip_path, "w", compression=zipfile.ZIP_STORED, allowZip64=True) as zf:
            for path in files:
                size = int(path.stat().st_size)
                total += size
                zf.write(str(path), _unique_arcname(used, path.name))
        return {
            "ok": True,
            "zip_path": str(zip_path),
            "file_count": len(files),
            "total_bytes": total,
            "error": None,
        }
    except Exception as exc:
        return {"ok": False, "zip_path": "", "file_count": 0, "total_bytes": 0, "error": str(exc)}
