from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from config_migration import MIGRATION_WARNING, migrate_legacy_media_config
from screen_runtime import ScreenRuntime


class NativeMediaMigrationTests(unittest.TestCase):
    def test_legacy_backend_and_paths_migrate_to_native(self) -> None:
        migrated, changed = migrate_legacy_media_config(
            {
                "screen_backend": "ffmpeg",
                "ffmpeg_path": "C:/legacy/bin",
                "ffprobe_path": "C:/legacy/probe",
                "ffplay_path": "C:/legacy/player",
                "source_mode": True,
                "theme_mode": "light",
            }
        )
        self.assertTrue(changed)
        self.assertEqual(migrated["screen_backend"], "rust")
        self.assertEqual(migrated["theme_mode"], "light")
        for removed in (
            "ffmpeg_path",
            "ffprobe_path",
            "ffplay_path",
            "source_mode",
        ):
            self.assertNotIn(removed, migrated)

    def test_external_backend_alias_migrates_and_is_removed(self) -> None:
        migrated, changed = migrate_legacy_media_config(
            {"backend": "external", "external_media_backend": "custom"}
        )
        self.assertTrue(changed)
        self.assertEqual(migrated, {"screen_backend": "rust"})

    def test_native_config_is_idempotent(self) -> None:
        original = {"screen_backend": "rust", "screen_native_preset": "r4_default"}
        migrated, changed = migrate_legacy_media_config(original)
        self.assertFalse(changed)
        self.assertEqual(migrated, original)
        self.assertEqual(MIGRATION_WARNING, "legacy_ffmpeg_config_migrated_to_native")

    def test_runtime_builds_only_native_commands(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            native = Path(raw_root) / "agoralink_media.exe"
            native.write_bytes(b"test-native-runtime")
            runtime = ScreenRuntime(script_dir=Path(raw_root))
            sender = runtime._build_native_sender_command(
                host="192.0.2.10",
                port=55134,
                native_exe=str(native),
            )
            receiver = runtime._build_native_receiver_command(
                55134,
                native_exe=str(native),
            )
        self.assertEqual(sender[1], "screen-send")
        self.assertEqual(receiver[1], "screen-recv")
        self.assertEqual(Path(sender[0]), native)
        self.assertEqual(Path(receiver[0]), native)
        joined = " ".join(sender + receiver).lower()
        self.assertNotIn("ffmpeg", joined)
        self.assertNotIn("ffplay", joined)
        self.assertNotIn("ffprobe", joined)


if __name__ == "__main__":
    unittest.main()
