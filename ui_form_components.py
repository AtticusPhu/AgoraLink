#!/usr/bin/env python3
"""Reusable form and state components for AgoraLink secondary pages."""

from __future__ import annotations

from typing import Callable, Iterable, List, Mapping, Optional, Sequence, Tuple

from kivy.clock import Clock
from kivy.graphics import Color, Line, Rectangle, RoundedRectangle
from kivy.metrics import dp, sp
from kivy.properties import BooleanProperty, ListProperty, StringProperty
from kivy.uix.behaviors import FocusBehavior
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.label import Label
from kivy.uix.popup import Popup
from kivy.uix.spinner import Spinner, SpinnerOption
from kivy.uix.textinput import TextInput
from kivy.uix.widget import Widget

from ui_components import RoundedButton
from ui_copy import tr
from ui_theme import SECONDARY_DARK_THEME
from ui_settings_schema import danger_action_requires_confirmation


PALETTE = SECONDARY_DARK_THEME.colors
UI_FONT = "RUDP_UI"


def secondary_color(name: str, alpha: Optional[float] = None) -> List[float]:
    value = list(PALETTE.get(name, PALETTE["text_primary"]))
    if alpha is not None:
        value[3] = float(alpha)
    return value


def _bind_wrapped(label: Label, *, horizontal_padding: float = 0) -> Label:
    def _sync(instance: Label, _value=None) -> None:
        instance.text_size = (max(1, instance.width - dp(horizontal_padding)), None)

    label.bind(width=_sync)
    _sync(label)
    return label


def _label(
    text: str = "",
    *,
    color_name: str = "text_primary",
    font_size: int = 14,
    bold: bool = False,
    halign: str = "left",
    valign: str = "middle",
    **kwargs,
) -> Label:
    kwargs.setdefault("font_name", UI_FONT)
    kwargs.setdefault("font_size", sp(font_size))
    kwargs.setdefault("color", secondary_color(color_name))
    kwargs.setdefault("halign", halign)
    kwargs.setdefault("valign", valign)
    kwargs.setdefault("bold", bold)
    return Label(text=str(text or ""), **kwargs)


class SecondaryPopup(Popup):
    """Modal secondary surface that always supports the desktop Esc shortcut."""

    def _handle_keyboard(self, window, key, *args):
        if key == 27:
            self.dismiss()
            return True
        return super()._handle_keyboard(window, key, *args)


class KeyboardRoundedButton(FocusBehavior, RoundedButton):
    """Rounded button with visible keyboard focus and desktop activation keys."""

    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self._resting_border_color = list(self.border_color)
        self.bind(focus=self._sync_keyboard_focus, disabled=self._sync_keyboard_focus)

    def _sync_keyboard_focus(self, *_args) -> None:
        if self.focus and not self.disabled:
            self.border_color = secondary_color("accent")
        else:
            self.border_color = list(self._resting_border_color)

    def keyboard_on_key_down(self, window, keycode, text, modifiers):
        key_name = str(keycode[1] if isinstance(keycode, (tuple, list)) else keycode).lower()
        if key_name in {"enter", "numpadenter", "space", "spacebar"} and not self.disabled:
            self.trigger_action(duration=0)
            return True
        return super().keyboard_on_key_down(window, keycode, text, modifiers)


class KeyboardSpinner(FocusBehavior, Spinner):
    """Compact spinner that can be traversed and changed without a mouse."""

    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        self._resting_background_color = list(self.background_color)
        self.bind(focus=self._sync_keyboard_focus, disabled=self._sync_keyboard_focus)

    def _sync_keyboard_focus(self, *_args) -> None:
        self.background_color = secondary_color("accent_soft") if self.focus and not self.disabled else list(
            self._resting_background_color
        )

    def keyboard_on_key_down(self, window, keycode, text, modifiers):
        key_name = str(keycode[1] if isinstance(keycode, (tuple, list)) else keycode).lower()
        if self.disabled:
            return super().keyboard_on_key_down(window, keycode, text, modifiers)
        if key_name in {"enter", "numpadenter", "space", "spacebar"}:
            self.is_open = not self.is_open
            return True
        if key_name in {"up", "down", "left", "right"} and self.values:
            values = list(self.values)
            try:
                index = values.index(self.text)
            except ValueError:
                index = 0
            delta = -1 if key_name in {"up", "left"} else 1
            self.text = values[(index + delta) % len(values)]
            return True
        return super().keyboard_on_key_down(window, keycode, text, modifiers)


