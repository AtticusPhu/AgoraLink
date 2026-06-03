#!/usr/bin/env python3
"""Business-layer chat store for AgoraLink.

This module wraps the lower-level ChatDatabase API and exposes operations the GUI
can call directly: group creation, member management, encrypted message storage,
and per-member delivery receipt updates.
"""

from __future__ import annotations

import json
import os
import secrets
import time
from file_transfer_common import is_unspecified_ip, normalize_peer_endpoint_ip
from typing import Dict, List, Optional, Tuple

from chat_db import ChatDatabase, make_id, now_ts


class ChatStore:
    def __init__(self, db_path: str, password: str, my_peer_id: str = "local"):
        self.db = ChatDatabase(db_path, password, my_peer_id=my_peer_id)
        self.my_peer_id = str(my_peer_id or "local")
        self._ensure_runtime_schema()

    def close(self) -> None:
        self.db.close()

    def _ensure_runtime_schema(self) -> None:
        # Older chat_db.py versions have group_members without endpoint columns.
        self._ensure_column("contacts", "peer_ip", "TEXT")
        self._ensure_column("contacts", "peer_port", "INTEGER")
        self._ensure_column("contacts", "remark_name", "TEXT")
        self._ensure_column("contacts", "nickname", "TEXT")
        self._ensure_column("contacts", "blocked_at", "REAL")
        self._ensure_column("group_members", "peer_ip", "TEXT")
        self._ensure_column("group_members", "peer_port", "INTEGER")
        self._ensure_column("group_members", "last_error", "TEXT")
        self.db.conn.commit()

    def _ensure_column(self, table: str, column: str, decl: str) -> None:
        rows = self.db.conn.execute(f"PRAGMA table_info({table})").fetchall()
        existing = {str(r["name"]) for r in rows}
        if column not in existing:
            self.db.conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {decl}")

    def set_local_profile(self, nickname: str = "", peer_id: str = "") -> None:
        if peer_id:
            self.my_peer_id = str(peer_id)
            self.db.my_peer_id = str(peer_id)
            self.db.conn.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('local_peer_id',?)", (str(peer_id),))
        if nickname:
            self.db.conn.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('local_nickname',?)", (str(nickname),))
        self.db.conn.commit()

    def get_local_profile(self) -> Dict[str, str]:
        def _get(key: str) -> str:
            row = self.db.conn.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
            return str(row["value"]) if row else ""
        return {"peer_id": _get("local_peer_id") or self.my_peer_id, "nickname": _get("local_nickname")}

    def upsert_contact(self, peer_id: str, *, display_name: str = "", nickname: str = "", remark_name: str = "", fingerprint: str = "", peer_ip: str = "", peer_port: int = 9999, trust_state: str = "trusted") -> None:
        pid = str(peer_id or "").strip()
        if not pid:
            raise ValueError("empty_peer_id")
        name = str(remark_name or display_name or nickname or pid)
        self.db.upsert_contact(pid, display_name=name, fingerprint=str(fingerprint or pid), trust_state=str(trust_state or "trusted"))
        existing = self.db.conn.execute("SELECT peer_ip, peer_port FROM contacts WHERE peer_id=?", (pid,)).fetchone()
        old_ip = str(existing["peer_ip"] or "") if existing else ""
        old_port = int(existing["peer_port"] or 9999) if existing else 9999
        clean_ip = normalize_peer_endpoint_ip(str(peer_ip or ""), fallback=old_ip)
        clean_port = int(peer_port or old_port or 9999)
        self.db.conn.execute(
            "UPDATE contacts SET peer_ip=?, peer_port=?, remark_name=?, nickname=? WHERE peer_id=?",
            (clean_ip, clean_port, str(remark_name or ""), str(nickname or display_name or ""), pid),
        )
        self.db.conn.commit()

    def list_contacts(self, trusted_only: bool = False) -> List[Dict[str, object]]:
        if trusted_only:
            rows = self.db.conn.execute("SELECT * FROM contacts WHERE trust_state='trusted' ORDER BY last_seen_at DESC").fetchall()
        else:
            rows = self.db.conn.execute("SELECT * FROM contacts ORDER BY last_seen_at DESC").fetchall()
        return [dict(r) for r in rows]

    def update_known_endpoint(self, *, peer_id: str = "", fingerprint: str = "", nickname: str = "", peer_ip: str = "", peer_port: int = 9999) -> bool:
        """Refresh a known contact/member endpoint after LAN discovery.

        Identity is stable; IP is only the latest reachable address. A discovery
        result with the same peer_id or fingerprint should update the stored
        endpoint instead of creating a duplicate contact.
        """
        clean_ip = normalize_peer_endpoint_ip(str(peer_ip or ""), fallback="")
        if not clean_ip:
            return False
        try:
            clean_port = int(peer_port or 9999)
        except Exception:
            clean_port = 9999
        pid = str(peer_id or "").strip()
        fp = str(fingerprint or "").strip()
        row = None
        if pid:
            row = self.db.conn.execute("SELECT peer_id FROM contacts WHERE peer_id=?", (pid,)).fetchone()
        if row is None and fp:
            row = self.db.conn.execute("SELECT peer_id FROM contacts WHERE fingerprint=?", (fp,)).fetchone()
        if row is None:
            return False
        real_pid = str(row["peer_id"] or "")
        ts = now_ts()
        self.db.conn.execute(
            "UPDATE contacts SET peer_ip=?, peer_port=?, last_seen_at=?, nickname=COALESCE(NULLIF(?,''), nickname), display_name=COALESCE(NULLIF(?,''), display_name) WHERE peer_id=?",
            (clean_ip, clean_port, ts, str(nickname or ""), str(nickname or ""), real_pid),
        )
        self.db.conn.execute(
            "UPDATE group_members SET peer_ip=?, peer_port=?, last_error=NULL WHERE peer_id=?",
            (clean_ip, clean_port, real_pid),
        )
        self.db.conn.commit()
        return True

    def delete_contact(self, peer_id: str, purge_data: bool = True) -> None:
        pid = str(peer_id or "").strip()
        if not pid:
            raise ValueError("empty_peer_id")
        if purge_data:
            parts = sorted([self.my_peer_id, pid])
            import hashlib
            cid = "direct_" + hashlib.sha256((parts[0] + ":" + parts[1]).encode("utf-8")).hexdigest()[:32]
            mids = [str(r["message_id"]) for r in self.db.conn.execute("SELECT message_id FROM messages WHERE conversation_id=? OR (group_id IS NULL AND (sender_peer_id=? OR receiver_peer_id=?))", (cid, pid, pid)).fetchall()]
            for mid in mids:
                self.db.conn.execute("DELETE FROM message_receipts WHERE message_id=?", (mid,))
            self.db.conn.execute("DELETE FROM messages WHERE conversation_id=? OR (group_id IS NULL AND (sender_peer_id=? OR receiver_peer_id=?))", (cid, pid, pid))
            self.db.conn.execute("DELETE FROM conversations WHERE conversation_id=? OR peer_id=?", (cid, pid))
        self.db.conn.execute("DELETE FROM contacts WHERE peer_id=?", (pid,))
        self.db.conn.execute("DELETE FROM group_members WHERE peer_id=?", (pid,))
        self.db.conn.commit()

    def delete_group_data(self, group_id: str) -> None:
        gid = str(group_id or "").strip()
        if not gid:
            raise ValueError("empty_group_id")
        mids = [str(r["message_id"]) for r in self.db.conn.execute("SELECT message_id FROM messages WHERE group_id=?", (gid,)).fetchall()]
        for mid in mids:
            self.db.conn.execute("DELETE FROM message_receipts WHERE message_id=?", (mid,))
        self.db.conn.execute("DELETE FROM messages WHERE group_id=?", (gid,))
        self.db.conn.execute("DELETE FROM group_members WHERE group_id=?", (gid,))
        self.db.conn.execute("DELETE FROM groups WHERE group_id=?", (gid,))
        self.db.conn.commit()

    def create_direct_conversation(self, peer_id: str, title: str = "") -> str:
        pid = str(peer_id or "").strip()
        if not pid:
            raise ValueError("empty_peer_id")
        parts = sorted([self.my_peer_id, pid])
        import hashlib
        cid = "direct_" + hashlib.sha256((parts[0] + ":" + parts[1]).encode("utf-8")).hexdigest()[:32]
        ts = now_ts()
        self.db.conn.execute(
            "INSERT OR IGNORE INTO conversations(conversation_id,peer_id,title,created_at,updated_at,last_message_id) VALUES(?,?,?,?,?,NULL)",
            (cid, pid, str(title or pid), ts, ts),
        )
        self.db.conn.execute("UPDATE conversations SET updated_at=? WHERE conversation_id=?", (ts, cid))
        self.db.conn.commit()
        return cid

    def list_direct_conversations(self) -> List[Dict[str, object]]:
        rows = self.db.conn.execute("SELECT * FROM conversations ORDER BY updated_at DESC, created_at DESC").fetchall()
        return [dict(r) for r in rows]

    def send_direct_message(self, peer_id: str, text: str, *, message_id: str = "", created_at: Optional[float] = None) -> Tuple[Dict[str, object], Dict[str, object]]:
        pid = str(peer_id or "").strip()
        body = str(text or "")
        if not pid:
            raise ValueError("empty_peer_id")
        if not body.strip():
            raise ValueError("empty_chat_message")
        cid = self.create_direct_conversation(pid)
        mid = str(message_id or ("msg_" + secrets.token_hex(16)))
        created = float(created_at or now_ts())
        self.db.save_message(
            message_id=mid,
            text=body,
            conversation_id=cid,
            sender_peer_id=self.my_peer_id,
            receiver_peer_id=pid,
            direction="outgoing",
            status="pending",
            created_at=created,
        )
        self.db.save_receipt(mid, pid, "pending")
        contact = None
        for c in self.list_contacts():
            if str(c.get("peer_id") or "") == pid:
                contact = c
                break
        if contact is None:
            contact = {"peer_id": pid, "peer_ip": "", "peer_port": 9999}
        msg = {"message_id": mid, "conversation_id": cid, "sender_peer_id": self.my_peer_id, "receiver_peer_id": pid, "text": body, "created_at": created}
        return msg, contact

    def create_group(self, group_id: str, title: str = "") -> str:
        gid = str(group_id or "").strip() or make_id("group")
        self.db.ensure_group(gid, title or gid, self.my_peer_id)
        self.add_group_member(gid, self.my_peer_id, display_name=self.my_peer_id, role="owner", member_state="active")
        return gid

    def list_groups(self) -> List[Dict[str, object]]:
        rows = self.db.conn.execute(
            "SELECT * FROM groups WHERE group_state!='deleted' ORDER BY updated_at DESC, created_at DESC"
        ).fetchall()
        return [dict(r) for r in rows]

    def add_group_member(
        self,
        group_id: str,
        peer_id: str,
        *,
        peer_ip: str = "",
        peer_port: int = 9999,
        display_name: str = "",
        role: str = "member",
        member_state: str = "active",
    ) -> None:
        gid = str(group_id or "").strip()
        pid = str(peer_id or "").strip()
        if not gid:
            raise ValueError("empty_group_id")
        if not pid:
            raise ValueError("empty_peer_id")
        self.db.ensure_group(gid, gid, self.my_peer_id)
        self.db.upsert_contact(pid, display_name=display_name or pid, trust_state="trusted")
        self.db.upsert_group_member(gid, pid, display_name=display_name or pid, role=role, member_state=member_state)
        self.db.conn.execute(
            "UPDATE group_members SET peer_ip=?, peer_port=?, last_error=NULL WHERE group_id=? AND peer_id=?",
            (str(peer_ip or ""), int(peer_port or 9999), gid, pid),
        )
        self.db.conn.commit()

    def remove_group_member(self, group_id: str, peer_id: str, removed: bool = True) -> None:
        state = "removed" if removed else "left"
        self._set_member_state(group_id, peer_id, state)

    def leave_group(self, group_id: str, peer_id: Optional[str] = None) -> None:
        self._set_member_state(group_id, peer_id or self.my_peer_id, "left")

    def _set_member_state(self, group_id: str, peer_id: str, state: str) -> None:
        ts = now_ts()
        self.db.conn.execute(
            "UPDATE group_members SET member_state=?, left_at=? WHERE group_id=? AND peer_id=?",
            (state, ts, str(group_id or ""), str(peer_id or "")),
        )
        self.db.conn.execute("UPDATE groups SET updated_at=? WHERE group_id=?", (ts, str(group_id or "")))
        self.db.conn.commit()

    def list_group_members(self, group_id: str, include_inactive: bool = True) -> List[Dict[str, object]]:
        if include_inactive:
            rows = self.db.conn.execute(
                "SELECT * FROM group_members WHERE group_id=? ORDER BY member_state, peer_id",
                (str(group_id or ""),),
            ).fetchall()
        else:
            rows = self.db.conn.execute(
                "SELECT * FROM group_members WHERE group_id=? AND member_state='active' ORDER BY peer_id",
                (str(group_id or ""),),
            ).fetchall()
        return [dict(r) for r in rows]

    def active_group_members_with_endpoint(self, group_id: str, include_self: bool = False) -> List[Dict[str, object]]:
        rows = self.db.conn.execute(
            """
            SELECT * FROM group_members
            WHERE group_id=? AND member_state='active'
            ORDER BY peer_id
            """,
            (str(group_id or ""),),
        ).fetchall()
        out = []
        for r in rows:
            obj = dict(r)
            if not include_self and str(obj.get("peer_id") or "") == self.my_peer_id:
                continue
            if not str(obj.get("peer_ip") or ""):
                obj["missing_endpoint"] = True
            out.append(obj)
        return out

    def send_group_message(self, group_id: str, text: str, *, message_id: str = "", created_at: Optional[float] = None) -> Tuple[Dict[str, object], List[Dict[str, object]]]:
        gid = str(group_id or "").strip()
        body = str(text or "")
        if not gid:
            raise ValueError("empty_group_id")
        if not body.strip():
            raise ValueError("empty_chat_message")
        mid = str(message_id or ("msg_" + secrets.token_hex(16)))
        created = float(created_at or now_ts())
        self.db.save_message(
            message_id=mid,
            text=body,
            group_id=gid,
            sender_peer_id=self.my_peer_id,
            receiver_peer_id="",
            direction="outgoing",
            status="pending",
            created_at=created,
        )
        recipients = self.active_group_members_with_endpoint(gid, include_self=False)
        for member in recipients:
            pid = str(member.get("peer_id") or "")
            self.db.save_receipt(mid, pid, "pending")
        self.db.conn.commit()
        msg = {
            "message_id": mid,
            "group_id": gid,
            "sender_peer_id": self.my_peer_id,
            "text": body,
            "created_at": created,
        }
        return msg, recipients

    def send_direct_file_message(self, peer_id: str, path: str, *, message_id: str = "", created_at: Optional[float] = None) -> Tuple[Dict[str, object], Dict[str, object]]:
        pid = str(peer_id or "").strip()
        if not pid:
            raise ValueError("empty_peer_id")
        p = os.path.abspath(str(path or ""))
        if not p:
            raise ValueError("empty_file_path")
        cid = self.create_direct_conversation(pid)
        mid = str(message_id or ("msg_" + secrets.token_hex(16)))
        created = float(created_at or now_ts())
        payload = {"kind": "file", "path": p, "name": os.path.basename(p), "size": os.path.getsize(p) if os.path.exists(p) else 0}
        self.db.save_message(
            message_id=mid,
            text=json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
            conversation_id=cid,
            sender_peer_id=self.my_peer_id,
            receiver_peer_id=pid,
            direction="outgoing",
            status="pending",
            created_at=created,
            body_type="file",
        )
        self.db.save_receipt(mid, pid, "pending")
        contact = next((c for c in self.list_contacts() if str(c.get("peer_id") or "") == pid), {"peer_id": pid, "peer_ip": "", "peer_port": 9999})
        msg = {"message_id": mid, "conversation_id": cid, "sender_peer_id": self.my_peer_id, "receiver_peer_id": pid, "text": payload["name"], "file_path": p, "created_at": created, "body_type": "file"}
        return msg, contact

    def send_group_file_message(self, group_id: str, path: str, *, message_id: str = "", created_at: Optional[float] = None) -> Tuple[Dict[str, object], List[Dict[str, object]]]:
        gid = str(group_id or "").strip()
        if not gid:
            raise ValueError("empty_group_id")
        p = os.path.abspath(str(path or ""))
        mid = str(message_id or ("msg_" + secrets.token_hex(16)))
        created = float(created_at or now_ts())
        payload = {"kind": "file", "path": p, "name": os.path.basename(p), "size": os.path.getsize(p) if os.path.exists(p) else 0}
        self.db.save_message(
            message_id=mid,
            text=json.dumps(payload, ensure_ascii=False, separators=(",", ":")),
            group_id=gid,
            sender_peer_id=self.my_peer_id,
            receiver_peer_id="",
            direction="outgoing",
            status="pending",
            created_at=created,
            body_type="file",
        )
        recipients = self.active_group_members_with_endpoint(gid, include_self=False)
        for member in recipients:
            self.db.save_receipt(mid, str(member.get("peer_id") or ""), "pending")
        self.db.conn.commit()
        msg = {"message_id": mid, "group_id": gid, "sender_peer_id": self.my_peer_id, "text": payload["name"], "file_path": p, "created_at": created, "body_type": "file"}
        return msg, recipients

    def save_incoming_chat_message(self, message: Dict[str, object], local_peer_id: str = "") -> str:
        mid = str(message.get("message_id") or "")
        if not mid:
            raise ValueError("empty_message_id")
        gid = str(message.get("group_id") or "")
        sender = str(message.get("sender_peer_id") or "")
        receiver = str(message.get("receiver_peer_id") or local_peer_id or self.my_peer_id)
        text = str(message.get("text") or "")
        created = float(message.get("created_at") or now_ts())
        if gid:
            self.db.ensure_group(gid, gid, sender or self.my_peer_id)
            if sender:
                self.add_group_member(gid, sender, display_name=sender, member_state="active")
            if receiver:
                self.add_group_member(gid, receiver, display_name=receiver, member_state="active")
        else:
            peer = sender if sender != self.my_peer_id else receiver
            if peer:
                conv_id = self.create_direct_conversation(peer)
                if not str(message.get("conversation_id") or ""):
                    message["conversation_id"] = conv_id
        if not self.db.message_exists(mid):
            self.db.save_message(
                message_id=mid,
                text=text,
                group_id=gid,
                conversation_id=str(message.get("conversation_id") or ""),
                sender_peer_id=sender,
                receiver_peer_id=receiver,
                direction="incoming",
                status="delivered",
                created_at=created,
                body_type=str(message.get("body_type") or "text"),
            )
        return mid

    def mark_chat_sent(self, message_id: str, peer_id: str = "") -> None:
        self.db.mark_message_status(str(message_id or ""), "sent", peer_id=str(peer_id or ""))

    def mark_chat_delivered(self, message_id: str, peer_id: str = "") -> None:
        self.db.mark_message_status(str(message_id or ""), "delivered", peer_id=str(peer_id or ""))
        self.refresh_group_message_status(str(message_id or ""))

    def mark_chat_failed(self, message_id: str, peer_id: str = "", error: str = "") -> None:
        self.db.mark_message_status(str(message_id or ""), "failed", peer_id=str(peer_id or ""), error=str(error or ""))
        self.refresh_group_message_status(str(message_id or ""))

    def refresh_group_message_status(self, message_id: str) -> str:
        rows = self.db.conn.execute("SELECT status FROM message_receipts WHERE message_id=?", (str(message_id or ""),)).fetchall()
        if not rows:
            return "pending"
        statuses = [str(r["status"] or "") for r in rows]
        if all(s == "delivered" for s in statuses):
            status = "delivered"
        elif any(s == "delivered" for s in statuses):
            status = "partially_delivered"
        elif any(s == "sent" for s in statuses):
            status = "sent"
        elif all(s == "failed" for s in statuses):
            status = "failed"
        else:
            status = "pending"
        self.db.conn.execute("UPDATE messages SET status=? WHERE message_id=?", (status, str(message_id or "")))
        self.db.conn.commit()
        return status

    def list_messages(self, group_id: str = "", conversation_id: str = "", limit: int = 100) -> List[Dict[str, object]]:
        return self.db.list_messages(group_id=group_id, conversation_id=conversation_id, limit=limit)

    def list_receipts(self, message_id: str) -> List[Dict[str, object]]:
        rows = self.db.conn.execute(
            "SELECT * FROM message_receipts WHERE message_id=? ORDER BY peer_id",
            (str(message_id or ""),),
        ).fetchall()
        return [dict(r) for r in rows]

    def receipt_summary(self, message_id: str) -> str:
        rows = self.list_receipts(message_id)
        if not rows:
            return ""
        delivered = sum(1 for r in rows if str(r.get("status") or "") == "delivered")
        failed = sum(1 for r in rows if str(r.get("status") or "") == "failed")
        total = len(rows)
        if failed:
            return f"delivered {delivered}/{total}, failed {failed}"
        return f"delivered {delivered}/{total}"
