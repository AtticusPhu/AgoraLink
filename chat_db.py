#!/usr/bin/env python3
"""SQLite chat database for AgoraLink.

Design choices for the first group-chat version:
- SQLite remains the local client database.
- Metadata such as contacts, groups, timestamps and status stays plaintext.
- Message bodies are encrypted with a storage key derived from the user's password.
- Group chat is LAN/P2P: sender sends the same logical message to active members one by one.
"""

from __future__ import annotations

import json
import os
import secrets
import sqlite3
import time
from pathlib import Path
from typing import Dict, Iterable, List, Optional

from chat_crypto import (
    decrypt_json_body,
    derive_storage_key,
    encrypt_json_body,
    encrypt_verifier,
    new_kdf_config,
    verify_storage_key,
)

PROJECT_NAME = "AgoraLink"
SCHEMA_VERSION = 1


def now_ts() -> float:
    return time.time()


def make_id(prefix: str) -> str:
    return f"{prefix}_{secrets.token_hex(16)}"


class ChatDatabase:
    def __init__(self, path: str, password: str, my_peer_id: str = "local"):
        self.path = Path(path).expanduser().resolve()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.my_peer_id = str(my_peer_id or "local")
        self.conn = sqlite3.connect(str(self.path), timeout=30.0, check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA foreign_keys=ON")
        self._create_schema()
        self.storage_key = self._open_storage_key(str(password or ""))

    def close(self) -> None:
        try:
            self.conn.close()
        except Exception:
            pass

    def _create_schema(self) -> None:
        self.conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS contacts (
                peer_id TEXT PRIMARY KEY,
                display_name TEXT,
                identity_pubkey BLOB,
                fingerprint TEXT,
                trust_state TEXT NOT NULL,
                first_seen_at REAL NOT NULL,
                last_seen_at REAL NOT NULL
            );

            CREATE TABLE IF NOT EXISTS groups (
                group_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                creator_peer_id TEXT NOT NULL,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                group_state TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS group_members (
                group_id TEXT NOT NULL,
                peer_id TEXT NOT NULL,
                display_name TEXT,
                role TEXT NOT NULL,
                member_state TEXT NOT NULL,
                joined_at REAL NOT NULL,
                left_at REAL,
                PRIMARY KEY (group_id, peer_id),
                FOREIGN KEY(group_id) REFERENCES groups(group_id)
            );

            CREATE TABLE IF NOT EXISTS conversations (
                conversation_id TEXT PRIMARY KEY,
                peer_id TEXT,
                title TEXT,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                last_message_id TEXT
            );

            CREATE TABLE IF NOT EXISTS messages (
                message_id TEXT PRIMARY KEY,
                conversation_id TEXT,
                group_id TEXT,
                sender_peer_id TEXT NOT NULL,
                receiver_peer_id TEXT,
                direction TEXT NOT NULL,
                body_type TEXT NOT NULL,
                encrypted_body BLOB NOT NULL,
                body_nonce BLOB NOT NULL,
                body_alg TEXT NOT NULL,
                created_at REAL NOT NULL,
                received_at REAL,
                sent_at REAL,
                delivered_at REAL,
                read_at REAL,
                status TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS message_receipts (
                message_id TEXT NOT NULL,
                peer_id TEXT NOT NULL,
                status TEXT NOT NULL,
                sent_at REAL,
                delivered_at REAL,
                read_at REAL,
                failed_at REAL,
                error TEXT,
                PRIMARY KEY (message_id, peer_id),
                FOREIGN KEY(message_id) REFERENCES messages(message_id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_group_created
            ON messages(group_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_messages_conversation_created
            ON messages(conversation_id, created_at);
            """
        )
        self._ensure_column("messages", "read_at", "REAL")
        self._ensure_column("message_receipts", "read_at", "REAL")
        self._set_meta("schema_version", str(SCHEMA_VERSION))
        self.conn.commit()

    def _ensure_column(self, table: str, column: str, decl: str) -> None:
        rows = self.conn.execute(f"PRAGMA table_info({table})").fetchall()
        existing = {str(r["name"]) for r in rows}
        if column not in existing:
            self.conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {decl}")

    def _get_meta(self, key: str) -> Optional[str]:
        row = self.conn.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
        return str(row["value"]) if row else None

    def _set_meta(self, key: str, value: str) -> None:
        self.conn.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?,?)", (str(key), str(value)))

    def _open_storage_key(self, password: str) -> bytes:
        raw_config = self._get_meta("storage_kdf")
        raw_verifier = self._get_meta("storage_verifier")
        if raw_config is None:
            config = new_kdf_config()
            key = derive_storage_key(password, config)
            self._set_meta("storage_kdf", json.dumps(config, ensure_ascii=False, separators=(",", ":")))
            self._set_meta("storage_verifier", json.dumps(encrypt_verifier(key), ensure_ascii=False, separators=(",", ":")))
            self.conn.commit()
            return key
        config = json.loads(raw_config)
        key = derive_storage_key(password, config)
        if raw_verifier:
            verify_storage_key(key, json.loads(raw_verifier))
        return key

    def upsert_contact(self, peer_id: str, display_name: str = "", identity_pubkey: bytes = b"", fingerprint: str = "", trust_state: str = "unverified") -> None:
        ts = now_ts()
        peer_id = str(peer_id or "")
        if not peer_id:
            return
        row = self.conn.execute("SELECT peer_id, first_seen_at FROM contacts WHERE peer_id=?", (peer_id,)).fetchone()
        first_seen = float(row["first_seen_at"]) if row else ts
        self.conn.execute(
            """
            INSERT OR REPLACE INTO contacts(peer_id,display_name,identity_pubkey,fingerprint,trust_state,first_seen_at,last_seen_at)
            VALUES(?,?,?,?,?,?,?)
            """,
            (peer_id, str(display_name or peer_id), bytes(identity_pubkey or b""), str(fingerprint or ""), str(trust_state or "unverified"), first_seen, ts),
        )
        self.conn.commit()

    def ensure_group(self, group_id: str, title: str, creator_peer_id: str) -> None:
        ts = now_ts()
        self.conn.execute(
            """
            INSERT OR IGNORE INTO groups(group_id,title,creator_peer_id,created_at,updated_at,group_state)
            VALUES(?,?,?,?,?,?)
            """,
            (group_id, title or group_id, creator_peer_id or self.my_peer_id, ts, ts, "active"),
        )
        self.conn.execute("UPDATE groups SET updated_at=? WHERE group_id=?", (ts, group_id))
        self.conn.commit()

    def upsert_group_member(self, group_id: str, peer_id: str, display_name: str = "", role: str = "member", member_state: str = "active") -> None:
        ts = now_ts()
        row = self.conn.execute("SELECT joined_at FROM group_members WHERE group_id=? AND peer_id=?", (group_id, peer_id)).fetchone()
        joined = float(row["joined_at"]) if row else ts
        left_at = ts if member_state in ("left", "removed") else None
        self.conn.execute(
            """
            INSERT OR REPLACE INTO group_members(group_id,peer_id,display_name,role,member_state,joined_at,left_at)
            VALUES(?,?,?,?,?,?,?)
            """,
            (group_id, peer_id, display_name or peer_id, role or "member", member_state or "active", joined, left_at),
        )
        self.conn.commit()

    def active_group_members(self, group_id: str) -> List[str]:
        rows = self.conn.execute(
            "SELECT peer_id FROM group_members WHERE group_id=? AND member_state='active' ORDER BY peer_id",
            (group_id,),
        ).fetchall()
        return [str(r["peer_id"]) for r in rows]

    @staticmethod
    def _aad_fields(message_id: str, conversation_id: Optional[str], group_id: Optional[str], sender_peer_id: str, receiver_peer_id: Optional[str], created_at: float) -> Dict[str, object]:
        return {
            "message_id": str(message_id or ""),
            "conversation_id": str(conversation_id or ""),
            "group_id": str(group_id or ""),
            "sender_peer_id": str(sender_peer_id or ""),
            "receiver_peer_id": str(receiver_peer_id or ""),
            "created_at": float(created_at or 0.0),
        }

    def save_message(
        self,
        *,
        message_id: str,
        text: str,
        sender_peer_id: str,
        receiver_peer_id: str = "",
        conversation_id: str = "",
        group_id: str = "",
        direction: str,
        status: str,
        created_at: Optional[float] = None,
        body_type: str = "text",
    ) -> None:
        created = float(created_at or now_ts())
        aad = self._aad_fields(message_id, conversation_id, group_id, sender_peer_id, receiver_peer_id, created)
        body = {"body_type": body_type, "text": str(text or ""), "format": "plain"}
        encrypted_body, nonce, alg = encrypt_json_body(self.storage_key, body, aad)
        self.conn.execute(
            """
            INSERT OR IGNORE INTO messages(
                message_id, conversation_id, group_id, sender_peer_id, receiver_peer_id,
                direction, body_type, encrypted_body, body_nonce, body_alg,
                created_at, received_at, sent_at, delivered_at, read_at, status
            ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
            """,
            (
                message_id, conversation_id or None, group_id or None, sender_peer_id, receiver_peer_id or None,
                direction, body_type, encrypted_body, nonce, alg,
                created, now_ts() if direction == "incoming" else None,
                now_ts() if direction == "outgoing" and status in ("sent", "delivered", "read") else None,
                now_ts() if status in ("delivered", "read") else None,
                now_ts() if status == "read" else None,
                status,
            ),
        )
        if group_id:
            self.conn.execute("UPDATE groups SET updated_at=?, group_state='active' WHERE group_id=?", (now_ts(), group_id))
        self.conn.commit()

    def message_exists(self, message_id: str) -> bool:
        row = self.conn.execute("SELECT 1 FROM messages WHERE message_id=?", (message_id,)).fetchone()
        return row is not None

    def mark_message_status(self, message_id: str, status: str, peer_id: str = "", error: str = "") -> None:
        ts = now_ts()
        if status == "read":
            self.conn.execute("UPDATE messages SET status=?, delivered_at=COALESCE(delivered_at,?), read_at=? WHERE message_id=?", (status, ts, ts, message_id))
        elif status == "delivered":
            self.conn.execute("UPDATE messages SET status=?, delivered_at=? WHERE message_id=?", (status, ts, message_id))
        elif status == "sent":
            self.conn.execute("UPDATE messages SET status=?, sent_at=? WHERE message_id=?", (status, ts, message_id))
        else:
            self.conn.execute("UPDATE messages SET status=? WHERE message_id=?", (status, message_id))
        if peer_id:
            self.save_receipt(message_id, peer_id, status, error=error)
        self.conn.commit()

    def save_receipt(self, message_id: str, peer_id: str, status: str, error: str = "") -> None:
        ts = now_ts()
        sent_at = ts if status in ("sent", "delivered", "read") else None
        delivered_at = ts if status in ("delivered", "read") else None
        read_at = ts if status == "read" else None
        failed_at = ts if status == "failed" else None
        self.conn.execute(
            """
            INSERT OR REPLACE INTO message_receipts(message_id,peer_id,status,sent_at,delivered_at,read_at,failed_at,error)
            VALUES(?,?,?,?,?,?,?,?)
            """,
            (message_id, peer_id, status, sent_at, delivered_at, read_at, failed_at, error or None),
        )

    def list_messages(self, group_id: str = "", conversation_id: str = "", limit: int = 50) -> List[Dict[str, object]]:
        if group_id:
            rows = self.conn.execute("SELECT * FROM messages WHERE group_id=? ORDER BY created_at DESC LIMIT ?", (group_id, int(limit))).fetchall()
        elif conversation_id:
            rows = self.conn.execute("SELECT * FROM messages WHERE conversation_id=? ORDER BY created_at DESC LIMIT ?", (conversation_id, int(limit))).fetchall()
        else:
            rows = self.conn.execute("SELECT * FROM messages ORDER BY created_at DESC LIMIT ?", (int(limit),)).fetchall()
        out = []
        for row in reversed(rows):
            obj = dict(row)
            aad = self._aad_fields(obj["message_id"], obj.get("conversation_id"), obj.get("group_id"), obj["sender_peer_id"], obj.get("receiver_peer_id"), obj["created_at"])
            body = decrypt_json_body(self.storage_key, obj["encrypted_body"], obj["body_nonce"], aad)
            obj["text"] = body.get("text", "")
            out.append(obj)
        return out
