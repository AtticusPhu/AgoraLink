#!/usr/bin/env python3
"""Service layer for AgoraLink.

The Kivy UI should call this module for application/business operations and
should not directly manipulate the chat database, transfer database, or message
state machines.  The UI may still handle rendering and process launching; the
services own data selection, state changes, and persistent transfer metadata.
"""

from __future__ import annotations

import json
import os
import time
from typing import Dict, Iterable, List, Optional, Tuple


class ContactService:
    def __init__(self, chat_store):
        self.chat_store = chat_store

    def list_contacts(self, trusted_only: bool = False) -> List[Dict[str, object]]:
        if self.chat_store is None:
            return []
        return self.chat_store.list_contacts(trusted_only=bool(trusted_only))

    def trusted_contacts(self) -> List[Dict[str, object]]:
        return self.list_contacts(trusted_only=True)

    def delete_contact_local(self, peer_id: str) -> None:
        if self.chat_store is None:
            return
        peer_id = str(peer_id or "")
        deleter = getattr(self.chat_store, "delete_contact_deep", None)
        if callable(deleter):
            deleter(peer_id)
        else:
            self.chat_store.delete_contact(peer_id, purge_data=True)

    def update_endpoint(self, peer_id: str, ip: str, port: int = 9999) -> None:
        if self.chat_store is None:
            return
        self.chat_store.update_contact_endpoint(str(peer_id or ""), str(ip or ""), int(port or 9999))

    def update_known_endpoints(self, records: Iterable[Dict[str, object]], endpoint_getter, peer_id_getter, fingerprint_getter, nickname_getter, invalid_ip_checker) -> int:
        if self.chat_store is None:
            return 0
        count = 0
        for rec in records or []:
            if not isinstance(rec, dict):
                continue
            ip, port = endpoint_getter(rec)
            if not ip or invalid_ip_checker(ip):
                continue
            self.chat_store.update_known_endpoint(
                peer_id=peer_id_getter(rec),
                fingerprint=fingerprint_getter(rec),
                nickname=nickname_getter(rec),
                peer_ip=ip,
                peer_port=port,
            )
            count += 1
        return count

    def save_accepted_contact(
        self,
        *,
        peer_id: str,
        nickname: str,
        fingerprint: str,
        peer_ip: str,
        peer_port: int,
        remark_name: str = "",
    ) -> None:
        if self.chat_store is None:
            return
        peer_id = str(peer_id or "").strip()
        if not peer_id:
            raise ValueError("peer_id is empty")
        nickname = str(nickname or peer_id).strip() or peer_id
        self.chat_store.upsert_contact(
            peer_id,
            display_name=nickname,
            nickname=nickname,
            remark_name=str(remark_name or ""),
            fingerprint=str(fingerprint or peer_id),
            peer_ip=str(peer_ip or ""),
            peer_port=int(peer_port or 9999),
            trust_state="trusted",
        )

    def find_contact(self, peer_id: str) -> Dict[str, object]:
        if self.chat_store is None:
            return {}
        pid = str(peer_id or "")
        for c in self.chat_store.list_contacts(trusted_only=False):
            if str(c.get("peer_id") or "") == pid:
                return dict(c)
        return {}

    def endpoint_for_peer(self, peer_id: str) -> Tuple[str, int]:
        contact = self.find_contact(peer_id)
        if contact:
            try:
                return str(contact.get("peer_ip") or ""), int(contact.get("peer_port") or 9999)
            except Exception:
                return str(contact.get("peer_ip") or ""), 9999
        if self.chat_store is not None:
            try:
                row = self.chat_store.db.conn.execute(
                    "SELECT peer_ip, peer_port FROM group_members WHERE peer_id=? AND COALESCE(peer_ip,'')!='' LIMIT 1",
                    (str(peer_id or ""),),
                ).fetchone()
                if row is not None:
                    return str(row["peer_ip"] or ""), int(row["peer_port"] or 9999)
            except Exception:
                pass
        return "", 9999


