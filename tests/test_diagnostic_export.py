from __future__ import annotations

import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

import diagnostic_export


class _RuntimeSnapshot:
    def get_state(self) -> dict[str, object]:
        return {"state": "idle", "backend": "rust"}

    def check_dependencies(self) -> dict[str, object]:
        return {"native_available": True}


class DiagnosticExportTests(unittest.TestCase):
    def test_sensitive_named_logs_are_skipped_and_not_exported(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            debug = root / "debug"
            output = root / "diagnostics"
            debug.mkdir()
            (debug / "runtime.log").write_text("safe-runtime-line", encoding="utf-8")
            (debug / "chat_protocol.log").write_text(
                "SECRET_CHAT_CONTENT",
                encoding="utf-8",
            )
            (debug / "receiver_pin.log").write_text("SECRET_PIN", encoding="utf-8")
            (debug / "private_key.log").write_text("SECRET_PRIVATE_KEY", encoding="utf-8")

            with mock.patch.object(diagnostic_export, "debug_log_dir", return_value=debug), mock.patch.object(
                diagnostic_export,
                "_network_info",
                return_value={"network": "test"},
            ), mock.patch.object(diagnostic_export, "_ports_info", return_value={"ports": []}), mock.patch.object(
                diagnostic_export,
                "_app_info",
                return_value={"version": "test"},
            ):
                path = diagnostic_export.export_diagnostic_bundle(
                    output,
                    screen_runtime=_RuntimeSnapshot(),
                    extra_json={"config_snapshot.json": {"screen_backend": "rust"}},
                )

            with zipfile.ZipFile(path, "r") as archive:
                names = archive.namelist()
                joined = b"\n".join(archive.read(name) for name in names)
            self.assertIn("debug/runtime.log", names)
            self.assertTrue(any(name.endswith("chat_protocol.log.skipped.txt") for name in names))
            self.assertNotIn(b"SECRET_CHAT_CONTENT", joined)
            self.assertNotIn(b"SECRET_PIN", joined)
            self.assertNotIn(b"SECRET_PRIVATE_KEY", joined)


if __name__ == "__main__":
    unittest.main()
