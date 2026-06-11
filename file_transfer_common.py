#!/usr/bin/env python3
"""Shared utilities for the RUDP file-transfer application."""

from __future__ import annotations

import hashlib
import ipaddress
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, Iterable, List, Optional

APP_HEADER_VERSION = 2
APP_HEADER_COMPAT_VERSIONS = {1, 2}
APP_HEADER_TYPE_FILE = "FILE"
CHAT_MESSAGE_TYPE = "CHAT_MESSAGE"
CHAT_ACK_TYPE = "CHAT_ACK"
CHAT_READ_TYPE = "CHAT_READ"
CONTACT_REQUEST_TYPE = "CONTACT_REQUEST"
CONTACT_RESPONSE_TYPE = "CONTACT_RESPONSE"
CHAT_FRAME_VERSION = 1
EOF_PAYLOAD = b"__RUDP_FILE_EOF__"
SEQ_HEADER = 1
SEQ_FIRST_BODY = 2
DATA_SEQ_UPPER_EXCLUSIVE = 1 << 63
MAX_DATA_SEQ = DATA_SEQ_UPPER_EXCLUSIVE - 1

DISCOVERY_MAGIC = "RUDP_DISCOVER_V1"
DISCOVERY_RESPONSE_MAGIC = "RUDP_RECEIVER_V1"
DEFAULT_DISCOVERY_PORT = 9998
TRANSFER_DECISION_MAGIC = "RUDP_TRANSFER_DECISION_V1"
TRANSFER_REQUEST_LOG_PREFIX = "TRANSFER_REQUEST_JSON:"
TRANSFER_STARTED_LOG_PREFIX = "TRANSFER_STARTED_JSON:"
TRANSFER_PROGRESS_LOG_PREFIX = "TRANSFER_PROGRESS_JSON:"
TRANSFER_SAVED_LOG_PREFIX = "TRANSFER_SAVED_JSON:"
TRANSFER_COMPLETE_LOG_PREFIX = "TRANSFER_COMPLETE_JSON:"
TRANSFER_FAILED_LOG_PREFIX = "TRANSFER_FAILED_JSON:"
USER_ERROR_LOG_PREFIX = "USER_ERROR_JSON:"
USER_STATUS_LOG_PREFIX = "USER_STATUS_JSON:"
CHAT_MESSAGE_LOG_PREFIX = "CHAT_MESSAGE_JSON:"
CHAT_ACK_LOG_PREFIX = "CHAT_ACK_JSON:"
CHAT_READ_LOG_PREFIX = "CHAT_READ_JSON:"
CONTACT_REQUEST_LOG_PREFIX = "CONTACT_REQUEST_JSON:"
CONTACT_RESPONSE_LOG_PREFIX = "CONTACT_RESPONSE_JSON:"


def is_unspecified_ip(value: str) -> bool:
    text = str(value or "").strip().lower()
    return text in ("", "0.0.0.0", "::", "[::]", "none", "null")


def normalize_peer_endpoint_ip(value: str, fallback: str = "") -> str:
    text = str(value or "").strip()
    if is_unspecified_ip(text):
        return str(fallback or "").strip()
    return text


def _new_chat_id(prefix: str) -> str:
    import secrets
    return str(prefix or "id") + "_" + secrets.token_hex(16)



def build_contact_request(*, request_id: str = "", sender_peer_id: str = "", sender_nickname: str = "", sender_fingerprint: str = "", sender_ip: str = "", sender_port: int = 9999, message: str = "") -> bytes:
    obj = {
        "type": CONTACT_REQUEST_TYPE,
        "version": CHAT_FRAME_VERSION,
        "request_id": str(request_id or _new_chat_id("contact_req")),
        "sender_peer_id": str(sender_peer_id or ""),
        "sender_nickname": str(sender_nickname or ""),
        "sender_fingerprint": str(sender_fingerprint or ""),
        "sender_ip": str(sender_ip or ""),
        "sender_port": int(sender_port or 9999),
        "message": str(message or ""),
        "created_at": time.time(),
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_contact_request(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data or b"").decode("utf-8"))
    except Exception as exc:
        raise ValueError("contact_request_not_valid_json") from exc
    if not isinstance(obj, dict) or obj.get("type") != CONTACT_REQUEST_TYPE:
        raise ValueError("unsupported_contact_request_type")
    if int(obj.get("version") or 0) != CHAT_FRAME_VERSION:
        raise ValueError("unsupported_contact_request_version")
    obj["request_id"] = str(obj.get("request_id") or _new_chat_id("contact_req"))
    obj["sender_peer_id"] = str(obj.get("sender_peer_id") or "")
    obj["sender_nickname"] = str(obj.get("sender_nickname") or "")
    obj["sender_fingerprint"] = str(obj.get("sender_fingerprint") or "")
    obj["sender_ip"] = str(obj.get("sender_ip") or "")
    obj["sender_port"] = int(obj.get("sender_port") or 9999)
    obj["message"] = str(obj.get("message") or "")
    obj["created_at"] = float(obj.get("created_at") or time.time())
    return obj


