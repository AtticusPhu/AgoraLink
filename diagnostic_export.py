#!/usr/bin/env python3
"""AgoraLink diagnostic bundle exporter."""

from __future__ import annotations

import json
import os
import socket
import time
import zipfile
from pathlib import Path
from typing import Dict, Optional

from app_paths import (
    APP_DIR,
    APP_VERSION,
    FROZEN,
    debug_log_dir,
    diagnostics_dir,
    get_app_info,
)
from port_utils import udp_port_status
from process_utils import run_no_console

MAX_LOG_BYTES = 5 * 1024 * 1024
DEFAULT_PORTS = (9999, 50020)


def _json_default(value):
    try:
        if isinstance(value, Path):
            return str(value)
    except Exception:
        pass
    return str(value)


def _write_json(zf: zipfile.ZipFile, arcname: str, data: object) -> None:
    zf.writestr(arcname, json.dumps(data, ensure_ascii=False, indent=2, default=_json_default))


def _read_tail_bytes(path: Path, max_bytes: int = MAX_LOG_BYTES) -> bytes:
    size = path.stat().st_size
    with path.open("rb") as f:
        if size > max_bytes:
            f.seek(-max_bytes, os.SEEK_END)
        return f.read(max_bytes)


def _add_log_file(zf: zipfile.ZipFile, path: Path, arcname: str) -> None:
    try:
        if path.exists() and path.is_file():
            zf.writestr(arcname, _read_tail_bytes(path))
    except Exception as exc:
        zf.writestr(f"errors/{path.name}.txt", f"failed to add log {path}: {exc}\n")


def _git_commit_hash() -> Optional[str]:
    if FROZEN:
        return None
    try:
        proc = run_no_console(
            ["git", "rev-parse", "HEAD"],
            cwd=str(APP_DIR),
            capture_output=True,
            text=True,
            timeout=3,
        )
        if proc.returncode == 0:
            value = str(proc.stdout or "").strip()
            return value or None
    except Exception:
        return None
    return None


def _app_info() -> Dict[str, object]:
    return get_app_info(version=APP_VERSION, git_commit=_git_commit_hash())


def _network_info() -> Dict[str, object]:
    info: Dict[str, object] = {
        "hostname": "",
        "fqdn": "",
        "addresses": [],
        "route_probe": {},
    }
    try:
        hostname = socket.gethostname()
        info["hostname"] = hostname
        info["fqdn"] = socket.getfqdn()
        addresses = set()
        try:
            for item in socket.getaddrinfo(hostname, None):
                sockaddr = item[4]
                if sockaddr:
                    addresses.add(str(sockaddr[0]))
        except Exception as exc:
            info["getaddrinfo_error"] = str(exc)
        info["addresses"] = sorted(addresses)
    except Exception as exc:
        info["hostname_error"] = str(exc)

    probes = []
    for target in ("8.8.8.8", "1.1.1.1"):
        sock = None
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            sock.settimeout(0.5)
            sock.connect((target, 80))
            probes.append({"target": target, "local": sock.getsockname()[0]})
        except Exception as exc:
            probes.append({"target": target, "error": str(exc)})
        finally:
            if sock is not None:
                try:
                    sock.close()
                except Exception:
                    pass
    info["route_probe"] = probes
    return info


def _ports_info() -> Dict[str, object]:
    ports = []
    for port in DEFAULT_PORTS:
        item = dict(udp_port_status(port))
        if item.get("available"):
            item["status"] = "available"
        elif item.get("occupied"):
            item["status"] = "in_use_or_blocked"
        else:
            item["status"] = "error"
        ports.append(item)
    return {"ports": ports}


def _screen_runtime_or_new(screen_runtime=None):
    if screen_runtime is not None:
        return screen_runtime
    from screen_runtime import ScreenRuntime

    return ScreenRuntime()


def _screen_state(screen_runtime=None) -> Dict[str, object]:
    try:
        runtime = _screen_runtime_or_new(screen_runtime)
        if hasattr(runtime, "get_state"):
            return dict(runtime.get_state())
        return {"error": "screen runtime has no get_state"}
    except Exception as exc:
        return {"error": str(exc)}


def _screen_dependencies(screen_runtime=None) -> Dict[str, object]:
    try:
        runtime = _screen_runtime_or_new(screen_runtime)
        if hasattr(runtime, "check_dependencies"):
            return dict(runtime.check_dependencies())
        return {"error": "screen runtime has no check_dependencies"}
    except Exception as exc:
        return {"error": str(exc)}


def _unique_zip_path(out_dir: Path, stamp: str) -> Path:
    base = out_dir / f"AgoraLink_diagnostic_{stamp}.zip"
    if not base.exists():
        return base
    for i in range(1, 1000):
        candidate = out_dir / f"AgoraLink_diagnostic_{stamp}_{i}.zip"
        if not candidate.exists():
            return candidate
    return base


def export_diagnostic_bundle(
    output_dir=None,
    *,
    screen_runtime=None,
    extra_json: Optional[Dict[str, object]] = None,
    extra_text: Optional[Dict[str, str]] = None,
) -> str:
    out_dir = Path(output_dir).expanduser() if output_dir else diagnostics_dir()
    out_dir.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d_%H%M%S")
    out_path = _unique_zip_path(out_dir, stamp)

    debug_dir = debug_log_dir()
    log_paths = []
    for name in ("sender_worker.log", "receiver_worker.log"):
        log_paths.append(debug_dir / name)
    try:
        for path in sorted(debug_dir.glob("*.log")):
            if path not in log_paths:
                log_paths.append(path)
    except Exception:
        pass

    with zipfile.ZipFile(out_path, "w", compression=zipfile.ZIP_DEFLATED, allowZip64=True) as zf:
        for path in log_paths:
            _add_log_file(zf, path, f"debug/{path.name}")

        _write_json(zf, "screen_runtime_state.json", _screen_state(screen_runtime))
        _write_json(zf, "screen_dependencies.json", _screen_dependencies(screen_runtime))
        _write_json(zf, "network_info.json", _network_info())
        _write_json(zf, "ports.json", _ports_info())
        _write_json(zf, "app_info.json", _app_info())

        for arcname, data in (extra_json or {}).items():
            safe_name = str(arcname or "").strip().replace("\\", "/")
            if safe_name:
                _write_json(zf, safe_name, data)

        for arcname, text in (extra_text or {}).items():
            safe_name = str(arcname or "").strip().replace("\\", "/")
            if safe_name:
                raw = str(text or "").encode("utf-8", errors="replace")
                if len(raw) > MAX_LOG_BYTES:
                    raw = raw[-MAX_LOG_BYTES:]
                zf.writestr(safe_name, raw)

    return str(out_path)