class GroupService:
    def __init__(self, chat_store):
        self.chat_store = chat_store

    def list_groups(self) -> List[Dict[str, object]]:
        if self.chat_store is None:
            return []
        return self.chat_store.list_groups()

    def create_group(self, group_id: str, title: str) -> None:
        if self.chat_store is None:
            return
        self.chat_store.create_group(str(group_id or ""), str(title or group_id or ""))

    def add_member_manual(self, group_id: str, peer_id: str, peer_ip: str = "", peer_port: int = 9999, display_name: str = "") -> None:
        if self.chat_store is None:
            return
        self.chat_store.add_group_member(
            str(group_id or ""),
            str(peer_id or ""),
            peer_ip=str(peer_ip or ""),
            peer_port=int(peer_port or 9999),
            display_name=str(display_name or peer_id or ""),
            member_state="active",
        )

    def add_member_from_contact(self, group_id: str, contact: Dict[str, object]) -> None:
        if self.chat_store is None or not contact:
            return
        pid = str(contact.get("peer_id") or "")
        self.chat_store.add_group_member(
            str(group_id or ""),
            pid,
            peer_ip=str(contact.get("peer_ip") or ""),
            peer_port=int(contact.get("peer_port") or 9999),
            display_name=str(contact.get("remark_name") or contact.get("display_name") or contact.get("nickname") or pid),
            member_state="active",
        )

    def remove_member(self, group_id: str, peer_id: str, removed: bool = True) -> None:
        if self.chat_store is None:
            return
        self.chat_store.remove_group_member(str(group_id or ""), str(peer_id or ""), removed=bool(removed))

    def leave_group_local(self, group_id: str, local_peer_id: str) -> None:
        if self.chat_store is None:
            return
        self.chat_store.leave_group(str(group_id or ""), str(local_peer_id or ""))

    def delete_group_data(self, group_id: str) -> None:
        if self.chat_store is None:
            return
        self.chat_store.delete_group_data(str(group_id or ""))

    def members(self, group_id: str, include_inactive: bool = True, active_only: bool = False) -> List[Dict[str, object]]:
        if self.chat_store is None:
            return []
        return self.chat_store.list_group_members(str(group_id or ""), include_inactive=bool(include_inactive), active_only=bool(active_only))

    def active_members_with_endpoint(self, group_id: str, include_self: bool = False) -> List[Dict[str, object]]:
        if self.chat_store is None:
            return []
        return self.chat_store.active_group_members_with_endpoint(str(group_id or ""), include_self=bool(include_self))


