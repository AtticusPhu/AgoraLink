#!/usr/bin/env python3
"""Standalone static UI preview for AgoraLink theme variants."""

from __future__ import annotations

import os

from kivy.app import App
from kivy.clock import Clock
from kivy.core.window import Window
from kivy.graphics import Color, Rectangle
from kivy.metrics import dp, sp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.label import Label
from kivy.uix.scrollview import ScrollView
from kivy.uix.textinput import TextInput
from kivy.uix.widget import Widget

import ui_components as uc
from ui_theme import THEME_VARIANTS, Theme, get_theme

Window.minimum_width = 900
Window.minimum_height = 600

PREVIEW_THEMES = [
    ("mist", "A  Mist Blue", "Muted blue, quiet, clean"),
    ("porcelain", "B  Porcelain Gray Blue", "White and gray first, blue as accent"),
    ("graphite", "C  Graphite Blue", "Default: professional graphite-leaning utility"),
]


def _label(text: str, *, size: str = "body", color_name: str = "text_primary", bold: bool = False, **kwargs) -> Label:
    kwargs.setdefault("font_name", uc.UI_FONT)
    kwargs.setdefault("font_size", sp(uc.THEME.font_size[size]))
    kwargs.setdefault("color", uc.color(color_name))
    kwargs.setdefault("halign", "left")
    kwargs.setdefault("valign", "middle")
    kwargs.setdefault("shorten", True)
    kwargs.setdefault("shorten_from", "right")
    lab = Label(text=text, bold=bold, **kwargs)
    lab.bind(size=lambda inst, _value: setattr(inst, "text_size", (max(1, inst.width), None)))
    return lab


def _bubble_row(widget, outgoing: bool = False) -> BoxLayout:
    row = BoxLayout(orientation="horizontal", size_hint_y=None)

    def _sync_height(*_args):
        row.height = max(dp(52), float(getattr(widget, "height", 0) or 0) + dp(6))

    widget.bind(height=_sync_height)
    Clock.schedule_once(lambda _dt: _sync_height(), 0)
    if outgoing:
        row.add_widget(Widget(size_hint_x=0.22))
        row.add_widget(widget)
    else:
        row.add_widget(widget)
        row.add_widget(Widget(size_hint_x=0.22))
    return row


def _panel(**kwargs) -> uc.RoundedCard:
    kwargs.setdefault("padding", [dp(14)] * 4)
    kwargs.setdefault("spacing", dp(10))
    kwargs.setdefault("bg_color", uc.color("surface"))
    kwargs.setdefault("border_color", uc.color("border_soft"))
    return uc.RoundedCard(**kwargs)


def build_left_panel() -> uc.RoundedCard:
    panel = _panel(size_hint_x=None, width=dp(238))
    panel.add_widget(uc.SectionHeader("AgoraLink", "Theme preview only"))
    search = uc.RoundedCard(
        size_hint_y=None,
        height=dp(42),
        radius=uc.THEME.radius["pill"],
        bg_color=uc.color("surface_muted"),
        border_color=uc.color("border_soft"),
        padding=[dp(14), 0, dp(14), 0],
    )
    search.add_widget(_label("Search contacts or groups", color_name="text_secondary", size_hint_y=None, height=dp(40)))
    panel.add_widget(search)
    panel.add_widget(uc.ConversationItem(title="Design Lab", preview="Ming: screen share accepted", meta_text="10:42", status_text="Group", badge_text="2", active=True))
    panel.add_widget(uc.ConversationItem(title="Ava Chen", preview="File received: project_notes.pdf", meta_text="09:18", status_text="Direct"))
    panel.add_widget(uc.ConversationItem(title="Build Room", preview="Diagnostics exported", meta_text="Tue", status_text="Group"))
    panel.add_widget(uc.ConversationItem(title="Local Test B", preview="Waiting for file confirmation", meta_text="", status_text="Device"))
    panel.add_widget(Widget())
    status = uc.RoundedCard(size_hint_y=None, height=dp(88), bg_color=uc.color("surface_muted"), border_color=uc.color("border_soft"), radius=uc.THEME.radius["medium"], padding=[dp(12)] * 4)
    status.add_widget(_label("Connection", size="caption", color_name="text_secondary", size_hint_y=None, height=dp(18)))
    line = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(30))
    line.add_widget(uc.StatusBadge(text="Online", status="success", width=dp(78)))
    line.add_widget(_label("UDP 9999 ready", size="caption", color_name="text_secondary"))
    status.add_widget(line)
    panel.add_widget(status)
    return panel


