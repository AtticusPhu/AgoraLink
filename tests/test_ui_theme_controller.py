#!/usr/bin/env python3
"""Deterministic tests for the observable product theme architecture."""

from __future__ import annotations

import gc
import math
import os
import sys
import threading
import unittest
import weakref
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from ui_theme import PRODUCT_DARK_THEME, PRODUCT_LIGHT_THEME, REQUIRED_SEMANTIC_TOKENS
from ui_theme_controller import ThemeController, normalize_theme_mode, theme_controller

RUN_KIVY_WIDGET_TESTS = os.environ.get("CI", "").strip().lower() not in {"1", "true", "yes"}
if os.environ.get("AGORALINK_RUN_KIVY_WIDGET_TESTS", "").strip() == "1":
    RUN_KIVY_WIDGET_TESTS = True

if RUN_KIVY_WIDGET_TESTS:
    from ui_components import FileTransferCard, MessageBubble, RoundedButton, ScreenShareCard
    from ui_device_details import ContactDetailsPage
    from ui_form_components import ConfirmationDialog, ThemedTextInput, ThemeSegmentedControl
    from ui_settings import SettingsCenter


class DummyWidget:
    def __init__(self) -> None:
        self.applied = 0
        self.theme_name = ""

    def apply_theme(self, theme) -> None:
        self.applied += 1
        self.theme_name = theme.name


def _linear(value: float) -> float:
    return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4


def _luminance(color) -> float:
    red, green, blue = (_linear(float(item)) for item in color[:3])
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue


def _contrast(first, second) -> float:
    high, low = sorted((_luminance(first), _luminance(second)), reverse=True)
    return (high + 0.05) / (low + 0.05)


class ThemeControllerTests(unittest.TestCase):
    def test_theme_defaults_to_light_when_missing(self) -> None:
        controller = ThemeController()
        self.assertEqual(controller.configure({}), "light")

    def test_theme_honors_existing_dark_value(self) -> None:
        controller = ThemeController()
        self.assertEqual(controller.configure({"theme_mode": "dark"}), "dark")

    def test_theme_normalizes_legacy_chinese_values(self) -> None:
        self.assertEqual(normalize_theme_mode("浅色"), "light")
        self.assertEqual(normalize_theme_mode("深色"), "dark")

    def test_theme_rejects_unknown_value(self) -> None:
        self.assertEqual(normalize_theme_mode("system"), "light")
        self.assertEqual(normalize_theme_mode("unknown"), "light")

    def test_theme_persists_mode_only(self) -> None:
        config = {"language": "zh", "receiver_port": 9999}
        saved = []
        controller = ThemeController()
        controller.configure(config, persist_callback=lambda value: saved.append(dict(value)))
        controller.set_mode("dark")
        self.assertEqual(config["theme_mode"], "dark")
        self.assertEqual(config["theme_schema_version"], 2)
        self.assertEqual(config["language"], "zh")
        self.assertEqual(config["receiver_port"], 9999)
        self.assertEqual(len(saved), 1)

    def test_theme_does_not_discard_other_config(self) -> None:
        config = {"language": "en", "custom": {"kept": True}}
        controller = ThemeController()
        controller.configure(config)
        controller.set_mode("dark")
        self.assertEqual(config["custom"], {"kept": True})

    def test_theme_toggle_light_to_dark(self) -> None:
        controller = ThemeController()
        controller.configure({})
        self.assertEqual(controller.toggle(persist=False), "dark")

    def test_theme_toggle_dark_to_light(self) -> None:
        controller = ThemeController()
        controller.configure({"theme_mode": "dark"})
        self.assertEqual(controller.toggle(persist=False), "light")

    def test_theme_revision_increments_once(self) -> None:
        controller = ThemeController()
        controller.configure({})
        before = controller.revision
        controller.set_mode("dark", persist=False)
        controller.set_mode("dark", persist=False)
        self.assertEqual(controller.revision, before + 1)

    def test_theme_switch_runs_on_kivy_thread(self) -> None:
        controller = ThemeController()
        controller.configure({})
        with patch("ui_theme_controller.Clock.schedule_once") as schedule:
            worker = threading.Thread(target=lambda: controller.set_mode("dark", persist=False))
            worker.start()
            worker.join(timeout=2)
        schedule.assert_called_once()
        self.assertEqual(controller.mode, "light")

    def test_theme_registry_uses_weak_refs(self) -> None:
        controller = ThemeController()
        widget = DummyWidget()
        controller.register(widget)
        reference = weakref.ref(widget)
        del widget
        gc.collect()
        self.assertIsNone(reference())
        self.assertEqual(controller.registered_count(), 0)

    def test_destroyed_widget_is_not_restyled(self) -> None:
        controller = ThemeController()
        widget = DummyWidget()
        controller.register(widget)
        reference = weakref.ref(widget)
        del widget
        gc.collect()
        controller.set_mode("dark", persist=False)
        self.assertIsNone(reference())

    def test_duplicate_registration_is_idempotent(self) -> None:
        controller = ThemeController()
        widget = DummyWidget()
        controller.register(widget)
        applied = widget.applied
        controller.register(widget)
        self.assertEqual(controller.registered_count(), 1)
        self.assertEqual(widget.applied, applied + 1)


