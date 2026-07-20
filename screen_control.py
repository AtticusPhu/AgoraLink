#!/usr/bin/env python3
"""AgoraLink screen sharing control message format.

This module only builds and validates control messages. Delivery is expected
to use AgoraLink's existing chat message channel. Native media remains on its
dedicated UDP path and does not enter the RUDP file transfer queue.
"""

from __future__ import annotations

import json
from datetime import datetime, timezone
from typing import Any, Dict, Mapping, Union

from screen_profile import DEFAULT_SCREEN_PROFILE, profile_id_from_info


SCREEN_SHARE_OFFER = "SCREEN_SHARE_OFFER"
SCREEN_SHARE_ACCEPT = "SCREEN_SHARE_ACCEPT"
SCREEN_SHARE_REJECT = "SCREEN_SHARE_REJECT"
SCREEN_SHARE_STOP = "SCREEN_SHARE_STOP"
SCREEN_SHARE_STATE = "SCREEN_SHARE_STATE"

SCREEN_CONTROL_VERSION = 1
DEFAULT_SCREEN_PORT = 55000
SCREEN_BACKEND_RUST = "rust"
SCREEN_BACKENDS = frozenset({SCREEN_BACKEND_RUST})

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


def _profiles_list(value: object) -> list:
    if value in (None, ""):
        return []
    if not isinstance(value, (list, tuple)):
        raise ValueError("profiles must be an array")
    profiles = []
    for item in value:
        if not isinstance(item, Mapping):
            raise ValueError("profile item must be an object")
        profiles.append(dict(item))
    return profiles


def _validate_profiles(value: object, field: str) -> None:
    for item in _profiles_list(value):
        _require_text(item.get("id") or item.get("name"), f"{field}.id")


def _audio_config(value: object) -> Dict[str, Any]:
    if value in (None, ""):
        return {}
    if not isinstance(value, Mapping):
        raise ValueError("audio must be an object")
    enabled = bool(value.get("enabled"))
    if not enabled:
        return {"enabled": False, "mode": "none"}
    mode = str(value.get("mode") or "system").strip().lower()
    if mode != "system":
        raise ValueError("audio.mode must be system")
    codec = str(value.get("codec") or "aac").strip().lower()
    if codec != "aac":
        raise ValueError("audio.codec must be aac")
    try:
        sample_rate = int(value.get("sample_rate") or 48000)
        channels = int(value.get("channels") or 2)
        bitrate = int(value.get("bitrate") or 128000)
    except Exception as exc:
        raise ValueError("audio sample_rate/channels/bitrate must be integers") from exc
    if sample_rate <= 0 or channels <= 0 or bitrate <= 0:
        raise ValueError("audio sample_rate/channels/bitrate must be positive")
    return {
        "enabled": True,
        "mode": "system",
        "codec": "aac",
        "sample_rate": sample_rate,
        "channels": channels,
        "bitrate": bitrate,
    }


def _backend_config(value: object) -> str:
    if value in (None, ""):
        return ""
    backend = str(value or "").strip().lower()
    if backend not in SCREEN_BACKENDS:
        raise ValueError("backend must be rust")
    return backend


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
    profiles: object = None,
    preferred_profile: object = None,
    audio: object = None,
    backend: object = None,
) -> Dict[str, object]:
    payload: Dict[str, Any] = {
        "host": _require_text(host, "host"),
        "port": _require_port(port),
        "profile_name": _require_text(profile_name, "profile_name"),
        "profile": dict(profile_dict),
    }
    audio_config = _audio_config(audio)
    if audio_config:
        payload["audio"] = audio_config
    backend_value = _backend_config(backend)
    if backend_value:
        payload["backend"] = backend_value
    advertised_profiles = _profiles_list(profiles)
    preferred = str(preferred_profile or "").strip()
    if advertised_profiles:
        payload["profiles"] = advertised_profiles
        payload["preferred_profile"] = preferred or profile_id_from_info(advertised_profiles[0], DEFAULT_SCREEN_PROFILE)
    message = _base_message(
        SCREEN_SHARE_OFFER,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        payload,
    )
    if advertised_profiles:
        message["profiles"] = advertised_profiles
        message["preferred_profile"] = payload["preferred_profile"]
    if audio_config:
        message["audio"] = audio_config
    if backend_value:
        message["backend"] = backend_value
    return message


