#!/usr/bin/env python3
"""Reusable Kivy widgets for the AgoraLink UI design preview."""

from __future__ import annotations

import time
from pathlib import Path
from typing import Callable, Dict, Iterable, List, Mapping, Optional

from kivy.animation import Animation
from kivy.clock import Clock
from kivy.core.text import LabelBase
from kivy.core.window import Window
from kivy.graphics import Color, Line, RoundedRectangle
from kivy.metrics import dp, sp
from kivy.properties import BooleanProperty, ListProperty, NumericProperty, StringProperty
from kivy.uix.behaviors import ButtonBehavior
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.label import Label
from kivy.uix.widget import Widget

from ui_theme import LIGHT_THEME, Color as ThemeColor, Theme

THEME = LIGHT_THEME
UI_ANIMATION_SECONDS = 0.14
PROGRESS_UI_INTERVAL_SECONDS = 0.15
TERMINAL_STATUSES = {"success", "failed", "danger", "rejected", "stopped", "complete", "completed"}


def _register_font_alias(alias: str, candidates: Iterable[str]) -> str:
    windows_fonts = Path("C:/Windows/Fonts")
    filenames = {
        "Microsoft YaHei UI": ("msyh.ttc", "msyhbd.ttc"),
        "Microsoft YaHei": ("msyh.ttc", "msyhbd.ttc"),
        "Segoe UI": ("segoeui.ttf", "segoeuib.ttf"),
        "Consolas": ("consola.ttf", "consolab.ttf"),
        "Cascadia Mono": ("CascadiaMono.ttf",),
        "Arial": ("arial.ttf", "arialbd.ttf"),
    }
    for family in candidates:
        for filename in filenames.get(str(family), (str(family),)):
            path = windows_fonts / filename
            if path.exists():
                try:
                    LabelBase.register(name=alias, fn_regular=str(path))
                    return alias
                except Exception:
                    pass
    return "Roboto"


UI_FONT = _register_font_alias("AgoraLinkPreviewUI", THEME.fonts["ui"])
MONO_FONT = _register_font_alias("AgoraLinkPreviewMono", THEME.fonts["mono"])


def set_theme(theme: Theme) -> None:
    """Set the active theme for newly created preview widgets."""
    global THEME
    if isinstance(theme, Theme):
        THEME = theme


def color(name: str, alpha: Optional[float] = None, theme: Optional[Theme] = None) -> List[float]:
    active = theme or THEME
    rgba = list(active.colors.get(name, active.colors["text_primary"]))
    if alpha is not None:
        rgba[3] = float(alpha)
    return rgba


def _mix(a: List[float], b: List[float], amount: float) -> List[float]:
    t = max(0.0, min(1.0, float(amount)))
    return [a[i] + (b[i] - a[i]) * t for i in range(4)]


def _animate_list_property(widget, name: str, target: Iterable[float], *, duration: float = UI_ANIMATION_SECONDS, animated: bool = True) -> None:
    values = list(target)
    try:
        current = list(getattr(widget, name))
        if len(current) == len(values) and all(abs(float(current[i]) - float(values[i])) < 0.001 for i in range(len(values))):
            return
    except Exception:
        pass
    if not animated:
        setattr(widget, name, values)
        return
    Animation.cancel_all(widget, name)
    Animation(**{name: values}, d=duration, t="out_quad").start(widget)


def _status_border_color(status: str) -> List[float]:
    text = str(status or "").strip().lower()
    if any(token in text for token in ("failed", "fail", "error", "reject", "rejected", "danger", "失败", "拒绝")):
        return color("danger_soft")
    if any(token in text for token in ("success", "complete", "completed", "done", "saved", "accepted", "已完成", "成功")):
        return color("success_soft")
    if any(token in text for token in ("wait", "pending", "starting", "打包", "等待")):
        return color("warning_soft")
    if any(token in text for token in ("active", "sending", "receiving", "progress", "watching", "投屏", "传输")):
        return color("accent_soft")
    return color("border")


def _bind_label_width(label: Label, vertical: bool = False) -> Label:
    def _sync(inst, _value=None):
        inst.text_size = (max(1, inst.width), None if not vertical else max(1, inst.height))

    label.bind(size=_sync)
    _sync(label)
    return label


def _bind_single_line_label(label: Label, padding: float = 0) -> Label:
    label.shorten = True
    if not str(getattr(label, "shorten_from", "") or ""):
        label.shorten_from = "right"

    def _sync(inst, _value=None):
        inst.text_size = (max(1, inst.width - padding), None)

    label.bind(size=_sync)
    _sync(label)
    return label


