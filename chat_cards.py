#!/usr/bin/env python3
"""Runtime chat card helpers for AgoraLink.

Cards are UI events, not database records. They let the Kivy chat stream render
system hints, file-transfer activity, and screen-share controls without changing
the chat schema or high-frequency transfer persistence behavior.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Dict, Iterable, List, Mapping, Optional


CARD_TEXT = "text"
CARD_SYSTEM = "system"
CARD_FILE_OFFER = "file_offer"
CARD_FILE_TRANSFER = "file_transfer"
CARD_SCREEN_OFFER = "screen_offer"
CARD_SCREEN_STATE = "screen_state"

CARD_TYPES = {
    CARD_TEXT,
    CARD_SYSTEM,
    CARD_FILE_OFFER,
    CARD_FILE_TRANSFER,
    CARD_SCREEN_OFFER,
    CARD_SCREEN_STATE,
}


@dataclass(frozen=True)
class ChatCardAction:
    label: str
    action: str = ""
    style: str = "secondary"

    def to_dict(self) -> Dict[str, object]:
        return asdict(self)


@dataclass(frozen=True)
class ChatCard:
    card_type: str
    title: str = ""
    subtitle: str = ""
    status: str = ""
    detail: str = ""
    direction: str = ""
    side: str = ""
    actions: List[Dict[str, object]] = field(default_factory=list)
    card_id: str = ""
    timestamp: float = 0.0
    meta: Dict[str, object] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, object]:
        return asdict(self)


def _clean_text(value: object) -> str:
    return str(value or "").strip()


def truncate_filename(name: object, max_chars: int = 48) -> str:
    """Return a UI-safe filename that preserves the extension when possible."""
    text = " ".join(_clean_text(name).split())
    if not text:
        text = "未命名文件"
    try:
        limit = max(8, int(max_chars or 48))
    except Exception:
        limit = 48
    if len(text) <= limit:
        return text
    dot = text.rfind(".")
    if dot > 0 and len(text) - dot <= 16:
        stem = text[:dot]
        ext = text[dot:]
        stem_budget = max(8, limit - len(ext) - 3)
        head = max(4, int(stem_budget * 0.6))
        tail = max(4, stem_budget - head)
        if head + tail < len(stem):
            return stem[:head] + "..." + stem[-tail:] + ext
        return text[: max(1, limit - 3)] + "..."
    head = max(4, (limit - 3) // 2)
    tail = max(4, limit - 3 - head)
    return text[:head] + "..." + text[-tail:]


def _clean_actions(actions: Optional[Iterable[object]]) -> List[Dict[str, object]]:
    result: List[Dict[str, object]] = []
    for item in actions or []:
        if isinstance(item, ChatCardAction):
            result.append(item.to_dict())
        elif isinstance(item, Mapping):
            result.append(
                ChatCardAction(
                    label=_clean_text(item.get("label")),
                    action=_clean_text(item.get("action")),
                    style=_clean_text(item.get("style")) or "secondary",
                ).to_dict()
            )
    return [item for item in result if item.get("label")]


def make_card(
    card_type: str,
    *,
    title: object = "",
    subtitle: object = "",
    status: object = "",
    detail: object = "",
    direction: object = "",
    side: object = "",
    actions: Optional[Iterable[object]] = None,
    card_id: object = "",
    timestamp: object = 0.0,
    meta: Optional[Mapping[str, object]] = None,
) -> Dict[str, object]:
    normalized_type = _clean_text(card_type) or CARD_SYSTEM
    if normalized_type not in CARD_TYPES:
        normalized_type = CARD_SYSTEM
    try:
        ts = float(timestamp or 0.0)
    except Exception:
        ts = 0.0
    return ChatCard(
        card_type=normalized_type,
        title=_clean_text(title),
        subtitle=_clean_text(subtitle),
        status=_clean_text(status),
        detail=_clean_text(detail),
        direction=_clean_text(direction),
        side=_clean_text(side),
        actions=_clean_actions(actions),
        card_id=_clean_text(card_id),
        timestamp=ts,
        meta=dict(meta or {}),
    ).to_dict()


def system_card(text: object, *, title: object = "System", card_id: object = "", timestamp: object = 0.0) -> Dict[str, object]:
    return make_card(CARD_SYSTEM, title=title, detail=text, card_id=card_id, timestamp=timestamp)


def file_transfer_card(
    *,
    title: object = "File transfer",
    subtitle: object = "",
    status: object = "",
    detail: object = "",
    card_id: object = "",
    timestamp: object = 0.0,
    meta: Optional[Mapping[str, object]] = None,
) -> Dict[str, object]:
    return make_card(
        CARD_FILE_TRANSFER,
        title=title,
        subtitle=subtitle,
        status=status,
        detail=detail,
        card_id=card_id,
        timestamp=timestamp,
        meta=meta,
    )


def screen_state_card(
    *,
    title: object = "Screen share",
    subtitle: object = "",
    status: object = "",
    detail: object = "",
    card_id: object = "",
    timestamp: object = 0.0,
    actions: Optional[Iterable[object]] = None,
    meta: Optional[Mapping[str, object]] = None,
) -> Dict[str, object]:
    return make_card(
        CARD_SCREEN_STATE,
        title=title,
        subtitle=subtitle,
        status=status,
        detail=detail,
        actions=actions,
        card_id=card_id,
        timestamp=timestamp,
        meta=meta,
    )
