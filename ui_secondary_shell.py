#!/usr/bin/env python3
"""Shared shell for AgoraLink settings and secondary desktop pages."""

from __future__ import annotations

from typing import Callable, Dict, Iterable, Optional, Sequence, Tuple

from kivy.graphics import Color, Line, Rectangle
from kivy.metrics import dp, sp
from kivy.properties import StringProperty
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.label import Label
from kivy.uix.scrollview import ScrollView
from kivy.uix.widget import Widget

from ui_form_components import _BackgroundBox, _bind_wrapped, _label, secondary_button, secondary_color


class SecondaryPageHeader(_BackgroundBox):
    def __init__(
        self,
        *,
        title: str,
        description: str = "",
        close_text: str = "Close",
        on_close: Optional[Callable] = None,
        primary_text: str = "",
        on_primary: Optional[Callable] = None,
        **kwargs,
    ) -> None:
        kwargs.setdefault("orientation", "horizontal")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(74))
        kwargs.setdefault("spacing", dp(14))
        kwargs.setdefault("padding", (dp(24), dp(12), dp(18), dp(10)))
        kwargs.setdefault("background_color", secondary_color("surface"))
        super().__init__(**kwargs)
        titles = BoxLayout(orientation="vertical", size_hint_x=1, spacing=dp(1))
        self.title_label = _label(
            title,
            font_size=18,
            bold=True,
            size_hint_y=None,
            height=dp(30),
            halign="left",
        )
        _bind_wrapped(self.title_label)
        titles.add_widget(self.title_label)
        self.description_label = _label(
            description,
            color_name="text_muted",
            font_size=11,
            size_hint_y=None,
            height=dp(22),
            halign="left",
        )
        _bind_wrapped(self.description_label)
        titles.add_widget(self.description_label)
        self.add_widget(titles)
        if primary_text and on_primary is not None:
            self.primary_button = secondary_button(primary_text, variant="primary", width=132, on_release=on_primary)
            self.add_widget(self.primary_button)
        else:
            self.primary_button = None
        self.close_button = secondary_button(close_text, variant="ghost", width=88, on_release=on_close)
        self.add_widget(self.close_button)
        with self.canvas.after:
            self._header_border_color = Color(*secondary_color("border"))
            self._header_border = Line(points=(self.x, self.y, self.right, self.y), width=1)
        self.bind(pos=self._sync_header_border, size=self._sync_header_border)

    def _sync_header_border(self, *_args) -> None:
        self._header_border.points = (self.x, self.y, self.right, self.y)

    def set_page(self, title: str, description: str = "") -> None:
        self.title_label.text = str(title or "")
        self.description_label.text = str(description or "")


class SettingsSidebar(_BackgroundBox):
    selected_key = StringProperty("")

    def __init__(
        self,
        *,
        entries: Sequence[Tuple[str, str]],
        selected_key: str,
        on_select: Callable[[str], None],
        **kwargs,
    ) -> None:
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("size_hint_x", None)
        kwargs.setdefault("width", dp(216))
        kwargs.setdefault("padding", (dp(16), dp(18), dp(14), dp(18)))
        kwargs.setdefault("spacing", dp(5))
        kwargs.setdefault("background_color", secondary_color("surface"))
        super().__init__(**kwargs)
        self._on_select = on_select
        self._buttons: Dict[str, object] = {}
        self.selected_key = str(selected_key or "")
        for key, label in entries:
            button = secondary_button(label, variant="ghost", compact=False)
            button.size_hint_y = None
            button.height = dp(44)
            button.halign = "left"
            button.valign = "middle"
            button.text_size = (dp(166), dp(44))
            button.bind(on_release=lambda _button, item_key=key: self.select(item_key))
            self._buttons[str(key)] = button
            self.add_widget(button)
        self.add_widget(Widget(size_hint_y=1))
        with self.canvas.after:
            self._side_border_color = Color(*secondary_color("border"))
            self._side_border = Line(points=(self.right, self.y, self.right, self.top), width=1)
        self.bind(pos=self._sync_side_border, size=self._sync_side_border)
        self._sync_selection()

    def _sync_side_border(self, *_args) -> None:
        self._side_border.points = (self.right, self.y, self.right, self.top)

    def select(self, key: str, *, notify: bool = True) -> None:
        value = str(key or "")
        if value not in self._buttons:
            return
        self.selected_key = value
        self._sync_selection()
        if notify:
            self._on_select(value)

    def _sync_selection(self) -> None:
        for key, button in self._buttons.items():
            active = key == self.selected_key
            button.bg_normal = secondary_color("accent" if active else "transparent")
            button.bg_hover = secondary_color("accent_hover" if active else "surface_muted")
            button.bg_down = secondary_color("accent_hover" if active else "accent_soft")
            button.text_normal = secondary_color("white" if active else "text_secondary")
            button.text_down = secondary_color("white" if active else "text_primary")
            button.border_color = secondary_color("accent" if active else "transparent")
            try:
                button._refresh_button_state(animated=False)
            except Exception:
                pass


