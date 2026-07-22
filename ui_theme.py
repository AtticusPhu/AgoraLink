#!/usr/bin/env python3
"""Semantic light and dark design tokens for AgoraLink."""

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


REQUIRED_SEMANTIC_TOKENS = frozenset(
    {
        "window_bg",
        "background",
        "surface",
        "surface_elevated",
        "surface_muted",
        "surface_selected",
        "input_bg",
        "input_bg_active",
        "menu_bg",
        "overlay",
        "text_primary",
        "text_secondary",
        "text_muted",
        "text_disabled",
        "border",
        "border_soft",
        "focus_ring",
        "accent",
        "accent_hover",
        "accent_pressed",
        "accent_soft",
        "on_accent",
        "success",
        "success_soft",
        "warning",
        "warning_soft",
        "danger",
        "danger_soft",
        "bubble_incoming",
        "bubble_outgoing",
        "card_file",
        "card_screen",
        "scrollbar",
        "selection",
        "shadow_card",
        "shadow_button",
    }
)

_RADIUS = {
    "small": 8,
    "medium": 12,
    "card": 16,
    "large": 20,
    "page": 24,
    "pill": 999,
}
_SPACING = {"xxs": 4, "xs": 8, "sm": 12, "md": 16, "lg": 24, "xl": 32}
_FONT_SIZE = {
    "caption": 11,
    "body": 14,
    "body_strong": 15,
    "title": 18,
    "headline": 22,
    "display": 28,
    "log": 12,
}
_FONTS = {
    "ui": ("Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI", "Arial"),
    "latin": ("Segoe UI", "Arial"),
    "mono": ("Consolas", "Cascadia Mono", "Courier New"),
}


def _with_compatibility_aliases(colors: Dict[str, Color]) -> Dict[str, Color]:
    result = dict(colors)
    result.update(
        {
            "surface_blue": result["card_screen"],
            "white": hex_to_rgba("#FFFFFF"),
            "transparent": (0.0, 0.0, 0.0, 0.0),
        }
    )
    return result


PRODUCT_LIGHT_THEME = Theme(
    name="AgoraLink Light - Graphite Blue",
    colors=_with_compatibility_aliases(
        {
            "window_bg": hex_to_rgba("#F5F7FA"),
            "background": hex_to_rgba("#F5F7FA"),
            "surface": hex_to_rgba("#FFFFFF"),
            "surface_elevated": hex_to_rgba("#FFFFFF"),
            "surface_muted": hex_to_rgba("#EEF2F6"),
            "surface_selected": hex_to_rgba("#E2ECF4"),
            "input_bg": hex_to_rgba("#EEF2F6"),
            "input_bg_active": hex_to_rgba("#FFFFFF"),
            "menu_bg": hex_to_rgba("#FFFFFF"),
            "overlay": hex_to_rgba("#20242A", 0.42),
            "text_primary": hex_to_rgba("#20242A"),
            "text_secondary": hex_to_rgba("#5F6B7A"),
            "text_muted": hex_to_rgba("#667482"),
            "text_disabled": hex_to_rgba("#9AA6B2"),
            "border": hex_to_rgba("#D8E0EA"),
            "border_soft": hex_to_rgba("#E4E9F0"),
            "focus_ring": hex_to_rgba("#3F7FA8"),
            "accent": hex_to_rgba("#3F7FA8"),
            "accent_hover": hex_to_rgba("#356C8F"),
            "accent_pressed": hex_to_rgba("#2F607F"),
            "accent_soft": hex_to_rgba("#D7E6F2"),
            "on_accent": hex_to_rgba("#FFFFFF"),
            "success": hex_to_rgba("#4C9362"),
            "success_soft": hex_to_rgba("#EDF6F0"),
            "warning": hex_to_rgba("#95681F"),
            "warning_soft": hex_to_rgba("#FFF4E2"),
            "danger": hex_to_rgba("#B64B4B"),
            "danger_soft": hex_to_rgba("#FAECEB"),
            "bubble_incoming": hex_to_rgba("#FFFFFF"),
            "bubble_outgoing": hex_to_rgba("#EAF1F7"),
            "card_file": hex_to_rgba("#F1F5F8"),
            "card_screen": hex_to_rgba("#EAF1F7"),
            "scrollbar": hex_to_rgba("#AAB7C4"),
            "selection": hex_to_rgba("#3F7FA8", 0.32),
            "shadow_card": hex_to_rgba("#142E52", 0.045),
            "shadow_button": hex_to_rgba("#142E52", 0.035),
        }
    ),
    radius=_RADIUS,
    spacing=_SPACING,
    font_size=_FONT_SIZE,
    fonts=_FONTS,
    shadow={"card": hex_to_rgba("#142E52", 0.045), "button": hex_to_rgba("#142E52", 0.035)},
)

