#!/usr/bin/env python3
"""Low-noise diagnostics secondary page."""

from __future__ import annotations

from typing import Callable, Iterable, Mapping, Optional, Sequence

from kivy.metrics import dp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.widget import Widget

from ui_copy import tr
from ui_form_components import AdvancedDisclosure, InlineStatusRow, ReadOnlyInfoRow, SecondaryPopup, SettingsSection, _bind_wrapped, _label, secondary_button, secondary_color
from ui_secondary_shell import SecondaryPageShell, fixed_action_footer, scrollable_content


def _status_kind(value: object) -> str:
    text = str(value or "").strip().lower()
    if any(token in text for token in ("error", "failed", "unavailable", "missing", "occupied", "错误", "失败", "不可用", "占用")):
        return "danger"
    if any(token in text for token in ("warning", "waiting", "degraded", "警告", "等待")):
        return "warning"
    if any(token in text for token in ("running", "available", "ready", "ok", "正常", "可用", "运行")):
        return "success"
    return "neutral"


class DiagnosticsPage(SecondaryPageShell):
    def __init__(
        self,
        *,
        lang: str,
        summary_items: Sequence[Mapping[str, object]],
        technical_sections: Optional[Mapping[str, object]] = None,
        recent_errors: str = "",
        on_close: Callable,
        on_export: Optional[Callable] = None,
        on_recheck: Optional[Callable] = None,
        on_preview: Optional[Callable] = None,
        **kwargs,
    ) -> None:
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        export_button = secondary_button(
            tr(self.lang, "export_diagnostics"),
            variant="primary",
            width=150,
            on_release=(lambda *_: on_export()) if callable(on_export) else None,
        )
        export_button.disabled = not callable(on_export)
        close_button = secondary_button(tr(self.lang, "close"), width=96, on_release=lambda *_: on_close())
        footer = fixed_action_footer(right_actions=(export_button, close_button))
        super().__init__(
            title=tr(self.lang, "diagnostics"),
            description=(
                "先查看关键状态；详细技术信息和日志默认折叠。"
                if self.lang == "zh"
                else "Review key status first. Technical details and logs remain collapsed by default."
            ),
            close_text=tr(self.lang, "close"),
            on_close=lambda *_: on_close(),
            footer=footer,
            **kwargs,
        )
        scroll, content = scrollable_content(max_width=900)
        summary = SettingsSection(
            tr(self.lang, "status_summary"),
            "异常状态会给出文字说明，不只依赖颜色。" if self.lang == "zh" else "Issues include text explanations and never rely on color alone.",
        )
        for index, item in enumerate(summary_items or ()):
            title = str(item.get("title") or item.get("label") or f"Status {index + 1}")
            status = str(item.get("status") or "-")
            detail = str(item.get("detail") or "")
            summary.add_row(
                InlineStatusRow(
                    setting_key=f"summary_{index}",
                    label=title,
                    description=detail,
                    value=status,
                    status_kind=str(item.get("kind") or _status_kind(status)),
                )
            )
        content.add_widget(summary)
        actions = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(10))
        if callable(on_recheck):
            actions.add_widget(secondary_button(tr(self.lang, "recheck"), variant="primary", width=120, on_release=lambda *_: on_recheck()))
        if callable(on_preview):
            actions.add_widget(secondary_button("UI Preview", width=120, on_release=lambda *_: on_preview()))
        actions.add_widget(Widget(size_hint_x=1))
        content.add_widget(actions)

        technical = AdvancedDisclosure(
            title=tr(self.lang, "technical_details"),
            description=(
                "端口、路径、组件校验和运行时状态。"
                if self.lang == "zh"
                else "Ports, paths, component verification, and runtime state."
            ),
        )
        for key, value in dict(technical_sections or {}).items():
            technical.add_row(ReadOnlyInfoRow(setting_key=str(key), label=str(key), value=value))
        content.add_widget(technical)

        errors = AdvancedDisclosure(
            title=tr(self.lang, "recent_errors"),
            description=(
                "默认仅显示最近摘要；完整日志包含在诊断包中。"
                if self.lang == "zh"
                else "Only a recent summary is shown; the diagnostic bundle contains complete logs."
            ),
        )
        error_text = str(recent_errors or "").strip() or tr(self.lang, "no_recent_errors")
        errors.add_row(
            ReadOnlyInfoRow(
                setting_key="recent_errors",
                label=tr(self.lang, "recent_errors"),
                description="",
                value=error_text,
                height=dp(88),
            )
        )
        content.add_widget(errors)
        self.set_content(scroll)


def create_diagnostics_popup(**kwargs) -> Popup:
    holder = {}
    provided_close = kwargs.pop("on_close", None)

    def _close() -> None:
        popup = holder.get("popup")
        if popup is not None:
            popup.dismiss()
        if callable(provided_close):
            provided_close()

    page = DiagnosticsPage(on_close=_close, **kwargs)
    popup = SecondaryPopup(
        title="",
        content=page,
        size_hint=(0.88, 0.90),
        auto_dismiss=False,
        separator_height=0,
        background="",
        background_color=secondary_color("background"),
    )
    holder["popup"] = popup
    return popup