def build_contact_response(request_id: str, *, accepted: bool, receiver_peer_id: str = "", receiver_nickname: str = "", receiver_fingerprint: str = "", receiver_ip: str = "", receiver_port: int = 9999, reason: str = "") -> bytes:
    obj = {
        "type": CONTACT_RESPONSE_TYPE,
        "version": CHAT_FRAME_VERSION,
        "request_id": str(request_id or ""),
        "accepted": bool(accepted),
        "receiver_peer_id": str(receiver_peer_id or ""),
        "receiver_nickname": str(receiver_nickname or ""),
        "receiver_fingerprint": str(receiver_fingerprint or ""),
        "receiver_ip": str(receiver_ip or ""),
        "receiver_port": int(receiver_port or 9999),
        "reason": str(reason or ("accepted" if accepted else "rejected")),
        "created_at": time.time(),
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_contact_response(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data or b"").decode("utf-8"))
    except Exception as exc:
        raise ValueError("contact_response_not_valid_json") from exc
    if not isinstance(obj, dict) or obj.get("type") != CONTACT_RESPONSE_TYPE:
        raise ValueError("unsupported_contact_response_type")
    if int(obj.get("version") or 0) != CHAT_FRAME_VERSION:
        raise ValueError("unsupported_contact_response_version")
    obj["request_id"] = str(obj.get("request_id") or "")
    obj["accepted"] = bool(obj.get("accepted"))
    obj["receiver_peer_id"] = str(obj.get("receiver_peer_id") or "")
    obj["receiver_nickname"] = str(obj.get("receiver_nickname") or "")
    obj["receiver_fingerprint"] = str(obj.get("receiver_fingerprint") or "")
    obj["receiver_ip"] = str(obj.get("receiver_ip") or "")
    obj["receiver_port"] = int(obj.get("receiver_port") or 9999)
    obj["reason"] = str(obj.get("reason") or "")
    obj["created_at"] = float(obj.get("created_at") or time.time())
    return obj


def build_contact_request_log(request: Dict[str, object], peer: str = "") -> str:
    obj = dict(request or {})
    obj["peer"] = str(peer or "")
    obj["logged_at"] = time.time()
    return CONTACT_REQUEST_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def build_contact_response_log(response: Dict[str, object], peer: str = "") -> str:
    obj = dict(response or {})
    obj["peer"] = str(peer or "")
    obj["logged_at"] = time.time()
    return CONTACT_RESPONSE_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))

def build_chat_message(text: str, *, message_id: str = "", conversation_id: str = "", group_id: str = "", sender_peer_id: str = "", receiver_peer_id: str = "", sender_device_id: str = "", body_type: str = "text", created_at: Optional[float] = None) -> bytes:
    body = str(text or "")
    if not body.strip():
        raise ValueError("empty_chat_message")
    obj = {
        "type": CHAT_MESSAGE_TYPE,
        "version": CHAT_FRAME_VERSION,
        "message_id": str(message_id or _new_chat_id("msg")),
        "conversation_id": str(conversation_id or ""),
        "group_id": str(group_id or ""),
        "sender_peer_id": str(sender_peer_id or ""),
        "receiver_peer_id": str(receiver_peer_id or ""),
        "sender_device_id": str(sender_device_id or ""),
        "created_at": float(created_at if created_at is not None else time.time()),
        "body_type": str(body_type or "text"),
        "text": body,
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_chat_message(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data or b"").decode("utf-8"))
    except Exception as exc:
        raise ValueError("chat_not_valid_json") from exc
    if not isinstance(obj, dict) or obj.get("type") != CHAT_MESSAGE_TYPE:
        raise ValueError("unsupported_chat_type")
    if int(obj.get("version") or 0) != CHAT_FRAME_VERSION:
        raise ValueError("unsupported_chat_version")
    text = str(obj.get("text") or "")
    if not text.strip():
        raise ValueError("empty_chat_message")
    obj["text"] = text
    obj["message_id"] = str(obj.get("message_id") or _new_chat_id("msg"))
    obj["conversation_id"] = str(obj.get("conversation_id") or "")
    obj["group_id"] = str(obj.get("group_id") or "")
    obj["sender_peer_id"] = str(obj.get("sender_peer_id") or "")
    obj["receiver_peer_id"] = str(obj.get("receiver_peer_id") or "")
    obj["sender_device_id"] = str(obj.get("sender_device_id") or "")
    obj["created_at"] = float(obj.get("created_at") or time.time())
    obj["body_type"] = str(obj.get("body_type") or "text")
    return obj


def build_chat_ack(message_id: str, *, conversation_id: str = "", group_id: str = "", receiver_peer_id: str = "", status: str = "delivered", received_at: Optional[float] = None) -> bytes:
    obj = {
        "type": CHAT_ACK_TYPE,
        "version": CHAT_FRAME_VERSION,
        "message_id": str(message_id or ""),
        "conversation_id": str(conversation_id or ""),
        "group_id": str(group_id or ""),
        "receiver_peer_id": str(receiver_peer_id or ""),
        "received_at": float(received_at if received_at is not None else time.time()),
        "status": str(status or "delivered"),
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_chat_ack(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data or b"").decode("utf-8"))
    except Exception as exc:
        raise ValueError("chat_ack_not_valid_json") from exc
    if not isinstance(obj, dict) or obj.get("type") != CHAT_ACK_TYPE:
        raise ValueError("unsupported_chat_ack_type")
    if int(obj.get("version") or 0) != CHAT_FRAME_VERSION:
        raise ValueError("unsupported_chat_ack_version")
    obj["message_id"] = str(obj.get("message_id") or "")
    obj["conversation_id"] = str(obj.get("conversation_id") or "")
    obj["group_id"] = str(obj.get("group_id") or "")
    obj["receiver_peer_id"] = str(obj.get("receiver_peer_id") or "")
    obj["received_at"] = float(obj.get("received_at") or time.time())
    obj["status"] = str(obj.get("status") or "delivered")
    return obj


