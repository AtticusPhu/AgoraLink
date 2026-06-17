#!/usr/bin/env python3
"""Reusable Kivy widgets for the AgoraLink UI design preview."""

from __future__ import annotations

from pathlib import Path
from typing import Iterable, List, Optional

from kivy.animation import Animation
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


def color(name: str, alpha: Optional[float] = None, theme: Theme = THEME) -> List[float]:
    rgba = list(theme.colors.get(name, theme.colors["text_primary"]))
    if alpha is not None:
        rgba[3] = float(alpha)
    return rgba


def _mix(a: List[float], b: List[float], amount: float) -> List[float]:
    t = max(0.0, min(1.0, float(amount)))
    return [a[i] + (b[i] - a[i]) * t for i in range(4)]


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
        super().__init__(**kwargs)
        self._init_rounded_canvas()


class RoundedButton(ButtonBehavior, Label):
    """Rounded text button with subtle hover and press feedback."""

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
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(THEME.font_size["body"]))
        kwargs.setdefault("halign", "center")
        kwargs.setdefault("valign", "middle")
        kwargs.setdefault("shorten", True)
        kwargs.setdefault("shorten_from", "right")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(38))
        super().__init__(**kwargs)
        self._disable_default_button_background()
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
            state=lambda *_: self._refresh_button_state(animated=True),
            hovered=lambda *_: self._refresh_button_state(animated=True),
        )
        Window.bind(mouse_pos=self._on_mouse_pos)
        self._update_button_canvas()

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

    def _refresh_button_state(self, animated: bool = False) -> None:
        target = list(self.bg_down if self.state == "down" else (self.bg_hover if self.hovered else self.bg_normal))
        self.color = self.text_down if self.state == "down" else self.text_normal
        if animated:
            Animation.cancel_all(self, "fill_color")
            Animation(fill_color=target, d=0.14, t="out_quad").start(self)
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
        self.fill_color = color(fill_name)
        self.color = color(text_name)

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


class MessageBubble(RoundedCard):
    direction = StringProperty("incoming")
    sender = StringProperty("")
    message = StringProperty("")

    def __init__(self, **kwargs):
        kwargs.setdefault("padding", [dp(14), dp(10), dp(14), dp(10)])
        kwargs.setdefault("spacing", dp(4))
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("size_hint_x", 0.78)
        super().__init__(**kwargs)
        self.sender_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["caption"]), color=color("text_secondary"), halign="left", valign="middle", size_hint_y=None, height=dp(18), shorten=True)
        self.message_label = Label(font_name=UI_FONT, font_size=sp(THEME.font_size["body"]), color=color("text_primary"), halign="left", valign="middle", shorten=True, shorten_from="right", size_hint_y=None, height=dp(38))
        _bind_single_line_label(self.sender_label)
        _bind_single_line_label(self.message_label)
        self.add_widget(self.sender_label)
        self.add_widget(self.message_label)
        self.bind(minimum_height=self.setter("height"))
        self.height = self.minimum_height
        self.bind(sender=self._sync_text, message=self._sync_text, direction=self._sync_style)
        self._sync_text()
        self._sync_style()

    def _sync_text(self, *_args) -> None:
        self.sender_label.text = str(self.sender or "")
        self.message_label.text = str(self.message or "")

    def _sync_style(self, *_args) -> None:
        if str(self.direction or "") == "outgoing":
            self.bg_color = color("surface_blue")
            self.border_color = color("border")
        else:
            self.bg_color = color("surface")
            self.border_color = color("border")


class RoundedProgressBar(Widget):
    value = NumericProperty(0)
    max = NumericProperty(100)
    track_color = ListProperty(color("surface_muted"))
    fill_color = ListProperty(color("accent"))
    radius = NumericProperty(THEME.radius["small"])

    def __init__(self, **kwargs):
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(8))
        super().__init__(**kwargs)
        with self.canvas:
            self._track_color = Color(*self.track_color)
            self._track = RoundedRectangle(pos=self.pos, size=self.size, radius=[self.radius])
            self._fill_color = Color(*self.fill_color)
            self._fill = RoundedRectangle(pos=self.pos, size=(0, self.height), radius=[self.radius])
        self.bind(pos=self._update_bar, size=self._update_bar, value=self._update_bar, max=self._update_bar, track_color=self._update_bar, fill_color=self._update_bar)
        self._update_bar()

    def _update_bar(self, *_args) -> None:
        try:
            ratio = max(0.0, min(1.0, float(self.value) / float(self.max or 100)))
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


class FileTransferCard(RoundedCard):
    title = StringProperty("File transfer")
    filename = StringProperty("document.pdf")
    detail = StringProperty("")
    status_text = StringProperty("Waiting")
    status = StringProperty("waiting")
    progress = NumericProperty(0)

    def __init__(self, **kwargs):
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("padding", [dp(16), dp(14), dp(16), dp(14)])
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
        self.bind(title=self._sync, filename=self._sync, detail=self._sync, status_text=self._sync, status=self._sync, progress=self._sync)
        self._sync()

    def _sync(self, *_args) -> None:
        self.title_label.text = str(self.title or "")
        self.file_label.text = str(self.filename or "")
        self.detail_label.text = str(self.detail or "")
        self.badge.text = str(self.status_text or "")
        self.badge.status = str(self.status or "neutral")
        self.progress_bar.value = float(self.progress or 0)


class ScreenShareCard(RoundedCard):
    title = StringProperty("Screen share")
    peer = StringProperty("Remote")
    detail = StringProperty("")
    status_text = StringProperty("Waiting")
    status = StringProperty("accent")

    def __init__(self, **kwargs):
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("padding", [dp(16), dp(14), dp(16), dp(14)])
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
        action_row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(34))
        action_row.add_widget(PillButton(text="Accept", size_hint_x=None, width=dp(86), bg_normal=color("accent"), bg_hover=color("accent_hover"), text_normal=color("white"), text_down=color("white"), border_color=color("accent")))
        action_row.add_widget(PillButton(text="Decline", size_hint_x=None, width=dp(86), bg_normal=color("surface_muted"), bg_hover=color("danger_soft"), text_normal=color("danger"), text_down=color("danger"), border_color=color("border")))
        action_row.add_widget(Widget())
        self.add_widget(header)
        self.add_widget(self.peer_label)
        self.add_widget(self.detail_label)
        self.add_widget(action_row)
        self.bind(minimum_height=self.setter("height"))
        self.height = self.minimum_height
        self.bind(title=self._sync, peer=self._sync, detail=self._sync, status_text=self._sync, status=self._sync)
        self._sync()

    def _sync(self, *_args) -> None:
        self.title_label.text = str(self.title or "")
        self.peer_label.text = str(self.peer or "")
        self.detail_label.text = str(self.detail or "")
        self.badge.text = str(self.status_text or "")
        self.badge.status = str(self.status or "neutral")


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