class _BackgroundBox(BoxLayout):
    background_color = ListProperty(secondary_color("surface"))
    border_color = ListProperty(secondary_color("border_soft"))
    border_width = 0
    radius = 0

    def __init__(self, **kwargs):
        background = kwargs.pop("background_color", secondary_color("surface"))
        border = kwargs.pop("border_color", secondary_color("border_soft"))
        self.border_width = float(kwargs.pop("border_width", 0))
        self.radius = float(kwargs.pop("radius", 0))
        super().__init__(**kwargs)
        self.background_color = list(background)
        self.border_color = list(border)
        with self.canvas.before:
            self._surface_color = Color(*self.background_color)
            if self.radius:
                self._surface = RoundedRectangle(pos=self.pos, size=self.size, radius=[self.radius])
            else:
                self._surface = Rectangle(pos=self.pos, size=self.size)
            outline_color = self.border_color if self.border_width > 0 else (0, 0, 0, 0)
            self._outline_color = Color(*outline_color)
            self._outline = Line(width=max(1.0, self.border_width))
        self.bind(
            pos=self._sync_canvas,
            size=self._sync_canvas,
            background_color=self._sync_canvas,
            border_color=self._sync_canvas,
        )
        self._sync_canvas()

    def _sync_canvas(self, *_args) -> None:
        self._surface_color.rgba = self.background_color
        self._surface.pos = self.pos
        self._surface.size = self.size
        self._outline_color.rgba = (
            self.border_color if self.border_width > 0 else (0, 0, 0, 0)
        )
        if self.radius:
            self._surface.radius = [self.radius]
            self._outline.rounded_rectangle = (
                self.x,
                self.y,
                self.width,
                self.height,
                self.radius,
            )
        else:
            self._outline.rectangle = (self.x, self.y, self.width, self.height)
        self._outline.width = max(1.0, self.border_width)


def secondary_button(
    text: str,
    *,
    variant: str = "secondary",
    width: Optional[float] = None,
    compact: bool = False,
    on_release: Optional[Callable] = None,
) -> KeyboardRoundedButton:
    role = str(variant or "secondary").lower()
    if role == "primary":
        style = {
            "bg_normal": secondary_color("accent"),
            "bg_hover": secondary_color("accent_hover"),
            "bg_down": secondary_color("accent_hover"),
            "text_normal": secondary_color("white"),
            "text_down": secondary_color("white"),
            "border_color": secondary_color("accent"),
        }
    elif role == "danger":
        style = {
            "bg_normal": secondary_color("transparent"),
            "bg_hover": secondary_color("danger_soft"),
            "bg_down": secondary_color("danger_soft"),
            "text_normal": secondary_color("danger"),
            "text_down": secondary_color("danger"),
            "border_color": secondary_color("transparent"),
        }
    elif role == "ghost":
        style = {
            "bg_normal": secondary_color("transparent"),
            "bg_hover": secondary_color("surface_muted"),
            "bg_down": secondary_color("surface_blue"),
            "text_normal": secondary_color("text_secondary"),
            "text_down": secondary_color("text_primary"),
            "border_color": secondary_color("transparent"),
        }
    else:
        style = {
            "bg_normal": secondary_color("surface_muted"),
            "bg_hover": secondary_color("surface_blue"),
            "bg_down": secondary_color("accent_soft"),
            "text_normal": secondary_color("text_primary"),
            "text_down": secondary_color("text_primary"),
            "border_color": secondary_color("border"),
        }
    button = KeyboardRoundedButton(
        text=str(text or ""),
        variant="custom",
        compact=compact,
        radius=dp(5),
        **style,
    )
    if width is not None:
        button.size_hint_x = None
        button.width = dp(width)
    if on_release is not None:
        button.bind(on_release=on_release)
    return button


class DarkSpinnerOption(SpinnerOption):
    def __init__(self, **kwargs):
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(13))
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(38))
        super().__init__(**kwargs)
        self.background_normal = ""
        self.background_down = ""
        self.background_color = secondary_color("surface_muted")
        self.color = secondary_color("text_primary")
        self.bind(state=self._sync_state)

    def _sync_state(self, *_args) -> None:
        self.background_color = secondary_color(
            "accent_soft" if self.state == "down" else "surface_muted"
        )