def build_chat_read(message_id: str, *, conversation_id: str = "", group_id: str = "", reader_peer_id: str = "", read_at: Optional[float] = None) -> bytes:
    obj = {
        "type": CHAT_READ_TYPE,
        "version": CHAT_FRAME_VERSION,
        "message_id": str(message_id or ""),
        "conversation_id": str(conversation_id or ""),
        "group_id": str(group_id or ""),
        "reader_peer_id": str(reader_peer_id or ""),
        "read_at": float(read_at if read_at is not None else time.time()),
        "status": "read",
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_chat_read(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data or b"").decode("utf-8"))
    except Exception as exc:
        raise ValueError("chat_read_not_valid_json") from exc
    if not isinstance(obj, dict) or obj.get("type") != CHAT_READ_TYPE:
        raise ValueError("unsupported_chat_read_type")
    if int(obj.get("version") or 0) != CHAT_FRAME_VERSION:
        raise ValueError("unsupported_chat_read_version")
    obj["message_id"] = str(obj.get("message_id") or "")
    obj["conversation_id"] = str(obj.get("conversation_id") or "")
    obj["group_id"] = str(obj.get("group_id") or "")
    obj["reader_peer_id"] = str(obj.get("reader_peer_id") or "")
    obj["read_at"] = float(obj.get("read_at") or time.time())
    obj["status"] = "read"
    return obj


def build_chat_read_log(read: Dict[str, object], peer: str = "") -> str:
    obj = dict(read or {})
    obj["peer"] = str(peer or "")
    obj["logged_at"] = time.time()
    return CHAT_READ_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def build_chat_message_log(message: Dict[str, object], peer: str = "") -> str:
    obj = dict(message or {})
    obj["peer"] = str(peer or "")
    obj["logged_at"] = time.time()
    return CHAT_MESSAGE_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def build_chat_ack_log(ack: Dict[str, object], peer: str = "") -> str:
    obj = dict(ack or {})
    obj["peer"] = str(peer or "")
    obj["logged_at"] = time.time()
    return CHAT_ACK_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def build_user_error(code: str, message: str = "", detail: str = "") -> str:
    obj = {"code": str(code or "unknown_error"), "message": str(message or ""), "detail": str(detail or ""), "ts": time.time()}
    return USER_ERROR_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def build_user_status(code: str, message: str = "", **extra) -> str:
    obj = {"code": str(code or "status"), "message": str(message or ""), "ts": time.time()}
    obj.update(extra)
    return USER_STATUS_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def _build_transfer_event(prefix: str, event_type: str, **fields) -> str:
    obj = {
        "type": str(event_type or "TRANSFER_EVENT"),
        "version": 1,
        "timestamp": time.time(),
    }
    obj.update(fields)
    return prefix + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def build_transfer_started_log(**fields) -> str:
    return _build_transfer_event(TRANSFER_STARTED_LOG_PREFIX, "TRANSFER_STARTED", **fields)


def build_transfer_progress_log(**fields) -> str:
    return _build_transfer_event(TRANSFER_PROGRESS_LOG_PREFIX, "TRANSFER_PROGRESS", **fields)


def build_transfer_saved_log(**fields) -> str:
    return _build_transfer_event(TRANSFER_SAVED_LOG_PREFIX, "TRANSFER_SAVED", **fields)


def build_transfer_complete_log(**fields) -> str:
    return _build_transfer_event(TRANSFER_COMPLETE_LOG_PREFIX, "TRANSFER_COMPLETE", **fields)


def build_transfer_failed_log(**fields) -> str:
    return _build_transfer_event(TRANSFER_FAILED_LOG_PREFIX, "TRANSFER_FAILED", **fields)


def sha256_file(path: str, block_size: int = 1024 * 1024) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            chunk = f.read(block_size)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def _run_powershell_json(command: str) -> object:
    """Run a small PowerShell query and parse JSON output.

    Used only on Windows. It avoids localized `ipconfig` parsing, because the
    object property names returned by PowerShell are stable across languages.
    """
    if not sys.platform.startswith("win"):
        return []
    try:
        cp = subprocess.run(
            ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", command],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="ignore",
            timeout=3.0,
            check=False,
        )
    except Exception:
        return []
    out = (cp.stdout or "").strip()
    if not out:
        return []
    try:
        return json.loads(out)
    except Exception:
        return []


