#!/usr/bin/env python3
"""Deterministic coverage for the v0.0.12 settings information architecture."""

from __future__ import annotations

import unittest
from pathlib import Path

from screen_runtime import DEFAULT_NATIVE_SCREEN_PRESET, NATIVE_SCREEN_PRESETS
from ui_copy import COPY, has_bilingual_copy
from ui_settings_schema import (
    DANGER_ACTION_IDS,
    SECTION_GROUPS,
    SETTING_BY_KEY,
    SETTING_DEFINITIONS,
    SETTINGS_SECTIONS,
    SettingsModel,
    danger_action_requires_confirmation,
    definitions_contain_forbidden_media_terms,
    merge_legacy_config,
    ordered_sections,
    schema_errors,
)


class SettingsSchemaTests(unittest.TestCase):
    def test_settings_schema_has_unique_keys(self) -> None:
        keys = [item.key for item in SETTING_DEFINITIONS]
        self.assertEqual(len(keys), len(set(keys)))
        self.assertEqual(schema_errors(), [])

    def test_settings_schema_has_bilingual_labels(self) -> None:
        for section in SETTINGS_SECTIONS:
            self.assertTrue(section.label_zh.strip(), section.key)
            self.assertTrue(section.label_en.strip(), section.key)
            self.assertTrue(section.description_zh.strip(), section.key)
            self.assertTrue(section.description_en.strip(), section.key)
        for definition in SETTING_DEFINITIONS:
            self.assertTrue(definition.label_zh.strip(), definition.key)
            self.assertTrue(definition.label_en.strip(), definition.key)
            self.assertTrue(definition.description_zh.strip(), definition.key)
            self.assertTrue(definition.description_en.strip(), definition.key)

    def test_settings_schema_advanced_fields_are_grouped(self) -> None:
        visible_group_keys = {
            key
            for groups in SECTION_GROUPS.values()
            for _group_name, keys in groups
            for key in keys
        }
        advanced_keys = {item.key for item in SETTING_DEFINITIONS if item.advanced}
        basic_keys = {item.key for item in SETTING_DEFINITIONS if not item.advanced}
        self.assertFalse(advanced_keys & visible_group_keys)
        self.assertEqual(basic_keys, visible_group_keys)

    def test_settings_schema_defaults_match_runtime(self) -> None:
        self.assertEqual(
            SETTING_BY_KEY["screen_native_preset"].default,
            DEFAULT_NATIVE_SCREEN_PRESET,
        )
        self.assertIn(DEFAULT_NATIVE_SCREEN_PRESET, NATIVE_SCREEN_PRESETS)
        self.assertEqual(SETTING_BY_KEY["receiver_port"].default, 9999)
        self.assertEqual(SETTING_BY_KEY["discovery_port"].default, 9998)
        self.assertEqual(SETTING_BY_KEY["transfer_payload_size"].default, 1400)
        self.assertEqual(SETTING_BY_KEY["transfer_request_timeout_sec"].default, 300)
        self.assertEqual(SETTING_BY_KEY["transfer_completion_timeout_sec"].default, 180)

    def test_settings_schema_numeric_ranges_valid(self) -> None:
        for definition in SETTING_DEFINITIONS:
            if definition.control_type != "number":
                continue
            self.assertIsNotNone(definition.minimum, definition.key)
            self.assertIsNotNone(definition.maximum, definition.key)
            self.assertLessEqual(definition.minimum, definition.default, definition.key)
            self.assertLessEqual(definition.default, definition.maximum, definition.key)

    def test_settings_navigation_order_stable(self) -> None:
        self.assertEqual(
            [item.key for item in ordered_sections()],
            ["general", "network", "transfer", "screen", "privacy", "storage", "about"],
        )

    def test_legacy_config_renders_without_error(self) -> None:
        migrated = merge_legacy_config(
            {
                "screen_share_audio_mode": "system",
                "screen_preset": "recommended",
                "save_dir": "C:/Received",
                "payload_size": "1392",
                "request_timeout": "240",
                "complete_timeout": "120",
                "removed_backend": "ffmpeg",
            }
        )
        self.assertTrue(migrated["screen_share_system_audio"])
        self.assertEqual(migrated["screen_native_preset"], "recommended")
        self.assertEqual(migrated["save_directory"], "C:/Received")
        self.assertEqual(migrated["transfer_payload_size"], 1392)
        self.assertNotIn("removed_backend", migrated)

    def test_settings_save_round_trip(self) -> None:
        values = {
            "language": "en",
            "theme_mode": "dark",
            "device_display_name": "Office PC",
            "receiver_port": 12000,
            "discovery_port": 12001,
            "save_directory": "C:/Received",
            "file_conflict_policy": "rename",
            "screen_native_preset": "recommended",
        }
        serialized = SettingsModel(values).serializable_values()
        reloaded = SettingsModel(serialized)
        for key, expected in values.items():
            self.assertEqual(reloaded.values[key], expected, key)

    def test_settings_reset_section(self) -> None:
        model = SettingsModel({"receiver_port": 12000, "discovery_port": 12001, "language": "en"})
        model.reset_section("network")
        self.assertEqual(model.values["receiver_port"], 9999)
        self.assertEqual(model.values["discovery_port"], 9998)
        self.assertEqual(model.values["language"], "en")

    def test_settings_reset_all(self) -> None:
        model = SettingsModel({"language": "en", "receiver_port": 12000, "screen_native_preset": "high_quality"})
        model.reset_all()
        self.assertEqual(model.values["language"], "zh")
        self.assertEqual(model.values["receiver_port"], 9999)
        self.assertEqual(model.values["screen_native_preset"], DEFAULT_NATIVE_SCREEN_PRESET)

    def test_settings_invalid_value_shows_inline_error(self) -> None:
        model = SettingsModel({"receiver_port": 70000})
        self.assertEqual(model.validate().get("receiver_port"), "range")
        self.assertEqual(model.errors.get("receiver_port"), "range")

    def test_settings_hidden_fields_not_saved(self) -> None:
        model = SettingsModel(
            {"about_full_technical_info": "secret path"},
            context={"technical_details_visible": False},
        )
        serialized = model.serializable_values()
        self.assertNotIn("about_full_technical_info", serialized)
        self.assertNotIn("chat_data_location", serialized)

    def test_native_only_screen_settings_have_no_backend_selector(self) -> None:
        screen_keys = {item.key for item in SETTING_DEFINITIONS if item.section == "screen"}
        self.assertNotIn("screen_backend", screen_keys)
        self.assertFalse(any("backend" in key for key in screen_keys))

    def test_ffmpeg_terms_not_present_in_current_ui(self) -> None:
        self.assertFalse(definitions_contain_forbidden_media_terms())
        root = Path(__file__).resolve().parents[1]
        ui_sources = [
            path
            for path in root.glob("ui_*.py")
            if path.name not in {"ui_preview.py", "ui_settings_schema.py"}
        ]
        combined = "\n".join(path.read_text(encoding="utf-8").lower() for path in ui_sources)
        for term in ("ffmpeg", "ffplay", "ffprobe"):
            self.assertNotIn(term, combined)

    def test_danger_actions_require_confirmation(self) -> None:
        for action_id in DANGER_ACTION_IDS:
            self.assertTrue(danger_action_requires_confirmation(action_id), action_id)
        self.assertFalse(danger_action_requires_confirmation("save_settings"))

    def test_secondary_page_titles_are_bilingual(self) -> None:
        for key in (
            "settings",
            "diagnostics",
            "contact_details",
            "group_management",
            "file_details",
            "screen_details",
        ):
            self.assertTrue(has_bilingual_copy(key), key)
            self.assertNotEqual(COPY["zh"][key], COPY["en"][key], key)


if __name__ == "__main__":
    unittest.main()