def _normalize_actions(actions: Optional[Iterable[object]]) -> List[Dict[str, object]]:
    result: List[Dict[str, object]] = []
    for item in actions or []:
        if not isinstance(item, Mapping):
            continue
        label = str(item.get("label") or "").strip()
        action = str(item.get("action") or "").strip()
        if not label:
            continue
        result.append(
            {
                "label": label,
                "action": action,
                "style": str(item.get("style") or "secondary").strip().lower() or "secondary",
            }
        )
    return result


def _action_button_width(label: str) -> float:
    try:
        return dp(min(118, max(72, 28 + len(str(label or "")) * 8)))
    except Exception:
        return dp(86)


def _button_style_kwargs(style: str) -> Dict[str, object]:
    style_name = str(style or "secondary").strip().lower()
    if style_name in ("primary", "accent", "success"):
        return {
            "bg_normal": color("accent"),
            "bg_hover": color("accent_hover"),
            "bg_down": color("accent_hover"),
            "text_normal": color("white"),
            "text_down": color("white"),
            "border_color": color("accent"),
        }
    if style_name in ("danger", "destructive", "reject"):
        return {
            "bg_normal": color("surface_muted"),
            "bg_hover": color("danger_soft"),
            "bg_down": color("danger_soft"),
            "text_normal": color("danger"),
            "text_down": color("danger"),
            "border_color": color("border"),
        }
    return {
        "bg_normal": color("surface_muted"),
        "bg_hover": color("accent_soft"),
        "bg_down": color("accent_soft"),
        "text_normal": color("text_primary"),
        "text_down": color("text_primary"),
        "border_color": color("border"),
    }


class _CardActionsMixin:
    def _init_action_support(self, actions: Optional[Iterable[object]], on_action: Optional[Callable[[str], None]]) -> None:
        self._actions: List[Dict[str, object]] = []
        self._on_action = on_action
        self.action_row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(34))
        self.set_actions(actions, on_action=on_action)

    def set_actions(self, actions: Optional[Iterable[object]], on_action: Optional[Callable[[str], None]] = None) -> None:
        if on_action is not None:
            self._on_action = on_action
        self._actions = _normalize_actions(actions)
        self._sync_actions()

    def _sync_actions(self) -> None:
        self.action_row.clear_widgets()
        if not self._actions:
            if getattr(self.action_row, "parent", None) is self:
                self.remove_widget(self.action_row)
            self.height = self.minimum_height
            return
        for action in self._actions[:3]:
            label = str(action.get("label") or "")
            action_id = str(action.get("action") or "")
            button = PillButton(
                text=label,
                size_hint_x=None,
                width=_action_button_width(label),
                disabled=not bool(action_id),
                **_button_style_kwargs(str(action.get("style") or "secondary")),
            )
            if action_id:
                button.bind(on_release=lambda _btn, aid=action_id: self._dispatch_action(aid))
            self.action_row.add_widget(button)
        self.action_row.add_widget(Widget(size_hint_x=1))
        if getattr(self.action_row, "parent", None) is not self:
            self.add_widget(self.action_row)
        self.height = self.minimum_height

    def _dispatch_action(self, action_id: str) -> None:
        callback = getattr(self, "_on_action", None)
        if callable(callback):
            callback(str(action_id or ""))


class _RoundedCanvasMixin:
    bg_color = ListProperty(color("surface"))
    border_color = ListProperty(color("border"))
    border_width = NumericProperty(1)
    radius = NumericProperty(THEME.radius["card"])

    def _init_rounded_canvas(self) -> None:
        with self.canvas.before:
            self._shadow_color = Color(0, 0, 0, 0)
            self._shadow_rect = RoundedRectangle(pos=self.pos, size=self.size, radius=[self.radius])
            self._bg_color_instr = Color(*self.bg_color)
            self._bg_rect = RoundedRectangle(pos=self.pos, size=self.size, radius=[self.radius])
            self._border_color_instr = Color(*self.border_color)
            self._border_line = Line(rounded_rectangle=(self.x, self.y, self.width, self.height, self.radius), width=self.border_width)
        self.bind(
            pos=self._update_rounded_canvas,
            size=self._update_rounded_canvas,
            bg_color=self._update_rounded_canvas,
            border_color=self._update_rounded_canvas,
            border_width=self._update_rounded_canvas,
            radius=self._update_rounded_canvas,
        )
        self._update_rounded_canvas()

    def _update_rounded_canvas(self, *_args) -> None:
        try:
            r = float(self.radius)
            self._shadow_color.rgba = THEME.shadow["card"]
            self._shadow_rect.pos = (self.x, self.y - dp(1))
            self._shadow_rect.size = self.size
            self._shadow_rect.radius = [r]
            self._bg_color_instr.rgba = self.bg_color
            self._bg_rect.pos = self.pos
            self._bg_rect.size = self.size
            self._bg_rect.radius = [r]
            self._border_color_instr.rgba = self.border_color
            self._border_line.rounded_rectangle = (self.x, self.y, self.width, self.height, r)
            self._border_line.width = float(self.border_width)
        except Exception:
            pass