def get_ipv4_interface_records() -> List[Dict[str, object]]:
    """Return usable IPv4 interface records.

    Each item contains at least `ip`. On Windows, `prefix_len` and
    `interface` are usually available through PowerShell. Link-local
    169.254/16, loopback and APIPA-style addresses are excluded.
    """
    records: List[Dict[str, object]] = []
    seen = set()

    if sys.platform.startswith("win"):
        ps = (
            "Get-NetIPAddress -AddressFamily IPv4 "
            "| Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' -and $_.AddressState -ne 'Deprecated' } "
            "| Select-Object InterfaceAlias,IPAddress,PrefixLength "
            "| ConvertTo-Json -Compress"
        )
        data = _run_powershell_json(ps)
        if isinstance(data, dict):
            data = [data]
        if isinstance(data, list):
            for row in data:
                if not isinstance(row, dict):
                    continue
                ip = str(row.get("IPAddress") or "").strip()
                if not _is_useful_ipv4(ip):
                    continue
                try:
                    prefix_len = int(row.get("PrefixLength"))
                except Exception:
                    prefix_len = None
                key = (ip, prefix_len)
                if key in seen:
                    continue
                seen.add(key)
                records.append({
                    "ip": ip,
                    "prefix_len": prefix_len,
                    "interface": str(row.get("InterfaceAlias") or ""),
                })

    # Cross-platform fallback. Prefix length may be unknown here, but the IP is
    # still useful for display and common /24 directed-broadcast guesses.
    try:
        hostname = socket.gethostname()
        for item in socket.getaddrinfo(hostname, None, socket.AF_INET):
            ip = item[4][0]
            if not _is_useful_ipv4(ip):
                continue
            key = (ip, None)
            if key not in seen:
                seen.add(key)
                records.append({"ip": ip, "prefix_len": None, "interface": ""})
    except Exception:
        pass

    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            # This does not send user data to 8.8.8.8. It only asks the OS
            # which source address would be selected for a normal routed flow.
            s.connect(("8.8.8.8", 80))
            ip = s.getsockname()[0]
            if _is_useful_ipv4(ip):
                key = (ip, None)
                if key not in seen and all(r.get("ip") != ip for r in records):
                    records.append({"ip": ip, "prefix_len": None, "interface": ""})
        finally:
            s.close()
    except Exception:
        pass

    return records


def _is_useful_ipv4(ip: str) -> bool:
    try:
        addr = ipaddress.ip_address(str(ip))
    except Exception:
        return False
    return bool(
        addr.version == 4
        and not addr.is_loopback
        and not addr.is_link_local
        and not addr.is_multicast
        and not addr.is_unspecified
    )


def get_local_ip_candidates() -> List[str]:
    """Return likely local IPv4 addresses without requiring third-party packages."""
    ips = []
    seen = set()
    for rec in get_ipv4_interface_records():
        ip = str(rec.get("ip") or "")
        if _is_useful_ipv4(ip) and ip not in seen:
            seen.add(ip)
            ips.append(ip)
    return sorted(ips)


def get_lan_broadcast_addresses() -> List[str]:
    """Return limited and directed broadcast targets for LAN discovery.

    Some corporate or campus Wi-Fi networks do not forward 255.255.255.255,
    while directed broadcasts such as 10.254.195.255 may still work.
    """
    targets = ["255.255.255.255"]
    seen = set(targets)
    for rec in get_ipv4_interface_records():
        ip = str(rec.get("ip") or "")
        prefix = rec.get("prefix_len")
        if not _is_useful_ipv4(ip):
            continue
        try:
            if prefix is not None:
                p = int(prefix)
                if 8 <= p <= 30:
                    net = ipaddress.ip_network(f"{ip}/{p}", strict=False)
                    bcast = str(net.broadcast_address)
                    if bcast not in seen:
                        targets.append(bcast)
                        seen.add(bcast)
        except Exception:
            pass

        # Extra /24 guess. It is harmless on normal LANs and helps when the OS
        # cannot report a prefix length.
        try:
            parts = ip.split(".")
            if len(parts) == 4:
                bcast24 = ".".join(parts[:3] + ["255"])
                if bcast24 not in seen:
                    targets.append(bcast24)
                    seen.add(bcast24)
        except Exception:
            pass
    return targets


def get_lan_probe_targets(max_hosts: int = 2048, manual_targets: Optional[Iterable[str]] = None) -> List[str]:
    """Return ordered host targets for RUDP discovery fallback.

    The order matters on large subnets. We probe explicit/manual targets first,
    then the sender's own /24, then the full interface prefix. This avoids the
    old behavior where a /22 network could spend most of the timeout probing
    earlier address ranges before reaching a receiver that is actually in the
    same /24 as the sender.
    """
    targets: List[str] = []
    seen = set()

    def add(host: str) -> None:
        try:
            ip = str(ipaddress.ip_address(str(host).strip()))
        except Exception:
            return
        if not _is_useful_ipv4(ip) or ip in seen:
            return
        seen.add(ip)
        targets.append(ip)

    if manual_targets:
        for host in manual_targets:
            add(str(host))

    records = get_ipv4_interface_records()

    # First probe the current /24 around each usable local address. This is the
    # most common case for two machines on the same Wi-Fi and is much faster
    # than walking a whole /22 or /21 in numeric order.
    for rec in records:
        ip = str(rec.get("ip") or "")
        if not _is_useful_ipv4(ip):
            continue
        try:
            net24 = ipaddress.ip_network(f"{ip}/24", strict=False)
            for host in net24.hosts():
                h = str(host)
                if h != ip:
                    add(h)
                if len(targets) >= int(max_hosts):
                    return targets
        except Exception:
            continue

    # Then probe the reported interface prefix. Cap the scope to avoid turning
    # discovery into a broad network scan on enterprise networks.
    for rec in records:
        ip = str(rec.get("ip") or "")
        prefix = rec.get("prefix_len")
        if not _is_useful_ipv4(ip):
            continue
        try:
            if prefix is None:
                prefix = 24
            p = int(prefix)
            if p < 22:
                p = 24
            if p > 30:
                continue
            net = ipaddress.ip_network(f"{ip}/{p}", strict=False)
            for host in net.hosts():
                h = str(host)
                if h != ip:
                    add(h)
                if len(targets) >= int(max_hosts):
                    return targets
        except Exception:
            continue
    return targets