class ThemeTokenTests(unittest.TestCase):
    def test_light_and_dark_have_same_semantic_tokens(self) -> None:
        self.assertEqual(set(PRODUCT_LIGHT_THEME.colors), set(PRODUCT_DARK_THEME.colors))

    def test_all_required_tokens_exist(self) -> None:
        for theme in (PRODUCT_LIGHT_THEME, PRODUCT_DARK_THEME):
            self.assertTrue(REQUIRED_SEMANTIC_TOKENS <= set(theme.colors))

    def test_theme_colors_are_valid_rgba(self) -> None:
        for theme in (PRODUCT_LIGHT_THEME, PRODUCT_DARK_THEME):
            for token, rgba in theme.colors.items():
                self.assertEqual(len(rgba), 4, token)
                self.assertTrue(all(math.isfinite(float(item)) and 0 <= float(item) <= 1 for item in rgba), token)

    def test_normal_text_contrast_meets_target(self) -> None:
        for theme in (PRODUCT_LIGHT_THEME, PRODUCT_DARK_THEME):
            self.assertGreaterEqual(_contrast(theme.colors["text_primary"], theme.colors["surface"]), 7.0)

    def test_muted_text_contrast_meets_target(self) -> None:
        for theme in (PRODUCT_LIGHT_THEME, PRODUCT_DARK_THEME):
            self.assertGreaterEqual(_contrast(theme.colors["text_muted"], theme.colors["surface"]), 4.5)

    def test_danger_success_warning_are_distinguishable(self) -> None:
        for theme in (PRODUCT_LIGHT_THEME, PRODUCT_DARK_THEME):
            values = {theme.colors[name] for name in ("danger", "success", "warning")}
            self.assertEqual(len(values), 3)

    def test_no_product_import_of_secondary_dark_theme(self) -> None:
        for path in ROOT.glob("*.py"):
            if path.name == "ui_theme.py":
                continue
            self.assertNotIn("SECONDARY_DARK_THEME", path.read_text(encoding="utf-8"), path.name)

    def test_no_import_time_palette_capture(self) -> None:
        for path in ROOT.glob("ui_*.py"):
            self.assertNotIn("PALETTE =", path.read_text(encoding="utf-8"), path.name)

    def test_no_dark_theme_placeholder_in_runtime_path(self) -> None:
        for path in ROOT.glob("*.py"):
            self.assertNotIn("DARK_THEME_PLACEHOLDER", path.read_text(encoding="utf-8"), path.name)