class RoundedCard(_RoundedCanvasMixin, BoxLayout):
    """A low-noise surface with rounded corners and a subtle border."""

    def __init__(self, **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("spacing", dp(THEME.spacing["sm"]))
        kwargs.setdefault("padding", [dp(THEME.spacing["md"])] * 4)
        kwargs.setdefault("bg_color", color("surface"))
        kwargs.setdefault("border_color", color("border"))
        super().__init__(**kwargs)
        self._init_rounded_canvas()


class RoundedButton(ButtonBehavior, Label):
    """Rounded text button with subtle hover and press feedback."""

    variant = StringProperty("")
    compact = BooleanProperty(False)
    bg_normal = ListProperty(color("surface_muted"))
    bg_hover = ListProperty(color("accent_soft"))
    bg_down = ListProperty(color("accent"))
    text_normal = ListProperty(color("text_primary"))
    text_down = ListProperty(color("white"))
    border_color = ListProperty(color("border"))
    radius = NumericProperty(THEME.radius["medium"])
    fill_color = ListProperty(color("surface_muted"))
    hovered = BooleanProperty(False)

    def __init__(self, **kwargs):
        is_compact = bool(kwargs.get("compact", False))
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(THEME.font_size["caption"] if is_compact else THEME.font_size["body"]))
        kwargs.setdefault("halign", "center")
        kwargs.setdefault("valign", "middle")
        kwargs.setdefault("shorten", True)
        kwargs.setdefault("shorten_from", "right")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(32 if is_compact else 38))
        super().__init__(**kwargs)
        self._disable_default_button_background()
        self._apply_variant()
        self.color = self.text_normal
        self.fill_color = list(self.bg_normal)
        self.bind(size=lambda inst, _value: setattr(inst, "text_size", (max(1, inst.width - dp(16)), max(1, inst.height))))
        with self.canvas.before:
            self._btn_fill = Color(*self.fill_color)
            self._btn_rect = RoundedRectangle(pos=self.pos, size=self.size, radius=[self._effective_radius()])
            self._btn_border_color = Color(*self.border_color)
            self._btn_border = Line(rounded_rectangle=(self.x, self.y, self.width, self.height, self._effective_radius()), width=1)
        self.bind(
            pos=self._update_button_canvas,
            size=self._update_button_canvas,
            radius=self._update_button_canvas,
            fill_color=self._update_button_canvas,
            border_color=self._update_button_canvas,
            variant=lambda *_: self._apply_variant(),
            disabled=lambda *_: self._refresh_button_state(animated=False),
            state=lambda *_: self._refresh_button_state(animated=True),
            hovered=lambda *_: self._refresh_button_state(animated=True),
        )
        Window.bind(mouse_pos=self._on_mouse_pos)
        self.bind(parent=self._on_parent)
        self._update_button_canvas()

    def _disable_default_button_background(self) -> None:
        for name, value in (
            ("background_normal", ""),
            ("background_down", ""),
            ("background_disabled_normal", ""),
            ("background_color", (0, 0, 0, 0)),
        ):
            try:
                setattr(self, name, value)
            except Exception:
                pass

    def _effective_radius(self) -> float:
        try:
            max_radius = max(0.0, min(float(self.width or 0), float(self.height or 0)) / 2.0)
            return max(0.0, min(float(self.radius), max_radius))
        except Exception:
            return float(THEME.radius["medium"])

    def _on_mouse_pos(self, _window, pos) -> None:
        if not self.get_root_window():
            return
        self.hovered = self.collide_point(*pos)

    def _on_parent(self, _instance, parent) -> None:
        if parent is not None:
            return
        try:
            Window.unbind(mouse_pos=self._on_mouse_pos)
        except Exception:
            pass

    def _apply_variant(self, *_args) -> None:
        variant = str(self.variant or "").strip().lower()
        if variant in ("primary", "active", "success", "accent"):
            self.bg_normal = color("accent")
            self.bg_hover = color("accent_hover")
            self.bg_down = color("accent_hover")
            self.text_normal = color("white")
            self.text_down = color("white")
            self.border_color = color("accent")
        elif variant in ("danger", "destructive", "reject"):
            self.bg_normal = color("danger_soft")
            self.bg_hover = color("danger_soft")
            self.bg_down = color("danger_soft")
            self.text_normal = color("danger")
            self.text_down = color("danger")
            self.border_color = color("border")
        elif variant == "ghost":
            self.bg_normal = color("transparent")
            self.bg_hover = color("surface_muted")
            self.bg_down = color("accent_soft")
            self.text_normal = color("text_secondary")
            self.text_down = color("text_primary")
            self.border_color = color("transparent")
        elif variant in ("", "secondary"):
            self.bg_normal = color("surface_muted")
            self.bg_hover = color("accent_soft")
            self.bg_down = color("accent_soft")
            self.text_normal = color("text_primary")
            self.text_down = color("text_primary")
            self.border_color = color("border")
        self._refresh_button_state(animated=False)

    def _refresh_button_state(self, animated: bool = False) -> None:
        if self.disabled:
            target = _mix(list(self.bg_normal), color("surface"), 0.50)
            self.color = color("text_muted")
        else:
            target = list(self.bg_down if self.state == "down" else (self.bg_hover if self.hovered else self.bg_normal))
            self.color = self.text_down if self.state == "down" else self.text_normal
        if animated:
            _animate_list_property(self, "fill_color", target)
        else:
            self.fill_color = target

    def _update_button_canvas(self, *_args) -> None:
        try:
            r = self._effective_radius()
            self._btn_fill.rgba = self.fill_color
            self._btn_rect.pos = self.pos
            self._btn_rect.size = self.size
            self._btn_rect.radius = [r]
            self._btn_border_color.rgba = self.border_color
            self._btn_border.rounded_rectangle = (self.x, self.y, self.width, self.height, r)
        except Exception:
            pass


