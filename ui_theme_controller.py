#!/usr/bin/env python3
"""Observable, process-wide Light/Dark theme state for AgoraLink."""

from __future__ import annotations

import threading
import weakref
from enum import Enum
from typing import Callable, MutableMapping, Optional

from kivy.clock import Clock
from kivy.core.window import Window
from kivy.event import EventDispatcher
from kivy.properties import NumericProperty, OptionProperty

from ui_theme import PRODUCT_DARK_THEME, PRODUCT_LIGHT_THEME, Theme


class ThemeMode(str, Enum):
    LIGHT = "light"
    DARK = "dark"


THEME_SCHEMA_VERSION = 2
_MODE_ALIASES = {
    "light": ThemeMode.LIGHT.value,
    "浅色": ThemeMode.LIGHT.value,
    "dark": ThemeMode.DARK.value,
    "深色": ThemeMode.DARK.value,
}


def normalize_theme_mode(value: object) -> str:
    return _MODE_ALIASES.get(str(value or "").strip().lower(), ThemeMode.LIGHT.value)


class ThemeController(EventDispatcher):
    mode = OptionProperty(ThemeMode.LIGHT.value, options=(ThemeMode.LIGHT.value, ThemeMode.DARK.value))
    revision = NumericProperty(0)

    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        self._widgets: weakref.WeakSet = weakref.WeakSet()
        self._config: Optional[MutableMapping[str, object]] = None
        self._persist_callback: Optional[Callable[[MutableMapping[str, object]], None]] = None
        self._ui_thread_id = threading.main_thread().ident
        self._last_apply_errors: list[str] = []

    def configure(
        self,
        config: MutableMapping[str, object],
        *,
        persist_callback: Optional[Callable[[MutableMapping[str, object]], None]] = None,
        initial_mode: object = None,
    ) -> str:
        self._config = config
        self._persist_callback = persist_callback
        source = config.get("theme_mode") if initial_mode is None else initial_mode
        normalized = normalize_theme_mode(source)
        self.set_mode(normalized, persist=False)
        return normalized

    def current_theme(self) -> Theme:
        return PRODUCT_DARK_THEME if self.mode == ThemeMode.DARK.value else PRODUCT_LIGHT_THEME

    def color(self, token: str):
        colors = self.current_theme().colors
        return colors.get(str(token), colors["text_primary"])

    def set_mode(self, mode: object, *, persist: bool = True) -> str:
        normalized = normalize_theme_mode(mode)
        if threading.get_ident() != self._ui_thread_id:
            Clock.schedule_once(lambda _dt: self.set_mode(normalized, persist=persist), 0)
            return normalized
        changed = normalized != self.mode
        if changed:
            self.mode = normalized
            self.revision += 1
        self._apply_window_theme()
        if changed:
            self._notify_widgets()
        if persist:
            self._persist_mode(normalized)
        return normalized

    def toggle(self, *, persist: bool = True) -> str:
        target = ThemeMode.DARK.value if self.mode == ThemeMode.LIGHT.value else ThemeMode.LIGHT.value
        return self.set_mode(target, persist=persist)

    def register(self, widget) -> None:
        if widget is None:
            return
        self._widgets.add(widget)
        self._apply_widget(widget)

    def unregister(self, widget) -> None:
        if widget is not None:
            self._widgets.discard(widget)

    def registered_count(self) -> int:
        return len(self._widgets)

    def _apply_window_theme(self) -> None:
        try:
            Window.clearcolor = self.color("window_bg")
        except Exception as exc:
            self._last_apply_errors.append(f"Window: {exc}")

    def _notify_widgets(self) -> None:
        self._last_apply_errors.clear()
        for widget in tuple(self._widgets):
            self._apply_widget(widget)

    def _apply_widget(self, widget) -> None:
        try:
            callback = getattr(widget, "apply_theme", None)
            if callable(callback):
                callback(self.current_theme())
        except Exception as exc:
            self._last_apply_errors.append(f"{type(widget).__name__}: {exc}")

    def _persist_mode(self, mode: str) -> None:
        if self._config is None:
            return
        self._config["theme_mode"] = mode
        self._config["theme_schema_version"] = THEME_SCHEMA_VERSION
        if self._persist_callback is not None:
            self._persist_callback(self._config)


class ThemableMixin:
    """Small lifecycle adapter for widgets that implement ``apply_theme``."""

    _theme_controller_attached = False

    def attach_theme_controller(self, controller: ThemeController | None = None) -> None:
        active = controller or theme_controller
        if getattr(self, "_theme_controller_attached", False):
            return
        self._theme_controller_attached = True
        self._attached_theme_controller = active
        active.register(self)

    def detach_theme_controller(self) -> None:
        active = getattr(self, "_attached_theme_controller", None)
        if active is not None:
            active.unregister(self)
        self._theme_controller_attached = False
        self._attached_theme_controller = None

    def on_parent(self, _instance, parent) -> None:
        if parent is None:
            self.detach_theme_controller()
        else:
            self.attach_theme_controller()


theme_controller = ThemeController()


def theme_color(name: str, alpha: float | None = None):
    value = list(theme_controller.color(name))
    if alpha is not None:
        value[3] = float(alpha)
    return value