def print_local_ip_candidates(prefix: str = "Local IPv4 addresses") -> None:
    ips = get_local_ip_candidates()
    if not ips:
        print(f"{prefix}: no non-loopback IPv4 address found")
        return
    print(f"{prefix}:")
    for idx, ip in enumerate(ips, start=1):
        print(f"  [{idx}] {ip}")


def build_discovery_response(
    receiver_port: int,
    receiver_name: str = "",
    *,
    peer_id: str = "",
    nickname: str = "",
    fingerprint: str = "",
    identity_fingerprint: str = "",
) -> bytes:
    nick = str(nickname or receiver_name or socket.gethostname())
    fp = str(fingerprint or identity_fingerprint or peer_id or "")
    pid = str(peer_id or fp or "")
    obj = {
        "magic": DISCOVERY_RESPONSE_MAGIC,
        "version": 2,
        "hostname": socket.gethostname(),
        "name": nick,
        "nickname": nick,
        "receiver_name": str(receiver_name or nick),
        "peer_id": pid,
        "fingerprint": fp,
        "identity_fingerprint": fp,
        "port": int(receiver_port),
        "ips": get_local_ip_candidates(),
        "ts": time.time(),
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_discovery_response(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data).decode("utf-8"))
    except Exception as exc:
        raise ValueError("invalid_discovery_response_json") from exc
    if obj.get("magic") != DISCOVERY_RESPONSE_MAGIC:
        raise ValueError("invalid_discovery_response_magic")
    return obj


def discover_receivers(
    discovery_port: int = DEFAULT_DISCOVERY_PORT,
    timeout: float = 4.0,
    broadcast_addresses: Optional[Iterable[str]] = None,
    enable_subnet_probe: bool = True,
    max_probe_hosts: int = 2048,
    extra_ports: Optional[Iterable[int]] = None,
    manual_targets: Optional[Iterable[str]] = None,
    port: Optional[int] = None,
) -> List[Dict[str, object]]:
    """Discover RUDP receivers on the current LAN.

    Discovery now uses three stages:
    1. UDP broadcast to the discovery port and optional transfer ports.
    2. Direct probe to manually supplied addresses, if any.
    3. Ordered local-subnet probe, prioritizing the current /24.

    The optional `port` parameter is accepted for compatibility with older GUI
    code that called discover_receivers(port=9998).
    """
    if port is not None:
        discovery_port = int(port)

    port_list: List[int] = []
    for candidate in [discovery_port] + list(extra_ports or []):
        try:
            p = int(candidate)
        except Exception:
            continue
        if 1 <= p <= 65535 and p not in port_list:
            port_list.append(p)
    if not port_list:
        port_list = [DEFAULT_DISCOVERY_PORT]

    if broadcast_addresses is None:
        targets = get_lan_broadcast_addresses()
    else:
        targets = list(broadcast_addresses)

    results: List[Dict[str, object]] = []
    seen = set()

    def add_result(data: bytes, addr) -> None:
        try:
            obj = parse_discovery_response(data)
        except ValueError:
            return
        endpoint_ip = str(addr[0])
        endpoint_port = int(obj.get("port") or 9999)
        key = (endpoint_ip, endpoint_port, str(obj.get("hostname") or ""))
        if key in seen:
            return
        seen.add(key)
        obj["endpoint_ip"] = endpoint_ip
        obj["endpoint_port"] = endpoint_port
        obj["ip"] = endpoint_ip
        obj["port"] = endpoint_port
        if not obj.get("nickname"):
            obj["nickname"] = str(obj.get("name") or obj.get("hostname") or "")
        if not obj.get("receiver_name"):
            obj["receiver_name"] = str(obj.get("name") or obj.get("nickname") or "")
        if not obj.get("fingerprint") and obj.get("identity_fingerprint"):
            obj["fingerprint"] = str(obj.get("identity_fingerprint") or "")
        if not obj.get("identity_fingerprint") and obj.get("fingerprint"):
            obj["identity_fingerprint"] = str(obj.get("fingerprint") or "")
        if not obj.get("peer_id"):
            obj["peer_id"] = str(obj.get("fingerprint") or obj.get("hostname") or f"{endpoint_ip}:{endpoint_port}")
        obj["source_addr"] = f"{addr[0]}:{addr[1]}"
        obj["discovery_ports_tried"] = port_list
        results.append(obj)

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("0.0.0.0", 0))
        sock.settimeout(0.12)
        payload = DISCOVERY_MAGIC.encode("ascii")

        def send_and_collect(target_list: List[str], collect_seconds: float) -> None:
            for host in target_list:
                for p in port_list:
                    try:
                        sock.sendto(payload, (host, int(p)))
                    except OSError:
                        pass
            deadline = time.time() + max(0.1, float(collect_seconds))
            while time.time() < deadline:
                try:
                    data, addr = sock.recvfrom(4096)
                except socket.timeout:
                    continue
                except OSError:
                    break
                add_result(data, addr)

        # Stage 1: limited and directed broadcast.
        send_and_collect(targets, min(max(0.8, float(timeout) * 0.35), float(timeout)))

        # Stage 2: explicit/manual targets, useful when the user typed a known
        # address and wants to verify that it is an RUDP receiver.
        manual_list: List[str] = []
        if manual_targets:
            for host in manual_targets:
                host = str(host or "").strip()
                if host:
                    manual_list.append(host)
        if manual_list and not results:
            send_and_collect(manual_list, 0.8)

        # Stage 3: current subnet fallback. It still sends only the RUDP
        # discovery magic, not generic ping or arbitrary port scans.
        if not results and enable_subnet_probe:
            probe_targets = get_lan_probe_targets(max_hosts=max_probe_hosts, manual_targets=manual_list)
            remaining = max(2.0, float(timeout) * 0.9)
            chunk_size = 96
            start = time.time()
            for i in range(0, len(probe_targets), chunk_size):
                if time.time() - start > remaining:
                    break
                send_and_collect(probe_targets[i:i + chunk_size], 0.18)
                if results:
                    break
    finally:
        try:
            sock.close()
        except Exception:
            pass
    return results


