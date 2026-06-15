#!/usr/bin/env python3
"""RUDP file sender for real two-machine transfer.

Example:
    python3 client.py --server-ip 192.168.1.25 --server-port 9999 --file ./demo.zip
"""

from __future__ import annotations

import sys

for _stream_name in ("stdout", "stderr"):
    _stream = getattr(sys, _stream_name, None)
    if _stream is not None:
        try:
            _stream.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass

import argparse
import math
import os
import secrets
import socket
import time
from pathlib import Path
from typing import List, Optional

from congestion import CubicCongestionControl
from file_transfer_common import (
    DATA_SEQ_UPPER_EXCLUSIVE,
    EOF_PAYLOAD,
    MAX_DATA_SEQ,
    SEQ_FIRST_BODY,
    SEQ_HEADER,
    build_chat_message,
    build_chat_ack_log,
    build_chat_read,
    build_chat_read_log,
    build_contact_request,
    build_contact_response_log,
    build_file_header,
    build_user_error,
    build_user_status,
    build_transfer_started_log,
    build_transfer_progress_log,
    build_transfer_complete_log,
    build_transfer_failed_log,
    parse_chat_ack,
    parse_contact_response,
    parse_transfer_decision,
    print_local_ip_candidates,
    sha256_file,
)
from protocol import (
    AEAD_TAG_LEN,
    DATA_FRAME_HEADER_LEN,
    MAX_DATA_APP_PAYLOAD,
    UDP_MAX_DATAGRAM_PAYLOAD,
    ReliableUDPSession,
    data_packet_wire_size,
)
from utils import HEADER_SIZE, setup_logger


def _sha256_hex_bytes(data: bytes) -> str:
    import hashlib
    return hashlib.sha256(bytes(data or b"")).hexdigest()


def _load_pinned_server_pub(path: str) -> Optional[bytes]:
    path = str(path or "").strip()
    if not path or not os.path.exists(path):
        return None
    raw = open(path, "rb").read().strip()
    if not raw:
        return None
    try:
        text = raw.decode("ascii")
        if len(text) == 64 and all(c in "0123456789abcdefABCDEF" for c in text):
            return bytes.fromhex(text)
    except Exception:
        pass
    return bytes(raw)


def _store_pinned_server_pub(path: str, pub_bytes: bytes) -> None:
    path = str(path or "").strip()
    if not path:
        return
    parent = os.path.dirname(path)
    if parent:
        os.makedirs(parent, exist_ok=True)
    tmp_path = path + ".tmp"
    with open(tmp_path, "wb") as f:
        f.write(bytes(pub_bytes).hex().encode("ascii") + b"\n")
    os.chmod(tmp_path, 0o600)
    os.replace(tmp_path, path)


def make_server_identity_validator(pin_file: str, logger, require_existing_pin: bool = False):
    state = {
        "pinned": _load_pinned_server_pub(pin_file),
        "announced": False,
        "identity_mismatch": None,
        "pin_file": str(pin_file or ""),
    }

    def validator(server_pub: bytes) -> bool:
        server_pub = bytes(server_pub or b"")
        if len(server_pub) != 32:
            state["identity_mismatch"] = {"reason": "invalid_receiver_identity", "pin_file": str(pin_file or "")}
            return False
        pinned = state.get("pinned")
        fingerprint = _sha256_hex_bytes(server_pub)
        if pinned is None:
            if require_existing_pin:
                logger.error(f"No pinned server key found: {pin_file}")
                return False
            _store_pinned_server_pub(pin_file, server_pub)
            state["pinned"] = server_pub
            logger.info(f"Pinned receiver identity by TOFU: fingerprint={fingerprint}, pin_file={pin_file}")
            return True
        if pinned != server_pub:
            expected = _sha256_hex_bytes(pinned)
            state["identity_mismatch"] = {
                "reason": "receiver_identity_changed",
                "expected": expected,
                "got": fingerprint,
                "pin_file": str(pin_file or ""),
            }
            logger.error(f"Pinned receiver key mismatch: expected={expected} got={fingerprint} pin_file={pin_file}")
            return False
        if not state.get("announced"):
            state["announced"] = True
            logger.info(f"Verified pinned receiver identity: fingerprint={fingerprint}, pin_file={pin_file}")
        return True

    validator.state = state  # type: ignore[attr-defined]
    return validator


class TokenBucketPacer:
    def __init__(self, rate_tokens_per_s: float, burst_seconds: float = 0.05):
        self.rate = float(rate_tokens_per_s)
        self.capacity = max(1.0, self.rate * float(burst_seconds)) if self.rate > 0 else 0.0
        self.tokens = self.capacity
        self.ts = time.time()

    def wait(self, tokens_needed: float) -> None:
        if self.rate <= 0:
            return
        need = float(tokens_needed)
        while True:
            now = time.time()
            elapsed = now - self.ts
            if elapsed > 0:
                self.tokens = min(self.capacity, self.tokens + elapsed * self.rate)
                self.ts = now
            if self.tokens >= need:
                self.tokens -= need
                return
            time.sleep(min(max((need - self.tokens) / self.rate, 0.0), 0.01))


def _format_duration(seconds: float) -> str:
    if seconds is None or not math.isfinite(float(seconds)) or float(seconds) < 0:
        return "unknown"
    seconds = int(round(float(seconds)))
    h, rem = divmod(seconds, 3600)
    m, sec = divmod(rem, 60)
    if h:
        return f"{h:d}:{m:02d}:{sec:02d}"
    return f"{m:d}:{sec:02d}"


def _effective_max_unacked_pkts(args) -> int:
    try:
        return max(0, int(getattr(args, "max_unacked_pkts", 1024) or 0))
    except Exception:
        return 1024


def _initial_adaptive_wifi_state(args) -> dict:
    adaptive_enabled = not bool(getattr(args, "disable_adaptive_wifi", False))
    adaptive_min_unacked = max(1, int(getattr(args, "adaptive_max_unacked_min", 960) or 960))
    adaptive_max_unacked = max(adaptive_min_unacked, int(getattr(args, "adaptive_max_unacked_max", 1536) or 1536))
    adaptive_step_unacked = max(1, int(getattr(args, "adaptive_max_unacked_step", 64) or 64))
    adaptive_eval_interval = max(2.0, float(getattr(args, "adaptive_eval_interval_sec", 5.0) or 5.0))
    adaptive_current_unacked = max(
        adaptive_min_unacked,
        min(adaptive_max_unacked, _effective_max_unacked_pkts(args)),
    )
    return {
        "enabled": adaptive_enabled,
        "min_unacked": adaptive_min_unacked,
        "max_unacked": adaptive_max_unacked,
        "step_unacked": adaptive_step_unacked,
        "eval_interval": adaptive_eval_interval,
        "current_unacked": adaptive_current_unacked,
    }


def _effective_sockbuf_bytes(args, attr: str = "sock_rcvbuf") -> int:
    try:
        explicit = int(getattr(args, attr, 0) or 0)
    except Exception:
        explicit = 0
    if explicit > 0:
        return explicit
    max_unacked = _effective_max_unacked_pkts(args)
    # For adaptive Wi-Fi mode, size socket buffers for the allowed upper bound,
    # not only for the initial in-flight window.
    try:
        if not bool(getattr(args, "disable_adaptive_wifi", False)):
            max_unacked = max(max_unacked, int(getattr(args, "adaptive_max_unacked_max", max_unacked) or max_unacked))
    except Exception:
        pass
    # Link socket buffer to max_unacked_pkts. 32 KiB per allowed in-flight DATA
    # packet gives 48 MiB at adaptive_max_unacked_max=1536, with a 16 MiB floor.
    return max(16 * 1024 * 1024, int(max_unacked) * 32 * 1024)