class SecondaryPageShell(_BackgroundBox):
    """Header, body, optional sidebar, and fixed footer for a secondary page."""

    def __init__(
        self,
        *,
        title: str,
        description: str = "",
        close_text: str = "Close",
        on_close: Optional[Callable] = None,
        sidebar_entries: Sequence[Tuple[str, str]] = (),
        selected_sidebar_key: str = "",
        on_sidebar_select: Optional[Callable[[str], None]] = None,
        footer: Optional[Widget] = None,
        **kwargs,
    ) -> None:
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("background_color", secondary_color("background"))
        super().__init__(**kwargs)
        self.header = SecondaryPageHeader(
            title=title,
            description=description,
            close_text=close_text,
            on_close=on_close,
        )
        self.add_widget(self.header)
        self.body = BoxLayout(orientation="horizontal", size_hint_y=1)
        self.sidebar: Optional[SettingsSidebar] = None
        if sidebar_entries and on_sidebar_select is not None:
            self.sidebar = SettingsSidebar(
                entries=sidebar_entries,
                selected_key=selected_sidebar_key,
                on_select=on_sidebar_select,
            )
            self.body.add_widget(self.sidebar)
        self.content_host = BoxLayout(
            orientation="vertical",
            size_hint_x=1,
            padding=(dp(24), dp(18), dp(24), dp(18)),
        )
        self.body.add_widget(self.content_host)
        self.add_widget(self.body)
        self.footer = footer
        if footer is not None:
            self.add_widget(footer)

    def set_content(self, widget: Widget) -> None:
        self.content_host.clear_widgets()
        self.content_host.add_widget(widget)


def fixed_action_footer(
    *,
    left_actions: Sequence[Widget] = (),
    right_actions: Sequence[Widget] = (),
) -> _BackgroundBox:
    footer = _BackgroundBox(
        orientation="horizontal",
        size_hint_y=None,
        height=dp(64),
        padding=(dp(20), dp(12), dp(20), dp(12)),
        spacing=dp(10),
        background_color=secondary_color("surface"),
    )
    with footer.canvas.after:
        footer._footer_border_color = Color(*secondary_color("border"))
        footer._footer_border = Line(points=(footer.x, footer.top, footer.right, footer.top), width=1)

    def _sync_footer(*_args) -> None:
        footer._footer_border.points = (footer.x, footer.top, footer.right, footer.top)

    footer.bind(pos=_sync_footer, size=_sync_footer)
    for item in left_actions:
        footer.add_widget(item)
    footer.add_widget(Widget(size_hint_x=1))
    for item in right_actions:
        footer.add_widget(item)
    return footer


def scrollable_content(*, max_width: float = 900) -> Tuple[ScrollView, BoxLayout]:
    scroll = ScrollView(size_hint=(1, 1), do_scroll_x=False)
    outer = BoxLayout(orientation="horizontal", size_hint_y=None)
    outer.bind(minimum_height=outer.setter("height"))
    outer.add_widget(Widget(size_hint_x=1))
    content = BoxLayout(
        orientation="vertical",
        size_hint_x=None,
        width=dp(max_width),
        size_hint_y=None,
        spacing=dp(22),
        padding=(0, 0, 0, dp(24)),
    )
    content.bind(minimum_height=content.setter("height"))
    outer.add_widget(content)
    outer.add_widget(Widget(size_hint_x=1))

    def _fit_content(instance: BoxLayout, width: float) -> None:
        available = max(dp(420), width - dp(4))
        content.width = min(dp(max_width), available)

    outer.bind(width=_fit_content)
    scroll.add_widget(outer)
    return scroll, content