class MessageService:
    def __init__(self, chat_store):
        self.chat_store = chat_store

    def _alias_direct_sender_by_peer_ip(self, message: Dict[str, object]) -> Dict[str, object]:
        """Map incoming direct messages to an existing contact when peer_id changed.

        Older builds sometimes stored contacts by nickname while newer builds send
        a stable peer_* id. The receiver log includes the network peer address; if
        that IP matches an existing contact, use the existing contact peer_id so
        the current direct chat can display the message immediately.
        """
        obj = dict(message or {})
        if self.chat_store is None or str(obj.get("group_id") or ""):
            return obj
        peer_text = str(obj.get("peer") or obj.get("sender") or "")
        ip = peer_text.split(":", 1)[0].strip() if peer_text else ""
        if not ip:
            return obj
        sender = str(obj.get("sender_peer_id") or "")
        try:
            for c in self.chat_store.list_contacts(trusted_only=False):
                contact_ip = str(c.get("peer_ip") or "").strip()
                contact_pid = str(c.get("peer_id") or "").strip()
                if contact_ip and contact_pid and contact_ip == ip and contact_pid != sender:
                    obj["sender_peer_id"] = contact_pid
                    return obj
        except Exception:
            pass
        return obj

    def create_direct_text(self, peer_id: str, text: str):
        if self.chat_store is None:
            raise RuntimeError("chat store is not available")
        return self.chat_store.send_direct_message(str(peer_id or ""), str(text or ""))

    def create_group_text(self, group_id: str, text: str):
        if self.chat_store is None:
            raise RuntimeError("chat store is not available")
        return self.chat_store.send_group_message(str(group_id or ""), str(text or ""))

    def create_direct_file(self, peer_id: str, path: str):
        if self.chat_store is None:
            raise RuntimeError("chat store is not available")
        return self.chat_store.send_direct_file_message(str(peer_id or ""), str(path or ""))

    def create_group_file(self, group_id: str, path: str):
        if self.chat_store is None:
            raise RuntimeError("chat store is not available")
        return self.chat_store.send_group_file_message(str(group_id or ""), str(path or ""))

    def save_incoming_message(self, message: Dict[str, object], local_peer_id: str = "") -> str:
        if self.chat_store is None:
            return ""
        obj = self._alias_direct_sender_by_peer_ip(dict(message or {}))
        return self.chat_store.save_incoming_chat_message(obj, local_peer_id=str(local_peer_id or ""))

    def ensure_incoming_file_placeholder_from_transfer_request(self, req: Dict[str, object], local_peer_id: str = "") -> str:
        """Create a visible incoming file-message card from FILE header metadata.

        This is a recovery path for cases where the receiver process started
        before the chat DB was unlocked, or the CHAT_MESSAGE frame is not
        persisted by the worker. The file transfer request still carries
        chat_message_id, so we can create the corresponding card in the UI DB.
        """
        if self.chat_store is None:
            return ""
        mid = str(req.get("chat_message_id") or "").strip()
        if not mid:
            return ""
        body = {
            "kind": "file",
            "name": str(req.get("name") or ""),
            "size": int(req.get("size") or 0),
            "chat_message_id": mid,
        }
        message = {
            "message_id": mid,
            "text": json.dumps(body, ensure_ascii=False, separators=(",", ":")),
            "group_id": str(req.get("chat_group_id") or ""),
            "conversation_id": str(req.get("chat_conversation_id") or ""),
            "sender_peer_id": str(req.get("chat_sender_peer_id") or req.get("sender_peer_id") or req.get("sender") or ""),
            "receiver_peer_id": str(req.get("chat_receiver_peer_id") or local_peer_id or ""),
            "peer": str(req.get("sender") or (str(req.get("sender_ip") or "") + ":" + str(req.get("sender_port") or ""))),
            "created_at": float(req.get("ts") or time.time()),
            "body_type": "file",
        }
        message = self._alias_direct_sender_by_peer_ip(message)
        return self.chat_store.save_incoming_chat_message(message, local_peer_id=str(local_peer_id or ""))


    def mark_sent(self, message_id: str, peer_id: str = "") -> None:
        if self.chat_store is not None:
            self.chat_store.mark_chat_sent(str(message_id or ""), str(peer_id or ""))

    def mark_delivered(self, message_id: str, peer_id: str = "") -> None:
        if self.chat_store is not None:
            self.chat_store.mark_chat_delivered(str(message_id or ""), str(peer_id or ""))

    def mark_failed(self, message_id: str, peer_id: str = "", error: str = "") -> None:
        if self.chat_store is not None:
            self.chat_store.mark_chat_failed(str(message_id or ""), str(peer_id or ""), error=str(error or ""))

    def mark_read(self, message_id: str, peer_id: str = "") -> None:
        if self.chat_store is not None:
            self.chat_store.mark_chat_read(str(message_id or ""), str(peer_id or ""))

    def mark_conversation_read(self, *, peer_id: str = "", group_id: str = "") -> None:
        if self.chat_store is None:
            return
        if group_id:
            for r in self.chat_store.unread_incoming_for_group(str(group_id or "")):
                self.chat_store.mark_incoming_read(str(r.get("message_id") or ""))
        elif peer_id:
            for r in self.chat_store.unread_incoming_for_direct(str(peer_id or "")):
                self.chat_store.mark_incoming_read(str(r.get("message_id") or ""))

    def mark_read_and_collect_receipts(self, *, peer_id: str = "", group_id: str = "", local_peer_id: str = "") -> Dict[str, List[str]]:
        """Mark current conversation incoming messages as read and group IDs by sender.

        Returns {sender_peer_id: [message_id, ...]} so the transport/UI layer can
        send CHAT_READ frames without touching storage details.
        """
        if self.chat_store is None:
            return {}
        by_sender: Dict[str, List[str]] = {}
        if group_id:
            unread = self.chat_store.unread_incoming_for_group(str(group_id or ""))
        elif peer_id:
            unread = self.chat_store.unread_incoming_for_direct(str(peer_id or ""))
        else:
            unread = []
        for r in unread:
            mid = str(r.get("message_id") or "")
            sender = str(r.get("sender_peer_id") or "")
            if not mid:
                continue
            self.chat_store.mark_incoming_read(mid)
            if sender and sender != str(local_peer_id or "") and not mid.startswith("live_"):
                by_sender.setdefault(sender, []).append(mid)
        return by_sender

    def receipt_summary(self, message_id: str) -> str:
        if self.chat_store is None:
            return ""
        return self.chat_store.receipt_summary(str(message_id or ""))

    def toggle_pinned(self, conversation_type: str, target_id: str) -> None:
        if self.chat_store is None:
            return
        self.chat_store.toggle_pinned(str(conversation_type or ""), str(target_id or ""))

    def is_pinned(self, conversation_type: str, target_id: str) -> bool:
        if self.chat_store is None:
            return False
        return bool(self.chat_store.is_pinned(str(conversation_type or ""), str(target_id or "")))

    def get_message(self, message_id: str) -> Dict[str, object]:
        if self.chat_store is None:
            return {}
        return self.chat_store.get_message(str(message_id or "")) or {}

    def list_messages(self, *, group_id: str = "", conversation_id: str = "", limit: int = 200) -> List[Dict[str, object]]:
        if self.chat_store is None:
            return []
        return self.chat_store.list_messages(group_id=str(group_id or ""), conversation_id=str(conversation_id or ""), limit=int(limit or 200))

    def create_direct_conversation(self, peer_id: str) -> str:
        if self.chat_store is None:
            return ""
        return self.chat_store.create_direct_conversation(str(peer_id or ""))

    def bind_file_path(self, message_id: str, saved_path: str) -> str:
        if self.chat_store is None:
            return ""
        self.chat_store.bind_message_file_path(str(message_id or ""), str(saved_path or ""))
        return str(message_id or "")

    def bind_latest_incoming_file_path(self, file_name: str, saved_path: str) -> str:
        if self.chat_store is None:
            return ""
        return self.chat_store.bind_latest_incoming_file_path(file_name=str(file_name or ""), saved_path=str(saved_path or ""))

    def retry_text_context(self, message_id: str, current_peer_id: str = "") -> Dict[str, object]:
        if self.chat_store is None:
            return {}
        mid = str(message_id or "").strip()
        row = self.chat_store.db.conn.execute("SELECT conversation_id, group_id, body_type FROM messages WHERE message_id=?", (mid,)).fetchone()
        if row is None:
            return {}
        if str(row["body_type"] or "text") != "text":
            return {"error": "retry_text_only"}
        group_id = str(row["group_id"] or "")
        conv_id = str(row["conversation_id"] or "")
        msgs = self.list_messages(group_id=group_id, conversation_id=conv_id, limit=500)
        msg = next((m for m in msgs if str(m.get("message_id") or "") == mid), None)
        if not msg:
            return {}
        text_body = str(msg.get("text") or "")
        created_at = float(msg.get("created_at") or time.time())
        recipients: List[Dict[str, object]] = []
        if group_id:
            recipients = [m for m in self.chat_store.list_group_members(group_id, active_only=True)]
        else:
            peer_id = str(msg.get("receiver_peer_id") or current_peer_id or "")
            if not peer_id:
                for c in self.chat_store.list_contacts(trusted_only=True):
                    if self.chat_store.create_direct_conversation(str(c.get("peer_id") or "")) == conv_id:
                        peer_id = str(c.get("peer_id") or "")
                        break
            contact = next((c for c in self.chat_store.list_contacts(trusted_only=False) if str(c.get("peer_id") or "") == peer_id), None)
            recipients = [contact] if contact else []
        return {
            "message_id": mid,
            "text": text_body,
            "group_id": group_id,
            "conversation_id": conv_id,
            "created_at": created_at,
            "recipients": recipients,
        }