def build_argparser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="AgoraLink sender: send files or encrypted chat messages through the RUDP protocol.")
    p.add_argument("--server-ip", required=True, help="Receiver IP or hostname")
    p.add_argument("--server-port", type=int, default=9999, help="Receiver UDP port")
    p.add_argument("--file", default="", help="File to send")
    p.add_argument("--chat-message", action="append", default=[], help="Send one chat message. Repeat to send multiple messages.")
    p.add_argument("--chat-db", default="", help="Optional local SQLite chat database path")
    p.add_argument("--chat-password", default="", help="Password used to derive the local chat storage key")
    p.add_argument("--chat-group-id", default="", help="Optional group_id for group chat")
    p.add_argument("--chat-conversation-id", default="", help="Optional one-to-one conversation_id")
    p.add_argument("--chat-sender-peer-id", default="local", help="Local sender peer_id")
    p.add_argument("--chat-receiver-peer-id", default="", help="Receiver peer_id")
    p.add_argument("--chat-message-id", default="", help="Optional fixed message_id for group fan-out")
    p.add_argument("--chat-created-at", type=float, default=0.0, help="Optional fixed created_at timestamp for group fan-out")
    p.add_argument("--chat-read-message-id", action="append", default=[], help="Send CHAT_READ receipt for message_id. Repeatable.")
    p.add_argument("--chat-reader-peer-id", default="", help="Reader peer_id for CHAT_READ receipt")
    p.add_argument("--chat-body-type", default="text", choices=["text", "file"], help="Chat message body type")
    p.add_argument("--contact-request", action="store_true", help="Send a contact request instead of a file/chat message")
    p.add_argument("--contact-request-id", default="", help="Optional fixed contact request id")
    p.add_argument("--contact-sender-peer-id", default="", help="Contact request sender peer_id")
    p.add_argument("--contact-sender-nickname", default="", help="Contact request sender nickname")
    p.add_argument("--contact-sender-fingerprint", default="", help="Contact request sender fingerprint")
    p.add_argument("--contact-message", default="", help="Optional contact request message")
    p.add_argument("--bind-ip", default="0.0.0.0", help="Local bind IP; usually keep 0.0.0.0")
    p.add_argument("--bind-port", type=int, default=0, help="Local bind port; 0 means auto")
    p.add_argument("--payload-size", type=int, default=1400, help="Application payload bytes per DATA packet")
    p.add_argument("--file-read-chunk-mb", type=int, default=4, help="Single-thread file read chunk size in MiB")
    p.add_argument("--sock-rcvbuf", type=int, default=0, help="UDP receive buffer bytes; 0 derives from max_unacked_pkts")
    p.add_argument("--sock-sndbuf", type=int, default=0, help="UDP send buffer bytes; 0 derives from max_unacked_pkts")
    p.add_argument("--handshake-timeout", type=float, default=3.0)
    p.add_argument("--handshake-max-retries", type=int, default=100)
    p.add_argument("--handshake-tail-timeout", type=float, default=60.0)
    p.add_argument("--final-ack-timeout", type=float, default=60.0)
    p.add_argument("--complete-timeout", type=float, default=60.0)
    p.add_argument("--request-timeout", type=float, default=300.0, help="Seconds to wait for receiver approval after sending metadata")
    p.add_argument("--no-request-confirmation", action="store_true", help="Do not wait for receiver approval before sending file body")
    p.add_argument("--stats-interval", type=float, default=1.0)
    p.add_argument("--progress-json-interval", type=float, default=0.2)
    p.add_argument("--no-progress-timeout", type=float, default=120.0, help="Fail if sender sees no effective transfer progress for this many seconds")
    p.add_argument("--server-pin-file", default="./rudp_receiver_ed25519.pin", help="TOFU receiver public-key pin file")
    p.add_argument("--require-existing-server-pin", action="store_true")
    p.add_argument("--disable-cc", action="store_true", help="Disable CUBIC congestion control")
    p.add_argument("--max-unacked-pkts", type=int, default=1024, help="Initial hard cap for outstanding DATA packets")
    p.add_argument("--disable-adaptive-wifi", action="store_true", help="Disable adaptive LAN window/burst tuning")
    p.add_argument("--adaptive-max-unacked-min", type=int, default=960, help="Adaptive Wi-Fi minimum DATA in-flight cap")
    p.add_argument("--adaptive-max-unacked-max", type=int, default=1536, help="Adaptive Wi-Fi maximum DATA in-flight cap")
    p.add_argument("--adaptive-max-unacked-step", type=int, default=64, help="Adaptive Wi-Fi window increase step in packets")
    p.add_argument("--adaptive-eval-interval-sec", type=float, default=5.0, help="Adaptive Wi-Fi control interval in seconds")
    p.add_argument("--lan-pacing-burst-pkts", type=int, default=32, help="LAN bulk pacing burst budget in DATA packets")
    p.add_argument("--lan-pacing-interval-ms", type=float, default=5.0, help="LAN bulk pacing burst interval in milliseconds")
    p.add_argument("--reorder-tolerance-pkts", type=int, default=128, help="Fast retransmit reorder tolerance in packets")
    p.add_argument("--send-rate-mbps", type=float, default=0.0, help="Optional sender-side rate limit")
    p.add_argument("--initial-rtt-ms", type=float, default=0.0)
    p.add_argument("--min-data-rto-sec", type=float, default=0.2)
    p.add_argument("--max-data-rto-sec", type=float, default=4.0)
    p.add_argument("--show-ips", action="store_true", help="Print local IP candidates before sending")
    p.add_argument("--verbose-protocol", action="store_true", help="Show high-volume internal ACK logs")
    return p


def _wait_for_handshake(session: ReliableUDPSession, args, logger, validator=None) -> None:
    session.begin_client_handshake(
        initial_rto=float(args.handshake_timeout),
        max_retries=max(0, int(args.handshake_max_retries)),
        handshake_tail_timeout=float(args.handshake_tail_timeout),
        final_ack_rto_cap=max(float(args.handshake_timeout), min(float(args.handshake_tail_timeout), 60.0)),
    )

    def check_identity_mismatch() -> None:
        state = getattr(validator, "state", {}) if validator is not None else {}
        mismatch = state.get("identity_mismatch") if isinstance(state, dict) else None
        if mismatch:
            try:
                session.abort("receiver_identity_changed")
            except Exception:
                pass
            expected = str(mismatch.get("expected") or "")
            got = str(mismatch.get("got") or "")
            pin_file = str(mismatch.get("pin_file") or "")
            detail = f"expected={expected} got={got} pin_file={pin_file}"
            raise RuntimeError("receiver_identity_changed:" + detail)

    while not session.wait_session_key_ready(timeout=0.1):
        check_identity_mismatch()
        if session.has_fatal_error():
            raise RuntimeError(session.get_fatal_error() or "fatal protocol error during handshake")
        if not session.running:
            raise RuntimeError("session stopped during handshake")

    while not session.wait_peer_established(timeout=0.1):
        check_identity_mismatch()
        if session.has_fatal_error():
            raise RuntimeError(session.get_fatal_error() or "fatal protocol error while confirming handshake")
        if not session.running:
            raise RuntimeError("session stopped while confirming handshake")

    logger.info("Handshake established")


def _wait_for_receiver_decision(session: ReliableUDPSession, timeout: float, logger) -> dict:
    deadline = time.time() + max(1.0, float(timeout or 300.0))
    logger.info("Transfer request submitted; waiting for receiver approval")
    while time.time() < deadline:
        if session.has_fatal_error():
            raise RuntimeError(session.get_fatal_error() or "fatal protocol error while waiting receiver approval")
        item = session.get_app_item(timeout=0.1)
        if item is None:
            continue
        _seq, data_or_len, is_len = item
        if is_len:
            continue
        try:
            decision = parse_transfer_decision(bytes(data_or_len or b""))
        except Exception:
            continue
        if bool(decision.get("accepted")):
            resume_offset = max(0, int(decision.get("resume_offset") or 0))
            if bool(decision.get("resume")) and resume_offset > 0:
                logger.info(
                    f"Receiver accepted transfer; resuming from offset={resume_offset} "
                    f"({float(decision.get('resume_pct') or 0.0):.2f}%)"
                )
                logger.info(build_user_status("resume_enabled", "Receiver requested resume", resume_offset=resume_offset, resume_pct=float(decision.get("resume_pct") or 0.0)))
            else:
                logger.info("Receiver accepted transfer; starting file body transmission")
            return decision
        reason = str(decision.get("reason") or "rejected")
        session.abort("receiver_rejected")
        raise PermissionError(f"{reason}")
    session.abort("receiver_approval_timeout")
    raise TimeoutError("approval_timeout")




def run_contact_request_client(args: argparse.Namespace) -> int:
    if not bool(getattr(args, "verbose_protocol", False)):
        os.environ.setdefault("RUDP_VERBOSE_PROTOCOL", "0")
    logger = setup_logger("AgoraLink-ContactSender")
    sender_peer_id = str(getattr(args, "contact_sender_peer_id", "") or getattr(args, "chat_sender_peer_id", "") or "local")
    sender_nickname = str(getattr(args, "contact_sender_nickname", "") or sender_peer_id)
    sock = None
    session = None
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, _effective_sockbuf_bytes(args, "sock_rcvbuf"))
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, _effective_sockbuf_bytes(args, "sock_sndbuf"))
        sock.bind((str(args.bind_ip), int(args.bind_port)))
        conn_id = secrets.randbits(64)
        validator = make_server_identity_validator(str(args.server_pin_file), logger, require_existing_pin=bool(args.require_existing_server_pin))
        session = ReliableUDPSession(conn_id, (str(args.server_ip), int(args.server_port)), sock, is_client=True, server_identity_validator=validator)
        session.configure_app_delivery(len_only=False, small_payload_threshold=0)
        session.start_threads(start_receiver=True)
        _wait_for_handshake(session, args, logger, validator=validator)
        payload = build_contact_request(
            request_id=str(getattr(args, "contact_request_id", "") or ""),
            sender_peer_id=sender_peer_id,
            sender_nickname=sender_nickname,
            sender_fingerprint=str(getattr(args, "contact_sender_fingerprint", "") or sender_peer_id),
            # Do not put 0.0.0.0 into CONTACT_REQUEST. The receiver should
            # store the UDP source address as our reachable endpoint.
            sender_ip="",
            sender_port=9999,
            message=str(getattr(args, "contact_message", "") or ""),
        )
        session.send_app_data(SEQ_HEADER, payload)
        deadline = time.time() + max(1.0, float(args.request_timeout or 300.0))
        while time.time() < deadline:
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error while waiting contact response")
            item = session.get_app_item(timeout=0.1)
            if item is None:
                continue
            _seq, data_or_len, is_len = item
            if is_len:
                continue
            try:
                resp = parse_contact_response(bytes(data_or_len or b""))
            except Exception:
                continue
            # Add the concrete endpoint used by this client before logging, so
            # the GUI can save the accepted contact with a usable IP instead of
            # any bind address such as 0.0.0.0.
            if is_unspecified_ip(str(resp.get("receiver_ip") or "")):
                resp["receiver_ip"] = str(args.server_ip)
            resp["receiver_port"] = int(args.server_port or resp.get("receiver_port") or 9999)
            logger.info(build_contact_response_log(resp, peer=f"{args.server_ip}:{args.server_port}"))
            if bool(resp.get("accepted")):
                logger.info("Contact request accepted")
                return 0
            logger.warning(f"Contact request rejected: {resp.get('reason')}")
            return 3
        raise TimeoutError("contact_request_timeout")
    finally:
        if session is not None:
            try:
                session.stop()
            except Exception:
                pass
        if sock is not None:
            try:
                sock.close()
            except Exception:
                pass

