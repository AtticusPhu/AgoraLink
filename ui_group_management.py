#!/usr/bin/env python3
"""Group management secondary page."""

from __future__ import annotations

from typing import Callable, Iterable, Mapping, Optional

from kivy.metrics import dp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.widget import Widget

from ui_copy import tr
from ui_form_components import ConfirmationDialog, DangerZone, EmptyStatePanel, SecondaryPopup, SettingsSection, _bind_wrapped, _label, secondary_button, secondary_color
from ui_secondary_shell import SecondaryPageShell, scrollable_content


class GroupManagementPage(SecondaryPageShell):
    def __init__(
        self,
        *,
        lang: str,
        group: Mapping[str, object],
        members: Iterable[Mapping[str, object]],
        local_peer_id: str,
        can_manage: bool,
        on_close: Callable,
        on_add_member: Optional[Callable] = None,
        on_remove_member: Optional[Callable[[str], None]] = None,
        on_leave: Optional[Callable] = None,
        on_dissolve: Optional[Callable] = None,
        **kwargs,
    ) -> None:
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        self.group = dict(group or {})
        self.members = [dict(member or {}) for member in members or []]
        self.local_peer_id = str(local_peer_id or "")
        self.can_manage = bool(can_manage)
        self.on_leave = on_leave
        self.on_dissolve = on_dissolve
        title = str(self.group.get("title") or self.group.get("group_id") or tr(self.lang, "group_management"))
        super().__init__(
            title=title,
            description=tr(self.lang, "group_management_description"),
            close_text=tr(self.lang, "close"),
            on_close=lambda *_: on_close(),
            **kwargs,
        )
        scroll, content = scrollable_content(max_width=780)
        active_members = [m for m in self.members if str(m.get("member_state") or "active") == "active"]
        summary = _label(
            (f"{len(active_members)} 位成员" if self.lang == "zh" else f"{len(active_members)} members"),
            color_name="text_secondary",
            font_size=13,
            size_hint_y=None,
            height=dp(26),
            halign="left",
        )
        _bind_wrapped(summary)
        content.add_widget(summary)
        actions = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(10))
        add_button = secondary_button(
            tr(self.lang, "add_member"),
            variant="primary",
            width=128,
            on_release=(lambda *_: on_add_member()) if callable(on_add_member) else None,
        )
        add_button.disabled = not (self.can_manage and callable(on_add_member))
        actions.add_widget(add_button)
        actions.add_widget(Widget(size_hint_x=1))
        content.add_widget(actions)
        section = SettingsSection(tr(self.lang, "members"))
        if not self.members:
            content.add_widget(
                EmptyStatePanel(
                    title=tr(self.lang, "empty_title"),
                    description=("此群组还没有可显示的成员。" if self.lang == "zh" else "This group has no members to display."),
                )
            )
        else:
            for member in self.members:
                section.add_row(self._member_row(member, on_remove_member))
            content.add_widget(section)
        danger = DangerZone(
            title="危险操作" if self.lang == "zh" else "Danger zone",
            description=(
                "退出会从本机移除该群组数据；群主解散群组会影响所有成员。"
                if self.lang == "zh"
                else "Leaving removes local group data; dissolving a group affects every member."
            ),
        )
        danger.add_action(
            tr(self.lang, "leave_group"),
            lambda *_: self._confirm("leave", tr(self.lang, "leave_group")),
            enabled=callable(on_leave),
        )
        if self.can_manage:
            danger.add_action(
                tr(self.lang, "dissolve_group"),
                lambda *_: self._confirm("dissolve", tr(self.lang, "dissolve_group")),
                enabled=callable(on_dissolve),
            )
        content.add_widget(danger)
        self.set_content(scroll)

    def _member_row(self, member: Mapping[str, object], on_remove_member) -> BoxLayout:
        row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(58), spacing=dp(12), padding=(0, dp(7), 0, dp(7)))
        details = BoxLayout(orientation="vertical", size_hint_x=1, spacing=0)
        name = str(member.get("display_name") or member.get("nickname") or member.get("peer_id") or "-")
        title = _label(name, font_size=13, bold=True, size_hint_y=None, height=dp(24), halign="left")
        _bind_wrapped(title)
        details.add_widget(title)
        role = str(member.get("role") or "member")
        state = str(member.get("member_state") or "active")
        role_text = "群主" if self.lang == "zh" and role == "owner" else "成员" if self.lang == "zh" else "Owner" if role == "owner" else "Member"
        status_text = "正常" if self.lang == "zh" and state == "active" else "已离开" if self.lang == "zh" else "Active" if state == "active" else "Left"
        subtitle = _label(f"{role_text} · {status_text}", color_name="text_muted", font_size=12, size_hint_y=None, height=dp(20), halign="left")
        _bind_wrapped(subtitle)
        details.add_widget(subtitle)
        row.add_widget(details)
        peer_id = str(member.get("peer_id") or "")
        removable = self.can_manage and role != "owner" and peer_id != self.local_peer_id and state == "active" and callable(on_remove_member)
        if removable:
            row.add_widget(
                secondary_button(
                    "移除" if self.lang == "zh" else "Remove",
                    variant="danger",
                    compact=True,
                    width=88,
                    on_release=lambda *_args, pid=peer_id: on_remove_member(pid),
                )
            )
        return row

    def _confirm(self, action: str, label: str) -> None:
        callback = self.on_dissolve if action == "dissolve" else self.on_leave
        if not callable(callback):
            return
        message = (
            "此操作会移除群组及相关本机数据，且不可恢复。"
            if self.lang == "zh"
            else "This removes the group and related local data and cannot be undone."
        )
        ConfirmationDialog(
            lang=self.lang,
            title=label,
            message=message,
            on_confirm=callback,
            confirm_text=label,
            danger=True,
        ).open()


def create_group_management_popup(**kwargs) -> Popup:
    holder = {}
    provided_close = kwargs.pop("on_close", None)

    def _close() -> None:
        popup = holder.get("popup")
        if popup is not None:
            popup.dismiss()
        if callable(provided_close):
            provided_close()

    page = GroupManagementPage(on_close=_close, **kwargs)
    popup = SecondaryPopup(
        title="",
        content=page,
        size_hint=(0.76, 0.88),
        auto_dismiss=False,
        separator_height=0,
        background="",
        background_color=secondary_color("background"),
    )
    holder["popup"] = popup
    return popup
