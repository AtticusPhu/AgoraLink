#!/usr/bin/env python3
"""Standalone static UI preview for AgoraLink's design system."""

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

from ui_components import (
    FileTransferCard,
    MessageBubble,
    PillButton,
    RoundedButton,
    RoundedCard,
    ScreenShareCard,
    SectionHeader,
    StatusBadge,
    THEME,
    UI_FONT,
    color,
)

Window.minimum_width = 900
Window.minimum_height = 600


class PreviewRoot(BoxLayout):
    def __init__(self, **kwargs):
        kwargs.setdefault("orientation", "horizontal")
        kwargs.setdefault("spacing", dp(THEME.spacing["sm"]))
        kwargs.setdefault("padding", [dp(THEME.spacing["md"])] * 4)
        super().__init__(**kwargs)
        with self.canvas.before:
            self._bg_color = Color(*color("background"))
            self._bg_rect = Rectangle(pos=self.pos, size=self.size)
        self.bind(pos=self._sync_bg, size=self._sync_bg)

    def _sync_bg(self, *_args):
        self._bg_rect.pos = self.pos
        self._bg_rect.size = self.size


def _label(text: str, *, size: str = "body", color_name: str = "text_primary", bold: bool = False, **kwargs) -> Label:
    kwargs.setdefault("font_name", UI_FONT)
    kwargs.setdefault("font_size", sp(THEME.font_size[size]))
    kwargs.setdefault("color", color(color_name))
    kwargs.setdefault("halign", "left")
    kwargs.setdefault("valign", "middle")
    kwargs.setdefault("shorten", True)
    kwargs.setdefault("shorten_from", "right")
    lab = Label(text=text, bold=bold, **kwargs)
    lab.bind(size=lambda inst, _value: setattr(inst, "text_size", (max(1, inst.width), None)))
    return lab


def _conversation_item(name: str, preview: str, badge: str = "", active: bool = False) -> RoundedCard:
    card = RoundedCard(
        size_hint_y=None,
        height=dp(72),
        padding=[dp(12), dp(10), dp(12), dp(10)],
        spacing=dp(2),
        bg_color=color("surface_blue") if active else color("surface"),
        border_color=color("accent_soft") if active else color("border"),
        radius=THEME.radius["medium"],
    )
    row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(24))
    row.add_widget(_label(name, size="body_strong", bold=True))
    if badge:
        row.add_widget(StatusBadge(text=badge, status="accent", width=dp(58)))
    card.add_widget(row)
    card.add_widget(_label(preview, size="caption", color_name="text_secondary", size_hint_y=None, height=dp(22)))
    return card


def _bubble_row(bubble: MessageBubble, outgoing: bool = False) -> BoxLayout:
    row = BoxLayout(orientation="horizontal", size_hint_y=None)
    def _sync_height(*_args):
        row.height = max(dp(48), float(bubble.height or 0) + dp(4))
    bubble.bind(height=_sync_height)
    Clock.schedule_once(lambda _dt: _sync_height(), 0)
    if outgoing:
        row.add_widget(Widget())
        row.add_widget(bubble)
    else:
        row.add_widget(bubble)
        row.add_widget(Widget())
    return row


def build_left_panel() -> RoundedCard:
    panel = RoundedCard(size_hint_x=None, width=dp(236), padding=[dp(14)] * 4, spacing=dp(10))
    panel.add_widget(SectionHeader("AgoraLink", "Local network productivity chat"))
    search = RoundedCard(size_hint_y=None, height=dp(44), radius=THEME.radius["pill"], bg_color=color("surface_muted"), border_color=color("border"), padding=[dp(14), 0, dp(14), 0])
    search.add_widget(_label("Search contacts or groups", color_name="text_secondary", size_hint_y=None, height=dp(42)))
    panel.add_widget(search)
    panel.add_widget(_conversation_item("Design Lab", "Ming: screen share accepted", "2", active=True))
    panel.add_widget(_conversation_item("Ava Chen", "File received: project_notes.pdf"))
    panel.add_widget(_conversation_item("Build Room", "Diagnostics exported"))
    panel.add_widget(_conversation_item("Local Test B", "Waiting for file confirmation"))
    panel.add_widget(Widget())
    status = RoundedCard(size_hint_y=None, height=dp(92), bg_color=color("surface_muted"), border_color=color("border"), radius=THEME.radius["medium"], padding=[dp(12)] * 4)
    status.add_widget(_label("Connection", size="caption", color_name="text_secondary", size_hint_y=None, height=dp(18)))
    line = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(30))
    line.add_widget(StatusBadge(text="Online", status="success", width=dp(78)))
    line.add_widget(_label("UDP 9999 ready", size="caption", color_name="text_secondary"))
    status.add_widget(line)
    panel.add_widget(status)
    return panel