def build_chat_panel() -> BoxLayout:
    panel = BoxLayout(orientation="vertical", spacing=dp(uc.THEME.spacing["md"]))
    header = _panel(size_hint_y=None, height=dp(74), orientation="horizontal", padding=[dp(18), dp(12), dp(18), dp(12)], spacing=dp(12))
    title_box = BoxLayout(orientation="vertical", spacing=dp(2))
    title_box.add_widget(_label("Design Lab", size="title", bold=True, size_hint_y=None, height=dp(28)))
    title_box.add_widget(_label("4 members · screen port 50021 · 720p30 h264_qsv", size="caption", color_name="text_secondary", size_hint_y=None, height=dp(20)))
    header.add_widget(title_box)
    header.add_widget(uc.PillButton(text="Screen", variant="primary", size_hint_x=None, width=dp(92)))
    header.add_widget(uc.PillButton(text="Diagnose", variant="secondary", size_hint_x=None, width=dp(104)))
    panel.add_widget(header)

    scroll = ScrollView(size_hint=(1, 1), bar_width=dp(4))
    feed = BoxLayout(orientation="vertical", spacing=dp(12), size_hint_y=None, padding=[dp(8)] * 4)
    feed.bind(minimum_height=feed.setter("height"))
    feed.add_widget(_bubble_row(uc.MessageBubble(sender="Ming", message="The new release candidate is ready for LAN testing.", direction="incoming"), outgoing=False))
    feed.add_widget(_bubble_row(uc.MessageBubble(sender="You", message="Send me the installer and start screen share when ready.", direction="outgoing"), outgoing=True))
    feed.add_widget(
        _bubble_row(
            uc.FileTransferCard(
                title="File transfer",
                filename="AgoraLink_Setup_v0.0.4_release_candidate_notes.pdf",
                detail="10.8 MB / 24 MB · 8.4 MB/s · ETA 00:02",
                status_text="Sending",
                status="accent",
                progress=46,
                size_hint_x=0.86,
            ),
            outgoing=True,
        )
    )
    feed.add_widget(
        _bubble_row(
            uc.ScreenShareCard(
                title="Screen share invite",
                peer="From Ming · AgoraLink Screen Viewer - Ming",
                detail="Profile 720p30_h264_qsv · UDP 50021",
                status_text="Waiting",
                status="waiting",
                size_hint_x=0.86,
            ),
            outgoing=False,
        )
    )
    feed.add_widget(_bubble_row(uc.MessageBubble(sender="You", message="This outgoing bubble should read as blue-gray, not bright sky blue.", direction="outgoing"), outgoing=True))
    scroll.add_widget(feed)
    panel.add_widget(scroll)

    input_bar = _panel(size_hint_y=None, height=dp(68), orientation="horizontal", spacing=dp(10), padding=[dp(14), dp(12), dp(14), dp(12)])
    message_input = TextInput(
        text="",
        hint_text="Message Design Lab",
        multiline=False,
        font_name=uc.UI_FONT,
        font_size=sp(uc.THEME.font_size["body"]),
        background_normal="",
        background_active="",
        background_color=uc.color("surface_muted"),
        foreground_color=uc.color("text_primary"),
        cursor_color=uc.color("accent"),
        padding=[dp(14), dp(10), dp(14), dp(8)],
    )
    input_bar.add_widget(message_input)
    input_bar.add_widget(uc.RoundedButton(text="Attach", variant="secondary", size_hint_x=None, width=dp(92)))
    input_bar.add_widget(uc.PillButton(text="Send", variant="primary", size_hint_x=None, width=dp(82)))
    panel.add_widget(input_bar)
    return panel


