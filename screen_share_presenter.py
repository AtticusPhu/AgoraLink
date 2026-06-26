#!/usr/bin/env python3
"""Pure presentation helpers for AgoraLink screen-share UI text."""

from __future__ import annotations

from typing import Iterable, Set


ACTIVE_SCREEN_STATES: Set[str] = {"pending_offer", "pending_accept", "sending", "receiving"}


def _lang(lang: object) -> str:
    return "zh" if str(lang or "").lower().startswith("zh") else "en"


def remote_peer_text(lang: object = "en") -> str:
    return "对方" if _lang(lang) == "zh" else "Remote"


def screen_share_button_text(active: bool, lang: object = "en") -> str:
    if active:
        return "停止投屏" if _lang(lang) == "zh" else "Stop Share"
    return "投屏" if _lang(lang) == "zh" else "Share"


def screen_share_active_states() -> Set[str]:
    return set(ACTIVE_SCREEN_STATES)


def screen_audio_text(audio: object = None, lang: object = "en") -> str:
    if isinstance(audio, dict):
        state = str(audio.get("state") or "").strip().lower()
        enabled = bool(audio.get("enabled"))
        mode = str(audio.get("mode") or "").strip().lower()
    else:
        text = str(audio or "").strip().lower()
        state = text
        enabled = text in ("system", "on", "enabled", "system_audio_on")
        mode = "system" if enabled else ""
    zh = _lang(lang) == "zh"
    if state in ("fallback_video_only", "audio_failed_video_only", "unavailable"):
        return "系统音频不可用，已继续视频投屏" if zh else "System audio unavailable · Video only"
    if state in ("failed", "audio_failed"):
        return "音频失败，已继续视频投屏" if zh else "Audio failed · Continued video only"
    if enabled and mode == "system":
        return "系统音频开启" if zh else "System audio on"
    return "无音频" if zh else "Video only"


def screen_detail_text(profile: object = "", port: object = "", audio: object = None, lang: object = "en") -> str:
    parts = []
    profile_text = str(profile or "").strip()
    port_text = "" if port in (None, "") else str(port)
    if profile_text:
        parts.append(f"profile: {profile_text}")
    if port_text:
        parts.append(f"port: {port_text}")
    if audio is not None:
        parts.append(screen_audio_text(audio, lang))
    return "  ".join(parts)


def screen_start_failed_text(reason: object = "", lang: object = "en") -> str:
    detail = str(reason or "").strip() or "unknown"
    return f"启动失败：{detail}" if _lang(lang) == "zh" else f"Start failed: {detail}"


def screen_stop_failed_text(reason: object = "", lang: object = "en") -> str:
    detail = str(reason or "").strip() or "unknown"
    return f"停止失败：{detail}" if _lang(lang) == "zh" else f"Stop failed: {detail}"


def screen_stopped_text(lang: object = "en") -> str:
    return "投屏已停止" if _lang(lang) == "zh" else "Screen sharing stopped"


def screen_rejected_by_peer_text(peer_label: object = "", lang: object = "en") -> str:
    name = str(peer_label or "").strip() or remote_peer_text(lang)
    return f"{name} 拒绝投屏" if _lang(lang) == "zh" else f"{name} rejected screen sharing"


def screen_rejected_local_text(lang: object = "en") -> str:
    return "已拒绝投屏" if _lang(lang) == "zh" else "Screen sharing rejected"


def screen_offer_title(lang: object = "en") -> str:
    return "投屏邀请" if _lang(lang) == "zh" else "Screen share invitation"


def normalize_screen_state(state: object) -> str:
    value = str(state or "idle").strip().lower()
    aliases = {
        "startup_failed": "failed",
        "stop_failed": "failed",
        "remote_rejected": "rejected",
        "remote_stopped": "stopped",
    }
    return aliases.get(value, value or "idle")


def screen_share_status_text(
    state: object,
    detail: object = "",
    *,
    lang: object = "en",
    peer_label: object = "",
    profile: object = "",
    port: object = "",
) -> str:
    raw_state = str(state or "idle").strip().lower()
    normalized = normalize_screen_state(raw_state)
    name = str(peer_label or "").strip()
    profile_text = str(profile or "").strip() or "-"
    port_text = str(port or "").strip() or "-"
    detail_text = str(detail or "").strip()
    if _lang(lang) == "zh":
        if normalized == "idle":
            return "空闲"
        if normalized == "pending_offer":
            return f"等待 {name} 接受投屏"
        if normalized == "pending_accept":
            return f"{name} 已接受，正在启动投屏"
        if normalized == "sending":
            return f"正在投屏给 {name}（profile: {profile_text}，port: {port_text}）"
        if normalized == "receiving":
            return f"正在观看 {name} 的屏幕（profile: {profile_text}，port: {port_text}）"
        if normalized == "rejected":
            return f"{name} 拒绝投屏" + (f"：{detail_text}" if detail_text else "")
        if normalized == "stopped":
            return f"{name} 已停止投屏" if name else "投屏已停止"
        if normalized == "failed":
            prefix = "停止失败" if raw_state == "stop_failed" else "启动失败"
            return prefix + (f"：{detail_text}" if detail_text else "")
        return str(state or "") + (f"：{detail_text}" if detail_text else "")
    if normalized == "idle":
        return "Idle"
    if normalized == "pending_offer":
        return f"Waiting for {name} to accept screen sharing"
    if normalized == "pending_accept":
        return f"{name} accepted, starting screen sharing"
    if normalized == "sending":
        return f"Sharing screen with {name} (profile: {profile_text}, port: {port_text})"
    if normalized == "receiving":
        return f"Watching {name}'s screen (profile: {profile_text}, port: {port_text})"
    if normalized == "rejected":
        return f"{name} rejected screen sharing" + (f": {detail_text}" if detail_text else "")
    if normalized == "stopped":
        return f"{name} stopped screen sharing" if name else "Screen sharing stopped"
    if normalized == "failed":
        prefix = "Stop failed" if raw_state == "stop_failed" else "Start failed"
        return prefix + (f": {detail_text}" if detail_text else "")
    return str(state or "") + (f": {detail_text}" if detail_text else "")
