#!/usr/bin/env python3
"""Small UDP port probing helpers for AgoraLink.

The checks bind a temporary UDP socket and close it immediately. They do not
enable port reuse and do not keep any socket alive after the function returns.
"""

from __future__ import annotations

import socket
from typing import Iterable, List, Optional


def _validate_port(port: object) -> int:
    value = int(port)
    if value < 1 or value > 65535:
        raise ValueError("port must be in 1..65535")
    return value


def udp_port_status(port: object, bind_host: str = "0.0.0.0") -> dict:
    host = str(bind_host or "0.0.0.0").strip() or "0.0.0.0"
    result = {
        "port": None,
        "protocol": "udp",
        "bind_host": host,
        "available": False,
        "occupied": False,
        "error": "",
    }
    sock = None
    try:
        port_num = _validate_port(port)
        result["port"] = port_num
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.bind((host, port_num))
        result["available"] = True
        result["occupied"] = False
    except OSError as exc:
        result["occupied"] = True
        result["error"] = str(exc)
    except Exception as exc:
        result["error"] = str(exc)
    finally:
        if sock is not None:
            try:
                sock.close()
            except Exception:
                pass
    return result


def is_udp_port_available(port: object, bind_host: str = "0.0.0.0") -> bool:
    return bool(udp_port_status(port, bind_host).get("available"))


def udp_ports_status(ports: Iterable[object], bind_host: str = "0.0.0.0") -> List[dict]:
    return [udp_port_status(port, bind_host) for port in ports]


def find_available_udp_port(ports: Iterable[object], bind_host: str = "0.0.0.0") -> Optional[int]:
    for item in udp_ports_status(ports, bind_host):
        if item.get("available"):
            try:
                return int(item.get("port"))
            except Exception:
                return None
    return None