def make_accept(
    session_id: object,
    sender_peer_id: object,
    receiver_peer_id: object,
    host: object,
    port: object,
    selected_profile: object,
    audio: object = None,
    backend: object = None,
) -> Dict[str, object]:
    screen_port = _require_port(port)
    selected_profile_id = profile_id_from_info(selected_profile, DEFAULT_SCREEN_PROFILE)
    selected_profile_info = dict(selected_profile) if isinstance(selected_profile, Mapping) else {"id": selected_profile_id, "name": selected_profile_id}
    audio_config = _audio_config(audio)
    backend_value = _backend_config(backend)
    payload: Dict[str, Any] = {
        "host": _require_text(host, "host"),
        "port": screen_port,
        "screen_port": screen_port,
        "selected_profile": selected_profile_id,
        "selected_profile_info": selected_profile_info,
    }
    if audio_config:
        payload["audio"] = audio_config
    if backend_value:
        payload["backend"] = backend_value
    message = _base_message(
        SCREEN_SHARE_ACCEPT,
        session_id,
        sender_peer_id,
        receiver_peer_id,
        payload,
    )
    message["screen_port"] = screen_port
    message["selected_profile"] = selected_profile_id
    message["selected_profile_info"] = selected_profile_info
    if audio_config:
        message["audio"] = audio_config
    if backend_value:
        message["backend"] = backend_value
    return message


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
    if "screen_port" in message:
        _require_port(message.get("screen_port"))
    if "profiles" in message:
        _validate_profiles(message.get("profiles"), "profiles")
    if "preferred_profile" in message:
        _require_text(message.get("preferred_profile"), "preferred_profile")
    if "selected_profile" in message:
        _require_text(message.get("selected_profile"), "selected_profile")
    if "selected_profile_info" in message and not isinstance(message.get("selected_profile_info"), Mapping):
        raise ValueError("selected_profile_info must be an object")
    if "audio" in message:
        _audio_config(message.get("audio"))
    if "backend" in message:
        _backend_config(message.get("backend"))
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
        if "profiles" in payload:
            _validate_profiles(payload.get("profiles"), "payload.profiles")
        if "preferred_profile" in payload:
            _require_text(payload.get("preferred_profile"), "payload.preferred_profile")
        if "audio" in payload:
            _audio_config(payload.get("audio"))
        if "backend" in payload:
            _backend_config(payload.get("backend"))
    elif message_type == SCREEN_SHARE_ACCEPT:
        _require_text(payload.get("host"), "payload.host")
        _require_port(payload.get("port"))
        if "screen_port" in payload:
            _require_port(payload.get("screen_port"))
        if "selected_profile" in payload:
            selected = payload.get("selected_profile")
            if isinstance(selected, Mapping):
                _require_text(selected.get("id") or selected.get("name"), "payload.selected_profile.id")
            else:
                _require_text(selected, "payload.selected_profile")
        if "selected_profile_info" in payload and not isinstance(payload.get("selected_profile_info"), Mapping):
            raise ValueError("payload.selected_profile_info must be an object")
        if "audio" in payload:
            _audio_config(payload.get("audio"))
        if "backend" in payload:
            _backend_config(payload.get("backend"))
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
    "SCREEN_BACKEND_RUST",
    "make_offer",
    "make_accept",
    "make_reject",
    "make_stop",
    "make_state",
    "parse_screen_control_message",
    "is_screen_control_message",
]