def dark_spinner(*, text: str, values: Iterable[str]) -> KeyboardSpinner:
    spinner = KeyboardSpinner(
        text=str(text or ""),
        values=tuple(str(item) for item in values),
        option_cls=DarkSpinnerOption,
        font_name=UI_FONT,
        font_size=sp(13),
        size_hint_y=None,
        height=dp(38),
        background_normal="",
        background_down="",
        background_disabled_normal="",
        background_color=secondary_color("surface_muted"),
        color=secondary_color("text_primary"),
    )
    return spinner


def dark_input(*, text: str = "", input_filter=None, password: bool = False) -> TextInput:
    widget = TextInput(
        text=str(text if text is not None else ""),
        multiline=False,
        input_filter=input_filter,
        password=password,
        font_name=UI_FONT,
        font_size=sp(13),
        size_hint_y=None,
        height=dp(38),
        padding=(dp(10), dp(9), dp(10), dp(7)),
        background_normal="",
        background_active="",
        background_color=secondary_color("surface_muted"),
        foreground_color=secondary_color("text_primary"),
        hint_text_color=secondary_color("text_muted"),
        cursor_color=secondary_color("accent"),
        selection_color=secondary_color("accent", 0.35),
    )
    return widget


class SettingDescription(Label):
    def __init__(self, text: str = "", **kwargs):
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(12))
        kwargs.setdefault("color", secondary_color("text_muted"))
        kwargs.setdefault("halign", "left")
        kwargs.setdefault("valign", "top")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(34))
        super().__init__(text=str(text or ""), **kwargs)
        _bind_wrapped(self)


class FormErrorText(Label):
    def __init__(self, text: str = "", **kwargs):
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(11))
        kwargs.setdefault("color", secondary_color("danger"))
        kwargs.setdefault("halign", "right")
        kwargs.setdefault("valign", "middle")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(18) if text else 0)
        super().__init__(text=str(text or ""), **kwargs)
        _bind_wrapped(self)

    def set_error(self, text: str) -> None:
        self.text = str(text or "")
        self.height = dp(18) if self.text else 0


class SettingRow(BoxLayout):
    """Desktop form row with a flexible label column and a control column."""

    setting_key = StringProperty("")

    def __init__(
        self,
        *,
        setting_key: str,
        label: str,
        description: str = "",
        control: Optional[Widget] = None,
        unit: str = "",
        restart_note: str = "",
        **kwargs,
    ) -> None:
        kwargs.setdefault("orientation", "horizontal")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(70 if description else 54))
        kwargs.setdefault("spacing", dp(18))
        kwargs.setdefault("padding", (0, dp(6), 0, dp(6)))
        super().__init__(**kwargs)
        self.setting_key = str(setting_key or "")
        self.control = control
        self.error_text = FormErrorText()

        text_column = BoxLayout(
            orientation="vertical",
            size_hint_x=0.47,
            spacing=dp(1),
        )
        title = _label(
            label,
            font_size=13,
            size_hint_y=None,
            height=dp(24),
            halign="left",
            valign="middle",
        )
        _bind_wrapped(title)
        text_column.add_widget(title)
        if description:
            text_column.add_widget(SettingDescription(description))
        else:
            text_column.add_widget(Widget(size_hint_y=1))
        self.add_widget(text_column)

        self.control_column = BoxLayout(
            orientation="vertical",
            size_hint_x=0.53,
            spacing=dp(1),
        )
        control_line = BoxLayout(
            orientation="horizontal",
            size_hint_y=None,
            height=dp(38),
            spacing=dp(8),
        )
        if control is not None:
            control_line.add_widget(control)
        if unit:
            unit_label = _label(
                unit,
                color_name="text_muted",
                font_size=11,
                size_hint_x=None,
                width=dp(max(38, min(90, 14 + len(unit) * 8))),
            )
            control_line.add_widget(unit_label)
        self.control_column.add_widget(control_line)
        note_text = str(restart_note or "")
        if note_text:
            note = _label(
                note_text,
                color_name="text_muted",
                font_size=10,
                halign="right",
                valign="middle",
                size_hint_y=None,
                height=dp(16),
            )
            _bind_wrapped(note)
            self.control_column.add_widget(note)
        self.control_column.add_widget(self.error_text)
        self.add_widget(self.control_column)

        with self.canvas.after:
            self._separator_color = Color(*secondary_color("border_soft"))
            self._separator = Line(points=(self.x, self.y, self.right, self.y), width=1)
        self.bind(pos=self._sync_separator, size=self._sync_separator)

    def _sync_separator(self, *_args) -> None:
        self._separator.points = (self.x, self.y, self.right, self.y)

    def get_value(self):
        return getattr(self.control, "text", None)

    def set_value(self, value: object) -> None:
        if self.control is not None and hasattr(self.control, "text"):
            self.control.text = str(value if value is not None else "")

    def set_error(self, text: str) -> None:
        self.error_text.set_error(text)


