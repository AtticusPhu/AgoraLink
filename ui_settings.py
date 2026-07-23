#!/usr/bin/env python3
"""Product settings center built from the shared settings schema."""

from __future__ import annotations

from typing import Callable, Dict, Mapping, Optional, Sequence, Tuple

from kivy.metrics import dp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.label import Label
from kivy.uix.widget import Widget

from ui_copy import tr
from ui_form_components import (
    AdvancedDisclosure,
    ConfirmationDialog,
    DangerZone,
    InlineStatusRow,
    NumberSettingRow,
    PathSettingRow,
    ReadOnlyInfoRow,
    SelectSettingRow,
    SettingRow,
    SettingsSection,
    TextSettingRow,
    ToggleSettingRow,
    _bind_wrapped,
    _label,
    secondary_button,
    secondary_color,
)
from ui_secondary_shell import SecondaryPageShell, fixed_action_footer, scrollable_content
from ui_settings_schema import (
    SECTION_GROUPS,
    SETTING_BY_KEY,
    SECTION_BY_KEY,
    SettingDefinition,
    SettingsModel,
    ordered_sections,
    settings_for_section,
)


GROUP_COPY = {
    "zh": {
        "interface": ("界面", "语言和外观设置。"),
        "device": ("此设备", "在附近设备和会话中显示的信息。"),
        "lan": ("局域网连接", "接收端口、设备发现和防火墙状态。"),
        "receive_files": ("接收文件", "保存位置、同名文件和续传行为。"),
        "send_queue": ("发送与队列", "多文件打包和任务处理方式。"),
        "quality": ("画质", "选择预设并查看实际分辨率、帧率和码率。"),
        "sound": ("声音", "系统声音仅在本机 native 音频能力可用时启用。"),
        "connection": ("连接", "屏幕共享端口和接收状态。"),
        "media_engine": ("内置媒体引擎", "当前设备上的捕获、编码、解码和渲染能力。"),
        "trusted_devices": ("受信任设备", "查看已允许建立加密会话的设备。"),
        "chat_data": ("聊天数据", "聊天数据库状态和本机存储位置。"),
        "confirmation": ("确认行为", "危险或外部请求始终需要用户确认。"),
        "storage_locations": ("存储", "应用数据、下载、临时文件和日志位置。"),
        "diagnostic_status": ("诊断", "先查看摘要，需要时再导出完整诊断包。"),
        "product": ("AgoraLink", "用于局域网聊天、文件传输和屏幕共享。"),
        "build": ("版本与构建", "当前软件包和内置组件信息。"),
    },
    "en": {
        "interface": ("Interface", "Language and appearance settings."),
        "device": ("This device", "Information shown to nearby devices and conversations."),
        "lan": ("Local network", "Receive ports, discovery, and firewall status."),
        "receive_files": ("Receiving files", "Save location, same-name files, and resume behavior."),
        "send_queue": ("Sending & queue", "Multi-file packaging and task handling."),
        "quality": ("Quality", "Choose a preset and review its resolution, frame rate, and bitrate."),
        "sound": ("Sound", "System audio is enabled only when native audio is available on this device."),
        "connection": ("Connection", "Screen sharing port and receive status."),
        "media_engine": ("Built-in media engine", "Capture, encode, decode, and rendering capabilities on this device."),
        "trusted_devices": ("Trusted devices", "Review devices allowed to establish encrypted conversations."),
        "chat_data": ("Chat data", "Chat database status and local storage location."),
        "confirmation": ("Confirmation behavior", "External and destructive requests always require confirmation."),
        "storage_locations": ("Storage", "Application data, downloads, temporary files, and logs."),
        "diagnostic_status": ("Diagnostics", "Review the summary first, then export a complete diagnostic bundle when needed."),
        "product": ("AgoraLink", "Local-network chat, file transfer, and screen sharing."),
        "build": ("Version & build", "Current package and bundled component information."),
    },
}


def _group_copy(lang: str, key: str) -> Tuple[str, str]:
    language = "en" if str(lang).lower().startswith("en") else "zh"
    return GROUP_COPY[language][key]