class FileTransferService:
    def __init__(self, transfer_store, chat_store=None):
        self.transfer_store = transfer_store
        self.chat_store = chat_store

    def create_outgoing_tasks(self, *, chat_message_id: str, recipients: Iterable[Dict[str, object]], path: str, total_bytes: int, conversation_id: str = "", group_id: str = "") -> None:
        if self.transfer_store is None:
            return
        import os
        for r in recipients:
            self.transfer_store.upsert_task(
                chat_message_id=chat_message_id,
                direction="outgoing",
                peer_id=str(r.get("peer_id") or ""),
                conversation_id=conversation_id,
                group_id=group_id,
                local_path=path,
                file_name=os.path.basename(path),
                total_bytes=total_bytes,
                status="queued",
            )

    def remember_runtime_task(self, runtime_tasks: Dict[str, Dict[str, object]], *, chat_message_id: str, path: str, recipients: List[Dict[str, object]], conversation_id: str = "", group_id: str = "", sender_peer_id: str = "", created_at: float = 0.0, total_bytes: int = 0) -> None:
        runtime_tasks[str(chat_message_id or "")] = {
            "message_id": str(chat_message_id or ""),
            "path": str(path or ""),
            "recipients": [dict(r) for r in recipients],
            "conversation_id": str(conversation_id or ""),
            "group_id": str(group_id or ""),
            "sender_peer_id": str(sender_peer_id or ""),
            "created_at": float(created_at or time.time()),
            "total": int(total_bytes or 0),
        }

    def progress_for_message(self, chat_message_id: str) -> Dict[str, object]:
        if self.transfer_store is None:
            return {}
        return self.transfer_store.get_progress(str(chat_message_id or ""))

    def max_updated_at(self) -> float:
        if self.transfer_store is None:
            return 0.0
        try:
            row = self.transfer_store.conn.execute("SELECT COALESCE(MAX(updated_at),0) AS t FROM file_transfers").fetchone()
            return float(row["t"] or 0.0)
        except Exception:
            return 0.0

    def upsert_outgoing_task(self, **kwargs) -> None:
        if self.transfer_store is None:
            return
        self.transfer_store.upsert_task(direction="outgoing", **kwargs)

    def upsert_incoming_task(self, **kwargs) -> None:
        if self.transfer_store is None:
            return
        self.transfer_store.upsert_task(direction="incoming", **kwargs)

    def update_progress(self, **kwargs) -> None:
        if self.transfer_store is None:
            return
        self.transfer_store.update_progress(**kwargs)

    def mark_failed(self, chat_message_id: str, peer_id: str = "", direction: str = "outgoing", error: str = "") -> None:
        if self.transfer_store is None:
            return
        self.transfer_store.mark_failed(str(chat_message_id or ""), peer_id=str(peer_id or ""), direction=str(direction or "outgoing"), error=str(error or ""))

    def bind_saved_path(self, chat_message_id: str, saved_path: str, peer_id: str = "") -> None:
        if self.transfer_store is None:
            return
        self.transfer_store.bind_saved_path(str(chat_message_id or ""), str(saved_path or ""), peer_id=str(peer_id or ""))

    def retry_context_from_message(self, chat_message_id: str, runtime_tasks: Dict[str, Dict[str, object]], local_peer_id: str = "") -> Dict[str, object]:
        """Return a restartable file-transfer context for an existing file message."""
        mid = str(chat_message_id or "")
        task = dict(runtime_tasks.get(mid) or {})
        if task:
            return {
                "message_id": mid,
                "path": str(task.get("path") or ""),
                "recipients": [dict(r) for r in (task.get("recipients") or [])],
                "conversation_id": str(task.get("conversation_id") or ""),
                "group_id": str(task.get("group_id") or ""),
                "sender_peer_id": str(task.get("sender_peer_id") or local_peer_id or ""),
                "created_at": float(task.get("created_at") or time.time()),
            }
        if self.chat_store is None:
            return {}
        row = self.chat_store.get_message(mid)
        if not row:
            return {}
        raw = str(row.get("text") or "")
        try:
            obj = json.loads(raw)
        except Exception:
            obj = {"path": raw}
        path = str(obj.get("path") or "")
        gid = str(row.get("group_id") or "")
        conv = str(row.get("conversation_id") or "")
        recipients: List[Dict[str, object]] = []
        if gid:
            recipients = self.chat_store.active_group_members_with_endpoint(gid, include_self=False)
        else:
            peer_id = str(row.get("receiver_peer_id") or "")
            if not peer_id:
                for c in self.chat_store.list_contacts():
                    if self.chat_store.create_direct_conversation(str(c.get("peer_id") or "")) == conv:
                        peer_id = str(c.get("peer_id") or "")
                        break
            contact = next((c for c in self.chat_store.list_contacts(trusted_only=False) if str(c.get("peer_id") or "") == peer_id), None)
            recipients = [contact] if contact else []
        return {
            "message_id": mid,
            "path": path,
            "recipients": recipients,
            "conversation_id": conv,
            "group_id": gid,
            "sender_peer_id": str(row.get("sender_peer_id") or local_peer_id or ""),
            "created_at": float(row.get("created_at") or time.time()),
        }