def build_chat_panel() -> BoxLayout:
    panel = BoxLayout(orientation="vertical", spacing=dp(THEME.spacing["md"]))
    header = RoundedCard(size_hint_y=None, height=dp(76), orientation="horizontal", padding=[dp(18), dp(12), dp(18), dp(12)], spacing=dp(12))
    title_box = BoxLayout(orientation="vertical", spacing=dp(2))
    title_box.add_widget(_label("Design Lab", size="title", bold=True, size_hint_y=None, height=dp(28)))
    title_box.add_widget(_label("4 members · screen port 50021 · 720p30 h264_qsv", size="caption", color_name="text_secondary", size_hint_y=None, height=dp(20)))
    header.add_widget(title_box)
    header.add_widget(PillButton(text="Screen", size_hint_x=None, width=dp(92), bg_normal=color("accent"), bg_hover=color("accent_hover"), text_normal=color("white"), text_down=color("white"), border_color=color("accent")))
    header.add_widget(PillButton(text="Diagnose", size_hint_x=None, width=dp(104)))
    panel.add_widget(header)

    scroll = ScrollView(size_hint=(1, 1), bar_width=dp(4))
    feed = BoxLayout(orientation="vertical", spacing=dp(12), size_hint_y=None, padding=[dp(8), dp(8), dp(8), dp(8)])
    feed.bind(minimum_height=feed.setter("height"))
    feed.add_widget(_bubble_row(MessageBubble(sender="Ming", message="The new release candidate is ready for LAN testing.", direction="incoming"), outgoing=False))
    feed.add_widget(_bubble_row(MessageBubble(sender="You", message="Send me the installer and start screen share when ready.", direction="outgoing"), outgoing=True))
    file_card = FileTransferCard(
        title="File transfer",
        filename="AgoraLink_Setup_v0.0.4_release_candidate_notes.pdf",
        detail="10.8 MB / 24 MB · 8.4 MB/s · ETA 00:02",
        status_text="Sending",
        status="accent",
        progress=46,
        size_hint_x=0.86,
    )
    feed.add_widget(_bubble_row(file_card, outgoing=True))
    screen_card = ScreenShareCard(
        title="Screen share invite",
        peer="From Ming · AgoraLink Screen Viewer - Ming",
        detail="Profile 720p30_h264_qsv · UDP 50021",
        status_text="Waiting",
        status="waiting",
        size_hint_x=0.86,
    )
    feed.add_widget(_bubble_row(screen_card, outgoing=False))
    complete_card = FileTransferCard(
        title="File transfer",
        filename="diagnostic_bundle_20260617.zip",
        detail="Saved to Downloads · 2.1 MB",
        status_text="Complete",
        status="success",
        progress=100,
        size_hint_x=0.86,
    )
    feed.add_widget(_bubble_row(complete_card, outgoing=False))
    failed_card = FileTransferCard(
        title="File transfer",
        filename="very_long_file_name_example_with_many_sections_and_final_document.pdf",
        detail="Receiver rejected the transfer",
        status_text="Rejected",
        status="failed",
        progress=0,
        size_hint_x=0.86,
    )
    feed.add_widget(_bubble_row(failed_card, outgoing=True))
    scroll.add_widget(feed)
    panel.add_widget(scroll)

    input_bar = RoundedCard(size_hint_y=None, height=dp(70), orientation="horizontal", spacing=dp(10), padding=[dp(14), dp(12), dp(14), dp(12)])
    message_input = TextInput(
        text="",
        hint_text="Message Design Lab",
        multiline=False,
        font_name=UI_FONT,
        font_size=sp(THEME.font_size["body"]),
        background_color=color("surface_muted"),
        foreground_color=color("text_primary"),
        cursor_color=color("accent"),
        padding=[dp(14), dp(10), dp(14), dp(8)],
    )
    input_bar.add_widget(message_input)
    input_bar.add_widget(RoundedButton(text="Attach", size_hint_x=None, width=dp(92)))
    input_bar.add_widget(PillButton(text="Send", size_hint_x=None, width=dp(82), bg_normal=color("accent"), bg_hover=color("accent_hover"), text_normal=color("white"), text_down=color("white"), border_color=color("accent")))
    panel.add_widget(input_bar)
    return panel


def build_right_panel() -> RoundedCard:
    panel = RoundedCard(size_hint_x=None, width=dp(248), padding=[dp(14)] * 4, spacing=dp(12))
    panel.add_widget(SectionHeader("Status", "Quiet operational overview"))
    row = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=None, height=dp(28))
    row.add_widget(StatusBadge(text="Stable", status="success", width=dp(84)))
    row.add_widget(_label("No protocol alerts", size="caption", color_name="text_secondary"))
    panel.add_widget(row)
    progress = FileTransferCard(
        title="Current transfer",
        filename="release_notes.pdf",
        detail="12.3 MB / 24 MB · throttled UI updates",
        status_text="Active",
        status="accent",
        progress=52,
    )
    panel.add_widget(progress)
    screen = ScreenShareCard(
        title="Screen runtime",
        peer="Watching Ming",
        detail="ffplay window visible · port 50021",
        status_text="Receiving",
        status="accent",
    )
    panel.add_widget(screen)
    panel.add_widget(Widget())
    footer = RoundedCard(size_hint_y=None, height=dp(96), bg_color=color("surface_muted"), border_color=color("border"), radius=THEME.radius["medium"], padding=[dp(12)] * 4)
    footer.add_widget(_label("Design intent", size="caption", color_name="text_secondary", size_hint_y=None, height=dp(20)))
    footer.add_widget(_label("Low-noise surfaces, clear status, compact repeated workflows.", size="body", color_name="text_primary"))
    panel.add_widget(footer)
    return panel


class AgoraLinkUIPreviewApp(App):
    title = "AgoraLink UI Preview"

    def build(self):
        Window.title = self.title
        Window.minimum_width = 900
        Window.minimum_height = 600
        Window.size = (1100, 680)
        Window.clearcolor = color("background")
        root = PreviewRoot()
        root.add_widget(build_left_panel())
        root.add_widget(build_chat_panel())
        root.add_widget(build_right_panel())
        if os.environ.get("CODEX_SHELL") == "1" and not os.environ.get("AGORALINK_UI_PREVIEW_HOLD"):
            Clock.schedule_once(lambda _dt: self.stop(), 1.5)
        return root


def main() -> int:
    AgoraLinkUIPreviewApp().run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