@unittest.skipUnless(RUN_KIVY_WIDGET_TESTS, "requires an interactive Kivy window provider")
class ExistingWidgetThemeTests(unittest.TestCase):
    def setUp(self) -> None:
        theme_controller.set_mode("light", persist=False)

    def tearDown(self) -> None:
        theme_controller.set_mode("light", persist=False)

    def _assert_changes(self, widget, attribute: str) -> None:
        before = tuple(getattr(widget, attribute))
        theme_controller.set_mode("dark", persist=False)
        after = tuple(getattr(widget, attribute))
        self.assertNotEqual(before, after)

    def test_existing_primary_button_updates(self) -> None:
        self._assert_changes(RoundedButton(text="Button"), "bg_normal")

    def test_existing_message_bubble_updates(self) -> None:
        self._assert_changes(MessageBubble(message="Hello"), "bg_color")

    def test_existing_file_card_updates(self) -> None:
        self._assert_changes(FileTransferCard(filename="file.txt"), "bg_color")

    def test_existing_screen_card_updates(self) -> None:
        self._assert_changes(ScreenShareCard(peer="Remote"), "bg_color")

    def test_existing_text_input_updates(self) -> None:
        self._assert_changes(ThemedTextInput(text="value"), "background_color")

    def test_new_widget_uses_current_theme(self) -> None:
        theme_controller.set_mode("dark", persist=False)
        button = RoundedButton(text="New")
        self.assertEqual(tuple(button.bg_normal), tuple(PRODUCT_DARK_THEME.colors["surface_muted"]))

    def test_toolbar_and_settings_selector_sync(self) -> None:
        from main_kivy import RUDPTransferRoot

        selector = ThemeSegmentedControl(value="light")
        toolbar_button = SimpleNamespace(text="", disabled=False)
        root = SimpleNamespace(lang="en", theme_btn=toolbar_button, theme_mode="light")
        RUDPTransferRoot._refresh_theme_toggle_text(root)
        self.assertEqual(toolbar_button.text, "Dark")

        scheduled = []
        with patch("main_kivy.Clock.schedule_once", side_effect=lambda callback, _delay: scheduled.append(callback)):
            RUDPTransferRoot._toggle_theme_from_toolbar(root)

        self.assertTrue(toolbar_button.disabled)
        self.assertEqual(theme_controller.mode, "dark")
        self.assertEqual(selector.value, "dark")
        RUDPTransferRoot._refresh_theme_toggle_text(root)
        self.assertEqual(toolbar_button.text, "Light")
        scheduled[0](0)
        self.assertFalse(toolbar_button.disabled)

        selector._select("light")
        self.assertEqual(theme_controller.mode, "light")

    def test_existing_settings_page_updates_and_preserves_state(self) -> None:
        page = SettingsCenter(
            lang="en",
            initial_values={"theme_mode": "light", "device_display_name": "Office PC"},
            context={},
            on_theme_change=lambda mode: theme_controller.set_mode(mode, persist=False),
        )
        name_row = page.rows["device_display_name"]
        name_row.input.text = "Unsaved name"
        selector = page.rows["theme_mode"].selector
        selector._select("dark")
        self.assertEqual(page.current_section, "general")
        self.assertEqual(name_row.input.text, "Unsaved name")
        self.assertEqual(selector.value, "dark")

    def test_settings_cancel_does_not_revert_theme(self) -> None:
        closed = []
        page = SettingsCenter(
            lang="en",
            initial_values={"theme_mode": "light"},
            context={},
            on_theme_change=lambda mode: theme_controller.set_mode(mode, persist=False),
            on_close=lambda: closed.append(True),
        )
        page.rows["theme_mode"].selector._select("dark")
        page._close()
        self.assertEqual(theme_controller.mode, "dark")
        self.assertEqual(closed, [True])

    def test_existing_confirmation_updates(self) -> None:
        dialog = ConfirmationDialog(
            lang="en",
            title="Confirm",
            message="Continue?",
            on_confirm=lambda: None,
        )
        before = tuple(dialog.popup.background_color)
        theme_controller.set_mode("dark", persist=False)
        self.assertNotEqual(before, tuple(dialog.popup.background_color))

    def test_existing_contact_popup_updates(self) -> None:
        page = ContactDetailsPage(
            lang="en",
            contact={"display_name": "Office PC", "peer_id": "peer-1"},
            on_close=lambda: None,
        )
        before = tuple(page.background_color)
        theme_controller.set_mode("dark", persist=False)
        self.assertNotEqual(before, tuple(page.background_color))

    def test_context_menu_closes_on_theme_switch(self) -> None:
        from main_kivy import RUDPTransferRoot

        resize_callback = Mock()
        overlay = SimpleNamespace(_context_resize_cb=resize_callback)
        root = SimpleNamespace(_chat_context_overlay=overlay)
        with (
            patch("main_kivy.Window.unbind") as unbind,
            patch("main_kivy.Window.remove_widget") as remove_widget,
        ):
            RUDPTransferRoot._close_chat_context_menu(root)
        unbind.assert_called_once_with(on_resize=resize_callback)
        remove_widget.assert_called_once_with(overlay)
        self.assertIsNone(root._chat_context_overlay)

    def test_spinner_dropdown_closes_on_theme_switch(self) -> None:
        from kivy.uix.spinner import Spinner
        from main_kivy import RUDPTransferRoot

        spinner = Spinner(text="Light", values=("Light", "Dark"))
        dropdown = Mock()
        spinner._dropdown = dropdown
        root = SimpleNamespace(walk=lambda: (spinner,))
        RUDPTransferRoot._dismiss_open_spinner_dropdowns(root)
        dropdown.dismiss.assert_called_once_with()

    def test_theme_switch_preserves_language_and_chat_state(self) -> None:
        from main_kivy import RUDPTransferRoot

        root = SimpleNamespace(
            lang="zh",
            theme_mode="light",
            current_peer_id="peer-1",
            current_group_id="group-1",
            chat_input=SimpleNamespace(text="未发送内容"),
            _close_chat_context_menu=Mock(),
            _dismiss_open_spinner_dropdowns=Mock(),
            _refresh_theme_toggle_text=Mock(),
            stop_receiver=Mock(),
            stop_screen_share=Mock(),
        )
        RUDPTransferRoot._on_global_theme_mode(root, None, "dark")
        self.assertEqual(root.lang, "zh")
        self.assertEqual(root.current_peer_id, "peer-1")
        self.assertEqual(root.current_group_id, "group-1")
        self.assertEqual(root.chat_input.text, "未发送内容")
        root.stop_receiver.assert_not_called()
        root.stop_screen_share.assert_not_called()

    def test_settings_selector_applies_immediately(self) -> None:
        selector = ThemeSegmentedControl(value="light")
        selector._select("dark")
        self.assertEqual(theme_controller.mode, "dark")

    def test_settings_selector_uses_integer_widths(self) -> None:
        selector = ThemeSegmentedControl(value="light")
        selector.width = 495
        selector._layout_buttons()
        light_width = selector._buttons["light"].width
        dark_width = selector._buttons["dark"].width
        self.assertEqual(light_width, round(light_width))
        self.assertEqual(dark_width, round(dark_width))
        self.assertEqual(light_width + dark_width + selector.spacing, 495)

    def test_language_switch_preserves_theme(self) -> None:
        config = {"language": "zh", "theme_mode": "dark"}
        controller = ThemeController()
        controller.configure(config)
        config["language"] = "en"
        self.assertEqual(controller.mode, "dark")

    def test_focus_and_disabled_state_survive_theme_switch(self) -> None:
        text_input = ThemedTextInput(text="draft")
        text_input.focus = True
        button = RoundedButton(text="Disabled", disabled=True)
        theme_controller.set_mode("dark", persist=False)
        self.assertTrue(text_input.focus)
        self.assertEqual(text_input.text, "draft")
        self.assertTrue(button.disabled)

    def test_window_clearcolor_updates(self) -> None:
        from kivy.core.window import Window

        theme_controller.set_mode("light", persist=False)
        before = tuple(Window.clearcolor)
        theme_controller.set_mode("dark", persist=False)
        self.assertNotEqual(before, tuple(Window.clearcolor))


if __name__ == "__main__":
    unittest.main()