class SettingsCenter(SecondaryPageShell):
    """Seven-section settings center with a fixed action footer."""

    def __init__(
        self,
        *,
        lang: str,
        initial_values: Optional[Mapping[str, object]] = None,
        context: Optional[Mapping[str, object]] = None,
        on_save: Optional[Callable[[Dict[str, object]], object]] = None,
        on_close: Optional[Callable] = None,
        on_browse_directory: Optional[Callable[[Callable[[str], None]], None]] = None,
        actions: Optional[Mapping[str, Callable]] = None,
        initial_section: str = "general",
        **kwargs,
    ) -> None:
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        self.model = SettingsModel(initial_values, context=context)
        self.context = dict(context or {})
        self.on_save_callback = on_save
        self.on_close_callback = on_close
        self.on_browse_directory = on_browse_directory
        self.actions: Dict[str, Callable] = dict(actions or {})
        self.rows: Dict[str, SettingRow] = {}
        self.current_section = initial_section if initial_section in SECTION_BY_KEY else "general"

        reset_button = secondary_button(
            tr(self.lang, "restore_defaults"),
            width=132,
            on_release=lambda *_: self.confirm_reset_current_section(),
        )
        cancel_button = secondary_button(
            tr(self.lang, "cancel"),
            width=96,
            on_release=lambda *_: self._close(),
        )
        save_button = secondary_button(
            tr(self.lang, "save_changes"),
            variant="primary",
            width=132,
            on_release=lambda *_: self.save(),
        )
        self.footer_status = _label(
            "",
            color_name="text_muted",
            font_size=11,
            size_hint_x=None,
            width=dp(230),
            halign="left",
        )
        _bind_wrapped(self.footer_status)
        footer = fixed_action_footer(
            left_actions=(reset_button, self.footer_status),
            right_actions=(cancel_button, save_button),
        )
        section = SECTION_BY_KEY[self.current_section]
        super().__init__(
            title=section.label(self.lang),
            description=section.description(self.lang),
            close_text=tr(self.lang, "close"),
            on_close=lambda *_: self._close(),
            sidebar_entries=tuple((item.key, item.label(self.lang)) for item in ordered_sections()),
            selected_sidebar_key=self.current_section,
            on_sidebar_select=self.show_section,
            footer=footer,
            **kwargs,
        )
        self.show_section(self.current_section)

    def _close(self) -> None:
        if callable(self.on_close_callback):
            self.on_close_callback()

    def show_section(self, section_key: str) -> None:
        key = str(section_key or "")
        if key not in SECTION_BY_KEY:
            return
        if self.rows and key != self.current_section:
            self._collect_rows()
        self.current_section = key
        if self.sidebar is not None and self.sidebar.selected_key != key:
            self.sidebar.select(key, notify=False)
        section = SECTION_BY_KEY[key]
        self.header.set_page(section.label(self.lang), section.description(self.lang))
        self.rows = {}
        scroll, content = scrollable_content(max_width=900)
        self._add_page_intro(content, key)
        for group_key, setting_keys in SECTION_GROUPS.get(key, ()):
            title, description = _group_copy(self.lang, group_key)
            group = SettingsSection(title, description)
            for setting_key in setting_keys:
                definition = SETTING_BY_KEY[setting_key]
                if not self.model.is_visible(definition):
                    continue
                row = self._build_row(definition)
                self.rows[definition.key] = row
                group.add_row(row)
            content.add_widget(group)

        advanced = [
            definition
            for definition in settings_for_section(key)
            if definition.advanced and self.model.is_visible(definition)
        ]
        if advanced:
            disclosure = AdvancedDisclosure(
                title=tr(self.lang, "advanced"),
                description=tr(self.lang, "advanced_description"),
            )
            for definition in advanced:
                row = self._build_row(definition)
                self.rows[definition.key] = row
                disclosure.add_row(row)
            content.add_widget(disclosure)

        self._add_section_actions(content, key)
        self.set_content(scroll)
        self.footer_status.text = ""
        self._bind_dynamic_rows(key)

    def _add_page_intro(self, content: BoxLayout, section_key: str) -> None:
        if section_key != "about":
            return
        hero = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(78), spacing=dp(16))
        monogram = Label(
            text="A",
            font_name="RUDP_UI",
            font_size=dp(24),
            color=secondary_color("white"),
            size_hint=(None, None),
            size=(dp(54), dp(54)),
            halign="center",
            valign="middle",
        )
        monogram.text_size = monogram.size
        from kivy.graphics import Color, RoundedRectangle

        with monogram.canvas.before:
            monogram._badge_color = Color(*secondary_color("accent"))
            monogram._badge_rect = RoundedRectangle(pos=monogram.pos, size=monogram.size, radius=[dp(8)])
        monogram.bind(
            pos=lambda inst, _value: setattr(inst._badge_rect, "pos", inst.pos),
            size=lambda inst, _value: setattr(inst._badge_rect, "size", inst.size),
        )
        hero.add_widget(monogram)
        text = BoxLayout(orientation="vertical", spacing=0)
        title = _label("AgoraLink", font_size=20, bold=True, size_hint_y=None, height=dp(34), halign="left")
        _bind_wrapped(title)
        text.add_widget(title)
        subtitle = _label(
            _group_copy(self.lang, "product")[1],
            color_name="text_muted",
            font_size=12,
            size_hint_y=None,
            height=dp(28),
            halign="left",
        )
        _bind_wrapped(subtitle)
        text.add_widget(subtitle)
        hero.add_widget(text)
        content.add_widget(hero)

    def _build_row(self, definition: SettingDefinition) -> SettingRow:
        value = self.context.get(definition.key, self.model.values.get(definition.key, definition.default))
        common = {
            "setting_key": definition.key,
            "label": definition.label(self.lang),
            "description": definition.description(self.lang),
            "unit": definition.unit(self.lang),
            "restart_note": tr(self.lang, "needs_restart") if definition.restart_required else "",
        }
        if definition.control_type == "toggle":
            row = ToggleSettingRow(
                value=bool(value),
                on_text=tr(self.lang, "enabled"),
                off_text=tr(self.lang, "disabled"),
                **common,
            )
            row.toggle.disabled = not self.model.is_enabled(definition)
        elif definition.control_type == "select":
            row = SelectSettingRow(
                value=value,
                choices=tuple((choice.value, choice.label(self.lang)) for choice in definition.choices),
                **common,
            )
        elif definition.control_type == "number":
            row = NumberSettingRow(value=value, **common)
        elif definition.control_type == "path":
            row = PathSettingRow(
                value=value,
                browse_text=tr(self.lang, "browse"),
                on_browse=self.on_browse_directory,
                **common,
            )
        elif definition.control_type == "text":
            row = TextSettingRow(value=value, password=definition.sensitive, **common)
        elif definition.control_type == "status":
            row = InlineStatusRow(
                value=value,
                status_kind=str(self.context.get(f"{definition.key}_kind") or "neutral"),
                **common,
            )
        else:
            row = ReadOnlyInfoRow(value=value, **common)
        return row

    def _bind_dynamic_rows(self, section: str) -> None:
        if section == "screen" and "screen_native_preset" in self.rows:
            row = self.rows["screen_native_preset"]
            spinner = getattr(row, "spinner", None)
            if spinner is not None:
                spinner.bind(text=lambda *_: self._update_screen_preset_rows())

    def _update_screen_preset_rows(self) -> None:
        row = self.rows.get("screen_native_preset")
        if row is None:
            return
        preset_id = str(row.get_value() or "r4_default")
        details = dict(self.context.get("screen_preset_details") or {})
        selected = dict(details.get(preset_id) or {})
        summary = selected.get("summary") or self.context.get("screen_preset_summary") or "-"
        repair = selected.get("repair") or "NACK"
        delay = selected.get("playout_delay_ms") or 250
        native_detail = selected.get("detail") or self.context.get("screen_native_detail") or "-"
        for key, value in (
            ("screen_preset_summary", summary),
            ("screen_repair_mode", repair),
            ("screen_playout_delay_ms", delay),
            ("screen_native_detail", native_detail),
        ):
            if key in self.rows:
                self.rows[key].set_value(value)

    def _add_section_actions(self, content: BoxLayout, section: str) -> None:
        if section == "network":
            self._add_action_row(
                content,
                (
                    (tr(self.lang, "recheck"), "recheck_network", "primary"),
                    (("防火墙设置" if self.lang == "zh" else "Firewall settings"), "firewall", "secondary"),
                ),
            )
        elif section == "storage":
            self._add_action_row(
                content,
                (
                    (tr(self.lang, "export_diagnostics"), "export_diagnostics", "primary"),
                    (tr(self.lang, "open_logs"), "open_logs", "secondary"),
                    (("清理临时文件" if self.lang == "zh" else "Clear temporary files"), "clean_temp", "secondary"),
                ),
            )
        elif section == "about":
            self._add_action_row(
                content,
                ((tr(self.lang, "copy_technical_info"), "copy_technical_info", "secondary"),),
            )
        elif section == "privacy":
            danger = DangerZone(
                title="危险操作" if self.lang == "zh" else "Danger zone",
                description=(
                    "删除信任、聊天数据或本机身份会影响现有会话。这些操作始终需要再次确认。"
                    if self.lang == "zh"
                    else "Removing trust, chat data, or this device identity affects existing conversations. These actions always require confirmation."
                ),
            )
            labels = (
                ("清除全部信任记录" if self.lang == "zh" else "Clear all trust", "clear_trust"),
                ("清除聊天数据库" if self.lang == "zh" else "Clear chat database", "clear_chat_database"),
                ("重置本机身份" if self.lang == "zh" else "Reset device identity", "reset_identity"),
            )
            for label, action_id in labels:
                callback = self.actions.get(action_id)
                danger.add_action(
                    label,
                    lambda *_args, aid=action_id, text=label: self._confirm_danger(aid, text),
                    enabled=callable(callback),
                )
            content.add_widget(danger)

    def _add_action_row(self, content: BoxLayout, definitions: Sequence[Tuple[str, str, str]]) -> None:
        row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(10))
        for label, action_id, variant in definitions:
            callback = self.actions.get(action_id)
            button = secondary_button(
                label,
                variant=variant,
                width=max(116, min(180, 36 + len(label) * 9)),
                on_release=(lambda *_args, cb=callback: cb()) if callable(callback) else None,
            )
            button.disabled = not callable(callback)
            row.add_widget(button)
        row.add_widget(Widget(size_hint_x=1))
        content.add_widget(row)

    def _confirm_danger(self, action_id: str, label: str) -> None:
        callback = self.actions.get(action_id)
        if not callable(callback):
            return
        ConfirmationDialog(
            lang=self.lang,
            title=label,
            message=tr(self.lang, "danger_irreversible"),
            on_confirm=callback,
            confirm_text=label,
            danger=True,
        ).open()

    def _collect_rows(self) -> None:
        for key, row in self.rows.items():
            definition = SETTING_BY_KEY.get(key)
            if definition is None or not definition.persist:
                continue
            try:
                self.model.set_value(key, row.get_value())
            except (TypeError, ValueError):
                self.model.values[key] = row.get_value()

    def _error_text(self, definition: SettingDefinition, code: str) -> str:
        if code == "range":
            return tr(
                self.lang,
                "value_out_of_range",
                minimum=definition.minimum,
                maximum=definition.maximum,
            )
        return tr(self.lang, "invalid_value")

    def save(self) -> bool:
        self._collect_rows()
        errors = self.model.validate()
        if errors:
            first_key = next(iter(errors))
            first = SETTING_BY_KEY[first_key]
            if first.section != self.current_section:
                self.show_section(first.section)
            for key, code in errors.items():
                row = self.rows.get(key)
                if row is not None:
                    row.set_error(self._error_text(SETTING_BY_KEY[key], code))
            self.footer_status.color = secondary_color("danger")
            self.footer_status.text = tr(self.lang, "invalid_value")
            return False
        values = self.model.serializable_values()
        result = True
        if callable(self.on_save_callback):
            try:
                callback_result = self.on_save_callback(values)
                if callback_result is False:
                    result = False
                elif isinstance(callback_result, str) and callback_result:
                    self.footer_status.text = callback_result
                    result = False
            except Exception as exc:
                self.footer_status.text = str(exc)
                result = False
        self.footer_status.color = secondary_color("success" if result else "danger")
        if not self.footer_status.text:
            self.footer_status.text = tr(self.lang, "changes_saved" if result else "changes_not_saved")
        return result

    def reset_current_section(self) -> None:
        self.model.reset_section(self.current_section)
        self.show_section(self.current_section)
        self.footer_status.color = secondary_color("text_muted")
        self.footer_status.text = tr(self.lang, "confirm_reset_message")

    def confirm_reset_current_section(self) -> None:
        ConfirmationDialog(
            lang=self.lang,
            title=tr(self.lang, "confirm_reset_title"),
            message=tr(self.lang, "confirm_reset_message"),
            on_confirm=self.reset_current_section,
            confirm_text=tr(self.lang, "restore_defaults"),
            danger=False,
        ).open()


def settings_navigation_labels(lang: str) -> Tuple[str, ...]:
    return tuple(section.label(lang) for section in ordered_sections())
