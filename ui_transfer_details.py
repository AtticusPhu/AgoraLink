#!/usr/bin/env python3
"""File transfer detail page using the shared secondary shell."""

from __future__ import annotations

from typing import Callable, Mapping, Optional, Sequence, Tuple

from kivy.metrics import dp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.widget import Widget

from ui_components import RoundedProgressBar
from ui_copy import tr
from ui_form_components import AdvancedDisclosure, ReadOnlyInfoRow, SecondaryPopup, SettingsSection, _bind_wrapped, _label, secondary_button, secondary_color
from ui_secondary_shell import SecondaryPageShell, scrollable_content


class FileTransferDetailsPage(SecondaryPageShell):
    def __init__(self, *, lang: str, details: Mapping[str, object], on_close: Callable, actions: Optional[Mapping[str, Callable]] = None, **kwargs):
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        self.details = dict(details or {})
        self.actions = dict(actions or {})
        super().__init__(
            title=tr(self.lang, "file_details"),
            description=tr(self.lang, "file_details_description"),
            close_text=tr(self.lang, "close"),
            on_close=lambda *_: on_close(),
            **kwargs,
        )
        scroll, content = scrollable_content(max_width=760)
        filename = str(self.details.get("file_name") or self.details.get("name") or "-")
        title = _label(filename, font_size=19, bold=True, size_hint_y=None, height=dp(34), halign="left")
        _bind_wrapped(title)
        content.add_widget(title)
        progress_value = float(self.details.get("progress") or self.details.get("pct") or 0.0)
        progress = RoundedProgressBar(value=max(0.0, min(100.0, progress_value)), size_hint_y=None, height=dp(10))
        content.add_widget(progress)
        summary = SettingsSection("传输状态" if self.lang == "zh" else "Transfer status")
        rows = (
            ("status", "当前状态" if self.lang == "zh" else "Status"),
            ("size", "文件大小" if self.lang == "zh" else "File size"),
            ("peer", "对方设备" if self.lang == "zh" else "Peer device"),
            ("speed", "速度" if self.lang == "zh" else "Speed"),
            ("eta", "剩余时间" if self.lang == "zh" else "Time remaining"),
            ("saved_path", "保存位置" if self.lang == "zh" else "Saved location"),
        )
        for key, label in rows:
            value = self.details.get(key)
            if value not in (None, ""):
                summary.add_row(ReadOnlyInfoRow(setting_key=key, label=label, value=value))
        content.add_widget(summary)
        self._add_actions(content)
        technical = AdvancedDisclosure(
            title=tr(self.lang, "technical_details"),
            description="仅用于故障排查。" if self.lang == "zh" else "Used only for troubleshooting.",
        )
        for key, label in (
            ("file_id", "File ID"),
            ("task_id", "Task ID"),
            ("offset", "Offset / ranges"),
            ("hash", "Hash"),
            ("conn_id", "Connection ID"),
        ):
            value = self.details.get(key)
            if value not in (None, ""):
                technical.add_row(ReadOnlyInfoRow(setting_key=key, label=label, value=value))
        content.add_widget(technical)
        self.set_content(scroll)

    def _add_actions(self, content: BoxLayout) -> None:
        definitions = (
            ("resume", "继续" if self.lang == "zh" else "Resume", "primary"),
            ("retry", tr(self.lang, "try_again"), "primary"),
            ("open_folder", "打开文件夹" if self.lang == "zh" else "Open folder", "secondary"),
            ("cancel", "取消传输" if self.lang == "zh" else "Cancel transfer", "danger"),
        )
        visible = [(key, label, variant) for key, label, variant in definitions if callable(self.actions.get(key))]
        if not visible:
            return
        row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(10))
        for key, label, variant in visible:
            callback = self.actions[key]
            row.add_widget(secondary_button(label, variant=variant, width=max(108, 32 + len(label) * 9), on_release=lambda *_args, cb=callback: cb()))
        row.add_widget(Widget(size_hint_x=1))
        content.add_widget(row)


def create_file_transfer_details_popup(**kwargs) -> Popup:
    holder = {}
    provided_close = kwargs.pop("on_close", None)

    def _close() -> None:
        popup = holder.get("popup")
        if popup is not None:
            popup.dismiss()
        if callable(provided_close):
            provided_close()

    page = FileTransferDetailsPage(on_close=_close, **kwargs)
    popup = SecondaryPopup(title="", content=page, size_hint=(0.72, 0.86), auto_dismiss=False, separator_height=0, background="", background_color=secondary_color("background"))
    holder["popup"] = popup
    return popup