PRODUCT_DARK_THEME = Theme(
    name="AgoraLink Dark - Graphite",
    colors=_with_compatibility_aliases(
        {
            "window_bg": hex_to_rgba("#111519"),
            "background": hex_to_rgba("#14181C"),
            "surface": hex_to_rgba("#191E23"),
            "surface_elevated": hex_to_rgba("#20262C"),
            "surface_muted": hex_to_rgba("#20262C"),
            "surface_selected": hex_to_rgba("#203A50"),
            "input_bg": hex_to_rgba("#20262C"),
            "input_bg_active": hex_to_rgba("#252D35"),
            "menu_bg": hex_to_rgba("#20262C"),
            "overlay": hex_to_rgba("#000000", 0.62),
            "text_primary": hex_to_rgba("#F0F3F6"),
            "text_secondary": hex_to_rgba("#C2CBD4"),
            "text_muted": hex_to_rgba("#A3AFBA"),
            "text_disabled": hex_to_rgba("#77838E"),
            "border": hex_to_rgba("#343D46"),
            "border_soft": hex_to_rgba("#2B333B"),
            "focus_ring": hex_to_rgba("#579BD7"),
            "accent": hex_to_rgba("#3A82C3"),
            "accent_hover": hex_to_rgba("#4892D2"),
            "accent_pressed": hex_to_rgba("#2F6FA8"),
            "accent_soft": hex_to_rgba("#203A50"),
            "on_accent": hex_to_rgba("#FFFFFF"),
            "success": hex_to_rgba("#5CB779"),
            "success_soft": hex_to_rgba("#1D3829"),
            "warning": hex_to_rgba("#D3A356"),
            "warning_soft": hex_to_rgba("#3A2D1D"),
            "danger": hex_to_rgba("#E66D6D"),
            "danger_soft": hex_to_rgba("#3A2326"),
            "bubble_incoming": hex_to_rgba("#20262C"),
            "bubble_outgoing": hex_to_rgba("#203A50"),
            "card_file": hex_to_rgba("#202A32"),
            "card_screen": hex_to_rgba("#1D2D3B"),
            "scrollbar": hex_to_rgba("#596571"),
            "selection": hex_to_rgba("#579BD7", 0.38),
            "shadow_card": hex_to_rgba("#000000", 0.18),
            "shadow_button": hex_to_rgba("#000000", 0.12),
        }
    ),
    radius=_RADIUS,
    spacing=_SPACING,
    font_size=_FONT_SIZE,
    fonts=_FONTS,
    shadow={"card": hex_to_rgba("#000000", 0.18), "button": hex_to_rgba("#000000", 0.12)},
)

# Compatibility aliases for preview code and older imports. Product modules use
# ThemeController and must not capture either alias at import time.
LIGHT_THEME = PRODUCT_LIGHT_THEME
SECONDARY_DARK_THEME = PRODUCT_DARK_THEME


def _preview_variant(name: str, values: Dict[str, str]) -> Theme:
    colors = dict(PRODUCT_LIGHT_THEME.colors)
    mapping = {
        "background": "background",
        "surface": "surface",
        "surface_muted": "surface_muted",
        "surface_blue": "card_screen",
        "border": "border",
        "border_soft": "border_soft",
        "accent": "accent",
        "accent_hover": "accent_hover",
        "accent_soft": "accent_soft",
        "text_primary": "text_primary",
        "text_secondary": "text_secondary",
        "text_muted": "text_muted",
    }
    for source, target in mapping.items():
        if source in values:
            colors[target] = hex_to_rgba(values[source])
    colors["window_bg"] = colors["background"]
    colors["surface_blue"] = colors["card_screen"]
    return Theme(
        name=name,
        colors=colors,
        radius=_RADIUS,
        spacing=_SPACING,
        font_size=_FONT_SIZE,
        fonts=_FONTS,
        shadow=PRODUCT_LIGHT_THEME.shadow,
    )


MIST_BLUE_THEME = _preview_variant(
    "Variant A - Mist Blue",
    {
        "background": "#F7FAFD",
        "surface": "#FFFFFF",
        "surface_muted": "#F1F6FB",
        "surface_blue": "#EEF5FB",
        "border": "#D9E4EE",
        "border_soft": "#E5ECF3",
        "accent": "#4F95CF",
        "accent_hover": "#437FAF",
        "accent_soft": "#DCEAF6",
        "text_primary": "#20242A",
        "text_secondary": "#667085",
        "text_muted": "#778595",
    },
)

PORCELAIN_GRAY_BLUE_THEME = _preview_variant(
    "Variant B - Porcelain Gray Blue",
    {
        "background": "#F7F8FA",
        "surface": "#FFFFFF",
        "surface_muted": "#F2F4F7",
        "surface_blue": "#EEF3F8",
        "border": "#DDE5EE",
        "border_soft": "#E8EDF3",
        "accent": "#4E8DBC",
        "accent_hover": "#41789F",
        "accent_soft": "#DDEAF5",
        "text_primary": "#1F2933",
        "text_secondary": "#667085",
        "text_muted": "#778595",
    },
)

GRAPHITE_BLUE_THEME = PRODUCT_LIGHT_THEME

PREVIEW_THEME_VARIANTS: Dict[str, Theme] = {
    "mist": MIST_BLUE_THEME,
    "porcelain": PORCELAIN_GRAY_BLUE_THEME,
    "graphite": GRAPHITE_BLUE_THEME,
}


def get_theme(mode: str = "light") -> Theme:
    key = str(mode or "light").strip().lower()
    if key == "dark":
        return PRODUCT_DARK_THEME
    if key in {"light", "current", "default"}:
        return PRODUCT_LIGHT_THEME
    return PREVIEW_THEME_VARIANTS.get(key, PRODUCT_LIGHT_THEME)
