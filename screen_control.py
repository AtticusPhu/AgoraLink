#!/usr/bin/env python3
"""AgoraLink screen sharing control message format.

This module only builds and validates control messages. Delivery is expected
to use AgoraLink's existing chat message channel. Video remains FFmpeg/UDP and
does not enter the RUDP file transfer queue.
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from typing import Any, Dict, Mapping, Union


SCREEN_SHARE_OFFER = "SCREEN_SHARE_OFFER"
SCREEN_SHARE_ACCEPT = "SCREEN_SHARE_ACCEPT"
SCREEN_SHARE_REJECT = "SCREEN_SHARE_REJECT"
SCREEN_SHARE_STOP = "SCREEN_SHARE_STOP"
SCREEN_SHARE_STATE = "SCREEN_SHARE_STATE"

SCREEN_CONTROL_VERSION = 1
DEFAULT_SCREEN_PORT = 50020

SCREEN_CONTROL_TYPES = frozenset(
    {
        SCREEN_SHARE_OFFER,
        SCREEN_SHARE_ACCEPT,
        SCREEN_SHARE_REJECT,
        SCREEN_SHARE_STOP,
        SCREEN_SHARE_STATE,
    }
)

REQUIRED_MESSAGE_FIELDS = (
    "type",
    "version",
    "session_id",
    "sender_peer_id",
    "receiver_peer_id",
    "timestamp",
    "payload",
)

JsonLike = Union[str, Mapping[str, Any]]


def _utc_timestamp() -> str:
    return datetime.now(timezone.utc).isoformat()


def _require_text(value: object, field: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise ValueError(f"{field} is required")
    return text


def _require_port(value: object) -> int:
    port = int(value)
    if port < 1 or port > 65535:
        raise ValueError("port must be in 1..65535")
    return port


def _base_message(
    message_type: str,
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    payload: Mapping[str, Any],
) -> Dict[str, object]:
    if message_type not in SCREEN_CONTROL_TYPES:
        raise ValueError(f"unsupported screen control type: {message_type}")
    if not isinstance(payload, Mapping):
        raise ValueError("payload must be an object")
    return {
        "type": message_type,
        "version": SCREEN_CONTROL_VERSION,
        "session_id": _require_text(session_id, "session_id"),
        "sender_peer_id": _require_text(sender_peer_id, "sender_peer_id"),
        "receiver_peer_id": _require_text(receiver_peer_id, "receiver_peer_id"),
        "timestamp": _utc_timestamp(),
        "payload": dict(payload),
    }


def make_offer(
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    host: object,
    port: object,
    profile_name: object,
    profile_dict: Mapping[str, Any],
) -> Dict[str, object]:
    return _base_message(
        SCREEN_SHARE_OFFER,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        {
            "host": _require_text(host, "host"),
            "port": _require_port(port),
            "profile_name": _require_text(profile_name, "profile_name"),
            "profile": dict(profile_dict),
        },
    )


def make_accept(
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    host: object,
    port: object,
    selected_profile: Mapping[str, Any],
) -> Dict[str, object]:
    return _base_message(
        SCREEN_SHARE_ACCEPT,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        {
            "host": _require_text(host, "host"),
            "port": _require_port(port),
            "selected_profile": dict(selected_profile),
        },
    )


def make_reject(
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    reason: object,
) -> Dict[str, object]:
    return _base_message(
        SCREEN_SHARE_REJECT,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        {"reason": str(reason or "")},
    )


def make_stop(
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    reason: object = "",
) -> Dict[str, object]:
    return _base_message(
        SCREEN_SHARE_STOP,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        {"reason": str(reason or "")},
    )


def make_state(
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    state: object,
    detail: object = "",
) -> Dict[str, object]:
    return _base_message(
        SCREEN_SHARE_STATE,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        {
            "state": _require_text(state, "state"),
            "detail": str(detail or ""),
        },
    )


def _load_message_object(obj_or_text: JsonLike) -> Dict[str, Any]:
    if isinstance(obj_or_text, str):
        try:
            parsed = json.loads(obj_or_text)
        except json.JSONDecodeError as exc:
            raise ValueError(f"invalid JSON: {exc}") from exc
    else:
        parsed = obj_or_text
    if not isinstance(parsed, Mapping):
        raise ValueError("screen control message must be a JSON object")
    return dict(parsed)


def _validate_common_fields(message: Mapping[str, Any]) -> None:
    for field in REQUIRED_MESSAGE_FIELDS:
        if field not in message:
            raise ValueError(f"missing field: {field}")
    if message.get("type") not in SCREEN_CONTROL_TYPES:
        raise ValueError(f"unsupported screen control type: {message.get('type')!r}")
    if int(message.get("version")) != SCREEN_CONTROL_VERSION:
        raise ValueError(f"unsupported screen control version: {message.get('version')!r}")
    _require_text(message.get("session_id"), "session_id")
    _require_text(message.get("sender_peer_id"), "sender_peer_id")
    _require_text(message.get("receiver_peer_id"), "receiver_peer_id")
    _require_text(message.get("timestamp"), "timestamp")
    if not isinstance(message.get("payload"), Mapping):
        raise ValueError("payload must be an object")


def _validate_payload(message: Mapping[str, Any]) -> None:
    message_type = str(message.get("type"))
    payload = dict(message.get("payload") or {})
    if message_type == SCREEN_SHARE_OFFER:
        _require_text(payload.get("host"), "payload.host")
        _require_port(payload.get("port"))
        _require_text(payload.get("profile_name"), "payload.profile_name")
        if not isinstance(payload.get("profile"), Mapping):
            raise ValueError("payload.profile must be an object")
    elif message_type == SCREEN_SHARE_ACCEPT:
        _require_text(payload.get("host"), "payload.host")
        _require_port(payload.get("port"))
        if not isinstance(payload.get("selected_profile"), Mapping):
            raise ValueError("payload.selected_profile must be an object")
    elif message_type == SCREEN_SHARE_REJECT:
        if "reason" not in payload:
            raise ValueError("missing field: payload.reason")
    elif message_type == SCREEN_SHARE_STOP:
        if "reason" not in payload:
            raise ValueError("missing field: payload.reason")
    elif message_type == SCREEN_SHARE_STATE:
        _require_text(payload.get("state"), "payload.state")
        if "detail" not in payload:
            raise ValueError("missing field: payload.detail")


def parse_screen_control_message(obj_or_text: JsonLike) -> Dict[str, Any]:
    message = _load_message_object(obj_or_text)
    _validate_common_fields(message)
    _validate_payload(message)
    message["payload"] = dict(message["payload"])
    return message


def is_screen_control_message(obj_or_text: JsonLike) -> bool:
    try:
        parse_screen_control_message(obj_or_text)
        return True
    except Exception:
        return False


__all__ = [
    "SCREEN_SHARE_OFFER",
    "SCREEN_SHARE_ACCEPT",
    "SCREEN_SHARE_REJECT",
    "SCREEN_SHARE_STOP",
    "SCREEN_SHARE_STATE",
    "SCREEN_CONTROL_VERSION",
    "DEFAULT_SCREEN_PORT",
    "make_offer",
    "make_accept",
    "make_reject",
    "make_stop",
    "make_state",
    "parse_screen_control_message",
    "is_screen_control_message",
]