class PillButton(RoundedButton):
    def __init__(self, **kwargs):
        kwargs.setdefault("radius", THEME.radius["pill"])
        kwargs.setdefault("height", dp(34))
        kwargs.setdefault("padding", [dp(16), 0])
        super().__init__(**kwargs)


class StatusBadge(Label):
    status = StringProperty("neutral")
    fill_color = ListProperty(color("surface_muted"))
    border_color = ListProperty(color("border"))
    radius = NumericProperty(THEME.radius["small"])
    min_width = NumericProperty(dp(52))
    max_width = NumericProperty(dp(118))

    def __init__(self, **kwargs):
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(THEME.font_size["caption"]))
        kwargs.setdefault("bold", True)
        kwargs.setdefault("halign", "center")
        kwargs.setdefault("valign", "middle")
        kwargs.setdefault("shorten", True)
        kwargs.setdefault("shorten_from", "right")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(24))
        kwargs.setdefault("size_hint_x", None)
        kwargs.setdefault("width", dp(76))
        super().__init__(**kwargs)
        self._status_initialized = False
        with self.canvas.before:
            self._badge_fill = Color(*self.fill_color)
            self._badge_rect = RoundedRectangle(pos=self.pos, size=self.size, radius=[self.radius])
            self._badge_border_color = Color(*self.border_color)
            self._badge_border = Line(rounded_rectangle=(self.x, self.y, self.width, self.height, self.radius), width=1)
        self.bind(pos=self._update_badge, size=self._update_badge, fill_color=self._update_badge, border_color=self._update_badge, status=self._apply_status)
        self.bind(size=lambda inst, _value: setattr(inst, "text_size", (max(1, inst.width - dp(14)), None)))
        self.bind(texture_size=lambda *_: self._fit_width(), text=lambda *_: self._fit_width(), max_width=lambda *_: self._fit_width())
        self._apply_status()
        self._fit_width()

    def _apply_status(self, *_args) -> None:
        mapping = {
            "success": ("success_soft", "success"),
            "warning": ("warning_soft", "warning"),
            "danger": ("danger_soft", "danger"),
            "accent": ("accent_soft", "accent"),
            "waiting": ("warning_soft", "warning"),
            "failed": ("danger_soft", "danger"),
            "neutral": ("surface_muted", "text_secondary"),
        }
        fill_name, text_name = mapping.get(str(self.status or "neutral"), mapping["neutral"])
        animated = bool(getattr(self, "_status_initialized", False))
        _animate_list_property(self, "fill_color", color(fill_name), animated=animated)
        self.color = color(text_name)
        self._status_initialized = True

    def _update_badge(self, *_args) -> None:
        try:
            r = float(self.radius)
            self._badge_fill.rgba = self.fill_color
            self._badge_rect.pos = self.pos
            self._badge_rect.size = self.size
            self._badge_rect.radius = [r]
            self._badge_border_color.rgba = self.border_color
            self._badge_border.rounded_rectangle = (self.x, self.y, self.width, self.height, r)
        except Exception:
            pass

    def _fit_width(self) -> None:
        try:
            natural = float((self.texture_size or [0, 0])[0]) + dp(22)
            self.width = min(float(self.max_width), max(float(self.min_width), natural))
        except Exception:
            pass