def run_chat_client(args: argparse.Namespace, messages: List[str], adaptive_wifi: Optional[dict] = None) -> int:
    if not bool(getattr(args, "verbose_protocol", False)):
        os.environ.setdefault("RUDP_VERBOSE_PROTOCOL", "0")
    logger = setup_logger("AgoraLink-ChatSender")
    clean_messages = [str(m or "") for m in messages if str(m or "").strip()]
    if not clean_messages:
        raise ValueError("empty_chat_message")
    adaptive_wifi = dict(adaptive_wifi or _initial_adaptive_wifi_state(args))
    adaptive_current_unacked = int(adaptive_wifi["current_unacked"])

    chat_db = None
    if str(getattr(args, "chat_db", "") or ""):
        if not str(getattr(args, "chat_password", "") or ""):
            raise ValueError("chat_password_required_for_chat_db")
        from chat_db import ChatDatabase
        chat_db = ChatDatabase(str(args.chat_db), str(args.chat_password), my_peer_id=str(args.chat_sender_peer_id or "local"))

    sock = None
    session = None
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, _effective_sockbuf_bytes(args, "sock_rcvbuf"))
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, _effective_sockbuf_bytes(args, "sock_sndbuf"))
        sock.bind((str(args.bind_ip), int(args.bind_port)))
        logger.info(f"Local socket: {sock.getsockname()}")
        logger.info(f"Chat receiver: {(args.server_ip, args.server_port)}")

        conn_id = secrets.randbits(64)
        validator = make_server_identity_validator(
            str(args.server_pin_file),
            logger,
            require_existing_pin=bool(args.require_existing_server_pin),
        )
        session = ReliableUDPSession(
            conn_id,
            (str(args.server_ip), int(args.server_port)),
            sock,
            is_client=True,
            server_identity_validator=validator,
        )
        session.configure_app_delivery(len_only=False, small_payload_threshold=0)
        session.recovery_pacing_enabled = True
        session.send_max_unacked_pkts = int(adaptive_current_unacked)
        session.configure_lan_bulk_pacing(
            burst_pkts=int(getattr(args, "lan_pacing_burst_pkts", 32) or 32),
            interval_ms=float(getattr(args, "lan_pacing_interval_ms", 5.0) or 5.0),
            payload_size=int(getattr(args, "payload_size", 1400) or 1400),
        )
        session.configure_reorder_tolerance(int(getattr(args, "reorder_tolerance_pkts", 128) or 128))
        min_data_rto = max(0.05, float(args.min_data_rto_sec or 0.2))
        max_data_rto = max(min_data_rto, float(args.max_data_rto_sec or 4.0))
        session.min_data_rto = min_data_rto
        session.max_rto = max_data_rto
        session.base_rto = min_data_rto
        initial_rtt_s = max(0.0, float(args.initial_rtt_ms or 0.0) / 1000.0)
        if not args.disable_cc:
            session.cc = CubicCongestionControl(min_rto=min_data_rto, max_rto=max_data_rto, initial_rtt=(initial_rtt_s if initial_rtt_s > 0.0 else None))
        session.start_threads(start_receiver=True)
        _wait_for_handshake(session, args, logger, validator=validator)

        pending = {}
        seq = SEQ_HEADER
        for msg in clean_messages:
            payload = build_chat_message(
                msg,
                message_id=str(args.chat_message_id or ""),
                conversation_id=str(args.chat_conversation_id or ""),
                group_id=str(args.chat_group_id or ""),
                sender_peer_id=str(args.chat_sender_peer_id or "local"),
                receiver_peer_id=str(args.chat_receiver_peer_id or ""),
                body_type=str(args.chat_body_type or "text"),
                created_at=(float(args.chat_created_at) if float(args.chat_created_at or 0.0) > 0 else None),
            )
            if len(payload) > MAX_DATA_APP_PAYLOAD:
                raise ValueError(f"chat_message_too_large:{len(payload)}")
            chat_obj = __import__("json").loads(payload.decode("utf-8"))
            message_id = str(chat_obj.get("message_id") or "")
            pending[message_id] = chat_obj
            if chat_db is not None:
                chat_db.save_message(
                    message_id=message_id,
                    text=msg,
                    conversation_id=str(args.chat_conversation_id or ""),
                    group_id=str(args.chat_group_id or ""),
                    sender_peer_id=str(args.chat_sender_peer_id or "local"),
                    receiver_peer_id=str(args.chat_receiver_peer_id or ""),
                    direction="outgoing",
                    body_type=str(args.chat_body_type or "text"),
                    status="sent",
                    created_at=float(chat_obj.get("created_at") or time.time()),
                )
            session.send_app_data(seq, payload)
            logger.info(f"CHAT_MESSAGE sent: message_id={message_id}, bytes={len(msg.encode('utf-8'))}")
            seq += 1

        drain_start = time.time()
        while session.get_unacked_count() > 0:
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error while draining chat DATA")
            if time.time() - drain_start > float(args.final_ack_timeout):
                session.abort("chat_data_drain_timeout")
                raise TimeoutError("chat data drain timeout")
            time.sleep(0.05)

        ack_deadline = time.time() + max(1.0, float(args.complete_timeout or 10.0))
        while pending and time.time() < ack_deadline:
            item = session.get_app_item(timeout=0.1)
            if item is None:
                continue
            _seq, data_or_len, is_len = item
            if is_len:
                continue
            try:
                ack = parse_chat_ack(bytes(data_or_len or b""))
            except Exception:
                continue
            mid = str(ack.get("message_id") or "")
            if mid in pending:
                pending.pop(mid, None)
                logger.info(build_chat_ack_log(ack, peer=f"{args.server_ip}:{args.server_port}"))
                logger.info(f"CHAT_ACK received: message_id={mid}, status={ack.get('status')}")
                if chat_db is not None:
                    chat_db.mark_message_status(mid, "delivered", peer_id=str(ack.get("receiver_peer_id") or args.chat_receiver_peer_id or ""))
        if pending:
            logger.warning(f"CHAT_ACK timeout for messages: {sorted(pending)}")
            if chat_db is not None:
                for mid in pending:
                    chat_db.mark_message_status(mid, "failed", peer_id=str(args.chat_receiver_peer_id or ""), error="chat_ack_timeout")

        session.send_app_data(seq, EOF_PAYLOAD)
        time.sleep(0.2)
        return 0 if not pending else 2
    finally:
        if chat_db is not None:
            chat_db.close()
        try:
            if session is not None:
                session.stop()
        except Exception:
            pass
        try:
            if sock is not None:
                sock.close()
        except Exception:
            pass


