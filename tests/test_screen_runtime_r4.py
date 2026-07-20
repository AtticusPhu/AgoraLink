from __future__ import annotations

import hashlib
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from screen_runtime import (
    DEFAULT_NATIVE_SCREEN_PRESET,
    NATIVE_SCREEN_PRESETS,
    ScreenRuntime,
    native_media_file_identity,
    native_screen_preset_info,
    resolve_native_screen_preset_id,
)


def option_value(command: list[str], option: str) -> str:
    index = command.index(option)
    return command[index + 1]


class R4NativeScreenPresetTests(unittest.TestCase):
    def setUp(self) -> None:
        self.runtime = ScreenRuntime(script_dir=Path(__file__).resolve().parents[1])
        self.native_exe = r"C:\AgoraLink\agoralink_media.exe"

    def test_default_preset_is_complete_r4_tuple(self) -> None:
        self.assertEqual(DEFAULT_NATIVE_SCREEN_PRESET, "r4_default")
        preset = native_screen_preset_info()
        self.assertEqual(
            {
                "width": preset["width"],
                "height": preset["height"],
                "fps": preset["fps"],
                "bitrate_mbps": preset["bitrate_mbps"],
                "playout_delay_ms": preset["playout_delay_ms"],
                "repair": preset["repair"],
                "adaptive_quality": preset["adaptive_quality"],
                "encoder": preset["encoder"],
                "convert_backend": preset["convert_backend"],
                "render_backend": preset["render_backend"],
            },
            {
                "width": 1920,
                "height": 1080,
                "fps": 60,
                "bitrate_mbps": 22,
                "playout_delay_ms": 250,
                "repair": "nack",
                "adaptive_quality": "off",
                "encoder": "auto",
                "convert_backend": "auto",
                "render_backend": "d3d11",
            },
        )

    def test_existing_valid_preset_ids_remain_selected(self) -> None:
        for preset_id in ("stable", "recommended", "high_quality"):
            resolved, invalid = resolve_native_screen_preset_id(preset_id)
            self.assertEqual(resolved, preset_id)
            self.assertFalse(invalid)
            self.assertEqual(native_screen_preset_info(preset_id)["id"], preset_id)

    def test_invalid_preset_falls_back_and_warns_once(self) -> None:
        invalid_id = "removed_r4_test_preset"
        with self.assertLogs("screen_runtime", level="WARNING") as captured:
            first = resolve_native_screen_preset_id(invalid_id)
            second = resolve_native_screen_preset_id(invalid_id)
        self.assertEqual(first, ("r4_default", True))
        self.assertEqual(second, ("r4_default", True))
        self.assertEqual(len(captured.records), 1)

    def test_r4_sender_command_contains_explicit_product_policy(self) -> None:
        command = self.runtime._build_native_sender_command(
            host="192.0.2.10",
            port=55134,
            native_exe=self.native_exe,
            native_preset="r4_default",
        )
        self.assertEqual(option_value(command, "--width"), "1920")
        self.assertEqual(option_value(command, "--height"), "1080")
        self.assertEqual(option_value(command, "--fps"), "60")
        self.assertEqual(option_value(command, "--bitrate-mbps"), "22")
        self.assertEqual(option_value(command, "--repair"), "nack")
        self.assertEqual(option_value(command, "--adaptive-quality"), "off")
        self.assertEqual(option_value(command, "--encoder"), "auto")
        self.assertEqual(option_value(command, "--convert-backend"), "auto")

    def test_r4_receiver_command_contains_playout_and_repair_policy(self) -> None:
        command = self.runtime._build_native_receiver_command(
            55134,
            native_exe=self.native_exe,
            native_preset="r4_default",
        )
        self.assertEqual(option_value(command, "--playout-delay-ms"), "250")
        self.assertEqual(option_value(command, "--repair"), "nack")
        self.assertEqual(option_value(command, "--render-backend"), "d3d11")

    def test_legacy_presets_are_not_rewritten_to_r4_values(self) -> None:
        self.assertEqual(NATIVE_SCREEN_PRESETS["stable"]["bitrate_mbps"], 20)
        self.assertEqual(NATIVE_SCREEN_PRESETS["recommended"]["bitrate_mbps"], 50)
        self.assertEqual(NATIVE_SCREEN_PRESETS["high_quality"]["bitrate_mbps"], 80)

    def test_portable_resolution_prefers_bundled_internal_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            bundled = root / "_internal" / "tools" / "agoralink_media" / "agoralink_media.exe"
            bundled.parent.mkdir(parents=True)
            bundled.write_bytes(b"bundled-r4")
            source = root / "source" / "rust-native" / "agoralink_media" / "target" / "release" / "agoralink_media.exe"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"source-r4")
            runtime = ScreenRuntime(script_dir=root / "source")
            with mock.patch.object(sys, "executable", str(root / "AgoraLink.exe")), mock.patch.object(
                sys, "frozen", True, create=True
            ), mock.patch.object(sys, "_MEIPASS", str(root / "_internal"), create=True):
                self.assertEqual(Path(runtime._find_native_media_exe()), bundled.resolve())

    def test_source_resolution_prefers_release_over_tools_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            release = root / "rust-native" / "agoralink_media" / "target" / "release" / "agoralink_media.exe"
            fallback = root / "tools" / "agoralink_media" / "agoralink_media.exe"
            release.parent.mkdir(parents=True)
            fallback.parent.mkdir(parents=True)
            release.write_bytes(b"release-r4")
            fallback.write_bytes(b"stale-fallback")
            runtime = ScreenRuntime(script_dir=root)
            with mock.patch.object(sys, "frozen", False, create=True):
                self.assertEqual(Path(runtime._find_native_media_exe()), release.resolve())

    def test_native_binary_identity_verifies_hash(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            native = Path(raw_root) / "agoralink_media.exe"
            payload = b"deterministic-r4-native"
            native.write_bytes(payload)
            expected = hashlib.sha256(payload).hexdigest().upper()
            identity = native_media_file_identity(native, expected_sha256=expected)
            self.assertTrue(identity["exists"])
            self.assertTrue(identity["hash_matches"])
            self.assertEqual(identity["sha256"], expected)

    def test_missing_native_binary_has_product_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            runtime = ScreenRuntime(script_dir=Path(raw_root), tool_finder=lambda _name: "")
            with mock.patch.object(sys, "frozen", False, create=True):
                with self.assertRaisesRegex(FileNotFoundError, "Rust native media executable not found"):
                    runtime._build_native_sender_command(host="192.0.2.10", port=55134)


if __name__ == "__main__":
    unittest.main()