class ToggleControl(KeyboardRoundedButton):
    active = BooleanProperty(False)
    enabled_text = StringProperty("On")
    disabled_text = StringProperty("Off")

    def __init__(self, *, on_text: str = "On", off_text: str = "Off", **kwargs):
        active = bool(kwargs.pop("active", False))
        self.enabled_text = str(on_text)
        self.disabled_text = str(off_text)
        kwargs.setdefault("size_hint_x", None)
        kwargs.setdefault("width", dp(96))
        kwargs.setdefault("height", dp(34))
        kwargs.setdefault("compact", True)
        kwargs.setdefault("radius", dp(17))
        super().__init__(**kwargs)
        self.active = active
        self.bind(on_release=lambda *_: setattr(self, "active", not self.active))
        self.bind(active=self._refresh_toggle)
        self._refresh_toggle()

    def _refresh_toggle(self, *_args) -> None:
        self.text = self.enabled_text if self.active else self.disabled_text
        self.bg_normal = secondary_color("accent" if self.active else "surface_muted")
        self.bg_hover = secondary_color("accent_hover" if self.active else "surface_blue")
        self.bg_down = secondary_color("accent_hover" if self.active else "accent_soft")
        self.text_normal = secondary_color("white" if self.active else "text_secondary")
        self.text_down = list(self.text_normal)
        resting_border = secondary_color("accent" if self.active else "border")
        self._resting_border_color = list(resting_border)
        self.border_color = secondary_color("accent") if self.focus else resting_border
        try:
            self._refresh_button_state(animated=False)
        except Exception:
            pass


class ToggleSettingRow(SettingRow):
    def __init__(self, *, value: bool = False, on_text: str = "On", off_text: str = "Off", **kwargs):
        self.toggle = ToggleControl(active=bool(value), on_text=on_text, off_text=off_text)
        super().__init__(control=self.toggle, **kwargs)

    def get_value(self) -> bool:
        return bool(self.toggle.active)

    def set_value(self, value: object) -> None:
        self.toggle.active = bool(value)


class SelectSettingRow(SettingRow):
    def __init__(
        self,
        *,
        value: object,
        choices: Sequence[Tuple[object, str]],
        **kwargs,
    ) -> None:
        self.choices = list(choices)
        self.label_to_value = {str(label): choice_value for choice_value, label in self.choices}
        self.value_to_label = {str(choice_value): str(label) for choice_value, label in self.choices}
        selected = self.value_to_label.get(str(value), str(value if value is not None else ""))
        self.spinner = dark_spinner(text=selected, values=self.label_to_value.keys())
        super().__init__(control=self.spinner, **kwargs)

    def get_value(self):
        return self.label_to_value.get(self.spinner.text, self.spinner.text)

    def set_value(self, value: object) -> None:
        self.spinner.text = self.value_to_label.get(str(value), str(value if value is not None else ""))


class TextSettingRow(SettingRow):
    def __init__(self, *, value: object = "", password: bool = False, **kwargs):
        self.input = dark_input(text=str(value if value is not None else ""), password=password)
        super().__init__(control=self.input, **kwargs)

    def get_value(self) -> str:
        return str(self.input.text or "")

    def set_value(self, value: object) -> None:
        self.input.text = str(value if value is not None else "")


class NumberSettingRow(TextSettingRow):
    def __init__(self, *, value: object = "", **kwargs):
        super().__init__(value=value, **kwargs)
        self.input.input_filter = "float"


