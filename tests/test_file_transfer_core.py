from __future__ import annotations

import hashlib
import tempfile
import unittest
import zipfile
from pathlib import Path

from app_services import FileTransferService
from file_packaging import package_files_to_zip
from file_transfer_common import (
    build_file_header,
    parse_file_header,
    resume_candidate_info,
    sha256_file,
    write_resume_meta,
)
from transfer_store import TransferStore


class FileTransferCoreTests(unittest.TestCase):
    def test_active_and_appended_pending_tasks_remain_independent(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            first = root / "first.bin"
            second = root / "second.bin"
            first.write_bytes(b"a" * 1024)
            second.write_bytes(b"b" * 2048)
            store = TransferStore(str(root / "transfer-state.db"))
            try:
                service = FileTransferService(store)
                runtime_tasks: dict[str, dict[str, object]] = {}
                peer = [{"peer_id": "peer-b", "peer_ip": "192.0.2.2", "peer_port": 9999}]

                for message_id, path in (("msg-active", first), ("msg-pending", second)):
                    service.remember_runtime_task(
                        runtime_tasks,
                        chat_message_id=message_id,
                        path=str(path),
                        recipients=peer,
                        total_bytes=path.stat().st_size,
                    )
                    service.create_outgoing_tasks(
                        chat_message_id=message_id,
                        recipients=peer,
                        path=str(path),
                        total_bytes=path.stat().st_size,
                    )

                service.update_progress(
                    chat_message_id="msg-active",
                    direction="outgoing",
                    peer_id="peer-b",
                    transferred_bytes=512,
                    total_bytes=1024,
                    pct=50.0,
                    status="transferring",
                )

                self.assertEqual(set(runtime_tasks), {"msg-active", "msg-pending"})
                self.assertEqual(
                    service.progress_for_message("msg-active")["status"],
                    "transferring",
                )
                self.assertEqual(service.progress_for_message("msg-pending")["status"], "queued")
                self.assertEqual(service.progress_for_message("msg-pending")["total_bytes"], 2048)
            finally:
                store.close()

    def test_file_hash_and_header_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            path = Path(raw_root) / "payload.bin"
            payload = b"AgoraLink deterministic payload\x00\xff"
            path.write_bytes(payload)
            expected = hashlib.sha256(payload).hexdigest()
            self.assertEqual(sha256_file(str(path)), expected)
            header = parse_file_header(
                build_file_header(
                    str(path),
                    1200,
                    expected,
                    chat_message_id="msg-1",
                    chat_conversation_id="conv-1",
                )
            )
            self.assertEqual(header["sha256"], expected)
            self.assertEqual(header["transfer_id"], expected)
            self.assertEqual(header["chat_message_id"], "msg-1")
            self.assertEqual(header["size"], len(payload))

    def test_resume_metadata_aligns_offset_and_rejects_hash_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root)
            meta = {
                "name": "resume.bin",
                "size": 4096,
                "payload_size": 512,
                "sha256": "a" * 64,
                "transfer_id": "a" * 64,
            }
            out_path = root / "resume.bin"
            part_path = Path(str(out_path) + ".part")
            part_path.write_bytes(b"x" * 1500)
            write_resume_meta(part_path, meta, out_path, 1500)

            candidate = resume_candidate_info(str(root), meta)
            self.assertTrue(candidate["resume_available"])
            self.assertEqual(candidate["resume_offset"], 1024)

            wrong_hash = dict(meta, sha256="b" * 64, transfer_id="b" * 64)
            rejected = resume_candidate_info(str(root), wrong_hash)
            self.assertFalse(rejected["resume_available"])
            self.assertEqual(rejected["reason"], "transfer_id_mismatch")

    def test_packaging_handles_spaces_chinese_apostrophes_and_duplicate_names(self) -> None:
        with tempfile.TemporaryDirectory() as raw_root:
            root = Path(raw_root) / "中文 O'Brien workspace"
            first_dir = root / "first folder"
            second_dir = root / "第二个 folder"
            first_dir.mkdir(parents=True)
            second_dir.mkdir(parents=True)
            first = first_dir / "same name.txt"
            second = second_dir / "same name.txt"
            first.write_text("first", encoding="utf-8")
            second.write_text("second", encoding="utf-8")

            result = package_files_to_zip(
                [str(first), str(second)],
                output_dir=str(root / "package output"),
            )
            self.assertTrue(result["ok"], result.get("error"))
            with zipfile.ZipFile(str(result["zip_path"]), "r") as archive:
                self.assertEqual(archive.namelist(), ["same name.txt", "same name_2.txt"])


if __name__ == "__main__":
    unittest.main()