class ConversationItem(ButtonBehavior, RoundedCard):
    title = StringProperty("")
    preview = StringProperty("")
    meta_text = StringProperty("")
    status_text = StringProperty("")
    badge_text = StringProperty("")
    active = BooleanProperty(False)
    hovered = BooleanProperty(False)

    def __init__(self, **kwargs):
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(60))
        kwargs.setdefault("padding", [dp(12), dp(8), dp(12), dp(8)])
        kwargs.setdefault("spacing", dp(4))
        kwargs.setdefault("radius", THEME.radius["medium"])
        kwargs.setdefault("bg_color", color("surface"))
        kwargs.setdefault("border_color", color("border_soft"))
        super().__init__(**kwargs)
        self._disable_default_button_background()

        top = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(24))
        self.title_label = Label(
            font_name=UI_FONT,
            font_size=sp(THEME.font_size["body_strong"]),
            bold=True,
            color=color("text_primary"),
            halign="left",
            valign="middle",
            shorten=True,
            shorten_from="right",
            size_hint_x=1,
        )
        self.meta_label = Label(
            font_name=UI_FONT,
            font_size=sp(THEME.font_size["caption"]),
            color=color("text_muted"),
            halign="right",
            valign="middle",
            shorten=True,
            shorten_from="right",
            size_hint_x=None,
            width=dp(48),
        )
        _bind_single_line_label(self.title_label)
        _bind_single_line_label(self.meta_label)
        top.add_widget(self.title_label)
        top.add_widget(self.meta_label)

        bottom = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(20))
        self.preview_label = Label(
            font_name=UI_FONT,
            font_size=sp(THEME.font_size["caption"]),
            color=color("text_secondary"),
            halign="left",
            valign="middle",
            shorten=True,
            shorten_from="right",
            size_hint_x=1,
        )
        self.status_label = Label(
            font_name=UI_FONT,
            font_size=sp(THEME.font_size["caption"]),
            color=color("text_muted"),
            halign="right",
            valign="middle",
            shorten=True,
            shorten_from="right",
            size_hint_x=None,
            width=dp(48),
        )
        self.badge = StatusBadge(text="", status="accent", min_width=0, max_width=0, width=0)
        _bind_single_line_label(self.preview_label)
        _bind_single_line_label(self.status_label)
        bottom.add_widget(self.preview_label)
        bottom.add_widget(self.status_label)
        bottom.add_widget(self.badge)

        self.add_widget(top)
        self.add_widget(bottom)
        self.bind(
            title=self._sync_text,
            preview=self._sync_text,
            meta_text=self._sync_text,
            status_text=self._sync_text,
            badge_text=self._sync_text,
            active=self._sync_style,
            hovered=self._sync_style,
            state=lambda *_: self._sync_style(),
        )
        self.bind(size=lambda *_: self._sync_text_size())
        Window.bind(mouse_pos=self._on_mouse_pos)
        self.bind(parent=self._on_parent)
        self._sync_text()
        self._sync_style()

    def _disable_default_button_background(self) -> None:
        for name, value in (
            ("background_normal", ""),
            ("background_down", ""),
            ("background_color", (0, 0, 0, 0)),
        ):
            try:
                setattr(self, name, value)
            except Exception:
                pass

    def _on_mouse_pos(self, _window, pos) -> None:
        if not self.get_root_window():
            return
        self.hovered = self.collide_point(*pos)

    def _on_parent(self, _instance, parent) -> None:
        if parent is not None:
            return
        try:
            Window.unbind(mouse_pos=self._on_mouse_pos)
        except Exception:
            pass

    def _sync_text(self, *_args) -> None:
        self.title_label.text = str(self.title or "")
        self.preview_label.text = str(self.preview or "")
        meta = str(self.meta_text or "").strip()
        status = str(self.status_text or "").strip()
        badge = str(self.badge_text or "").strip()
        self.meta_label.text = meta
        self.meta_label.opacity = 1.0 if meta else 0.0
        self.meta_label.width = dp(48) if meta else 0
        self.status_label.text = status
        self.status_label.opacity = 1.0 if status else 0.0
        self.status_label.width = dp(48) if status else 0
        if badge:
            self.badge.text = badge
            self.badge.min_width = dp(34)
            self.badge.max_width = dp(62)
            self.badge.opacity = 1.0
            self.badge._fit_width()
        else:
            self.badge.text = ""
            self.badge.min_width = 0
            self.badge.max_width = 0
            self.badge.width = 0
            self.badge.opacity = 0.0
        self._sync_text_size()

    def _sync_text_size(self) -> None:
        try:
            self.title_label.text_size = (max(1, self.title_label.width), None)
            self.preview_label.text_size = (max(1, self.preview_label.width), None)
            self.meta_label.text_size = (max(1, self.meta_label.width), None)
            self.status_label.text_size = (max(1, self.status_label.width), None)
        except Exception:
            pass

    def _sync_style(self, *_args) -> None:
        try:
            if self.state == "down":
                self.bg_color = color("accent_soft")
                self.border_color = color("accent")
            elif self.active:
                self.bg_color = color("surface_blue")
                self.border_color = color("accent_soft")
            elif self.hovered:
                self.bg_color = color("surface_muted")
                self.border_color = color("border")
            else:
                self.bg_color = color("surface")
                self.border_color = color("border_soft")
        except Exception:
            pass