class PathSettingRow(SettingRow):
    def __init__(
        self,
        *,
        value: object = "",
        browse_text: str = "Browse",
        on_browse: Optional[Callable[[Callable[[str], None]], None]] = None,
        **kwargs,
    ) -> None:
        self.input = dark_input(text=str(value if value is not None else ""))
        line = BoxLayout(orientation="horizontal", spacing=dp(8))
        line.add_widget(self.input)
        button = secondary_button(browse_text, compact=True, width=84)
        if on_browse is None:
            button.disabled = True
        else:
            button.bind(on_release=lambda *_: on_browse(self._accept_path))
        line.add_widget(button)
        self.browse_button = button
        super().__init__(control=line, **kwargs)

    def _accept_path(self, path: str) -> None:
        if str(path or "").strip():
            self.input.text = str(path)

    def get_value(self) -> str:
        return str(self.input.text or "")

    def set_value(self, value: object) -> None:
        self.input.text = str(value if value is not None else "")


class ReadOnlyInfoRow(SettingRow):
    def __init__(self, *, value: object = "", value_color: str = "text_secondary", **kwargs):
        self.value_label = _label(
            str(value if value is not None else ""),
            color_name=value_color,
            font_size=13,
            halign="right",
            valign="middle",
            shorten=True,
            shorten_from="center",
        )
        _bind_wrapped(self.value_label)
        super().__init__(control=self.value_label, **kwargs)

    def get_value(self):
        return self.value_label.text

    def set_value(self, value: object) -> None:
        self.value_label.text = str(value if value is not None else "")


class InlineStatusRow(ReadOnlyInfoRow):
    def __init__(self, *, value: object = "", status_kind: str = "neutral", **kwargs):
        self.status_kind = str(status_kind or "neutral")
        super().__init__(value=value, value_color=self._status_color(), **kwargs)

    def _status_color(self) -> str:
        if self.status_kind in {"success", "available", "active"}:
            return "success"
        if self.status_kind in {"warning", "waiting"}:
            return "warning"
        if self.status_kind in {"danger", "error", "unavailable"}:
            return "danger"
        return "text_secondary"


