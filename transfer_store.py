#!/usr/bin/env python3
"""Persistent file transfer state for AgoraLink.

This module deliberately stores transfer status outside the Kivy UI state.  A
file message card should be able to recover progress/status from SQLite after a
refresh or process restart, and retry should use chat_message_id as the stable
binding between the chat card and the file transfer task.
"""

from __future__ import annotations

import os
import sqlite3
import time
from pathlib import Path
from typing import Dict, List, Optional


def _now() -> float:
    return time.time()


class TransferStore:
    def __init__(self, db_path: str):
        self.db_path = str(Path(db_path).expanduser().resolve())
        Path(self.db_path).parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(self.db_path, check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        self._init_schema()

    def _init_schema(self) -> None:
        self.conn.execute(
            """
            CREATE TABLE IF NOT EXISTS file_transfers(
                transfer_key TEXT PRIMARY KEY,
                chat_message_id TEXT NOT NULL,
                transfer_id TEXT,
                file_id TEXT,
                direction TEXT NOT NULL,
                peer_id TEXT,
                conversation_id TEXT,
                group_id TEXT,
                local_path TEXT,
                remote_path TEXT,
                file_name TEXT,
                total_bytes INTEGER DEFAULT 0,
                transferred_bytes INTEGER DEFAULT 0,
                pct REAL DEFAULT 0,
                avg_mbps REAL DEFAULT 0,
                eta TEXT,
                status TEXT DEFAULT 'queued',
                error TEXT,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                completed_at REAL
            )
            """
        )
        for col, ddl in {
            "transfer_id": "ALTER TABLE file_transfers ADD COLUMN transfer_id TEXT",
            "file_id": "ALTER TABLE file_transfers ADD COLUMN file_id TEXT",
            "remote_path": "ALTER TABLE file_transfers ADD COLUMN remote_path TEXT",
            "pct": "ALTER TABLE file_transfers ADD COLUMN pct REAL DEFAULT 0",
            "avg_mbps": "ALTER TABLE file_transfers ADD COLUMN avg_mbps REAL DEFAULT 0",
            "eta": "ALTER TABLE file_transfers ADD COLUMN eta TEXT",
            "error": "ALTER TABLE file_transfers ADD COLUMN error TEXT",
            "completed_at": "ALTER TABLE file_transfers ADD COLUMN completed_at REAL",
        }.items():
            try:
                self.conn.execute(ddl)
            except sqlite3.OperationalError:
                pass
        self.conn.execute("CREATE INDEX IF NOT EXISTS idx_file_transfers_message ON file_transfers(chat_message_id)")
        self.conn.execute("CREATE INDEX IF NOT EXISTS idx_file_transfers_updated ON file_transfers(updated_at)")
        self.conn.commit()

    @staticmethod
    def make_key(chat_message_id: str, direction: str, peer_id: str = "") -> str:
        mid = str(chat_message_id or "").strip()
        direction = str(direction or "outgoing").strip() or "outgoing"
        peer = str(peer_id or "").strip() or "_"
        return f"{mid}:{direction}:{peer}"

    def upsert_task(
        self,
        *,
        chat_message_id: str,
        direction: str,
        peer_id: str = "",
        conversation_id: str = "",
        group_id: str = "",
        local_path: str = "",
        remote_path: str = "",
        file_name: str = "",
        total_bytes: int = 0,
        status: str = "queued",
        transfer_id: str = "",
        file_id: str = "",
    ) -> str:
        mid = str(chat_message_id or "").strip()
        if not mid:
            return ""
        direction = str(direction or "outgoing").strip() or "outgoing"
        peer_id = str(peer_id or "").strip()
        key = self.make_key(mid, direction, peer_id)
        now = _now()
        if not file_name:
            file_name = os.path.basename(str(local_path or remote_path or ""))
        self.conn.execute(
            """
            INSERT INTO file_transfers(
                transfer_key, chat_message_id, transfer_id, file_id, direction, peer_id,
                conversation_id, group_id, local_path, remote_path, file_name,
                total_bytes, transferred_bytes, pct, avg_mbps, eta, status, error,
                created_at, updated_at, completed_at
            ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
            ON CONFLICT(transfer_key) DO UPDATE SET
                transfer_id=COALESCE(NULLIF(excluded.transfer_id,''), file_transfers.transfer_id),
                file_id=COALESCE(NULLIF(excluded.file_id,''), file_transfers.file_id),
                conversation_id=COALESCE(NULLIF(excluded.conversation_id,''), file_transfers.conversation_id),
                group_id=COALESCE(NULLIF(excluded.group_id,''), file_transfers.group_id),
                local_path=COALESCE(NULLIF(excluded.local_path,''), file_transfers.local_path),
                remote_path=COALESCE(NULLIF(excluded.remote_path,''), file_transfers.remote_path),
                file_name=COALESCE(NULLIF(excluded.file_name,''), file_transfers.file_name),
                total_bytes=CASE WHEN excluded.total_bytes > 0 THEN excluded.total_bytes ELSE file_transfers.total_bytes END,
                status=excluded.status,
                updated_at=excluded.updated_at
            """,
            (
                key, mid, transfer_id, file_id, direction, peer_id,
                conversation_id or None, group_id or None, local_path or None, remote_path or None, file_name or None,
                int(total_bytes or 0), 0, 0.0, 0.0, "", status or "queued", None,
                now, now, None,
            ),
        )
        self.conn.commit()
        return key

    def update_progress(
        self,
        *,
        chat_message_id: str,
        direction: str = "",
        peer_id: str = "",
        transferred_bytes: int = 0,
        total_bytes: int = 0,
        pct: float = 0.0,
        avg_mbps: float = 0.0,
        eta: str = "",
        status: str = "transferring",
        error: str = "",
    ) -> None:
        mid = str(chat_message_id or "").strip()
        if not mid:
            return
        direction = str(direction or "")
        peer_id = str(peer_id or "")
        rows = []
        if direction:
            key = self.make_key(mid, direction, peer_id)
            rows = [key]
            # Ensure a row exists even if the caller did not create it before.
            self.upsert_task(chat_message_id=mid, direction=direction, peer_id=peer_id, total_bytes=total_bytes, status=status)
        else:
            rows = [str(r["transfer_key"]) for r in self.conn.execute("SELECT transfer_key FROM file_transfers WHERE chat_message_id=?", (mid,)).fetchall()]
            if not rows:
                key = self.upsert_task(chat_message_id=mid, direction="incoming", peer_id=peer_id, total_bytes=total_bytes, status=status)
                rows = [key] if key else []
        now = _now()
        completed_at = now if status in ("completed", "received") else None
        for key in rows:
            self.conn.execute(
                """
                UPDATE file_transfers SET
                    transferred_bytes=?,
                    total_bytes=CASE WHEN ? > 0 THEN ? ELSE total_bytes END,
                    pct=?,
                    avg_mbps=?,
                    eta=?,
                    status=?,
                    error=?,
                    updated_at=?,
                    completed_at=COALESCE(?, completed_at)
                WHERE transfer_key=?
                """,
                (
                    int(transferred_bytes or 0),
                    int(total_bytes or 0),
                    int(total_bytes or 0),
                    float(pct or 0.0),
                    float(avg_mbps or 0.0),
                    str(eta or ""),
                    str(status or "transferring"),
                    str(error or "") or None,
                    now,
                    completed_at,
                    key,
                ),
            )
        self.conn.commit()

    def bind_saved_path(self, chat_message_id: str, saved_path: str, peer_id: str = "") -> None:
        mid = str(chat_message_id or "").strip()
        if not mid or not saved_path:
            return
        try:
            total = os.path.getsize(saved_path) if os.path.exists(saved_path) else 0
        except Exception:
            total = 0
        self.upsert_task(
            chat_message_id=mid,
            direction="incoming",
            peer_id=peer_id,
            local_path=saved_path,
            remote_path=saved_path,
            file_name=os.path.basename(saved_path),
            total_bytes=total,
            status="received",
        )
        self.update_progress(
            chat_message_id=mid,
            direction="incoming",
            peer_id=peer_id,
            transferred_bytes=total,
            total_bytes=total,
            pct=100.0 if total else 0.0,
            eta="0:00",
            status="received",
        )

    def mark_failed(self, chat_message_id: str, peer_id: str = "", direction: str = "outgoing", error: str = "") -> None:
        row = self.get_progress(chat_message_id)
        total = int(row.get("total_bytes") or 0) if row else 0
        sent = int(row.get("transferred_bytes") or 0) if row else 0
        pct = float(row.get("pct") or 0) if row else 0.0
        self.update_progress(
            chat_message_id=chat_message_id,
            direction=direction,
            peer_id=peer_id,
            transferred_bytes=sent,
            total_bytes=total,
            pct=pct,
            status="failed",
            error=error or "failed",
        )

    def get_rows(self, chat_message_id: str) -> List[Dict[str, object]]:
        rows = self.conn.execute(
            "SELECT * FROM file_transfers WHERE chat_message_id=? ORDER BY updated_at DESC",
            (str(chat_message_id or ""),),
        ).fetchall()
        return [dict(r) for r in rows]

    def get_progress(self, chat_message_id: str) -> Dict[str, object]:
        rows = self.get_rows(chat_message_id)
        if not rows:
            return {}

        # Prefer a completed/received row, especially when it has a valid local
        # path. This prevents the UI from showing an older in-progress row after
        # the file has already been saved successfully.
        for r in rows:
            status = str(r.get("status") or "")
            local_path = str(r.get("local_path") or r.get("remote_path") or "")
            try:
                actual_size = os.path.getsize(local_path) if local_path and os.path.exists(local_path) else 0
            except Exception:
                actual_size = 0
            if status in ("completed", "received") or actual_size > 0:
                base = dict(r)
                total = max(int(base.get("total_bytes") or 0), actual_size)
                if total > 0:
                    base.update({
                        "total_bytes": total,
                        "transferred_bytes": total,
                        "pct": 100.0,
                        "eta": "0:00",
                        "status": "received" if str(base.get("direction") or "") == "incoming" else "completed",
                    })
                return base

        if len(rows) == 1:
            return rows[0]

        total = max(int(r.get("total_bytes") or 0) for r in rows)
        transferred = max(int(r.get("transferred_bytes") or 0) for r in rows)
        pct = (transferred * 100.0 / total) if total else max(float(r.get("pct") or 0) for r in rows)
        statuses = [str(r.get("status") or "") for r in rows]
        if all(s in ("completed", "received") for s in statuses):
            status = "completed"
        elif any(s == "failed" for s in statuses) and not any(s in ("transferring", "queued", "offered", "accepted") for s in statuses):
            status = "failed"
        elif any(s in ("transferring", "accepted", "offered") for s in statuses):
            status = "transferring"
        else:
            status = statuses[0] if statuses else "queued"
        base = dict(rows[0])
        base.update({
            "total_bytes": total,
            "transferred_bytes": transferred,
            "pct": pct,
            "avg_mbps": max(float(r.get("avg_mbps") or 0.0) for r in rows),
            "eta": rows[0].get("eta") or "",
            "status": status,
        })
        return base

    def close(self) -> None:
        self.conn.close()
