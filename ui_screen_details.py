#!/usr/bin/env python3
"""Screen sharing detail page using user-facing status summaries."""

from __future__ import annotations

from typing import Callable, Mapping, Optional

from kivy.metrics import dp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.widget import Widget

from ui_copy import tr
from ui_form_components import AdvancedDisclosure, ReadOnlyInfoRow, SecondaryPopup, SettingsSection, _bind_wrapped, _label, secondary_button, secondary_color
from ui_secondary_shell import SecondaryPageShell, scrollable_content


class ScreenShareDetailsPage(SecondaryPageShell):
    def __init__(self, *, lang: str, details: Mapping[str, object], on_close: Callable, actions: Optional[Mapping[str, Callable]] = None, **kwargs):
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        self.details = dict(details or {})
        self.actions = dict(actions or {})
        super().__init__(
            title=tr(self.lang, "screen_details"),
            description=tr(self.lang, "screen_details_description"),
            close_text=tr(self.lang, "close"),
            on_close=lambda *_: on_close(),
            **kwargs,
        )
        scroll, content = scrollable_content(max_width=760)
        peer = str(self.details.get("peer") or self.details.get("peer_label") or "-")
        title = _label(peer, font_size=19, bold=True, size_hint_y=None, height=dp(34), halign="left")
        _bind_wrapped(title)
        content.add_widget(title)
        summary = SettingsSection("共享状态" if self.lang == "zh" else "Sharing status")
        for key, label in (
            ("status", "当前状态" if self.lang == "zh" else "Status"),
            ("direction", "方向" if self.lang == "zh" else "Direction"),
            ("quality", "当前画质" if self.lang == "zh" else "Quality"),
            ("audio", "声音" if self.lang == "zh" else "Sound"),
            ("duration", "持续时间" if self.lang == "zh" else "Duration"),
            ("connection", "连接质量" if self.lang == "zh" else "Connection"),
        ):
            value = self.details.get(key)
            if value not in (None, ""):
                summary.add_row(ReadOnlyInfoRow(setting_key=key, label=label, value=value))
        content.add_widget(summary)
        self._add_actions(content)
        technical = AdvancedDisclosure(
            title=tr(self.lang, "technical_details"),
            description="仅在诊断连接问题时查看。" if self.lang == "zh" else "Review only when diagnosing a connection problem.",
        )
        for key, label in (
            ("session_id", "Session ID"),
            ("port", "Media port"),
            ("profile", "Native profile"),
            ("runtime_state", "Runtime state"),
        ):
            value = self.details.get(key)
            if value not in (None, ""):
                technical.add_row(ReadOnlyInfoRow(setting_key=key, label=label, value=value))
        content.add_widget(technical)
        self.set_content(scroll)

    def _add_actions(self, content: BoxLayout) -> None:
        definitions = (
            ("stop", tr(self.lang, "stop_sharing"), "danger"),
            ("retry", tr(self.lang, "try_again"), "primary"),
            ("settings", tr(self.lang, "open_settings"), "secondary"),
        )
        visible = [(key, label, variant) for key, label, variant in definitions if callable(self.actions.get(key))]
        if not visible:
            return
        row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(10))
        for key, label, variant in visible:
            callback = self.actions[key]
            row.add_widget(secondary_button(label, variant=variant, width=max(110, 32 + len(label) * 9), on_release=lambda *_args, cb=callback: cb()))
        row.add_widget(Widget(size_hint_x=1))
        content.add_widget(row)


def create_screen_share_details_popup(**kwargs) -> Popup:
    holder = {}
    provided_close = kwargs.pop("on_close", None)

    def _close() -> None:
        popup = holder.get("popup")
        if popup is not None:
            popup.dismiss()
        if callable(provided_close):
            provided_close()

    page = ScreenShareDetailsPage(on_close=_close, **kwargs)
    popup = SecondaryPopup(title="", content=page, size_hint=(0.72, 0.86), auto_dismiss=False, separator_height=0, background="", background_color=secondary_color("background"))
    holder["popup"] = popup
    return popup