class MessageBubble(RoundedCard):
    direction = StringProperty("incoming")
    sender = StringProperty("")
    message = StringProperty("")

    def __init__(self, **kwargs):
        kwargs.setdefault("padding", [dp(14), dp(10), dp(14), dp(10)])
        kwargs.setdefault("spacing", dp(4))
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("size_hint_x", 0.78)
        kwargs.setdefault("radius", THEME.radius["card"])
        super().__init__(**kwargs)
        self.sender_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["caption"]), color=color("text_secondary"), halign="left", valign="middle", size_hint_y=None, height=0, shorten=True)
        self.message_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["body"]), color=color("text_primary"), halign="left", valign="top", shorten=False, size_hint_y=None)
        _bind_single_line_label(self.sender_label)
        self.message_label.bind(width=lambda inst, _value: setattr(inst, "text_size", (max(1, inst.width), None)))
        self.message_label.bind(texture_size=lambda inst, value: setattr(inst, "height", max(dp(24), float(value[1]) + dp(2))))
        self.add_widget(self.sender_label)
        self.add_widget(self.message_label)
        self.bind(minimum_height=self.setter("height"))
        self.bind(width=lambda *_: self._sync_text_size())
        self.height = self.minimum_height
        self.bind(sender=self._sync_text, message=self._sync_text, direction=self._sync_style)
        self._sync_text()
        self._sync_style()

    def _sync_text(self, *_args) -> None:
        sender = str(self.sender or "").strip()
        self.sender_label.text = sender
        self.sender_label.height = dp(18) if sender else 0
        self.sender_label.opacity = 1.0 if sender else 0.0
        self.message_label.text = str(self.message or "")
        self._sync_text_size()

    def _sync_style(self, *_args) -> None:
        if str(self.direction or "") == "outgoing":
            self.bg_color = color("surface_blue")
            self.border_color = color("border")
        else:
            self.bg_color = color("surface")
            self.border_color = color("border")

    def _sync_text_size(self) -> None:
        try:
            content_width = max(1, self.width - dp(28))
            self.sender_label.text_size = (content_width, None)
            self.message_label.text_size = (content_width, None)
            self.message_label.texture_update()
            self.message_label.height = max(dp(24), float(self.message_label.texture_size[1]) + dp(2))
            self.height = self.minimum_height
        except Exception:
            pass