def build_right_panel(theme: Theme) -> uc.RoundedCard:
    panel = _panel(size_hint_x=None, width=dp(256))
    panel.add_widget(uc.SectionHeader("Controls", "Buttons, badges, and tokens"))
    button_row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(34))
    button_row.add_widget(uc.PillButton(text="Primary", variant="primary", size_hint_x=1, compact=True))
    button_row.add_widget(uc.PillButton(text="Secondary", variant="secondary", size_hint_x=1, compact=True))
    panel.add_widget(button_row)
    danger_row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(34))
    danger_row.add_widget(uc.PillButton(text="Danger", variant="danger", size_hint_x=1, compact=True))
    danger_row.add_widget(uc.PillButton(text="Ghost", variant="ghost", size_hint_x=1, compact=True))
    panel.add_widget(danger_row)
    badge_grid = BoxLayout(orientation="vertical", spacing=dp(8), size_hint_y=None, height=dp(104))
    for status_text, status in (("Waiting", "waiting"), ("Active", "accent"), ("Success", "success"), ("Failed", "failed")):
        row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(22))
        row.add_widget(uc.StatusBadge(text=status_text, status=status, width=dp(86)))
        row.add_widget(_label(f"{status_text} keeps text visible", size="caption", color_name="text_secondary"))
        badge_grid.add_widget(row)
    panel.add_widget(badge_grid)
    panel.add_widget(
        uc.FileTransferCard(
            title="File card",
            filename="very_long_file_name_example_with_many_sections_and_final_document.pdf",
            detail="Long filenames should not stretch the card",
            status_text="Complete",
            status="success",
            progress=100,
        )
    )
    panel.add_widget(
        uc.ScreenShareCard(
            title="Screen runtime",
            peer="Watching Ming",
            detail="ffplay window visible · port 50021",
            status_text="Active",
            status="accent",
        )
    )
    token_box = uc.RoundedCard(size_hint_y=None, height=dp(118), bg_color=uc.color("surface_muted"), border_color=uc.color("border_soft"), radius=uc.THEME.radius["medium"], padding=[dp(12)] * 4)
    token_box.add_widget(_label(theme.name, size="body_strong", bold=True, size_hint_y=None, height=dp(22)))
    for name in ("background", "surface_blue", "accent"):
        row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(24))
        swatch = uc.RoundedCard(size_hint_x=None, width=dp(34), height=dp(18), size_hint_y=None, padding=0, radius=6, bg_color=uc.color(name), border_color=uc.color("border"))
        row.add_widget(swatch)
        row.add_widget(_label(name, size="caption", color_name="text_secondary"))
        token_box.add_widget(row)
    panel.add_widget(token_box)
    return panel


class PreviewRoot(BoxLayout):
    def __init__(self, **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("spacing", dp(10))
        kwargs.setdefault("padding", [dp(14)] * 4)
        super().__init__(**kwargs)
        self.active_theme_key = "graphite"
        with self.canvas.before:
            self._bg_color = Color(*uc.color("background"))
            self._bg_rect = Rectangle(pos=self.pos, size=self.size)
        self.bind(pos=self._sync_bg, size=self._sync_bg)
        self.select_theme(self.active_theme_key)

    def _sync_bg(self, *_args):
        self._bg_rect.pos = self.pos
        self._bg_rect.size = self.size

    def select_theme(self, key: str) -> None:
        self.active_theme_key = str(key or "graphite")
        theme = get_theme(self.active_theme_key)
        uc.set_theme(theme)
        Window.clearcolor = uc.color("background")
        self._bg_color.rgba = uc.color("background")
        self.clear_widgets()
        self._build_header(theme)
        self._build_preview(theme)

    def _build_header(self, theme: Theme) -> None:
        header = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(48), spacing=dp(10))
        title_box = BoxLayout(orientation="vertical", spacing=dp(1))
        title_box.add_widget(_label("AgoraLink Theme Calibration", size="title", bold=True, size_hint_y=None, height=dp(26)))
        title_box.add_widget(_label("Preview only. Graphite Blue is the current default theme.", size="caption", color_name="text_secondary", size_hint_y=None, height=dp(18)))
        header.add_widget(title_box)
        for key, label, _description in PREVIEW_THEMES:
            button = uc.PillButton(text=label, variant="primary" if key == self.active_theme_key else "secondary", size_hint_x=None, width=dp(156), compact=True)
            button.bind(on_release=lambda _btn, k=key: self.select_theme(k))
            header.add_widget(button)
        self.add_widget(header)
        note = _label(theme.name, size="caption", color_name="text_secondary", size_hint_y=None, height=dp(20))
        self.add_widget(note)

    def _build_preview(self, theme: Theme) -> None:
        content = BoxLayout(orientation="horizontal", spacing=dp(12), size_hint_y=1)
        content.add_widget(build_left_panel())
        content.add_widget(build_chat_panel())
        content.add_widget(build_right_panel(theme))
        self.add_widget(content)


class AgoraLinkUIPreviewApp(App):
    title = "AgoraLink UI Theme Preview"

    def build(self):
        Window.title = self.title
        Window.minimum_width = 900
        Window.minimum_height = 600
        Window.size = (1100, 680)
        root = PreviewRoot()
        if os.environ.get("CODEX_SHELL") == "1" and not os.environ.get("AGORALINK_UI_PREVIEW_HOLD"):
            Clock.schedule_once(lambda _dt: self.stop(), 1.5)
        return root


def main() -> int:
    AgoraLinkUIPreviewApp().run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