def build_transfer_decision(accepted: bool, reason: str = "", conn_id: int = 0, **extra) -> bytes:
    obj = {
        "magic": TRANSFER_DECISION_MAGIC,
        "version": 2,
        "accepted": bool(accepted),
        "reason": str(reason or ""),
        "conn_id": int(conn_id or 0),
        "ts": time.time(),
    }
    for key, value in (extra or {}).items():
        if key not in obj:
            obj[str(key)] = value
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_transfer_decision(data: bytes) -> Dict[str, object]:
    try:
        obj = json.loads(bytes(data).decode("utf-8"))
    except Exception as exc:
        raise ValueError("invalid_transfer_decision_json") from exc
    if obj.get("magic") != TRANSFER_DECISION_MAGIC:
        raise ValueError("invalid_transfer_decision_magic")
    return obj


def build_transfer_request_obj(conn_id: int, peer_addr, meta: Dict[str, object], out_path: Path, **extra) -> Dict[str, object]:
    obj = {
        "conn_id": int(conn_id),
        "sender": f"{peer_addr[0]}:{peer_addr[1]}",
        "sender_ip": str(peer_addr[0]),
        "sender_port": int(peer_addr[1]),
        "name": str(meta.get("name") or ""),
        "size": int(meta.get("size") or 0),
        "sha256": str(meta.get("sha256") or ""),
        "save_path": str(out_path),
        "chat_message_id": str(meta.get("chat_message_id") or ""),
        "chat_conversation_id": str(meta.get("chat_conversation_id") or ""),
        "chat_group_id": str(meta.get("chat_group_id") or ""),
        "chat_sender_peer_id": str(meta.get("chat_sender_peer_id") or ""),
        "chat_receiver_peer_id": str(meta.get("chat_receiver_peer_id") or ""),
        "ts": time.time(),
    }
    obj.update(extra or {})
    return obj


def build_transfer_request_log(conn_id: int, peer_addr, meta: Dict[str, object], out_path: Path, **extra) -> str:
    obj = build_transfer_request_obj(conn_id, peer_addr, meta, out_path, **extra)
    return TRANSFER_REQUEST_LOG_PREFIX + json.dumps(obj, ensure_ascii=False, separators=(",", ":"))



def safe_filename(name: str, fallback: str = "received.bin") -> str:
    name = os.path.basename(str(name or "").strip())
    if not name or name in (".", ".."):
        name = fallback
    forbidden = '<>:"/\\|?*\x00'
    name = "".join("_" if ch in forbidden else ch for ch in name)
    name = name.strip().strip(".")
    return name or fallback


def unique_path(directory: str, filename: str) -> Path:
    base_dir = Path(directory).expanduser().resolve()
    base_dir.mkdir(parents=True, exist_ok=True)
    filename = safe_filename(filename)
    candidate = base_dir / filename
    if not candidate.exists() and not Path(str(candidate) + ".part").exists():
        return candidate

    stem = candidate.stem
    suffix = candidate.suffix
    for i in range(1, 10000):
        alt = base_dir / f"{stem}_{i}{suffix}"
        if not alt.exists() and not Path(str(alt) + ".part").exists():
            return alt
    raise RuntimeError("cannot allocate unique output filename")