class RoundedProgressBar(Widget):
    value = NumericProperty(0)
    display_value = NumericProperty(0)
    max = NumericProperty(100)
    track_color = ListProperty(color("surface_muted"))
    fill_color = ListProperty(color("accent"))
    radius = NumericProperty(THEME.radius["small"])

    def __init__(self, **kwargs):
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(8))
        kwargs.setdefault("track_color", color("surface_muted"))
        kwargs.setdefault("fill_color", color("accent"))
        super().__init__(**kwargs)
        self._pending_display_value: Optional[float] = None
        self._progress_clock = None
        self._last_progress_apply_ts = 0.0
        self.display_value = float(self.value or 0)
        with self.canvas:
            self._track_color = Color(*self.track_color)
            self._track = RoundedRectangle(pos=self.pos, size=self.size, radius=[self.radius])
            self._fill_color = Color(*self.fill_color)
            self._fill = RoundedRectangle(pos=self.pos, size=(0, self.height), radius=[self.radius])
        self.bind(pos=self._update_bar, size=self._update_bar, display_value=self._update_bar, max=self._update_bar, track_color=self._update_bar, fill_color=self._update_bar)
        self.bind(value=self._schedule_display_value)
        self._update_bar()

    def set_value(self, value: float, *, immediate: bool = False) -> None:
        if immediate:
            self.value = float(value or 0)
            self._cancel_progress_clock()
            self._apply_display_value(float(value or 0), immediate=True)
            return
        self.value = float(value or 0)

    def _cancel_progress_clock(self) -> None:
        event = getattr(self, "_progress_clock", None)
        if event is not None:
            try:
                event.cancel()
            except Exception:
                pass
        self._progress_clock = None
        self._pending_display_value = None

    def _schedule_display_value(self, *_args) -> None:
        target = max(0.0, min(float(self.max or 100), float(self.value or 0)))
        if target <= 0.0 or target >= float(self.max or 100):
            self._cancel_progress_clock()
            self._apply_display_value(target, immediate=True)
            return
        self._pending_display_value = target
        if self._progress_clock is not None:
            return
        now = time.time()
        delay = max(0.0, PROGRESS_UI_INTERVAL_SECONDS - (now - float(self._last_progress_apply_ts or 0.0)))
        self._progress_clock = Clock.schedule_once(self._flush_display_value, delay)

    def _flush_display_value(self, _dt) -> None:
        self._progress_clock = None
        target = self._pending_display_value
        self._pending_display_value = None
        if target is None:
            return
        self._apply_display_value(float(target), immediate=False)

    def _apply_display_value(self, value: float, *, immediate: bool = False) -> None:
        try:
            target = max(0.0, min(float(self.max or 100), float(value or 0)))
            self._last_progress_apply_ts = time.time()
            Animation.cancel_all(self, "display_value")
            if immediate or target < float(self.display_value or 0):
                self.display_value = target
            else:
                Animation(display_value=target, d=UI_ANIMATION_SECONDS, t="out_quad").start(self)
        except Exception:
            self.display_value = float(value or 0)

    def _update_bar(self, *_args) -> None:
        try:
            ratio = max(0.0, min(1.0, float(self.display_value) / float(self.max or 100)))
            r = float(self.radius)
            self._track_color.rgba = self.track_color
            self._track.pos = self.pos
            self._track.size = self.size
            self._track.radius = [r]
            self._fill_color.rgba = self.fill_color
            self._fill.pos = self.pos
            self._fill.size = (max(dp(2), self.width * ratio) if ratio > 0 else 0, self.height)
            self._fill.radius = [r]
        except Exception:
            pass


class FileTransferCard(_CardActionsMixin, RoundedCard):
    title = StringProperty("File transfer")
    filename = StringProperty("document.pdf")
    detail = StringProperty("")
    status_text = StringProperty("Waiting")
    status = StringProperty("waiting")
    progress = NumericProperty(0)

    def __init__(self, **kwargs):
        actions = kwargs.pop("actions", None)
        on_action = kwargs.pop("on_action", None)
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("padding", [dp(14), dp(12), dp(14), dp(12)])
        kwargs.setdefault("spacing", dp(8))
        kwargs.setdefault("bg_color", color("surface_blue"))
        kwargs.setdefault("border_color", color("border"))
        super().__init__(**kwargs)
        header = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(26))
        self.title_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["body_strong"]), bold=True, color=color("text_primary"), halign="left", valign="middle", shorten=True, size_hint_x=1)
        self.badge = StatusBadge(text=self.status_text, status=self.status)
        _bind_single_line_label(self.title_label)
        header.add_widget(self.title_label)
        header.add_widget(self.badge)
        self.file_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["body"]), color=color("text_primary"), halign="left", valign="middle", shorten=True, shorten_from="center", size_hint_x=1, size_hint_y=None, height=dp(24))
        self.detail_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["caption"]), color=color("text_secondary"), halign="left", valign="middle", shorten=True, shorten_from="right", size_hint_x=1, size_hint_y=None, height=dp(22))
        _bind_single_line_label(self.file_label)
        _bind_single_line_label(self.detail_label)
        self.progress_bar = RoundedProgressBar(value=self.progress, size_hint_x=1)
        self.add_widget(header)
        self.add_widget(self.file_label)
        self.add_widget(self.detail_label)
        self.add_widget(self.progress_bar)
        self.bind(minimum_height=self.setter("height"))
        self.height = self.minimum_height
        self._init_action_support(actions, on_action)
        self.bind(title=self._sync, filename=self._sync, detail=self._sync, status_text=self._sync, status=self._sync, progress=self._sync)
        self._sync()

    def _sync(self, *_args) -> None:
        self.title_label.text = str(self.title or "")
        self.file_label.text = str(self.filename or "")
        self.detail_label.text = str(self.detail or "")
        self.badge.text = str(self.status_text or "")
        self.badge.status = str(self.status or "neutral")
        status_text = f"{self.status} {self.status_text}".lower()
        terminal = bool(str(self.status or "").lower() in TERMINAL_STATUSES or any(token in status_text for token in ("complete", "completed", "failed", "rejected", "saved", "已完成", "失败", "拒绝")))
        self.progress_bar.set_value(float(self.progress or 0), immediate=terminal)
        _animate_list_property(self, "border_color", _status_border_color(status_text), animated=True)


