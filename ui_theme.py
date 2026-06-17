#!/usr/bin/env python3
"""Design tokens for the AgoraLink UI preview.

The first theme is intentionally light-only.  The object shape leaves room for
dark mode later without making the current UI migration larger than needed.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Dict, Tuple

Color = Tuple[float, float, float, float]


def hex_to_rgba(value: str, alpha: float = 1.0) -> Color:
    text = str(value or "").strip().lstrip("#")
    if len(text) != 6:
        raise ValueError(f"invalid hex color: {value!r}")
    return (
        int(text[0:2], 16) / 255.0,
        int(text[2:4], 16) / 255.0,
        int(text[4:6], 16) / 255.0,
        float(alpha),
    )


@dataclass(frozen=True)
class Theme:
    name: str
    colors: Dict[str, Color]
    radius: Dict[str, int]
    spacing: Dict[str, int]
    font_size: Dict[str, int]
    fonts: Dict[str, Tuple[str, ...]]
    shadow: Dict[str, Color]


LIGHT_THEME = Theme(
    name="Calm Rounded Productivity Blue",
    colors={
        "background": hex_to_rgba("#F6FAFF"),
        "surface": hex_to_rgba("#FFFFFF"),
        "surface_muted": hex_to_rgba("#EEF6FF"),
        "surface_blue": hex_to_rgba("#EAF4FF"),
        "text_primary": hex_to_rgba("#1F2933"),
        "text_secondary": hex_to_rgba("#667085"),
        "text_muted": hex_to_rgba("#98A2B3"),
        "border": hex_to_rgba("#D8E6F5"),
        "border_soft": hex_to_rgba("#E6EEF7"),
        "accent": hex_to_rgba("#5AA7E8"),
        "accent_hover": hex_to_rgba("#4B98D9"),
        "accent_soft": hex_to_rgba("#DCEEFF"),
        "success": hex_to_rgba("#34C759"),
        "success_soft": hex_to_rgba("#E9F8EE"),
        "warning": hex_to_rgba("#FFB020"),
        "warning_soft": hex_to_rgba("#FFF4E2"),
        "danger": hex_to_rgba("#FF4D4F"),
        "danger_soft": hex_to_rgba("#FFE9E8"),
        "white": hex_to_rgba("#FFFFFF"),
        "transparent": (0, 0, 0, 0),
    },
    radius={
        "small": 8,
        "medium": 12,
        "card": 16,
        "large": 20,
        "page": 24,
        "pill": 999,
    },
    spacing={
        "xxs": 4,
        "xs": 8,
        "sm": 12,
        "md": 16,
        "lg": 24,
        "xl": 32,
    },
    font_size={
        "caption": 11,
        "body": 14,
        "body_strong": 15,
        "title": 18,
        "headline": 22,
        "display": 28,
        "log": 12,
    },
    fonts={
        "ui": ("Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI", "Arial"),
        "latin": ("Segoe UI", "Arial"),
        "mono": ("Consolas", "Cascadia Mono", "Courier New"),
    },
    shadow={
        "card": (0.08, 0.18, 0.32, 0.045),
        "button": (0.08, 0.18, 0.32, 0.035),
    },
)

DARK_THEME_PLACEHOLDER = None


def get_theme(mode: str = "light") -> Theme:
    """Return the requested theme; only light mode is implemented for now."""
    return LIGHT_THEME