def allocate_output_path(directory: str, filename: str, policy: str = "rename") -> Path:
    """Return final output path for a received file.

    policy values:
    - rename: choose a unique path without replacing existing files;
    - overwrite: use the original safe filename and replace it only after the
      .part file has been fully received and verified.
    """
    base_dir = Path(directory).expanduser().resolve()
    base_dir.mkdir(parents=True, exist_ok=True)
    safe = safe_filename(filename)
    original = base_dir / safe
    policy = str(policy or "rename").strip().lower()
    if policy == "overwrite":
        return original
    return unique_path(str(base_dir), safe)


def probe_save_directory(directory: str, required_bytes: int = 0, reserve_bytes: int = 100 * 1024 * 1024) -> Dict[str, object]:
    """Check whether a receive directory is usable before accepting a file."""
    base_dir = Path(directory).expanduser().resolve()
    try:
        base_dir.mkdir(parents=True, exist_ok=True)
    except Exception as exc:
        return {"ok": False, "code": "save_dir_create_failed", "detail": str(exc), "path": str(base_dir)}
    if not base_dir.is_dir():
        return {"ok": False, "code": "save_dir_not_directory", "detail": str(base_dir), "path": str(base_dir)}
    try:
        test_path = base_dir / ".rudp_write_test.tmp"
        with open(test_path, "wb") as f:
            f.write(b"ok")
        try:
            test_path.unlink()
        except Exception:
            pass
    except Exception as exc:
        return {"ok": False, "code": "save_dir_not_writable", "detail": str(exc), "path": str(base_dir)}
    try:
        import shutil
        usage = shutil.disk_usage(str(base_dir))
        need = max(0, int(required_bytes or 0)) + max(0, int(reserve_bytes or 0))
        if int(usage.free) < need:
            return {
                "ok": False,
                "code": "disk_space_not_enough",
                "detail": f"free={int(usage.free)} required={need}",
                "path": str(base_dir),
                "free_bytes": int(usage.free),
                "required_bytes": int(need),
            }
    except Exception:
        # If the OS cannot report disk usage, do not block the transfer.
        pass
    return {"ok": True, "code": "ok", "path": str(base_dir)}


def output_conflict_info(directory: str, filename: str) -> Dict[str, object]:
    base_dir = Path(directory).expanduser().resolve()
    safe = safe_filename(filename)
    original = base_dir / safe
    part = Path(str(original) + ".part")
    exists = original.exists()
    part_exists = part.exists()
    return {
        "original_path": str(original),
        "part_path": str(part),
        "file_exists": bool(exists),
        "part_exists": bool(part_exists),
        "conflict": bool(exists or part_exists),
    }




def make_transfer_id(meta_or_sha256: Dict[str, object] | str) -> str:
    """Return a stable file-transfer id.

    For v2, the full-file SHA256 is used as the transfer id. This keeps resume
    negotiation simple: only a partial file with the same final SHA256 can be
    resumed.
    """
    if isinstance(meta_or_sha256, dict):
        value = str(meta_or_sha256.get("transfer_id") or meta_or_sha256.get("sha256") or "").strip().lower()
    else:
        value = str(meta_or_sha256 or "").strip().lower()
    return value


def resume_meta_path(part_path: Path | str) -> Path:
    return Path(str(part_path) + ".meta.json")


def read_resume_meta(part_path: Path | str) -> Dict[str, object]:
    meta_path = resume_meta_path(part_path)
    try:
        obj = json.loads(meta_path.read_text(encoding="utf-8"))
    except Exception:
        return {}
    return obj if isinstance(obj, dict) else {}