class SettingsSection(BoxLayout):
    def __init__(self, title: str, description: str = "", **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("spacing", dp(2))
        kwargs.setdefault("padding", (0, 0, 0, dp(10)))
        super().__init__(**kwargs)
        self.bind(minimum_height=self.setter("height"))
        header_height = dp(50 if description else 34)
        header = BoxLayout(orientation="vertical", size_hint_y=None, height=header_height)
        title_label = _label(
            title,
            font_size=15,
            bold=True,
            size_hint_y=None,
            height=dp(28),
            halign="left",
        )
        _bind_wrapped(title_label)
        header.add_widget(title_label)
        if description:
            subtitle = _label(
                description,
                color_name="text_muted",
                font_size=11,
                size_hint_y=None,
                height=dp(20),
                halign="left",
            )
            _bind_wrapped(subtitle)
            header.add_widget(subtitle)
        self.add_widget(header)
        self.rows = BoxLayout(orientation="vertical", size_hint_y=None, spacing=0)
        self.rows.bind(minimum_height=self.rows.setter("height"))
        self.add_widget(self.rows)

    def add_row(self, row: Widget) -> None:
        self.rows.add_widget(row)


class AdvancedDisclosure(BoxLayout):
    expanded = BooleanProperty(False)

    def __init__(self, *, title: str, description: str = "", expanded: bool = False, **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("spacing", dp(4))
        super().__init__(**kwargs)
        self.expanded = bool(expanded)
        self.bind(minimum_height=self.setter("height"))
        self.header_button = secondary_button("", variant="ghost", compact=True)
        self.header_button.size_hint_x = 1
        self.header_button.halign = "left"
        self.header_button.bind(on_release=lambda *_: setattr(self, "expanded", not self.expanded))
        self.add_widget(self.header_button)
        self.description_label = _label(
            description,
            color_name="text_muted",
            font_size=11,
            size_hint_y=None,
            height=dp(22 if description else 0),
            halign="left",
        )
        _bind_wrapped(self.description_label)
        self.add_widget(self.description_label)
        self.content = BoxLayout(orientation="vertical", size_hint_y=None, spacing=0)
        self.content.bind(minimum_height=self.content.setter("height"))
        self.add_widget(self.content)
        self.title_text = str(title or "")
        self.bind(expanded=self._sync_expanded)
        self._sync_expanded()

    def _sync_expanded(self, *_args) -> None:
        self.header_button.text = ("v  " if self.expanded else ">  ") + self.title_text
        self.content.opacity = 1.0 if self.expanded else 0.0
        self.content.disabled = not self.expanded
        self.content.height = self.content.minimum_height if self.expanded else 0

    def add_row(self, row: Widget) -> None:
        self.content.add_widget(row)
        self._sync_expanded()


class DangerZone(_BackgroundBox):
    def __init__(self, *, title: str, description: str, **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("spacing", dp(8))
        kwargs.setdefault("padding", dp(14))
        kwargs.setdefault("radius", dp(6))
        kwargs.setdefault("background_color", secondary_color("surface"))
        kwargs.setdefault("border_color", secondary_color("border"))
        kwargs.setdefault("border_width", 1)
        super().__init__(**kwargs)
        self.bind(minimum_height=self.setter("height"))
        title_label = _label(title, color_name="danger", font_size=14, bold=True, size_hint_y=None, height=dp(26), halign="left")
        _bind_wrapped(title_label)
        self.add_widget(title_label)
        description_label = _label(description, color_name="text_muted", font_size=11, size_hint_y=None, height=dp(34), halign="left", valign="top")
        _bind_wrapped(description_label)
        self.add_widget(description_label)
        self.actions = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(36), spacing=dp(8))
        self.add_widget(self.actions)

    def add_action(self, label: str, callback: Callable, *, enabled: bool = True) -> RoundedButton:
        button = secondary_button(label, variant="danger", compact=True, on_release=callback)
        button.disabled = not enabled
        self.actions.add_widget(button)
        return button


class EmptyStatePanel(_BackgroundBox):
    def __init__(self, *, title: str, description: str, action_text: str = "", on_action: Optional[Callable] = None, **kwargs):
        kwargs.setdefault("orientation", "vertical")
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(172))
        kwargs.setdefault("padding", dp(24))
        kwargs.setdefault("spacing", dp(8))
        kwargs.setdefault("radius", dp(6))
        kwargs.setdefault("background_color", secondary_color("surface_muted"))
        super().__init__(**kwargs)
        self.add_widget(_label(title, font_size=16, bold=True, size_hint_y=None, height=dp(28), halign="center"))
        detail = _label(description, color_name="text_muted", font_size=12, size_hint_y=None, height=dp(44), halign="center", valign="top")
        _bind_wrapped(detail)
        self.add_widget(detail)
        if action_text and on_action is not None:
            row = BoxLayout(size_hint_y=None, height=dp(38), padding=(dp(120), 0, dp(120), 0))
            row.add_widget(secondary_button(action_text, variant="primary", compact=True, on_release=on_action))
            self.add_widget(row)


class ConfirmationDialog:
    """Consistent confirmation shell; destructive callbacks only run on confirm."""

    def __init__(
        self,
        *,
        lang: str,
        title: str,
        message: str,
        on_confirm: Callable[[], None],
        confirm_text: str = "",
        danger: bool = True,
    ) -> None:
        self.lang = lang
        self.on_confirm = on_confirm
        content = _BackgroundBox(
            orientation="vertical",
            spacing=dp(14),
            padding=dp(20),
            background_color=secondary_color("surface"),
        )
        title_label = _label(title, font_size=17, bold=True, size_hint_y=None, height=dp(30), halign="left")
        _bind_wrapped(title_label)
        content.add_widget(title_label)
        body = _label(message, color_name="text_secondary", font_size=12, size_hint_y=1, halign="left", valign="top")
        _bind_wrapped(body)
        content.add_widget(body)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(10))
        buttons.add_widget(Widget(size_hint_x=1))
        cancel = secondary_button(tr(lang, "cancel"), width=96, on_release=lambda *_: self.popup.dismiss())
        confirm = secondary_button(
            confirm_text or tr(lang, "confirm_danger"),
            variant="danger" if danger else "primary",
            width=128,
            on_release=self._confirm,
        )
        buttons.add_widget(cancel)
        buttons.add_widget(confirm)
        content.add_widget(buttons)
        self.popup = SecondaryPopup(
            title="",
            content=content,
            size_hint=(None, None),
            size=(dp(520), dp(280)),
            auto_dismiss=False,
            separator_height=0,
            background="",
            background_color=secondary_color("background"),
        )

    def _confirm(self, *_args) -> None:
        self.popup.dismiss()
        self.on_confirm()

    def open(self) -> None:
        self.popup.open()


