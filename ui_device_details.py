#!/usr/bin/env python3
"""Task-oriented contact detail page for AgoraLink."""

from __future__ import annotations

from typing import Callable, Mapping, Optional

from kivy.metrics import dp, sp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.widget import Widget

from ui_copy import tr
from ui_form_components import (
    AccentMonogram,
    AdvancedDisclosure,
    ConfirmationDialog,
    DangerZone,
    ReadOnlyInfoRow,
    SecondaryPopup,
    SettingsSection,
    _bind_wrapped,
    _label,
    secondary_button,
    secondary_color,
)
from ui_secondary_shell import SecondaryPageShell, scrollable_content


def _display_name(contact: Mapping[str, object]) -> str:
    return str(
        contact.get("remark_name")
        or contact.get("display_name")
        or contact.get("nickname")
        or contact.get("peer_id")
        or "Contact"
    )


def _initials(name: str) -> str:
    parts = [part for part in str(name or "A").replace("_", " ").split() if part]
    if len(parts) >= 2:
        return (parts[0][0] + parts[-1][0]).upper()
    text = parts[0] if parts else "A"
    return text[:2].upper()


class ContactDetailsPage(SecondaryPageShell):
    def __init__(
        self,
        *,
        lang: str,
        contact: Mapping[str, object],
        on_close: Callable,
        on_message: Optional[Callable] = None,
        on_send_file: Optional[Callable] = None,
        on_share_screen: Optional[Callable] = None,
        on_edit_note: Optional[Callable] = None,
        on_delete: Optional[Callable] = None,
        **kwargs,
    ) -> None:
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        self.contact = dict(contact or {})
        self.on_close_callback = on_close
        self.on_delete_callback = on_delete
        super().__init__(
            title=tr(self.lang, "contact_details"),
            description=tr(self.lang, "contact_details_description"),
            close_text=tr(self.lang, "close"),
            on_close=lambda *_: on_close(),
            **kwargs,
        )
        scroll, content = scrollable_content(max_width=760)
        content.add_widget(self._profile_header())
        content.add_widget(
            self._action_row(
                (
                    (tr(self.lang, "message"), "primary", on_message),
                    (tr(self.lang, "send_file"), "primary", on_send_file),
                    (tr(self.lang, "share_screen"), "primary", on_share_screen),
                )
            )
        )
        content.add_widget(
            self._action_row(((tr(self.lang, "edit_note"), "secondary", on_edit_note),))
        )
        security = AdvancedDisclosure(
            title=tr(self.lang, "security_details"),
            description=(
                "仅在核对设备身份或排查连接时需要。"
                if self.lang == "zh"
                else "Needed only when verifying device identity or troubleshooting a connection."
            ),
        )
        rows = (
            ("peer_id", "设备标识" if self.lang == "zh" else "Peer ID"),
            ("fingerprint", "身份指纹" if self.lang == "zh" else "Fingerprint"),
            ("trust_state", "信任状态" if self.lang == "zh" else "Trust state"),
            ("last_seen", "最近会话" if self.lang == "zh" else "Last seen"),
        )
        for key, label in rows:
            value = self.contact.get(key) or "-"
            security.add_row(
                ReadOnlyInfoRow(
                    setting_key=key,
                    label=label,
                    description="",
                    value=value,
                )
            )
        content.add_widget(security)
        danger = DangerZone(
            title="危险操作" if self.lang == "zh" else "Danger zone",
            description=tr(self.lang, "delete_contact_message"),
        )
        danger.add_action(
            tr(self.lang, "delete_contact"),
            lambda *_: self._confirm_delete(),
            enabled=callable(on_delete),
        )
        content.add_widget(danger)
        self.set_content(scroll)

    def _profile_header(self) -> BoxLayout:
        name = _display_name(self.contact)
        header = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(94), spacing=dp(18))
        avatar = AccentMonogram(
            _initials(name),
            radius=dp(35),
            font_size=sp(22),
            size_hint=(None, None),
            size=(dp(70), dp(70)),
        )
        header.add_widget(avatar)
        details = BoxLayout(orientation="vertical", spacing=0)
        title = _label(name, font_size=20, bold=True, size_hint_y=None, height=dp(34), halign="left")
        _bind_wrapped(title)
        details.add_widget(title)
        online = bool(self.contact.get("online") or str(self.contact.get("connection_state") or "").lower() == "online")
        status = tr(self.lang, "online" if online else "offline")
        state = _label(
            status,
            color_name="success" if online else "text_muted",
            font_size=12,
            size_hint_y=None,
            height=dp(24),
            halign="left",
        )
        _bind_wrapped(state)
        details.add_widget(state)
        ip = str(self.contact.get("peer_ip") or "").strip()
        port = str(self.contact.get("peer_port") or "").strip()
        endpoint = f"{ip}:{port}" if ip and port else ip or "-"
        endpoint_label = _label(endpoint, color_name="text_muted", font_size=12, size_hint_y=None, height=dp(22), halign="left")
        _bind_wrapped(endpoint_label)
        details.add_widget(endpoint_label)
        header.add_widget(details)
        return header

    def _action_row(self, actions) -> BoxLayout:
        row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(10))
        for label, variant, callback in actions:
            button = secondary_button(
                label,
                variant=variant,
                width=max(108, min(150, 38 + len(label) * 9)),
                on_release=(lambda *_args, cb=callback: cb()) if callable(callback) else None,
            )
            button.disabled = not callable(callback)
            row.add_widget(button)
        row.add_widget(Widget(size_hint_x=1))
        return row

    def _confirm_delete(self) -> None:
        if not callable(self.on_delete_callback):
            return
        ConfirmationDialog(
            lang=self.lang,
            title=tr(self.lang, "delete_contact"),
            message=tr(self.lang, "delete_contact_message"),
            on_confirm=self.on_delete_callback,
            confirm_text=tr(self.lang, "delete_contact"),
            danger=True,
        ).open()


def create_contact_details_popup(**kwargs) -> Popup:
    holder = {}

    def _close() -> None:
        popup = holder.get("popup")
        if popup is not None:
            popup.dismiss()

    provided_close = kwargs.pop("on_close", None)

    def _combined_close() -> None:
        _close()
        if callable(provided_close):
            provided_close()

    page = ContactDetailsPage(on_close=_combined_close, **kwargs)
    popup = SecondaryPopup(
        title="",
        content=page,
        size_hint=(0.72, 0.88),
        auto_dismiss=False,
        separator_height=0,
        background="",
        background_color=secondary_color("background"),
    )
    holder["popup"] = popup
    return popup