def write_resume_meta(part_path: Path | str, meta: Dict[str, object], out_path: Path | str, received_bytes: int) -> None:
    part = Path(part_path)
    meta_path = resume_meta_path(part)
    obj = {
        "version": 2,
        "transfer_id": make_transfer_id(meta),
        "name": str(meta.get("name") or ""),
        "size": int(meta.get("size") or 0),
        "sha256": str(meta.get("sha256") or "").strip().lower(),
        "payload_size": int(meta.get("payload_size") or 0),
        "received_bytes": int(max(0, int(received_bytes or 0))),
        "out_path": str(out_path),
        "part_path": str(part),
        "updated_at": time.time(),
    }
    tmp = meta_path.with_suffix(meta_path.suffix + ".tmp")
    tmp.write_text(json.dumps(obj, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
    os.replace(tmp, meta_path)


def remove_resume_meta(part_path: Path | str) -> None:
    try:
        resume_meta_path(part_path).unlink()
    except FileNotFoundError:
        pass
    except Exception:
        pass


def resume_candidate_info(directory: str, meta: Dict[str, object]) -> Dict[str, object]:
    """Inspect whether an existing .part file can be resumed.

    The current implementation uses implicit file offsets: the sender starts at
    `resume_offset`, while the receiver appends to the existing .part file.
    The offset is aligned down to payload_size to avoid partial-chunk ambiguity.
    """
    base_dir = Path(directory).expanduser().resolve()
    safe = safe_filename(str(meta.get("name") or "received.bin"))
    out_path = base_dir / safe
    part_path = Path(str(out_path) + ".part")
    info: Dict[str, object] = {
        "resume_available": False,
        "resume_offset": 0,
        "resume_pct": 0.0,
        "out_path": str(out_path),
        "part_path": str(part_path),
        "meta_path": str(resume_meta_path(part_path)),
        "reason": "not_available",
    }
    if not part_path.exists() or not part_path.is_file():
        info["reason"] = "part_missing"
        return info
    saved = read_resume_meta(part_path)
    if not saved:
        info["reason"] = "meta_missing"
        return info
    expected_transfer_id = make_transfer_id(meta)
    saved_transfer_id = make_transfer_id(saved)
    if expected_transfer_id and saved_transfer_id != expected_transfer_id:
        info["reason"] = "transfer_id_mismatch"
        return info
    for key in ("size", "payload_size"):
        try:
            if int(saved.get(key) or 0) != int(meta.get(key) or 0):
                info["reason"] = f"{key}_mismatch"
                return info
        except Exception:
            info["reason"] = f"{key}_invalid"
            return info
    if str(saved.get("sha256") or "").strip().lower() != str(meta.get("sha256") or "").strip().lower():
        info["reason"] = "sha256_mismatch"
        return info
    try:
        actual_size = int(part_path.stat().st_size)
    except Exception:
        info["reason"] = "part_stat_failed"
        return info
    total = int(meta.get("size") or 0)
    payload_size = max(1, int(meta.get("payload_size") or 1))
    saved_bytes = int(saved.get("received_bytes") or 0)
    offset = min(actual_size, saved_bytes, total)
    if offset <= 0:
        info["reason"] = "empty_part"
        return info
    if offset >= total:
        # A full .part exists but has not been finalized. Re-verify by receiving
        # zero bytes is not supported in the first resume version; restart or
        # overwrite is safer.
        offset = total
    aligned = offset - (offset % payload_size)
    if aligned <= 0 and offset > 0:
        aligned = 0
    info.update({
        "resume_available": bool(aligned > 0 and aligned < total),
        "resume_offset": int(aligned),
        "resume_pct": (float(aligned) * 100.0 / float(total)) if total > 0 else 0.0,
        "actual_part_size": actual_size,
        "meta_received_bytes": saved_bytes,
        "reason": "ok" if aligned > 0 and aligned < total else "not_resumable_size",
    })
    return info

def build_file_header(
    path: str,
    payload_size: int,
    sha256_hex: Optional[str] = None,
    *,
    chat_message_id: str = "",
    chat_conversation_id: str = "",
    chat_group_id: str = "",
    chat_sender_peer_id: str = "",
    chat_receiver_peer_id: str = "",
) -> bytes:
    p = Path(path)
    if not p.is_file():
        raise FileNotFoundError(str(path))
    size = p.stat().st_size
    if sha256_hex is None:
        sha256_hex = sha256_file(str(p))
    obj: Dict[str, object] = {
        "type": APP_HEADER_TYPE_FILE,
        "version": APP_HEADER_VERSION,
        "transfer_id": make_transfer_id(str(sha256_hex)),
        "name": p.name,
        "size": int(size),
        "payload_size": int(payload_size),
        "sha256": str(sha256_hex),
        "resume_supported": True,
        "mtime_ns": int(p.stat().st_mtime_ns),
        "chat_message_id": str(chat_message_id or ""),
        "chat_conversation_id": str(chat_conversation_id or ""),
        "chat_group_id": str(chat_group_id or ""),
        "chat_sender_peer_id": str(chat_sender_peer_id or ""),
        "chat_receiver_peer_id": str(chat_receiver_peer_id or ""),
    }
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def parse_file_header(data: bytes) -> Dict[str, object]:
    if not isinstance(data, (bytes, bytearray)):
        raise ValueError("header_not_bytes")
    try:
        obj = json.loads(bytes(data).decode("utf-8"))
    except Exception as exc:
        raise ValueError("header_not_valid_json") from exc

    if obj.get("type") != APP_HEADER_TYPE_FILE:
        raise ValueError("unsupported_header_type")
    version = int(obj.get("version", 0))
    if version not in APP_HEADER_COMPAT_VERSIONS:
        raise ValueError("unsupported_header_version")

    name = safe_filename(str(obj.get("name") or "received.bin"))
    size = int(obj.get("size"))
    payload_size = int(obj.get("payload_size"))
    sha256_hex = str(obj.get("sha256") or "").strip().lower()

    if size < 0:
        raise ValueError("negative_file_size")
    if payload_size <= 0:
        raise ValueError("nonpositive_payload_size")
    if sha256_hex and (len(sha256_hex) != 64 or any(c not in "0123456789abcdef" for c in sha256_hex)):
        raise ValueError("invalid_sha256")

    return {
        "type": APP_HEADER_TYPE_FILE,
        "version": version,
        "transfer_id": make_transfer_id(str(obj.get("transfer_id") or sha256_hex)),
        "name": name,
        "size": size,
        "payload_size": payload_size,
        "sha256": sha256_hex,
        "resume_supported": bool(obj.get("resume_supported", version >= 2)),
        "mtime_ns": int(obj.get("mtime_ns") or 0),
        "chat_message_id": str(obj.get("chat_message_id") or ""),
        "chat_conversation_id": str(obj.get("chat_conversation_id") or ""),
        "chat_group_id": str(obj.get("chat_group_id") or ""),
        "chat_sender_peer_id": str(obj.get("chat_sender_peer_id") or ""),
        "chat_receiver_peer_id": str(obj.get("chat_receiver_peer_id") or ""),
    }