class ErrorStateDialog:
    """User-facing error summary with optional next steps and folded details."""

    def __init__(
        self,
        *,
        lang: str,
        title: str,
        reason: str,
        suggestion: str,
        technical_details: str = "",
        on_retry: Optional[Callable] = None,
        on_settings: Optional[Callable] = None,
        on_export: Optional[Callable] = None,
    ) -> None:
        self.lang = "en" if str(lang).lower().startswith("en") else "zh"
        content = _BackgroundBox(
            orientation="vertical",
            spacing=dp(12),
            padding=dp(20),
            background_color=secondary_color("surface"),
        )
        heading = _label(title, font_size=17, bold=True, size_hint_y=None, height=dp(30), halign="left")
        _bind_wrapped(heading)
        content.add_widget(heading)
        reason_label = _label(reason, color_name="text_secondary", font_size=13, size_hint_y=None, height=dp(48), halign="left", valign="top")
        _bind_wrapped(reason_label)
        content.add_widget(reason_label)
        suggestion_label = _label(suggestion, color_name="text_muted", font_size=12, size_hint_y=None, height=dp(44), halign="left", valign="top")
        _bind_wrapped(suggestion_label)
        content.add_widget(suggestion_label)
        if str(technical_details or "").strip():
            disclosure = AdvancedDisclosure(
                title=tr(self.lang, "technical_details"),
                description=("用于排查问题，不需要在正常操作中查看。" if self.lang == "zh" else "For troubleshooting; not needed during normal use."),
            )
            disclosure.add_row(
                ReadOnlyInfoRow(
                    setting_key="error_details",
                    label=tr(self.lang, "technical_details"),
                    value=str(technical_details),
                    height=dp(78),
                )
            )
            content.add_widget(disclosure)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(8))
        buttons.add_widget(Widget(size_hint_x=1))
        for label, callback, variant in (
            (tr(self.lang, "try_again"), on_retry, "primary"),
            (tr(self.lang, "open_settings"), on_settings, "secondary"),
            (tr(self.lang, "export_diagnostics"), on_export, "secondary"),
        ):
            if callable(callback):
                buttons.add_widget(
                    secondary_button(
                        label,
                        variant=variant,
                        width=max(128, min(168, 28 + len(label) * 7)),
                        on_release=lambda *_args, cb=callback: self._invoke(cb),
                    )
                )
        buttons.add_widget(secondary_button(tr(self.lang, "close"), width=92, on_release=lambda *_: self.dismiss()))
        content.add_widget(buttons)
        self.popup = SecondaryPopup(
            title="",
            content=content,
            size_hint=(0.62, 0.58),
            auto_dismiss=False,
            separator_height=0,
            background="",
            background_color=secondary_color("background"),
        )

    def _invoke(self, callback: Callable) -> None:
        self.dismiss()
        callback()

    def open(self) -> None:
        self.popup.open()

    def dismiss(self) -> None:
        self.popup.dismiss()


class ToastMessage(Label):
    def __init__(self, text: str, *, kind: str = "neutral", **kwargs):
        color_name = "success" if kind == "success" else "danger" if kind == "danger" else "text_primary"
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("font_size", sp(12))
        kwargs.setdefault("color", secondary_color(color_name))
        kwargs.setdefault("size_hint", (None, None))
        kwargs.setdefault("height", dp(38))
        kwargs.setdefault("width", dp(300))
        kwargs.setdefault("halign", "center")
        kwargs.setdefault("valign", "middle")
        super().__init__(text=str(text or ""), **kwargs)
        self.text_size = self.size
        with self.canvas.before:
            self._toast_bg_color = Color(*secondary_color("surface_muted", 0.98))
            self._toast_bg = RoundedRectangle(pos=self.pos, size=self.size, radius=[dp(5)])
        self.bind(pos=self._sync_toast, size=self._sync_toast)

    def _sync_toast(self, *_args) -> None:
        self._toast_bg.pos = self.pos
        self._toast_bg.size = self.size
        self.text_size = self.size

    def show_in(self, parent: Widget, *, seconds: float = 2.2) -> None:
        if not hasattr(parent, "add_widget"):
            return
        parent.add_widget(self)
        Clock.schedule_once(lambda _dt: self.parent and self.parent.remove_widget(self), seconds)