class ScreenShareCard(_CardActionsMixin, RoundedCard):
    title = StringProperty("Screen share")
    peer = StringProperty("Remote")
    detail = StringProperty("")
    status_text = StringProperty("Waiting")
    status = StringProperty("accent")

    def __init__(self, **kwargs):
        actions = kwargs.pop("actions", None)
        on_action = kwargs.pop("on_action", None)
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("padding", [dp(14), dp(12), dp(14), dp(12)])
        kwargs.setdefault("spacing", dp(8))
        kwargs.setdefault("bg_color", color("surface_blue"))
        kwargs.setdefault("border_color", color("border"))
        super().__init__(**kwargs)
        header = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(28))
        self.title_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["body_strong"]), bold=True, color=color("text_primary"), halign="left", valign="middle", shorten=True, size_hint_x=1)
        self.badge = StatusBadge(text=self.status_text, status=self.status)
        _bind_single_line_label(self.title_label)
        header.add_widget(self.title_label)
        header.add_widget(self.badge)
        self.peer_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["body"]), color=color("text_primary"), halign="left", valign="middle", shorten=True, shorten_from="right", size_hint_x=1, size_hint_y=None, height=dp(24))
        self.detail_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["caption"]), color=color("text_secondary"), halign="left", valign="middle", shorten=True, shorten_from="right", size_hint_x=1, size_hint_y=None, height=dp(22))
        _bind_single_line_label(self.peer_label)
        _bind_single_line_label(self.detail_label)
        self.add_widget(header)
        self.add_widget(self.peer_label)
        self.add_widget(self.detail_label)
        self.bind(minimum_height=self.setter("height"))
        self.height = self.minimum_height
        self._init_action_support(actions, on_action)
        self.bind(title=self._sync, peer=self._sync, detail=self._sync, status_text=self._sync, status=self._sync)
        self._sync()

    def _sync(self, *_args) -> None:
        self.title_label.text = str(self.title or "")
        self.peer_label.text = str(self.peer or "")
        self.detail_label.text = str(self.detail or "")
        self.badge.text = str(self.status_text or "")
        self.badge.status = str(self.status or "neutral")
        status_text = f"{self.status} {self.status_text}".lower()
        _animate_list_property(self, "border_color", _status_border_color(status_text), animated=True)


class EmptyState(RoundedCard):
    def __init__(self, title: str = "Nothing here", subtitle: str = "", **kwargs):
        kwargs.setdefault("padding", [dp(24)] * 4)
        kwargs.setdefault("spacing", dp(8))
        kwargs.setdefault("size_hint_y", None)
        super().__init__(**kwargs)
        title_label = Label(text=title, font_name=UI_FONT, font_size=sp(THEME.font_size["title"]), bold=True, color=color("text_primary"), halign="center", valign="middle", shorten=True, size_hint_y=None, height=dp(32))
        sub = Label(text=subtitle, font_name=UI_FONT, font_size=sp(THEME.font_size["body"]), color=color("text_secondary"), halign="center", valign="middle", shorten=True, size_hint_y=None, height=dp(24))
        _bind_single_line_label(title_label)
        _bind_single_line_label(sub)
        self.add_widget(title_label)
        self.add_widget(sub)
        self.bind(minimum_height=self.setter("height"))
        self.height = self.minimum_height


class SectionHeader(BoxLayout):
    def __init__(self, title: str, subtitle: str = "", **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("spacing", dp(2))
        kwargs.setdefault("size_hint_y", None)
        super().__init__(**kwargs)
        title_label = Label(text=title, font_name=UI_FONT, font_size=sp(THEME.font_size["title"]), bold=True, color=color("text_primary"), halign="left", valign="middle", shorten=True, size_hint_x=1, size_hint_y=None, height=dp(26))
        _bind_single_line_label(title_label)
        self.add_widget(title_label)
        if subtitle:
            sub = Label(text=subtitle, font_name=UI_FONT, font_size=sp(THEME.font_size["caption"]), color=color("text_secondary"), halign="left", valign="middle", shorten=True, size_hint_x=1, size_hint_y=None, height=dp(18))
            _bind_single_line_label(sub)
            self.add_widget(sub)
        self.bind(minimum_height=self.setter("height"))
        self.height = self.minimum_height
