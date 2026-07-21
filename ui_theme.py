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
    name="Variant C - Graphite Blue",
    colors={
        "background": hex_to_rgba("#F5F7FA"),
        "surface": hex_to_rgba("#FFFFFF"),
        "surface_muted": hex_to_rgba("#EEF2F6"),
        "surface_blue": hex_to_rgba("#EAF1F7"),
        "text_primary": hex_to_rgba("#20242A"),
        "text_secondary": hex_to_rgba("#5F6B7A"),
        "text_muted": hex_to_rgba("#93A0AD"),
        "border": hex_to_rgba("#D8E0EA"),
        "border_soft": hex_to_rgba("#E4E9F0"),
        "accent": hex_to_rgba("#3F7FA8"),
        "accent_hover": hex_to_rgba("#356C8F"),
        "accent_soft": hex_to_rgba("#D7E6F2"),
        "success": hex_to_rgba("#4C9362"),
        "success_soft": hex_to_rgba("#EDF6F0"),
        "warning": hex_to_rgba("#A9772B"),
        "warning_soft": hex_to_rgba("#FFF4E2"),
        "danger": hex_to_rgba("#B64B4B"),
        "danger_soft": hex_to_rgba("#FAECEB"),
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


def _variant(name: str, colors: Dict[str, str]) -> Theme:
    base = dict(LIGHT_THEME.colors)
    base.update(
        {
            "background": hex_to_rgba(colors["background"]),
            "surface": hex_to_rgba(colors["surface"]),
            "surface_muted": hex_to_rgba(colors["surface_muted"]),
            "surface_blue": hex_to_rgba(colors["surface_blue"]),
            "border": hex_to_rgba(colors["border"]),
            "border_soft": hex_to_rgba(colors["border_soft"]),
            "accent": hex_to_rgba(colors["accent"]),
            "accent_hover": hex_to_rgba(colors["accent_hover"]),
            "accent_soft": hex_to_rgba(colors["accent_soft"]),
            "text_primary": hex_to_rgba(colors["text_primary"]),
            "text_secondary": hex_to_rgba(colors["text_secondary"]),
            "text_muted": hex_to_rgba(colors["text_muted"]),
            # State colors are intentionally muted for the preview variants.
            "success": hex_to_rgba(colors.get("success", "#4FA66A")),
            "success_soft": hex_to_rgba(colors.get("success_soft", "#EDF7F0")),
            "warning": hex_to_rgba(colors.get("warning", "#B7791F")),
            "warning_soft": hex_to_rgba(colors.get("warning_soft", "#FFF4E0")),
            "danger": hex_to_rgba(colors.get("danger", "#C44B4B")),
            "danger_soft": hex_to_rgba(colors.get("danger_soft", "#FBEAEA")),
        }
    )
    return Theme(
        name=name,
        colors=base,
        radius=LIGHT_THEME.radius,
        spacing=LIGHT_THEME.spacing,
        font_size=LIGHT_THEME.font_size,
        fonts=LIGHT_THEME.fonts,
        shadow=LIGHT_THEME.shadow,
    )


MIST_BLUE_THEME = _variant(
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
        "text_muted": "#98A2B3",
        "success": "#4FA66A",
        "success_soft": "#EDF7F0",
        "warning": "#B7791F",
        "warning_soft": "#FFF5E5",
        "danger": "#C44B4B",
        "danger_soft": "#FBEAEA",
    },
)

PORCELAIN_GRAY_BLUE_THEME = _variant(
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
        "text_muted": "#98A2B3",
        "success": "#4D9F65",
        "success_soft": "#EEF7F1",
        "warning": "#AE7A28",
        "warning_soft": "#FFF4E4",
        "danger": "#BA4A4A",
        "danger_soft": "#FAECEB",
    },
)

GRAPHITE_BLUE_THEME = _variant(
    "Variant C - Graphite Blue",
    {
        "background": "#F5F7FA",
        "surface": "#FFFFFF",
        "surface_muted": "#EEF2F6",
        "surface_blue": "#EAF1F7",
        "border": "#D8E0EA",
        "border_soft": "#E4E9F0",
        "accent": "#3F7FA8",
        "accent_hover": "#356C8F",
        "accent_soft": "#D7E6F2",
        "text_primary": "#20242A",
        "text_secondary": "#5F6B7A",
        "text_muted": "#93A0AD",
        "success": "#4C9362",
        "success_soft": "#EDF6F0",
        "warning": "#A9772B",
        "warning_soft": "#FFF4E2",
        "danger": "#B64B4B",
        "danger_soft": "#FAECEB",
    },
)


# Secondary windows intentionally use a darker graphite surface.  Keeping these
# tokens separate lets the settings redesign match the selected Windows desktop
# direction without changing the established chat, transfer, or screen cards.
SECONDARY_DARK_THEME = Theme(
    name="Secondary Graphite",
    colors={
        "background": hex_to_rgba("#14181C"),
        "surface": hex_to_rgba("#191E23"),
        "surface_muted": hex_to_rgba("#20262C"),
        "surface_blue": hex_to_rgba("#1D2D3B"),
        "text_primary": hex_to_rgba("#F0F3F6"),
        "text_secondary": hex_to_rgba("#B5BEC8"),
        "text_muted": hex_to_rgba("#84909C"),
        "border": hex_to_rgba("#313943"),
        "border_soft": hex_to_rgba("#282F36"),
        "accent": hex_to_rgba("#2E78BE"),
        "accent_hover": hex_to_rgba("#3788D2"),
        "accent_soft": hex_to_rgba("#203A50"),
        "success": hex_to_rgba("#45A06A"),
        "success_soft": hex_to_rgba("#1D3829"),
        "warning": hex_to_rgba("#C18D3D"),
        "warning_soft": hex_to_rgba("#3A2D1D"),
        "danger": hex_to_rgba("#E15B5B"),
        "danger_soft": hex_to_rgba("#3A2326"),
        "white": hex_to_rgba("#FFFFFF"),
        "transparent": (0, 0, 0, 0),
    },
    radius={
        "small": 4,
        "medium": 6,
        "card": 8,
        "large": 10,
        "page": 10,
        "pill": 999,
    },
    spacing=LIGHT_THEME.spacing,
    font_size=LIGHT_THEME.font_size,
    fonts=LIGHT_THEME.fonts,
    shadow={
        "card": (0.0, 0.0, 0.0, 0.18),
        "button": (0.0, 0.0, 0.0, 0.12),
    },
)

THEME_VARIANTS: Dict[str, Theme] = {
    "current": LIGHT_THEME,
    "default": LIGHT_THEME,
    "light": LIGHT_THEME,
    "mist": MIST_BLUE_THEME,
    "porcelain": PORCELAIN_GRAY_BLUE_THEME,
    "graphite": GRAPHITE_BLUE_THEME,
    "secondary-dark": SECONDARY_DARK_THEME,
}


def get_theme(mode: str = "light") -> Theme:
    """Return a named preview theme; Graphite Blue is the default."""
    key = str(mode or "light").strip().lower()
    if key in THEME_VARIANTS:
        return THEME_VARIANTS[key]
    return LIGHT_THEME