def run_chat_read_client(args: argparse.Namespace, message_ids: List[str]) -> int:
    if not bool(getattr(args, "verbose_protocol", False)):
        os.environ.setdefault("RUDP_VERBOSE_PROTOCOL", "0")
    logger = setup_logger("AgoraLink-ChatReadSender")
    clean_ids = [str(m or "").strip() for m in message_ids if str(m or "").strip()]
    if not clean_ids:
        raise ValueError("empty_chat_read_message_id")

    sock = None
    session = None
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, _effective_sockbuf_bytes(args, "sock_rcvbuf"))
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, _effective_sockbuf_bytes(args, "sock_sndbuf"))
        sock.bind((str(args.bind_ip), int(args.bind_port)))
        conn_id = secrets.randbits(64)
        validator = make_server_identity_validator(
            str(args.server_pin_file),
            logger,
            require_existing_pin=bool(args.require_existing_server_pin),
        )
        session = ReliableUDPSession(
            conn_id,
            (str(args.server_ip), int(args.server_port)),
            sock,
            is_client=True,
            server_identity_validator=validator,
        )
        session.configure_app_delivery(len_only=False, small_payload_threshold=0)
        session.start_threads(start_receiver=True)
        _wait_for_handshake(session, args, logger, validator=validator)
        seq = SEQ_HEADER
        reader = str(args.chat_reader_peer_id or args.chat_sender_peer_id or "local")
        for mid in clean_ids:
            payload = build_chat_read(
                mid,
                conversation_id=str(args.chat_conversation_id or ""),
                group_id=str(args.chat_group_id or ""),
                reader_peer_id=reader,
            )
            if len(payload) > MAX_DATA_APP_PAYLOAD:
                raise ValueError(f"chat_read_too_large:{len(payload)}")
            session.send_app_data(seq, payload)
            logger.info(build_chat_read_log({"message_id": mid, "conversation_id": str(args.chat_conversation_id or ""), "group_id": str(args.chat_group_id or ""), "reader_peer_id": reader, "status": "read"}, peer=f"{args.server_ip}:{args.server_port}"))
            seq += 1
        drain_start = time.time()
        while session.get_unacked_count() > 0:
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error while draining chat read DATA")
            if time.time() - drain_start > float(args.final_ack_timeout):
                session.abort("chat_read_drain_timeout")
                raise TimeoutError("chat read drain timeout")
            time.sleep(0.05)
        session.send_app_data(seq, EOF_PAYLOAD)
        time.sleep(0.1)
        return 0
    finally:
        try:
            if session is not None:
                session.stop()
        except Exception:
            pass
        try:
            if sock is not None:
                sock.close()
        except Exception:
            pass


def run_client(args: argparse.Namespace) -> int:
    if not bool(getattr(args, "verbose_protocol", False)):
        os.environ.setdefault("RUDP_VERBOSE_PROTOCOL", "0")
    logger = setup_logger("RUDP-Sender")
    if args.show_ips:
        print_local_ip_candidates()

    adaptive_wifi = _initial_adaptive_wifi_state(args)

    if bool(getattr(args, "contact_request", False)):
        return run_contact_request_client(args)

    chat_read_ids = [str(x or "") for x in (getattr(args, "chat_read_message_id", []) or []) if str(x or "").strip()]
    if chat_read_ids and not str(args.file or ""):
        return run_chat_read_client(args, chat_read_ids)

    chat_messages = [str(x or "") for x in (getattr(args, "chat_message", []) or []) if str(x or "").strip()]
    if chat_messages and not str(args.file or ""):
        return run_chat_client(args, chat_messages, adaptive_wifi)

    input_file = Path(args.file).expanduser().resolve()
    if not input_file.is_file():
        raise FileNotFoundError(str(input_file))

    payload_size = int(args.payload_size)
    if payload_size <= 0 or payload_size > MAX_DATA_APP_PAYLOAD:
        raise ValueError(
            "payload-size must be in (0, {max_payload}], because wire size = "
            "HEADER_SIZE({header}) + DATA_FRAME_HEADER_LEN({frame}) + payload + AES-GCM tag({tag}) "
            "must stay <= UDP payload limit {udp_limit}".format(
                max_payload=MAX_DATA_APP_PAYLOAD,
                header=HEADER_SIZE,
                frame=DATA_FRAME_HEADER_LEN,
                tag=AEAD_TAG_LEN,
                udp_limit=UDP_MAX_DATAGRAM_PAYLOAD,
            )
        )

    total_bytes = input_file.stat().st_size
    logger.info(f"Calculating SHA256: {input_file}")
    file_sha256 = sha256_file(str(input_file))
    header_msg = build_file_header(
        str(input_file),
        payload_size=payload_size,
        sha256_hex=file_sha256,
        chat_message_id=str(getattr(args, "chat_message_id", "") or ""),
        chat_conversation_id=str(getattr(args, "chat_conversation_id", "") or ""),
        chat_group_id=str(getattr(args, "chat_group_id", "") or ""),
        chat_sender_peer_id=str(getattr(args, "chat_sender_peer_id", "") or ""),
        chat_receiver_peer_id=str(getattr(args, "chat_receiver_peer_id", "") or ""),
    )
    if len(header_msg) > MAX_DATA_APP_PAYLOAD:
        raise ValueError(f"file metadata header too large: {len(header_msg)} bytes")

    max_total_data_pkts = MAX_DATA_SEQ - SEQ_FIRST_BODY
    max_total_bytes = max_total_data_pkts * int(payload_size)
    if total_bytes > max_total_bytes:
        raise ValueError(f"file too large for DATA sequence space; max_total_bytes={max_total_bytes}")

    sock = None
    session = None
    start_ts = time.time()
    resume_offset = 0
    bytes_sent = 0
    pkts_sent = 0
    last_report_ts = start_ts
    last_report_bytes = 0
    last_json_ts = start_ts
    last_json_bytes = 0
    peak_mbps = 0.0
    last_effective_progress_ts = start_ts
    last_unacked_snapshot = -1
    last_lan_stats_ts = start_ts
    last_lan_sent_pkts = 0
    last_lan_acked_pkts = 0.0
    last_lan_retx_pkts = 0
    last_lan_wire_bytes_sent = 0
    last_lan_bulk_sent_pkts = 0
    last_lan_bulk_blocked_pacing = 0
    last_lan_bulk_blocked_cwnd = 0
    last_lan_bulk_blocked_unacked = 0
    last_lan_ack_deleted_pkts = 0
    last_lan_ack_scan_pkts = 0
    last_lan_blocked_cc = 0
    last_lan_blocked_unacked = 0
    summary_lan_samples = 0
    summary_sent_pps_sum = 0.0
    summary_payload_mbps_sum = 0.0
    summary_wire_mbps_sum = 0.0
    summary_rtt_ms_sum = 0.0
    summary_rtt_ms_max = 0.0
    summary_retrans_total = 0
    summary_bulk_blocked_unacked_total = 0
    summary_bulk_sent_total = 0
    summary_bulk_budget_sum = 0.0
    summary_bulk_budget_max = 0
    summary_wifi_guard_entries = 0

    lan_base_burst = max(1, int(getattr(args, "lan_pacing_burst_pkts", 32) or 32))
    lan_guard_burst = max(1, min(lan_base_burst, max(1, lan_base_burst // 2)))
    lan_current_burst = lan_base_burst
    wifi_guard_active = False
    wifi_guard_bad_streak = 0
    wifi_guard_good_streak = 0

    adaptive_enabled = bool(adaptive_wifi["enabled"])
    adaptive_min_unacked = int(adaptive_wifi["min_unacked"])
    adaptive_max_unacked = int(adaptive_wifi["max_unacked"])
    adaptive_step_unacked = int(adaptive_wifi["step_unacked"])
    adaptive_eval_interval = float(adaptive_wifi["eval_interval"])
    adaptive_current_unacked = int(adaptive_wifi["current_unacked"])
    adaptive_state = "DISABLED" if not adaptive_enabled else "STABLE"
    adaptive_last_eval_ts = start_ts
    adaptive_samples = 0
    adaptive_payload_mbps_sum = 0.0
    adaptive_wire_mbps_sum = 0.0
    adaptive_rtt_ms_sum = 0.0
    adaptive_max_rtt_ms = 0.0
    adaptive_retrans_sum = 0
    adaptive_blocked_unacked_sum = 0
    adaptive_prev_payload_mbps = 0.0
    adaptive_good_windows = 0
    adaptive_bad_windows = 0
    adaptive_window_up_events = 0
    adaptive_window_down_events = 0
    adaptive_burst_up_events = 0
    adaptive_burst_down_events = 0
    adaptive_eval_events = 0

    def _set_lan_burst(burst_pkts: int, reason: str = "", source: str = "WIFI_GUARD") -> None:
        nonlocal lan_current_burst
        burst_pkts = max(1, int(burst_pkts or 1))
        if burst_pkts == int(lan_current_burst):
            return
        lan_current_burst = burst_pkts
        try:
            session.configure_lan_bulk_pacing(
                burst_pkts=int(burst_pkts),
                interval_ms=float(getattr(args, "lan_pacing_interval_ms", 5.0) or 5.0),
                payload_size=int(payload_size),
            )
            logger.info(f"{source} burst={int(burst_pkts)}, reason={reason}")
        except Exception as exc:
            logger.info(f"{source} update_failed: {exc}")

    def _set_adaptive_unacked(new_cap: int, reason: str = "") -> None:
        nonlocal adaptive_current_unacked
        new_cap = max(adaptive_min_unacked, min(adaptive_max_unacked, int(new_cap or adaptive_current_unacked)))
        if new_cap == int(adaptive_current_unacked):
            return
        old_cap = int(adaptive_current_unacked)
        adaptive_current_unacked = int(new_cap)
        try:
            if session is not None:
                session.send_max_unacked_pkts = int(new_cap)
            logger.info(f"ADAPTIVE_WIFI_STATS action=window, old_max_unacked={old_cap}, max_unacked={int(new_cap)}, burst={int(lan_current_burst)}, state={adaptive_state}, reason={reason}")
        except Exception as exc:
            logger.info(f"ADAPTIVE_WIFI_STATS update_failed=window, error={exc}")

    def _maybe_adapt_wifi(now_ts: float, payload_mbps: float, wire_mbps: float, rtt_ms: float, retrans: int, blocked_unacked: int, bulk_budget: int) -> None:
        nonlocal adaptive_state, adaptive_last_eval_ts, adaptive_samples, adaptive_payload_mbps_sum, adaptive_wire_mbps_sum
        nonlocal adaptive_rtt_ms_sum, adaptive_max_rtt_ms, adaptive_retrans_sum, adaptive_blocked_unacked_sum
        nonlocal adaptive_prev_payload_mbps, adaptive_good_windows, adaptive_bad_windows, adaptive_eval_events
        nonlocal adaptive_window_up_events, adaptive_window_down_events, adaptive_burst_up_events, adaptive_burst_down_events
        nonlocal wifi_guard_active, summary_wifi_guard_entries
        if not adaptive_enabled:
            return
        adaptive_samples += 1
        adaptive_payload_mbps_sum += float(payload_mbps)
        adaptive_wire_mbps_sum += float(wire_mbps)
        adaptive_rtt_ms_sum += float(rtt_ms)
        adaptive_max_rtt_ms = max(float(adaptive_max_rtt_ms), float(rtt_ms))
        adaptive_retrans_sum += int(retrans)
        adaptive_blocked_unacked_sum += int(blocked_unacked)
        if float(now_ts) - float(adaptive_last_eval_ts) < float(adaptive_eval_interval):
            return
        if adaptive_samples <= 0:
            adaptive_last_eval_ts = float(now_ts)
            return

        avg_payload = adaptive_payload_mbps_sum / max(1, adaptive_samples)
        avg_wire = adaptive_wire_mbps_sum / max(1, adaptive_samples)
        avg_rtt = adaptive_rtt_ms_sum / max(1, adaptive_samples)
        max_rtt = float(adaptive_max_rtt_ms)
        retrans_sum = int(adaptive_retrans_sum)
        blocked_sum = int(adaptive_blocked_unacked_sum)
        prev_payload = float(adaptive_prev_payload_mbps or 0.0)
        adaptive_eval_events += 1

        throughput_drop = bool(prev_payload > 1.0 and avg_payload < prev_payload * 0.82)
        severe_bad = bool(max_rtt >= 800.0 or retrans_sum >= 160)
        bad = bool(max_rtt >= 700.0 or avg_rtt >= 320.0 or retrans_sum >= 96 or (throughput_drop and retrans_sum >= 32))
        good = bool(avg_rtt <= 240.0 and max_rtt <= 520.0 and retrans_sum <= 24 and avg_payload >= max(prev_payload * 0.95, 1.0))
        headroom = bool(blocked_sum >= max(300, adaptive_samples * 40))

        action = "hold"
        reason = f"avg_payload={avg_payload:.2f}, avg_rtt={avg_rtt:.1f}, max_rtt={max_rtt:.1f}, retrans={retrans_sum}, blocked_unacked={blocked_sum}, prev_payload={prev_payload:.2f}"

        if bad:
            adaptive_bad_windows += 1
            adaptive_good_windows = 0
            adaptive_state = "GUARD"
            if int(lan_current_burst) > int(lan_guard_burst):
                # First reaction: reduce burst only. Do not shrink the in-flight
                # window unless the path is still bad while already guarded.
                adaptive_burst_down_events += 1
                _set_lan_burst(lan_guard_burst, reason=reason, source="ADAPTIVE_WIFI_STATS")
                wifi_guard_active = True
                summary_wifi_guard_entries += 1
                action = "burst_down"
            elif severe_bad or adaptive_bad_windows >= 2:
                target = max(adaptive_min_unacked, int(adaptive_current_unacked) - int(adaptive_step_unacked))
                if target < int(adaptive_current_unacked):
                    adaptive_window_down_events += 1
                    _set_adaptive_unacked(target, reason=reason)
                    action = "window_down"
                    adaptive_bad_windows = 0
        elif good:
            adaptive_good_windows += 1
            adaptive_bad_windows = 0
            if int(lan_current_burst) < int(lan_base_burst) and adaptive_good_windows >= 2:
                adaptive_burst_up_events += 1
                _set_lan_burst(lan_base_burst, reason=reason, source="ADAPTIVE_WIFI_STATS")
                wifi_guard_active = False
                adaptive_state = "RECOVERY"
                action = "burst_restore"
                adaptive_good_windows = 0
            elif headroom and adaptive_good_windows >= 2 and int(adaptive_current_unacked) < int(adaptive_max_unacked):
                target = min(adaptive_max_unacked, int(adaptive_current_unacked) + int(adaptive_step_unacked))
                adaptive_window_up_events += 1
                adaptive_state = "PROBE_UP"
                _set_adaptive_unacked(target, reason=reason)
                action = "window_up"
                adaptive_good_windows = 0
            else:
                adaptive_state = "STABLE"
        else:
            adaptive_good_windows = 0
            adaptive_bad_windows = 0
            adaptive_state = "STABLE"

        logger.info(
            "ADAPTIVE_WIFI_STATS "
            f"action={action}, state={adaptive_state}, max_unacked={int(adaptive_current_unacked)}, burst={int(lan_current_burst)}, "
            f"avg_payload_mbps={avg_payload:.2f}, avg_wire_mbps={avg_wire:.2f}, avg_rtt_ms={avg_rtt:.1f}, "
            f"max_rtt_ms={max_rtt:.1f}, retrans={retrans_sum}, blocked_unacked={blocked_sum}, "
            f"bulk_budget={int(bulk_budget)}, good_windows={int(adaptive_good_windows)}, bad_windows={int(adaptive_bad_windows)}"
        )

        adaptive_prev_payload_mbps = float(avg_payload)
        adaptive_last_eval_ts = float(now_ts)
        adaptive_samples = 0
        adaptive_payload_mbps_sum = 0.0
        adaptive_wire_mbps_sum = 0.0
        adaptive_rtt_ms_sum = 0.0
        adaptive_max_rtt_ms = 0.0
        adaptive_retrans_sum = 0
        adaptive_blocked_unacked_sum = 0

    def note_effective_progress(unacked_count: Optional[int] = None, now_ts: Optional[float] = None) -> None:
        nonlocal last_effective_progress_ts, last_unacked_snapshot
        try:
            uc = int(session.get_unacked_count() if unacked_count is None and session is not None else unacked_count)
        except Exception:
            uc = -1
        if uc != last_unacked_snapshot:
            last_unacked_snapshot = uc
            last_effective_progress_ts = float(now_ts if now_ts is not None else time.time())

    def check_no_progress(stage: str, now_ts: Optional[float] = None) -> None:
        timeout = max(0.0, float(getattr(args, "no_progress_timeout", 120.0) or 0.0))
        if timeout <= 0:
            return
        now_val = float(now_ts if now_ts is not None else time.time())
        if now_val - last_effective_progress_ts > timeout:
            if session is not None:
                session.abort("network_no_progress")
            raise TimeoutError(f"network_no_progress:{stage}")

    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, _effective_sockbuf_bytes(args, "sock_rcvbuf"))
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, _effective_sockbuf_bytes(args, "sock_sndbuf"))
        sock.bind((str(args.bind_ip), int(args.bind_port)))
        logger.info(f"Local socket: {sock.getsockname()}")
        logger.info(f"Receiver: {(args.server_ip, args.server_port)}")
        logger.info(f"File: {input_file.name}, size={total_bytes} bytes, sha256={file_sha256}")

        conn_id = secrets.randbits(64)
        validator = make_server_identity_validator(
            str(args.server_pin_file),
            logger,
            require_existing_pin=bool(args.require_existing_server_pin),
        )
        session = ReliableUDPSession(
            conn_id,
            (str(args.server_ip), int(args.server_port)),
            sock,
            is_client=True,
            server_identity_validator=validator,
        )
        session.configure_app_delivery(len_only=False, small_payload_threshold=0)
        session.recovery_pacing_enabled = True
        session.send_max_unacked_pkts = int(adaptive_current_unacked)
        session.configure_lan_bulk_pacing(
            burst_pkts=int(getattr(args, "lan_pacing_burst_pkts", 32) or 32),
            interval_ms=float(getattr(args, "lan_pacing_interval_ms", 5.0) or 5.0),
            payload_size=int(getattr(args, "payload_size", 1400) or 1400),
        )
        session.configure_reorder_tolerance(int(getattr(args, "reorder_tolerance_pkts", 128) or 128))
        logger.info(
            f"ADAPTIVE_WIFI_STATS action=init, enabled={int(bool(adaptive_enabled))}, "
            f"state={adaptive_state}, max_unacked={int(adaptive_current_unacked)}, "
            f"min_unacked={int(adaptive_min_unacked)}, max_unacked_limit={int(adaptive_max_unacked)}, "
            f"burst={int(lan_current_burst)}, burst_min={int(lan_guard_burst)}, eval_interval={float(adaptive_eval_interval):.1f}s"
        )

        min_data_rto = max(0.05, float(args.min_data_rto_sec or 0.2))
        max_data_rto = max(min_data_rto, float(args.max_data_rto_sec or 4.0))
        session.min_data_rto = min_data_rto
        session.max_rto = max_data_rto
        session.base_rto = min_data_rto

        initial_rtt_s = max(0.0, float(args.initial_rtt_ms or 0.0) / 1000.0)
        if not args.disable_cc:
            session.cc = CubicCongestionControl(
                min_rto=min_data_rto,
                max_rto=max_data_rto,
                initial_rtt=(initial_rtt_s if initial_rtt_s > 0.0 else None),
            )
            session.app_pacing_enabled = True
            logger.info("Congestion control: CUBIC enabled")
        else:
            session.cc = None
            session.app_pacing_enabled = False
            logger.info("Congestion control: disabled")

        byte_pacer = None
        if float(args.send_rate_mbps or 0.0) > 0.0:
            byte_pacer = TokenBucketPacer(float(args.send_rate_mbps) * 1_000_000.0 / 8.0)
            logger.info(f"Application rate limit: {float(args.send_rate_mbps):.3f} Mbps")

        session.start_threads(start_receiver=True)
        _wait_for_handshake(session, args, logger, validator=validator)
        note_effective_progress()
        logger.info(build_transfer_started_log(
            conn_id=int(getattr(session, "conn_id", 0) or 0),
            chat_message_id=str(getattr(args, "chat_message_id", "") or ""),
            file_name=input_file.name,
            direction="outgoing",
            peer=f"{args.server_ip}:{args.server_port}",
            total_bytes=int(total_bytes),
            payload_size=int(payload_size),
            sock_sndbuf=int(_effective_sockbuf_bytes(args, "sock_sndbuf")),
            sock_rcvbuf=int(_effective_sockbuf_bytes(args, "sock_rcvbuf")),
            status="started",
        ))

        def send_payload(seq: int, payload: bytes) -> None:
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error")
            wire_size = data_packet_wire_size(len(payload))
            if byte_pacer is not None:
                byte_pacer.wait(wire_size)
            session.send_app_data(seq, payload)

        def report(force: bool = False) -> None:
            nonlocal last_report_ts, last_report_bytes, last_json_ts, last_json_bytes, peak_mbps, last_lan_stats_ts, last_lan_sent_pkts, last_lan_acked_pkts, last_lan_retx_pkts, last_lan_wire_bytes_sent, last_lan_bulk_sent_pkts, last_lan_bulk_blocked_pacing, last_lan_bulk_blocked_cwnd, last_lan_bulk_blocked_unacked, last_lan_ack_deleted_pkts, last_lan_ack_scan_pkts, last_lan_blocked_cc, last_lan_blocked_unacked, summary_lan_samples, summary_sent_pps_sum, summary_payload_mbps_sum, summary_wire_mbps_sum, summary_rtt_ms_sum, summary_rtt_ms_max, summary_retrans_total, summary_bulk_blocked_unacked_total, summary_bulk_sent_total, summary_bulk_budget_sum, summary_bulk_budget_max, summary_wifi_guard_entries, wifi_guard_active, wifi_guard_bad_streak, wifi_guard_good_streak, adaptive_state, adaptive_last_eval_ts, adaptive_samples, adaptive_payload_mbps_sum, adaptive_wire_mbps_sum, adaptive_rtt_ms_sum, adaptive_max_rtt_ms, adaptive_retrans_sum, adaptive_blocked_unacked_sum, adaptive_prev_payload_mbps, adaptive_good_windows, adaptive_bad_windows, adaptive_window_up_events, adaptive_window_down_events, adaptive_burst_up_events, adaptive_burst_down_events, adaptive_eval_events
            now = time.time()
            elapsed = max(now - start_ts, 1e-6)
            pct = (bytes_sent * 100.0 / total_bytes) if total_bytes > 0 else 100.0
            remaining_bytes = max(int(total_bytes) - int(bytes_sent), 0)
            unacked_count = session.get_unacked_count()
            note_effective_progress(unacked_count, now)

            json_due = force or (now - last_json_ts) >= float(getattr(args, "progress_json_interval", 0.2) or 0.2)
            if json_due:
                j_interval = max(now - last_json_ts, 1e-6)
                j_delta = max(0, int(bytes_sent) - int(last_json_bytes))
                current_mbps = (j_delta * 8.0) / j_interval / 1e6
                avg_mbps = (bytes_sent * 8.0) / elapsed / 1e6
                peak_mbps = max(float(peak_mbps or 0.0), current_mbps)
                rate_bps = (j_delta / j_interval) if j_delta > 0 else ((bytes_sent / elapsed) if bytes_sent > 0 else 0.0)
                eta_text = _format_duration(remaining_bytes / rate_bps) if rate_bps > 0 else "unknown"
                logger.info(build_transfer_progress_log(
                    conn_id=int(getattr(session, "conn_id", 0) or 0),
                    chat_message_id=str(getattr(args, "chat_message_id", "") or ""),
                    file_name=input_file.name,
                    direction="outgoing",
                    peer=f"{args.server_ip}:{args.server_port}",
                    transferred_bytes=int(bytes_sent),
                    total_bytes=int(total_bytes),
                    pct=round(float(pct), 3),
                    current_mbps=round(float(current_mbps), 3),
                    avg_mbps=round(float(avg_mbps), 3),
                    peak_mbps=round(float(peak_mbps), 3),
                    elapsed_sec=round(float(elapsed), 3),
                    eta=eta_text,
                    status="transferring" if pct < 100.0 else "completed",
                    unacked=int(unacked_count),
                ))
                last_json_ts = now
                last_json_bytes = int(bytes_sent)

            text_due = force or (now - last_report_ts) >= float(args.stats_interval)
            if not text_due:
                return
            interval = max(now - last_report_ts, 1e-6)
            avg_mbps = (bytes_sent * 8.0) / elapsed / 1e6
            int_mbps = ((bytes_sent - last_report_bytes) * 8.0) / interval / 1e6
            interval_bytes = int(bytes_sent) - int(last_report_bytes)
            rate_bps = (interval_bytes / interval) if interval_bytes > 0 else ((bytes_sent / elapsed) if bytes_sent > 0 else 0.0)
            eta_text = _format_duration(remaining_bytes / rate_bps) if rate_bps > 0 else "unknown"
            logger.info(
                f"Progress: {bytes_sent}/{total_bytes} bytes ({pct:.2f}%), "
                f"pkts={pkts_sent}, avg={avg_mbps:.2f} Mbps, interval={int_mbps:.2f} Mbps, "
                f"eta={eta_text}, unacked={unacked_count}"
            )
            try:
                snap = session.get_experiment_snapshot() if session is not None else {}
                lan_interval = max(now - float(last_lan_stats_ts or start_ts), 1e-6)
                sent_pkts_total = int(snap.get("data_packets_sent_original") or pkts_sent or 0)
                acked_pkts_total = float(snap.get("total_app_bytes_acked") or 0.0) / max(float(payload_size), 1.0)
                retx_total = int(snap.get("data_packets_retx_total") or 0)
                wire_bytes_total = int(snap.get("total_wire_bytes_sent") or snap.get("data_wire_bytes_sent_total") or 0)
                bulk_sent_total = int(snap.get("bulk_sent_pkts") or 0)
                bulk_blocked_pacing_total = int(snap.get("bulk_blocked_pacing") or 0)
                bulk_blocked_cwnd_total = int(snap.get("bulk_blocked_cwnd") or 0)
                bulk_blocked_unacked_total = int(snap.get("bulk_blocked_unacked") or 0)
                ack_deleted_total = int(snap.get("ack_deleted_pkts") or 0)
                ack_scan_total = int(snap.get("ack_scan_pkts") or 0)
                blocked_cc_total = int(snap.get("send_blocked_by_cc_count") or 0)
                blocked_unacked_total = int(snap.get("send_blocked_by_unacked_count") or 0)
                sent_pps = max(0.0, float(sent_pkts_total - int(last_lan_sent_pkts))) / lan_interval
                ack_pps = max(0.0, float(acked_pkts_total - float(last_lan_acked_pkts))) / lan_interval
                retrans = max(0, int(retx_total - int(last_lan_retx_pkts)))
                wire_mbps = max(0.0, float(wire_bytes_total - int(last_lan_wire_bytes_sent)) * 8.0 / lan_interval / 1e6)
                payload_mbps = max(0.0, float(sent_pps) * float(payload_size) * 8.0 / 1e6)
                bulk_sent_delta = max(0, int(bulk_sent_total - int(last_lan_bulk_sent_pkts)))
                bulk_blocked_pacing = max(0, int(bulk_blocked_pacing_total - int(last_lan_bulk_blocked_pacing)))
                bulk_blocked_cwnd = max(0, int(bulk_blocked_cwnd_total - int(last_lan_bulk_blocked_cwnd)))
                bulk_blocked_unacked = max(0, int(bulk_blocked_unacked_total - int(last_lan_bulk_blocked_unacked)))
                ack_deleted_delta = max(0, int(ack_deleted_total - int(last_lan_ack_deleted_pkts)))
                ack_scan_delta = max(0, int(ack_scan_total - int(last_lan_ack_scan_pkts)))
                blocked_cc = max(0, int(blocked_cc_total - int(last_lan_blocked_cc)))
                blocked_unacked = max(0, int(blocked_unacked_total - int(last_lan_blocked_unacked)))
                cwnd = snap.get("cwnd")
                cwnd_pkts = (float(cwnd) / max(float(payload_size), 1.0)) if cwnd is not None else 0.0
                rtt_s = snap.get("srtt") or snap.get("ack1_srtt_s") or 0.0
                rtt_ms = float(rtt_s or 0.0) * 1000.0
                bulk_budget_now = int(snap.get("bulk_budget_pkts") or 0)

                if sent_pkts_total > int(last_lan_sent_pkts):
                    summary_lan_samples += 1
                    summary_sent_pps_sum += float(sent_pps)
                    summary_payload_mbps_sum += float(payload_mbps)
                    summary_wire_mbps_sum += float(wire_mbps)
                    summary_rtt_ms_sum += float(rtt_ms)
                    summary_rtt_ms_max = max(float(summary_rtt_ms_max), float(rtt_ms))
                    summary_retrans_total += int(retrans)
                    summary_bulk_blocked_unacked_total += int(bulk_blocked_unacked)
                    summary_bulk_sent_total += int(bulk_sent_delta)
                    summary_bulk_budget_sum += float(bulk_budget_now)
                    summary_bulk_budget_max = max(int(summary_bulk_budget_max), int(bulk_budget_now))

                # Wi-Fi jitter guard: avoid keeping large bursts when the path
                # shows queue inflation or a sudden retransmission spike.  The
                # final 2% of a transfer is intentionally frozen because ACK,
                # unacked and retransmission samples are often tail-biased.
                adaptive_tail_frozen = bool(total_bytes > 0 and pct >= 98.0)
                if not adaptive_tail_frozen:
                    jitter_bad = bool(rtt_ms >= 500.0 or retrans >= 32)
                    jitter_good = bool(rtt_ms > 0.0 and rtt_ms <= 250.0 and retrans <= 2)
                    if jitter_bad:
                        wifi_guard_bad_streak += 1
                        wifi_guard_good_streak = 0
                    elif jitter_good:
                        wifi_guard_good_streak += 1
                        wifi_guard_bad_streak = 0
                    else:
                        wifi_guard_good_streak = 0

                    if (not wifi_guard_active) and (wifi_guard_bad_streak >= 2 or retrans >= 32):
                        wifi_guard_active = True
                        summary_wifi_guard_entries += 1
                        _set_lan_burst(lan_guard_burst, reason=f"rtt_ms={rtt_ms:.1f}, retrans={retrans}")
                    elif wifi_guard_active and wifi_guard_good_streak >= 6:
                        wifi_guard_active = False
                        _set_lan_burst(lan_base_burst, reason=f"stable_rtt_ms={rtt_ms:.1f}, retrans={retrans}")

                    _maybe_adapt_wifi(now, payload_mbps, wire_mbps, rtt_ms, retrans, bulk_blocked_unacked, bulk_budget_now)

                logger.info(
                    f"LAN_STATS payload_size={int(payload_size)}, payload_mbps={payload_mbps:.2f}, wire_mbps={wire_mbps:.2f}, "
                    f"unacked={int(snap.get('unacked_pkts') or unacked_count or 0)}, inflight_bytes={int(snap.get('inflight_bytes') or 0)}, "
                    f"cwnd_pkts={cwnd_pkts:.1f}, sent_pps={sent_pps:.1f}, ack_pps={ack_pps:.1f}, "
                    f"retrans={retrans}, rtt={rtt_ms:.2f}ms, wifi_guard={int(bool(wifi_guard_active))}, lan_burst={int(lan_current_burst)}, "
                    f"max_unacked={int(adaptive_current_unacked)}, adaptive_state={adaptive_state}, "
                    f"bulk_budget={int(bulk_budget_now)}, bulk_sent={bulk_sent_delta}, "
                    f"bulk_blocked_pacing={bulk_blocked_pacing}, bulk_blocked_cwnd={bulk_blocked_cwnd}, bulk_blocked_unacked={bulk_blocked_unacked}, "
                    f"ack_deleted={ack_deleted_delta}, ack_scan={ack_scan_delta}, inflight_repairs={int(snap.get('inflight_repair_count') or 0)}, "
                    f"blocked_cc={blocked_cc}, blocked_unacked={blocked_unacked}"
                )
                last_lan_stats_ts = now
                last_lan_sent_pkts = sent_pkts_total
                last_lan_acked_pkts = acked_pkts_total
                last_lan_retx_pkts = retx_total
                last_lan_wire_bytes_sent = wire_bytes_total
                last_lan_bulk_sent_pkts = bulk_sent_total
                last_lan_bulk_blocked_pacing = bulk_blocked_pacing_total
                last_lan_bulk_blocked_cwnd = bulk_blocked_cwnd_total
                last_lan_bulk_blocked_unacked = bulk_blocked_unacked_total
                last_lan_ack_deleted_pkts = ack_deleted_total
                last_lan_ack_scan_pkts = ack_scan_total
                last_lan_blocked_cc = blocked_cc_total
                last_lan_blocked_unacked = blocked_unacked_total
            except Exception as exc:
                logger.info(f"LAN_STATS unavailable: {exc}")
            last_report_ts = now
            last_report_bytes = bytes_sent

        send_payload(SEQ_HEADER, header_msg)
        decision = {}
        if not bool(getattr(args, "no_request_confirmation", False)):
            decision = _wait_for_receiver_decision(session, float(args.request_timeout), logger)

        resume_offset = max(0, int((decision or {}).get("resume_offset") or 0))
        if resume_offset > total_bytes:
            resume_offset = 0
        # Keep the implicit offset protocol on clean payload boundaries.
        if resume_offset > 0:
            resume_offset -= resume_offset % payload_size
        remaining_bytes_total = max(0, int(total_bytes) - int(resume_offset))
        remaining_pkts = int(math.ceil(remaining_bytes_total / payload_size)) if remaining_bytes_total > 0 else 0
        seq_eof = SEQ_FIRST_BODY + remaining_pkts
        if seq_eof >= DATA_SEQ_UPPER_EXCLUSIVE:
            raise ValueError(f"remaining file portion too large for DATA sequence space; seq_eof={seq_eof}")

        session.set_complete_expectations(seq_eof, total_bytes)
        session.send_range_announce(SEQ_HEADER, seq_eof)

        bytes_sent = int(resume_offset)
        last_report_bytes = int(resume_offset)
        if resume_offset > 0:
            logger.info(f"Resume enabled: offset={resume_offset}/{total_bytes} bytes ({resume_offset * 100.0 / max(total_bytes, 1):.2f}%)")
        report(force=True)

        seq = SEQ_FIRST_BODY
        read_chunk_bytes = max(int(payload_size), int(getattr(args, "file_read_chunk_mb", 4) or 4) * 1024 * 1024)
        logger.info(f"Sender batch read enabled: read_chunk={read_chunk_bytes // (1024 * 1024)} MiB, payload={payload_size} bytes")
        pending_tail = b""
        loop_cached_now = time.time()
        last_loop_check_ts = loop_cached_now
        bulk_batch = []
        bulk_batch_bytes = 0
        bulk_batch_limit = 64

        def flush_bulk_batch(force: bool = False) -> None:
            nonlocal seq, bytes_sent, pkts_sent, bulk_batch, bulk_batch_bytes, loop_cached_now, last_loop_check_ts, last_effective_progress_ts
            if not bulk_batch:
                return
            if (not force) and len(bulk_batch) < bulk_batch_limit:
                return
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error")
            if byte_pacer is not None:
                try:
                    byte_pacer.wait(sum(data_packet_wire_size(len(x)) for x in bulk_batch))
                except Exception:
                    # Fall back to per-batch best-effort pacing if a custom pacer rejects large waits.
                    for x in bulk_batch:
                        byte_pacer.wait(data_packet_wire_size(len(x)))
            sent_n = int(session.bulk_send_app_data(seq, bulk_batch, priority="bulk"))
            if sent_n != len(bulk_batch):
                raise RuntimeError(f"bulk_send short send: {sent_n}/{len(bulk_batch)}")
            seq += sent_n
            bytes_sent += int(bulk_batch_bytes)
            pkts_sent += sent_n
            bulk_batch = []
            bulk_batch_bytes = 0
            if (pkts_sent & 31) == 0:
                loop_cached_now = time.time()
            if force or (pkts_sent & 63) == 0 or (loop_cached_now - last_loop_check_ts) >= 0.010:
                last_effective_progress_ts = loop_cached_now
                report(force=False)
                check_no_progress("transferring", now_ts=loop_cached_now)
                last_loop_check_ts = loop_cached_now

        with open(input_file, "rb") as f:
            if resume_offset > 0:
                f.seek(resume_offset)
            while True:
                read_chunk = f.read(read_chunk_bytes)
                if not read_chunk:
                    break

                if pending_tail:
                    send_block = pending_tail + read_chunk
                    pending_tail = b""
                else:
                    send_block = read_chunk

                full_len = (len(send_block) // int(payload_size)) * int(payload_size)
                offset = 0
                while offset < full_len:
                    chunk = send_block[offset:offset + int(payload_size)]
                    bulk_batch.append(chunk)
                    bulk_batch_bytes += len(chunk)
                    offset += len(chunk)
                    if len(bulk_batch) >= bulk_batch_limit:
                        flush_bulk_batch(force=True)

                if full_len < len(send_block):
                    pending_tail = send_block[full_len:]

        if pending_tail:
            bulk_batch.append(pending_tail)
            bulk_batch_bytes += len(pending_tail)
        flush_bulk_batch(force=True)

        if seq != seq_eof:
            raise AssertionError(f"seq mismatch: seq={seq}, expected={seq_eof}")

        drain_start = time.time()
        while session.get_unacked_count() > 0:
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error while draining DATA")
            if time.time() - drain_start > float(args.final_ack_timeout):
                session.abort("data_drain_timeout_before_eof")
                raise TimeoutError("data drain timeout before EOF")
            report(force=False)
            check_no_progress("waiting_ack")
            time.sleep(0.1)

        send_payload(seq_eof, EOF_PAYLOAD)
        report(force=True)

        drain_start = time.time()
        while session.get_unacked_count() > 0:
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error while draining EOF")
            if time.time() - drain_start > float(args.final_ack_timeout):
                session.abort("eof_drain_timeout")
                raise TimeoutError("EOF drain timeout")
            report(force=False)
            check_no_progress("waiting_ack")
            time.sleep(0.1)

        complete_deadline = time.time() + max(1.0, float(args.complete_timeout))
        while not session.wait_for_complete(timeout=0.2):
            if session.has_fatal_error():
                raise RuntimeError(session.get_fatal_error() or "fatal protocol error while waiting COMPLETE")
            check_no_progress("waiting_complete")
            if time.time() >= complete_deadline:
                session.abort("complete_timeout")
                raise TimeoutError("complete_timeout")

        complete_info = session.get_received_complete_info() or {}
        ok = (
            int(complete_info.get("ack_base", -1)) == seq_eof + 1
            and int(complete_info.get("seen_max", -1)) == seq_eof
            and int(complete_info.get("expected_total", -1)) == total_bytes
            and int(complete_info.get("body_bytes_recv", -1)) == total_bytes
        )
        if not ok:
            session.abort("complete_sanity_mismatch")
            raise RuntimeError(f"receiver COMPLETE mismatch: {complete_info}")

        elapsed = max(time.time() - start_ts, 1e-6)
        final_avg = ((max(0, bytes_sent - resume_offset)) * 8.0 / elapsed / 1e6)
        try:
            final_snap = session.get_experiment_snapshot() if session is not None else {}
        except Exception:
            final_snap = {}
        sample_n = max(1, int(summary_lan_samples or 0))
        summary_avg_sent_pps = float(summary_sent_pps_sum) / float(sample_n)
        summary_avg_payload_mbps = float(summary_payload_mbps_sum) / float(sample_n)
        summary_avg_wire_mbps = float(summary_wire_mbps_sum) / float(sample_n)
        summary_avg_rtt_ms = float(summary_rtt_ms_sum) / float(sample_n)
        summary_avg_bulk_budget = float(summary_bulk_budget_sum) / float(sample_n)
        final_unacked = int(final_snap.get("unacked_pkts") or 0)
        final_inflight_repairs = int(final_snap.get("inflight_repair_count") or 0)
        final_retrans_total = int(final_snap.get("data_packets_retx_total") or summary_retrans_total or 0)
        final_bulk_blocked_unacked_total = int(final_snap.get("bulk_blocked_unacked") or summary_bulk_blocked_unacked_total or 0)
        logger.info(
            "SUMMARY_STATS "
            f"avg_mbps={final_avg:.3f}, peak_mbps={float(peak_mbps or 0.0):.3f}, elapsed_sec={elapsed:.3f}, "
            f"samples={int(summary_lan_samples)}, avg_sent_pps={summary_avg_sent_pps:.1f}, "
            f"avg_payload_mbps={summary_avg_payload_mbps:.2f}, avg_wire_mbps={summary_avg_wire_mbps:.2f}, "
            f"avg_rtt_ms={summary_avg_rtt_ms:.2f}, max_rtt_ms={float(summary_rtt_ms_max):.2f}, "
            f"retrans_total={int(final_retrans_total)}, bulk_blocked_unacked_total={int(final_bulk_blocked_unacked_total)}, "
            f"bulk_sent_total={int(summary_bulk_sent_total)}, avg_bulk_budget={summary_avg_bulk_budget:.1f}, "
            f"max_bulk_budget={int(summary_bulk_budget_max)}, wifi_guard_entries={int(summary_wifi_guard_entries)}, "
            f"adaptive_enabled={int(bool(adaptive_enabled))}, adaptive_evals={int(adaptive_eval_events)}, "
            f"adaptive_window_up={int(adaptive_window_up_events)}, adaptive_window_down={int(adaptive_window_down_events)}, "
            f"adaptive_burst_up={int(adaptive_burst_up_events)}, adaptive_burst_down={int(adaptive_burst_down_events)}, "
            f"final_max_unacked={int(adaptive_current_unacked)}, final_lan_burst={int(lan_current_burst)}, "
            f"adaptive_state={adaptive_state}, final_unacked={final_unacked}, inflight_repairs={final_inflight_repairs}"
        )
        logger.info(build_transfer_complete_log(
            conn_id=int(getattr(session, "conn_id", 0) or 0),
            chat_message_id=str(getattr(args, "chat_message_id", "") or ""),
            file_name=input_file.name,
            direction="outgoing",
            peer=f"{args.server_ip}:{args.server_port}",
            transferred_bytes=int(bytes_sent),
            total_bytes=int(total_bytes),
            pct=100.0,
            current_mbps=0.0,
            avg_mbps=round(float(final_avg), 3),
            peak_mbps=round(float(peak_mbps), 3),
            elapsed_sec=round(float(elapsed), 3),
            eta="0:00",
            status="completed",
        ))
        logger.info(f"Transfer complete: {bytes_sent} bytes in {elapsed:.3f}s, avg={final_avg:.2f} Mbps")
        return 0

    finally:
        try:
            if session is not None:
                session.stop()
        except Exception:
            pass
        try:
            if sock is not None:
                sock.close()
        except Exception:
            pass


def _classify_user_error(exc: Exception) -> tuple[str, str]:
    text = str(exc or "")
    if isinstance(exc, TimeoutError):
        if "approval_timeout" in text or "receiver approval" in text:
            return "approval_timeout", "Receiver confirmation timed out"
        if "network_no_progress" in text:
            return "network_no_progress", "Network made no progress for too long"
        if "complete_timeout" in text or "COMPLETE" in text:
            return "complete_timeout", "File data was sent, but receiver completion confirmation timed out"
        if "EOF" in text:
            return "eof_timeout", "EOF acknowledgement timed out"
        if "data drain" in text:
            return "data_drain_timeout", "Data acknowledgement timed out"
    if "receiver_identity_changed" in text or "Pinned receiver key mismatch" in text:
        return "receiver_identity_changed", "Receiver identity changed"
    if "save_dir_not_writable" in text or "save_dir_create_failed" in text or "save_dir_not_directory" in text:
        return "save_dir_not_writable", "The receiver save directory is not writable"
    if "disk_space_not_enough" in text:
        return "disk_space_not_enough", "The receiver does not have enough disk space"
    if "output_open_failed" in text:
        return "output_open_failed", "The receiver could not create the output file"
    if isinstance(exc, PermissionError) or "receiver_rejected" in text or "rejected" in text or "file_exists_cancelled" in text:
        return "receiver_rejected", "Receiver rejected the transfer"
    if isinstance(exc, FileNotFoundError):
        return "local_file_not_found", "Local file was not found"
    if "network_no_progress" in text:
        return "network_no_progress", "Network made no progress for too long"
    return "transfer_failed", "Transfer failed"


def main() -> int:
    args = build_argparser().parse_args()
    try:
        return run_client(args)
    except KeyboardInterrupt:
        return 130
    except Exception as exc:
        logger = setup_logger("RUDP-Sender")
        code, message = _classify_user_error(exc)
        logger.error(build_user_error(code, message, str(exc)))
        logger.error(f"Fatal: {exc}")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
