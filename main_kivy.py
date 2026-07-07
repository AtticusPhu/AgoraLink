#!/usr/bin/env python3
"""Kivy GUI entry point for RUDPTransfer.

The same executable is used in two modes:
- GUI mode: no special arguments, starts the Kivy desktop UI.
- Worker mode: --worker sender|receiver ..., runs the CLI sender/receiver logic.

This design keeps the PyInstaller folder build self-contained. The GUI does not
need an external python.exe or visible source files after packaging.
"""

from __future__ import annotations

import json
import os
import hashlib
import re
import secrets
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple

from app_paths import APP_DIR, APP_NAME, APP_VERSION, FROZEN, IS_WINDOWS, RESOURCE_DIR, debug_log_dir, temp_dir, user_data_dir
from file_transfer_presenter import (
    file_accepted_text,
    file_card_title,
    file_completed_text,
    file_error_message,
    file_failed_text,
    file_incoming_text,
    file_offer_title,
    file_progress_detail,
    file_progress_text,
    file_rejected_by_peer_text,
    file_rejected_local_text,
    file_resume_text,
    file_size_detail,
    file_transfer_card_detail,
    file_waiting_confirm_text,
    file_waiting_text,
    folder_not_supported_text,
    format_file_size,
    is_failed_status,
    multi_file_card_title,
    multi_file_summary,
    remote_peer_text as file_transfer_remote_peer_text,
    transfer_status_label,
    truncate_filename,
    unnamed_file_text,
)
from process_utils import popen_no_console, run_no_console
from screen_share_presenter import (
    screen_audio_text,
    screen_detail_text,
    screen_offer_title,
    screen_rejected_by_peer_text,
    screen_rejected_local_text,
    screen_share_active_states,
    screen_share_button_text,
    screen_share_status_text,
    screen_start_failed_text,
    screen_stop_failed_text,
    screen_stopped_text,
)

PROGRESS_RE = re.compile(
    r"Progress:\s+(?P<sent>\d+)/(?:\s*)?(?P<total>\d+)\s+bytes\s+\((?P<pct>[0-9.]+)%\).*?"
    r"avg=(?P<avg>[0-9.]+)\s+Mbps.*?eta=(?P<eta>[^,\s]+)"
)
COMPLETE_RE = re.compile(r"Transfer complete:\s+(?P<total>\d+)\s+bytes.*?avg=(?P<avg>[0-9.]+)\s+Mbps")
RECEIVED_SAVE_RE = re.compile(r"Session\s+(?P<conn>\d+):\s+saved\s+(?P<path>.+?),\s+bytes=")
RECEIVE_PROGRESS_RE = re.compile(r"Session\s+(?P<conn>\d+):\s+(?P<sent>\d+)/(?:\s*)?(?P<total>\d+)\s+bytes\s+\((?P<pct>[0-9.]+)%\).*?avg=(?P<avg>[0-9.]+)\s+Mbps.*?eta=(?P<eta>[^,\s]+)")


def append_worker_debug_log(role: str, line: str) -> None:
    try:
        path = debug_log_dir() / f"{str(role or 'worker')}_worker.log"
        with path.open("a", encoding="utf-8", errors="replace") as f:
            f.write(str(line or ""))
            if line and not str(line).endswith("\n"):
                f.write("\n")
    except Exception:
        pass


def gui_config_file() -> Path:
    return user_data_dir() / "gui_settings.json"


def load_gui_config() -> Dict[str, object]:
    path = gui_config_file()
    try:
        if path.exists():
            data = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(data, dict):
                return data
    except Exception:
        pass
    return {}


def save_gui_config(data: Dict[str, object]) -> None:
    try:
        path = gui_config_file()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(dict(data or {}), ensure_ascii=False, indent=2), encoding="utf-8")
    except Exception:
        pass


def receiver_pin_file(ip: str, port: int) -> Path:
    """Return the per-receiver TOFU pin path used by the sender role.

    The local receiver identity key is intentionally separate from these trust
    records. A machine may alternate between sending and receiving, and a sender
    may talk to multiple receivers; therefore one global pin file is unsafe.
    """
    pins_dir = user_data_dir() / "receiver_pins"
    pins_dir.mkdir(parents=True, exist_ok=True)
    raw = f"{str(ip or '').strip()}_{int(port or 9999)}"
    safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", raw).strip("._") or "receiver"
    return pins_dir / f"{safe}.pin"


def is_image_file_for_preview(path_or_name: str) -> bool:
    ext = Path(str(path_or_name or "")).suffix.lower()
    return ext in {".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp"}


def file_icon_text(path_or_name: str, is_dir: bool = False) -> str:
    if is_dir:
        return "DIR"
    ext = Path(str(path_or_name or "")).suffix.lower().lstrip(".")
    if not ext:
        return "FILE"
    mapping = {
        "jpg": "IMG", "jpeg": "IMG", "png": "IMG", "bmp": "IMG", "gif": "IMG", "webp": "IMG",
        "mp4": "VID", "mkv": "VID", "avi": "VID", "mov": "VID", "wmv": "VID",
        "mp3": "AUD", "wav": "AUD", "flac": "AUD", "aac": "AUD",
        "zip": "ZIP", "rar": "RAR", "7z": "7Z",
        "pdf": "PDF",
        "doc": "DOC", "docx": "DOC", "xls": "XLS", "xlsx": "XLS", "ppt": "PPT", "pptx": "PPT",
        "py": "PY", "json": "JSON", "txt": "TXT", "md": "MD",
        "exe": "EXE", "msi": "MSI",
    }
    return mapping.get(ext, ext[:4].upper())


def file_type_badge(path_or_name: str) -> str:
    ext = Path(str(path_or_name or "")).suffix.lower().lstrip(".")
    if not ext:
        return "FILE"
    mapping = {
        "zip": "ZIP", "rar": "RAR", "7z": "7Z",
        "pdf": "PDF", "doc": "DOC", "docx": "DOC", "xls": "XLS", "xlsx": "XLS", "ppt": "PPT", "pptx": "PPT",
        "mp4": "MP4", "mkv": "VID", "avi": "VID", "mov": "MOV",
        "mp3": "MP3", "wav": "AUD", "flac": "AUD",
        "exe": "EXE", "msi": "MSI",
        "txt": "TXT", "py": "PY", "json": "JSON",
    }
    return mapping.get(ext, ext[:4].upper())


def shorten_middle(value: str, max_chars: int = 34) -> str:
    text = str(value or "")
    if len(text) <= max_chars:
        return text
    keep = max(4, (max_chars - 1) // 2)
    return text[:keep] + "…" + text[-keep:]


def configure_stdio_utf8() -> None:
    """Force UTF-8 text I/O for worker logs.

    In packaged Windows builds, worker processes write logs through stdout/stderr
    pipes. Some environments still default to an ANSI code page, which can turn
    Chinese file names into mojibake before the GUI reads them. Reconfiguring the
    streams here keeps subprocess logs Unicode-safe.
    """
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        if stream is None:
            continue
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass
    os.environ.setdefault("PYTHONUTF8", "1")
    os.environ.setdefault("PYTHONIOENCODING", "utf-8")


def run_worker(argv: List[str]) -> int:
    """Run sender/receiver logic without importing Kivy."""
    if len(argv) < 2:
        print("missing worker role", file=sys.stderr)
        return 2
    role = argv[1].strip().lower()
    worker_args = argv[2:]
    if role == "sender":
        import client
        parser = client.build_argparser()
        args = parser.parse_args(worker_args)
        try:
            return int(client.run_client(args))
        except KeyboardInterrupt:
            return 130
        except Exception as exc:
            client.setup_logger("RUDP-Sender").error(f"Fatal: {exc}")
            return 2
    if role == "receiver":
        import server
        parser = server.build_argparser()
        args = parser.parse_args(worker_args)
        receiver = None
        logger = server.setup_logger("RUDP-Receiver")
        try:
            receiver = server.RUDPFileReceiver(args)
            receiver.start()
        except KeyboardInterrupt:
            return 130
        except OSError as exc:
            logger.error(f"Fatal: receiver_start_failed: {exc}")
            return 2
        except Exception as exc:
            logger.error(f"Fatal: receiver_start_failed: {exc}")
            return 2
        finally:
            if receiver is not None:
                receiver.stop()
        return 0
    print(f"unknown worker role: {role}", file=sys.stderr)
    return 2


if len(sys.argv) >= 2 and sys.argv[1] == "--worker":
    configure_stdio_utf8()
    raise SystemExit(run_worker(sys.argv[1:]))


# GUI imports are intentionally below the worker dispatch.
from kivy.app import App
from kivy.animation import Animation
from kivy.clock import Clock
from kivy.core.window import Window
from kivy.core.text import LabelBase, DEFAULT_FONT
from kivy.metrics import dp
from kivy.properties import StringProperty, BooleanProperty, ObjectProperty
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.button import Button
from kivy.uix.checkbox import CheckBox
from kivy.uix.filechooser import FileChooserListView
from kivy.uix.floatlayout import FloatLayout
from kivy.uix.gridlayout import GridLayout
from kivy.uix.image import Image
from kivy.uix.label import Label
from kivy.uix.popup import Popup
from kivy.uix.progressbar import ProgressBar
from kivy.uix.recycleview import RecycleView
from kivy.uix.recycleboxlayout import RecycleBoxLayout
from kivy.uix.recycleview.views import RecycleDataViewBehavior
from kivy.uix.scrollview import ScrollView
from kivy.uix.spinner import Spinner, SpinnerOption
from kivy.uix.tabbedpanel import TabbedPanel, TabbedPanelItem
from kivy.uix.textinput import TextInput
from kivy.graphics import Color, RoundedRectangle, Triangle, Line

from file_transfer_common import (
    CHAT_MESSAGE_LOG_PREFIX,
    CHAT_ACK_LOG_PREFIX,
    CHAT_READ_LOG_PREFIX,
    CONTACT_REQUEST_LOG_PREFIX,
    CONTACT_RESPONSE_LOG_PREFIX,
    DEFAULT_DISCOVERY_PORT,
    TRANSFER_REQUEST_LOG_PREFIX,
    TRANSFER_STARTED_LOG_PREFIX,
    TRANSFER_PROGRESS_LOG_PREFIX,
    TRANSFER_SAVED_LOG_PREFIX,
    TRANSFER_COMPLETE_LOG_PREFIX,
    TRANSFER_FAILED_LOG_PREFIX,
    USER_ERROR_LOG_PREFIX,
    USER_STATUS_LOG_PREFIX,
    discover_receivers,
    get_local_ip_candidates,
    is_unspecified_ip,
    normalize_peer_endpoint_ip,
)
from screen_control import (
    DEFAULT_SCREEN_PORT,
    SCREEN_BACKEND_FFMPEG,
    SCREEN_BACKEND_RUST,
    SCREEN_SHARE_ACCEPT,
    SCREEN_SHARE_OFFER,
    SCREEN_SHARE_REJECT,
    SCREEN_SHARE_STATE,
    SCREEN_SHARE_STOP,
    make_accept,
    make_offer,
    make_reject,
    make_stop,
    parse_screen_control_message,
)
from screen_profile import (
    DEFAULT_SCREEN_PROFILE,
    PROFILES_BY_NAME,
    choose_advertised_profile,
    get_advertised_profiles,
    profile_id_from_info,
    profile_info,
)
from screen_runtime import (
    NATIVE_LITE_FFMPEG_UNAVAILABLE_MESSAGE,
    NATIVE_LITE_VIDEO_ONLY_MESSAGE,
    ScreenRuntime,
)
from diagnostic_export import export_diagnostic_bundle
from file_packaging import package_files_to_zip
from port_utils import find_available_udp_port, udp_port_status, udp_ports_status
from chat_cards import (
    CARD_FILE_OFFER,
    CARD_FILE_TRANSFER,
    CARD_SCREEN_OFFER,
    CARD_SCREEN_STATE,
    CARD_SYSTEM,
    make_card,
    system_card,
)
try:
    from ui_components import (
        ConversationItem as UIConversationItem,
        FileTransferCard as UIFileTransferCard,
        MessageBubble as UIMessageBubble,
        PillButton as UIPillButton,
        RoundedButton as UIRoundedButton,
        RoundedCard as UIRoundedCard,
        ScreenShareCard as UIScreenShareCard,
        StatusBadge as UIStatusBadge,
        color as ui_component_color,
    )
except Exception:
    UIConversationItem = None
    UIFileTransferCard = None
    UIMessageBubble = None
    UIPillButton = None
    UIRoundedButton = None
    UIRoundedCard = None
    UIScreenShareCard = None
    UIStatusBadge = None
    ui_component_color = None

SCREEN_CONTROL_TEXT_PREFIX = "__AGORALINK_SCREEN_CONTROL__:"
MAIN_UDP_PORT = 9999
SCREEN_PORT_CANDIDATES = tuple(range(DEFAULT_SCREEN_PORT, DEFAULT_SCREEN_PORT + 6))
RUST_SCREEN_PORT_CANDIDATES = tuple(range(55000, 56000))
SCREEN_BACKEND_VALUES = (SCREEN_BACKEND_FFMPEG, SCREEN_BACKEND_RUST)
RUST_SCREEN_PORTS_BUSY_MESSAGE = "Rust native screen ports 55000-55999 are unavailable."
MAIN_UDP_PORT_BUSY_MESSAGE = "UDP 9999 已被占用，请关闭旧的 AgoraLink 或修改配置后重启。"
SCREEN_PORTS_BUSY_MESSAGE = "投屏端口 50020-50025 均被占用，无法启动接收端。"

I18N: Dict[str, Dict[str, str]] = {
    "zh": {
        "title": "AgoraLink 文件传输与聊天",
        "toggle_lang": "English",
        "send_tab": "发送文件",
        "recv_tab": "接收文件",
        "chat_tab": "聊天",
        "contacts_tab": "联系人",
        "scan_devices": "扫描设备",
        "chat_db": "聊天数据库",
        "chat_password": "聊天密码",
        "local_peer_id": "本机 peer_id",
        "unlock_chat": "解锁聊天库",
        "group_id": "群组 ID",
        "group_title": "群组名称",
        "create_group": "创建/保存群组",
        "member_peer_id": "成员 peer_id",
        "member_ip": "成员 IP",
        "member_port": "成员端口",
        "add_member": "添加/更新成员",
        "remove_member": "移除成员",
        "leave_group": "退出群组",
        "chat_message": "群聊消息",
        "send_group_msg": "发送群消息",
        "refresh_chat": "刷新聊天",
        "chat_locked": "聊天数据库未解锁。",
        "chat_unlocked": "聊天数据库已解锁。",
        "need_group": "请填写群组 ID。",
        "need_member": "请填写成员 peer_id。",
        "need_chat_message": "请输入聊天消息。",
        "local_ip": "本机 IPv4：{ips}",
        "receiver_ip": "接收端 IP",
        "port": "端口",
        "discovery_port": "发现端口",
        "manual_hint": "可手动输入 IP，也可搜索接收端",
        "search": "搜索接收端",
        "search_working": "搜索中",
        "choose_receiver": "发现结果",
        "no_receiver": "未发现接收端",
        "searching": "正在搜索局域网接收端...",
        "search_done": "发现完成：{n} 个接收端",
        "search_none": "没有发现接收端，请确认接收端已启动；若仍失败，请手动输入 IP。搜索会尝试 UDP 9998 和传输端口 9999。",
        "file": "待发送文件",
        "choose_file": "选择文件",
        "send": "开始发送",
        "stop": "停止",
        "payload": "Payload 大小",
        "complete_timeout": "完成等待秒数",
        "progress": "进度",
        "eta": "剩余时间：{eta}",
        "speed": "平均速度：{speed}",
        "size": "大小：{size}",
        "save_dir": "保存目录",
        "choose_dir": "选择目录",
        "bind": "监听地址",
        "allow_peer": "只允许发送端 IP，可空",
        "receiver_name": "接收端名称，可空",
        "once": "接收一次后停止",
        "start_recv": "启动接收端",
        "stop_recv": "停止接收端",
        "firewall": "放行 Windows 防火墙端口",
        "clear": "清空日志",
        "need_ip": "请填写接收端 IP，或先搜索接收端。",
        "need_file": "请选择有效文件。",
        "running": "进程仍在运行。",
        "stopped": "已停止。",
        "started": "已启动。",
        "ready": "就绪",
        "unknown": "未知",
        "browse": "浏览",
        "cancel": "取消",
        "select": "选择",
        "request_timeout": "确认等待秒数",
        "approval_timeout": "确认等待秒数",
        "incoming_request_title": "收到传输请求",
        "incoming_request": "发送端：{sender}\n文件名：{name}\n大小：{size}\n保存路径：{path}\nSHA256：{sha256}",
        "accept": "接收",
        "reject": "拒绝",
        "request_waiting": "已提交传输请求，等待接收端确认...",
        "approval_hint": "发送端提交请求后，将在这里弹出确认窗口",
        "transfer_finished": "本次传输结束。",
        "receive_transfer_finished": "本次传输完成。",
        "transfer_failed": "传输失败：{reason}",
        "retry": "重新发送",
        "retry_ready": "可以点击“重新发送”再次尝试。",
        "approval_timeout_msg": "等待接收端确认超时。",
        "receiver_rejected_msg": "接收端已拒绝本次传输。",
        "save_dir_not_writable_msg": "接收端保存路径不可写。",
        "disk_space_not_enough_msg": "接收端磁盘空间不足。",
        "network_no_progress_msg": "网络长时间无进展，传输已中断。",
        "receiver_unreachable_msg": "接收端无响应，可能已关闭或网络断开。",
        "receiver_identity_changed_msg": "接收端身份发生变化。为避免误发文件，已停止连接。请确认接收端确实是目标设备；如设备重装或重新生成身份密钥，可删除该接收端对应的 pin 文件后重新信任。",
        "complete_timeout_msg": "文件已发送完成，但未收到接收端完成确认。",
        "sha256_mismatch_msg": "文件校验失败，接收文件可能不完整。",
        "output_open_failed_msg": "接收端无法创建输出文件。",
        "receive_idle_timeout_msg": "长时间没有收到发送端数据，接收已中断。",
        "file_conflict": "目标文件已存在，请选择处理方式：",
        "policy_rename": "自动重命名",
        "policy_overwrite": "覆盖",
        "policy_cancel": "取消",
        "native_dialog_failed": "系统文件选择窗口打开失败，已切换为内置选择窗口。",
    },
    "en": {
        "title": "AgoraLink File Transfer and Chat",
        "toggle_lang": "中文",
        "send_tab": "Send File",
        "recv_tab": "Receive File",
        "chat_tab": "Chat",
        "contacts_tab": "Contacts",
        "scan_devices": "Scan Devices",
        "chat_db": "Chat DB",
        "chat_password": "Chat Password",
        "local_peer_id": "Local peer_id",
        "unlock_chat": "Unlock Chat DB",
        "group_id": "Group ID",
        "group_title": "Group Title",
        "create_group": "Create/Save Group",
        "member_peer_id": "Member peer_id",
        "member_ip": "Member IP",
        "member_port": "Member Port",
        "add_member": "Add/Update Member",
        "remove_member": "Remove Member",
        "leave_group": "Leave Group",
        "chat_message": "Group Message",
        "send_group_msg": "Send Group Message",
        "refresh_chat": "Refresh Chat",
        "chat_locked": "Chat database is locked.",
        "chat_unlocked": "Chat database unlocked.",
        "need_group": "Enter a group ID.",
        "need_member": "Enter a member peer_id.",
        "need_chat_message": "Enter a chat message.",
        "local_ip": "Local IPv4: {ips}",
        "receiver_ip": "Receiver IP",
        "port": "Port",
        "discovery_port": "Discovery Port",
        "manual_hint": "Enter IP manually or search LAN receivers",
        "search": "Search Receivers",
        "search_working": "Searching",
        "choose_receiver": "Discovered Receivers",
        "no_receiver": "No receiver found",
        "searching": "Searching LAN receivers...",
        "search_done": "Discovery finished: {n} receiver(s)",
        "search_none": "No receiver found. Make sure the receiver is running. Search tries UDP 9998 and transfer port 9999; manual IP mode can still be used.",
        "file": "File to Send",
        "choose_file": "Choose File",
        "send": "Start Sending",
        "stop": "Stop",
        "payload": "Payload Size",
        "complete_timeout": "Complete Timeout (s)",
        "progress": "Progress",
        "eta": "ETA: {eta}",
        "speed": "Average Speed: {speed}",
        "size": "Size: {size}",
        "save_dir": "Save Directory",
        "choose_dir": "Choose Directory",
        "bind": "Bind Address",
        "allow_peer": "Allowed Sender IP, optional",
        "receiver_name": "Receiver Name, optional",
        "once": "Stop after one transfer",
        "start_recv": "Start Receiver",
        "stop_recv": "Stop Receiver",
        "firewall": "Allow Windows Firewall Ports",
        "clear": "Clear Log",
        "need_ip": "Enter a receiver IP, or search receivers first.",
        "need_file": "Choose a valid file.",
        "running": "The process is still running.",
        "stopped": "Stopped.",
        "started": "Started.",
        "ready": "Ready",
        "unknown": "unknown",
        "browse": "Browse",
        "cancel": "Cancel",
        "select": "Select",
        "request_timeout": "Approval Timeout (s)",
        "approval_timeout": "Approval Timeout (s)",
        "incoming_request_title": "Incoming Transfer Request",
        "incoming_request": "Sender: {sender}\nFile: {name}\nSize: {size}\nSave path: {path}\nSHA256: {sha256}",
        "accept": "Accept",
        "reject": "Reject",
        "request_waiting": "Transfer request submitted; waiting for receiver approval...",
        "approval_hint": "After a sender submits a request, a confirmation dialog appears here.",
        "transfer_finished": "This transfer has finished.",
        "receive_transfer_finished": "This transfer has completed.",
        "transfer_failed": "Transfer failed: {reason}",
        "retry": "Retry",
        "retry_ready": "You can click Retry to send the same file again.",
        "approval_timeout_msg": "Receiver confirmation timed out.",
        "receiver_rejected_msg": "The receiver rejected this transfer.",
        "save_dir_not_writable_msg": "The receiver save directory is not writable.",
        "disk_space_not_enough_msg": "The receiver does not have enough disk space.",
        "network_no_progress_msg": "The network made no progress for too long; the transfer was stopped.",
        "receiver_unreachable_msg": "The receiver is not responding; it may be closed or disconnected.",
        "receiver_identity_changed_msg": "The receiver identity has changed. The connection was stopped to avoid sending the file to an untrusted device. Confirm the receiver is the intended device; if it was reinstalled or regenerated its identity key, delete that receiver's pin file and trust it again.",
        "complete_timeout_msg": "File data was sent, but receiver completion confirmation timed out.",
        "sha256_mismatch_msg": "File verification failed; the received file may be incomplete.",
        "output_open_failed_msg": "The receiver could not create the output file.",
        "receive_idle_timeout_msg": "No data was received from the sender for too long; receiving has stopped.",
        "file_conflict": "Target file already exists. Choose a policy:",
        "policy_rename": "Auto rename",
        "policy_overwrite": "Overwrite",
        "policy_cancel": "Cancel",
        "policy_resume": "Resume",
        "resume_detected": "Incomplete file found: {done} / {total} received ({pct:.2f}%).",
        "resume_enabled": "Receiver requested resume from {offset}.",
        "native_dialog_failed": "The system file dialog failed. Falling back to the built-in chooser.",
    },
}



CHAT_UI_TEXT = {
    "zh": {
        "search_hint": "搜索好友 / 群聊 / 设备",
        "recent": "最近聊天",
        "contacts": "联系人",
        "devices": "在线设备",
        "scan_devices": "扫描设备",
        "add_contact": "加联系人",
        "choose_chat": "请选择会话",
        "input_hint": "输入消息，按 Enter 发送",
        "send": "发送",
        "send_file": "发送文件",
        "choose_file_or_folder": "选择发送内容",
        "choose_files_multi": "选择文件",
        "choose_folder_package": "选择文件夹",
        "cancel": "取消",
        "multi_file_send_mode": "已选择 {n} 个文件。请选择发送方式：",
        "multi_folder_send_mode": "已选择 {n} 个文件夹。请选择发送方式：",
        "send_separately": "分别发送",
        "package_send": "合并发送",
        "packaging_files": "正在打包 {n} 个项目...",
        "package_created": "已生成打包文件：{name}",
        "package_failed": "打包失败：{error}",
        "mixed_picker_title": "选择文件或文件夹",
        "mixed_picker_up": "上一级",
        "mixed_picker_home": "主页",
        "mixed_picker_refresh": "刷新",
        "mixed_picker_clear": "清空选择",
        "mixed_picker_send": "发送选中项",
        "mixed_picker_selected": "已选择 {n} 个项目",
        "mixed_picker_auto_package_on": "多选默认合并为 ZIP：开启",
        "mixed_picker_auto_package_off": "多选默认合并为 ZIP：关闭",
        "mixed_picker_open": "进入",
        "multi_auto_package_setting": "多选后默认合并为 ZIP",
        "right_title": "群成员 / 设备信息",
        "shared_files": "共享文件",
        "new_group": "新建群",
        "add_member": "加成员",
        "remove_member": "移除成员",
        "leave_group": "退出群",
        "delete_friend": "删好友",
        "one_to_one": "一对一：{name}",
        "group_chat": "群聊：{title} ({gid})",
        "device_title": "在线设备：先添加联系人后聊天",
        "no_devices": "未发现在线设备\n点击下方“扫描设备”",
        "chat_locked": "聊天数据库未解锁",
        "no_chat": "暂无会话或联系人",
        "refresh_failed": "刷新失败\n{error}",
        "file": "文件",
        "unknown_size": "大小未知",
        "pending": "pending",
        "sending_to": "发送给 {peer}",
        "completed": "已完成",
        "failed": "失败",
        "waiting_receive": "等待接收",
        "receiving": "接收中",
        "received": "已接收",
        "state": "状态",
        "total_size": "总大小",
        "open_folder": "打开所在位置",
        "waiting_saved": "等待文件保存",
        "not_saved": "未保存",
        "group_prefix": "群聊",
        "friend_prefix": "好友",
        "no_message": "暂无消息",
        "group_name": "群名",
        "members_count": "成员数量",
        "contact": "联系人",
        "peer_id": "peer_id",
        "fingerprint": "fingerprint",
        "ip": "IP",
        "group_members_title": "群成员",
        "friend_info": "好友信息",
        "member_detail_hint": "点击成员查看详细信息",
        "member_detail": "成员详情",
        "nickname": "昵称",
        "member_state": "成员状态",
        "endpoint": "地址",
        "ctx_open_chat": "打开聊天",
        "ctx_view_profile": "查看资料",
        "ctx_delete_friend": "删除好友",
        "ctx_join_group": "加入群",
        "ctx_leave_group": "退出群聊",
        "ctx_rescan_ip": "重新扫描 IP",
        "ctx_add_contact": "添加联系人",
        "ctx_pin": "置顶会话",
        "ctx_unpin": "取消置顶",
        "detail_hide": "隐藏详情",
        "detail_show": "查看详情",
        "retry": "重发",
        "retry_text_only": "只能重发文本消息",
        "retry_file_missing": "原文件不存在，无法重发",
        "retry_file": "重发文件",
        "today": "今天",
        "yesterday": "昨天",
    },
    "en": {
        "search_hint": "Search friends / groups / devices",
        "recent": "Recent",
        "contacts": "Contacts",
        "devices": "Devices",
        "scan_devices": "Scan",
        "add_contact": "Add",
        "choose_chat": "Select a chat",
        "input_hint": "Type a message, press Enter to send",
        "send": "Send",
        "send_file": "File",
        "choose_file_or_folder": "Choose content to send",
        "choose_files_multi": "Choose files",
        "choose_folder_package": "Choose folder (package automatically)",
        "cancel": "Cancel",
        "multi_file_send_mode": "{n} files selected. Choose how to send:",
        "send_separately": "Send separately",
        "package_send": "Merge and send",
        "packaging_files": "Packaging {n} item(s)...",
        "package_created": "Package created: {name}",
        "package_failed": "Packaging failed: {error}",
        "right_title": "Members / Device Info",
        "shared_files": "Shared Files",
        "new_group": "New Group",
        "add_member": "Add Member",
        "remove_member": "Remove",
        "leave_group": "Leave",
        "delete_friend": "Delete",
        "one_to_one": "Chat with {name}",
        "group_chat": "Group: {title} ({gid})",
        "device_title": "Online device: add contact first",
        "no_devices": "No online devices\nClick Scan",
        "chat_locked": "Chat database is locked",
        "no_chat": "No chats or contacts",
        "refresh_failed": "Refresh failed\n{error}",
        "file": "File",
        "unknown_size": "Unknown size",
        "pending": "pending",
        "sending_to": "Sending to {peer}",
        "completed": "Complete",
        "failed": "Failed",
        "waiting_receive": "Waiting",
        "receiving": "Receiving",
        "received": "Received",
        "state": "Status",
        "total_size": "Total",
        "open_folder": "Open Folder",
        "waiting_saved": "Waiting for file",
        "not_saved": "Not saved",
        "group_prefix": "Group",
        "friend_prefix": "Friend",
        "no_message": "No messages",
        "group_name": "Group",
        "members_count": "Members",
        "contact": "Contact",
        "peer_id": "peer_id",
        "fingerprint": "fingerprint",
        "ip": "IP",
        "group_members_title": "Group Members",
        "friend_info": "Friend Info",
        "member_detail_hint": "Click a member to view details",
        "member_detail": "Member Details",
        "nickname": "Nickname",
        "member_state": "Member State",
        "endpoint": "Endpoint",
        "ctx_open_chat": "Open Chat",
        "ctx_view_profile": "View Profile",
        "ctx_delete_friend": "Delete Friend",
        "ctx_join_group": "Add to Group",
        "ctx_leave_group": "Leave Group",
        "ctx_rescan_ip": "Rescan IP",
        "ctx_add_contact": "Add Contact",
        "ctx_pin": "Pin Chat",
        "ctx_unpin": "Unpin Chat",
        "detail_hide": "Hide Details",
        "detail_show": "Details",
        "retry": "Retry",
        "retry_text_only": "Only text messages can be retried",
        "retry_file_missing": "Original file not found; cannot retry",
        "retry_file": "Retry file",
        "today": "Today",
        "yesterday": "Yesterday",
    },
}

def find_cjk_font() -> Optional[str]:
    """Return a font that can render Chinese.

    Priority is:
    1. bundled fonts under assets/fonts, when the project owner supplies them;
    2. common system CJK fonts already installed on the operating system.
    """
    candidates: List[Path] = []
    bundled_font_dir = RESOURCE_DIR / "assets" / "fonts"
    if bundled_font_dir.exists():
        for pattern in ("*.ttf", "*.ttc", "*.otf"):
            candidates.extend(sorted(bundled_font_dir.glob(pattern)))
    if IS_WINDOWS:
        win = Path(os.environ.get("WINDIR", r"C:\Windows")) / "Fonts"
        # Prefer UI-oriented CJK fonts. Emoji-like glyphs are deliberately not
        # used in the custom picker, so a stable Chinese UI font is enough.
        candidates.extend([
            win / "msyh.ttc",       # Microsoft YaHei
            win / "msyh.ttf",
            win / "msyhbd.ttc",
            win / "Microsoft YaHei UI.ttf",
            win / "simhei.ttf",
            win / "simsun.ttc",
            win / "Deng.ttf",
            win / "Dengb.ttf",
        ])
    elif sys.platform == "darwin":
        candidates.extend([
            Path("/System/Library/Fonts/PingFang.ttc"),
            Path("/System/Library/Fonts/STHeiti Light.ttc"),
            Path("/System/Library/Fonts/Supplemental/Songti.ttc"),
        ])
    else:
        candidates.extend([
            Path("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
            Path("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc"),
            Path("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"),
            Path("/usr/share/fonts/truetype/arphic/uming.ttc"),
        ])
    for candidate in candidates:
        if candidate.exists():
            return str(candidate)
    return None


def register_ui_font() -> str:
    font_path = find_cjk_font()
    if font_path:
        try:
            LabelBase.register(name="RUDP_UI", fn_regular=font_path)
            return "RUDP_UI"
        except Exception:
            pass
    return DEFAULT_FONT


UI_FONT = register_ui_font()


# Centralized UI theme. Kivy color values are RGBA floats in the range 0..1.
# Change these values if you want a different visual style.
THEME = {
    # Graphite Blue: restrained blue-gray UI with small-area accent color.
    "window_bg": (0.961, 0.969, 0.980, 1),
    "panel_bg": (1.000, 1.000, 1.000, 1),
    "primary": (0.247, 0.498, 0.659, 1),
    "primary_active": (0.208, 0.424, 0.561, 1),
    "secondary": (0.933, 0.949, 0.965, 1),
    "secondary_active": (0.843, 0.902, 0.949, 1),
    "success": (0.298, 0.576, 0.384, 1),
    "success_active": (0.247, 0.490, 0.325, 1),
    "danger": (0.714, 0.294, 0.294, 1),
    "danger_active": (0.624, 0.247, 0.247, 1),
    "text": (0.125, 0.141, 0.165, 1),
    "muted_text": (0.373, 0.420, 0.478, 1),
    "on_primary": (1.000, 1.000, 1.000, 1),
    "on_secondary": (0.125, 0.141, 0.165, 1),
    "input_bg": (0.933, 0.949, 0.965, 1),
    "input_text": (0.125, 0.141, 0.165, 1),
    "input_cursor": (0.247, 0.498, 0.659, 1),
    "log_bg": (0.933, 0.949, 0.965, 1),
    "log_text": (0.125, 0.141, 0.165, 1),
    "disabled": (0.576, 0.627, 0.678, 1),
    "bubble_mine": (0.918, 0.945, 0.969, 1),
    "bubble_other": (1.000, 1.000, 1.000, 1),
    "menu_bg": (1.000, 1.000, 1.000, 1),
    "menu_hover": (0.933, 0.949, 0.965, 1),
    "menu_danger_text": (0.714, 0.294, 0.294, 1),
}

_BUTTON_ROLES = {
    "primary": ("primary", "on_primary"),
    "active": ("primary_active", "on_primary"),
    "secondary": ("secondary", "on_secondary"),
    "success": ("success", "on_primary"),
    "danger": ("danger", "on_primary"),
    "input": ("input_bg", "input_text"),
}


def modern_button_style(role: str) -> Dict[str, object]:
    if ui_component_color is None:
        return {}
    role_name = str(role or "secondary").strip().lower()
    if role_name in ("primary", "active", "success"):
        return {
            "bg_normal": ui_component_color("accent"),
            "bg_hover": ui_component_color("accent_hover"),
            "bg_down": ui_component_color("accent_hover"),
            "text_normal": ui_component_color("white"),
            "text_down": ui_component_color("white"),
            "border_color": ui_component_color("accent"),
        }
    if role_name in ("danger", "destructive"):
        return {
            "bg_normal": ui_component_color("danger_soft"),
            "bg_hover": ui_component_color("danger_soft"),
            "bg_down": ui_component_color("danger_soft"),
            "text_normal": ui_component_color("danger"),
            "text_down": ui_component_color("danger"),
            "border_color": ui_component_color("border"),
        }
    if role_name == "ghost":
        return {
            "bg_normal": ui_component_color("transparent"),
            "bg_hover": ui_component_color("surface_muted"),
            "bg_down": ui_component_color("accent_soft"),
            "text_normal": ui_component_color("text_secondary"),
            "text_down": ui_component_color("text_primary"),
            "border_color": ui_component_color("transparent"),
        }
    return {
        "bg_normal": ui_component_color("surface_muted"),
        "bg_hover": ui_component_color("accent_soft"),
        "bg_down": ui_component_color("accent_soft"),
        "text_normal": ui_component_color("text_primary"),
        "text_down": ui_component_color("text_primary"),
        "border_color": ui_component_color("border"),
    }


def style_button(button: Button, role: str = "secondary") -> Button:
    if UIRoundedButton is not None and isinstance(button, UIRoundedButton):
        try:
            for name, value in modern_button_style(role).items():
                setattr(button, name, value)
            button._refresh_button_state(animated=False)
            return button
        except Exception:
            pass
    bg_key, fg_key = _BUTTON_ROLES.get(role, _BUTTON_ROLES["secondary"])
    button.font_name = UI_FONT
    button.background_normal = ""
    button.background_down = ""
    button.background_disabled_normal = ""
    button.background_color = THEME[bg_key]
    button.color = THEME[fg_key]
    return button


def make_button(role: str = "secondary", **kwargs) -> Button:
    kwargs.setdefault("font_name", UI_FONT)
    btn = Button(**kwargs)
    return style_button(btn, role)


def make_input(**kwargs) -> TextInput:
    kwargs.setdefault("font_name", UI_FONT)
    kwargs.setdefault("background_color", THEME["input_bg"])
    kwargs.setdefault("foreground_color", THEME["input_text"])
    kwargs.setdefault("cursor_color", THEME["input_cursor"])
    kwargs.setdefault("selection_color", (0.10, 0.35, 0.72, 0.24))
    return TextInput(**kwargs)


def make_label(**kwargs) -> Label:
    kwargs.setdefault("font_name", UI_FONT)
    kwargs.setdefault("color", THEME["text"])
    return Label(**kwargs)


class ThemedSpinnerOption(SpinnerOption):
    """Low-noise Spinner dropdown row that follows the active UI theme."""

    def __init__(self, **kwargs):
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("size_hint_y", None)
        kwargs.setdefault("height", dp(38))
        super().__init__(**kwargs)
        self.background_normal = ""
        self.background_down = ""
        self.background_disabled_normal = ""
        self.color = ui_component_color("text_primary") if ui_component_color is not None else THEME["text"]
        self.background_color = ui_component_color("surface") if ui_component_color is not None else THEME["panel_bg"]
        self.bind(state=lambda *_: self._sync_theme_state())
        self._sync_theme_state()

    def _sync_theme_state(self) -> None:
        try:
            if self.disabled:
                self.color = ui_component_color("text_muted") if ui_component_color is not None else THEME["disabled"]
                self.background_color = ui_component_color("surface_muted") if ui_component_color is not None else THEME["secondary"]
                return
            self.color = ui_component_color("text_primary") if ui_component_color is not None else THEME["text"]
            if self.state == "down":
                self.background_color = ui_component_color("accent_soft") if ui_component_color is not None else THEME["secondary_active"]
            else:
                self.background_color = ui_component_color("surface") if ui_component_color is not None else THEME["panel_bg"]
        except Exception:
            pass


def style_spinner(spinner: Spinner) -> Spinner:
    spinner.font_name = UI_FONT
    spinner.option_cls = ThemedSpinnerOption
    spinner.background_normal = ""
    spinner.background_down = ""
    spinner.background_disabled_normal = ""
    spinner.background_color = ui_component_color("surface_muted") if ui_component_color is not None else THEME["input_bg"]
    spinner.color = ui_component_color("text_primary") if ui_component_color is not None else THEME["input_text"]
    return spinner


def apply_ui_font(widget) -> None:
    if hasattr(widget, "font_name"):
        try:
            widget.font_name = UI_FONT
        except Exception:
            pass
    if hasattr(widget, "title_font"):
        try:
            widget.title_font = UI_FONT
        except Exception:
            pass
    for child in getattr(widget, "children", []) or []:
        apply_ui_font(child)


def style_popup(popup: Popup) -> Popup:
    popup.title_font = UI_FONT
    try:
        popup.background = ""
    except Exception:
        pass
    try:
        popup.background_color = ui_component_color("background") if ui_component_color is not None else THEME["window_bg"]
    except Exception:
        pass
    try:
        popup.separator_color = ui_component_color("border_soft") if ui_component_color is not None else THEME["secondary_active"]
    except Exception:
        pass
    try:
        popup.title_color = ui_component_color("text_primary") if ui_component_color is not None else THEME["text"]
    except Exception:
        pass
    try:
        popup.separator_height = dp(1)
    except Exception:
        pass
    return popup


def apply_card_background(widget, bg_key: str = "panel_bg", radius: int = 18) -> None:
    try:
        color = THEME.get(bg_key, THEME["panel_bg"])
        with widget.canvas.before:
            widget._bg_instr = Color(*color)
            widget._bg_rect = RoundedRectangle(pos=widget.pos, size=widget.size, radius=[radius, radius, radius, radius])
        def _sync(*_args):
            try:
                widget._bg_rect.pos = widget.pos
                widget._bg_rect.size = widget.size
            except Exception:
                pass
        widget.bind(pos=_sync, size=_sync)
    except Exception:
        pass


def apply_bubble_background(widget, bg_key: str, mine: bool, radius: int = 16) -> None:
    """Rounded chat bubble with a small WhatsApp-like tail."""
    try:
        color = THEME.get(bg_key, THEME["panel_bg"])
        with widget.canvas.before:
            widget._bg_instr = Color(*color)
            widget._bg_rect = RoundedRectangle(pos=widget.pos, size=widget.size, radius=[radius, radius, radius, radius])
            widget._tail = Triangle(points=[0, 0, 0, 0, 0, 0])
        def _sync(*_args):
            try:
                x, y = widget.pos
                w, h = widget.size
                widget._bg_rect.pos = (x, y)
                widget._bg_rect.size = (w, h)
                if mine:
                    widget._tail.points = [x + w - dp(2), y + h - dp(18), x + w + dp(10), y + h - dp(12), x + w - dp(2), y + h - dp(6)]
                else:
                    widget._tail.points = [x + dp(2), y + h - dp(18), x - dp(10), y + h - dp(12), x + dp(2), y + h - dp(6)]
            except Exception:
                pass
        widget.bind(pos=_sync, size=_sync)
    except Exception:
        apply_card_background(widget, bg_key, radius=radius)


def bind_label_wrap(label: Label) -> Label:
    label.bind(size=lambda inst, val: setattr(inst, "text_size", val))
    return label


def row(label: str, widget, label_width: int = 150) -> BoxLayout:
    box = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(8))
    lab = Label(
        text=label,
        font_name=UI_FONT,
        color=THEME["muted_text"],
        size_hint_x=None,
        width=dp(label_width),
        halign="right",
        valign="middle",
        shorten=True,
        shorten_from="right",
    )
    bind_label_wrap(lab)
    box.add_widget(lab)
    box.add_widget(widget)
    return box


class WorkerProcess:
    def __init__(self, role: str, log_callback, exit_callback, progress_callback=None):
        self.role = role
        self.log_callback = log_callback
        self.exit_callback = exit_callback
        self.progress_callback = progress_callback
        self.proc: Optional[subprocess.Popen] = None
        self.reader_thread: Optional[threading.Thread] = None

    def is_running(self) -> bool:
        return self.proc is not None and self.proc.poll() is None

    def start(self, args: List[str]) -> None:
        if self.is_running():
            self.log_callback("Process is already running.\n")
            return
        cmd = self._base_cmd() + ["--worker", self.role] + args
        env = os.environ.copy()
        env.setdefault("PYTHONUTF8", "1")
        env.setdefault("PYTHONIOENCODING", "utf-8")
        kwargs = dict(
            cwd=str(user_data_dir()),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
            env=env,
        )
        if IS_WINDOWS:
            kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
        self.proc = popen_no_console(cmd, **kwargs)
        self.reader_thread = threading.Thread(target=self._reader, daemon=True)
        self.reader_thread.start()

    def _base_cmd(self) -> List[str]:
        if FROZEN:
            return [sys.executable]
        return [sys.executable, str(APP_DIR / "main_kivy.py")]

    def _reader(self) -> None:
        assert self.proc is not None
        try:
            for line in self.proc.stdout or []:
                append_worker_debug_log(self.role, line)
                self.log_callback(line)
                if self.progress_callback is not None:
                    self._try_progress(line)
        finally:
            rc = self.proc.wait() if self.proc is not None else None
            Clock.schedule_once(lambda _dt, result=rc: self.exit_callback(result), 0)

    def _try_progress(self, line: str) -> None:
        m = PROGRESS_RE.search(line)
        if m:
            try:
                self.progress_callback({
                    "sent": int(m.group("sent")),
                    "total": int(m.group("total")),
                    "pct": float(m.group("pct")),
                    "avg": float(m.group("avg")),
                    "eta": m.group("eta"),
                    "complete": False,
                })
            except Exception:
                pass
            return
        m = COMPLETE_RE.search(line)
        if m:
            try:
                total = int(m.group("total"))
                self.progress_callback({
                    "sent": total,
                    "total": total,
                    "pct": 100.0,
                    "avg": float(m.group("avg")),
                    "eta": "0:00",
                    "complete": True,
                })
            except Exception:
                pass

    def stop(self) -> None:
        if not self.is_running():
            return
        assert self.proc is not None
        try:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3.0)
            except Exception:
                try:
                    self.proc.kill()
                except Exception:
                    pass
        except Exception:
            pass


class LogBox(BoxLayout):
    MAX_LINES = 1000

    def __init__(self, **kwargs):
        super().__init__(orientation="vertical", **kwargs)
        self.text = make_input(readonly=True, multiline=True, size_hint_y=1, background_color=THEME["log_bg"], foreground_color=THEME["log_text"], cursor_color=THEME["log_text"])
        self._lines: List[str] = []
        self.add_widget(self.text)

    def append(self, s: str) -> None:
        raw = str(s or "")
        if not raw:
            return
        def _append(_dt):
            try:
                parts = raw.splitlines()
                if raw.endswith("\n"):
                    parts.append("")
                self._lines.extend(parts)
                if len(self._lines) > self.MAX_LINES:
                    self._lines = self._lines[-self.MAX_LINES:]
                self.text.text = "\n".join(self._lines)
                self.text.cursor = (0, len(self._lines))
            except Exception:
                pass
        Clock.schedule_once(_append, 0)

    def clear(self) -> None:
        self._lines = []
        self.text.text = ""


def open_file_location(path: str) -> None:
    try:
        pth = Path(str(path or "")).expanduser()
        target = pth if pth.is_dir() else pth.parent
        if not target.exists():
            return
        if IS_WINDOWS:
            os.startfile(str(target))  # type: ignore[attr-defined]
        elif sys.platform == "darwin":
            subprocess.Popen(["open", str(target)])
        else:
            subprocess.Popen(["xdg-open", str(target)])
    except Exception:
        pass


class CircularProgressButton(Button):
    def __init__(self, *, pct: float = 0.0, failed: bool = False, complete: bool = False, **kwargs):
        kwargs.setdefault("text", "↻" if failed else ("✓" if complete else f"{int(max(0, min(100, pct)))}%"))
        kwargs.setdefault("font_name", UI_FONT)
        kwargs.setdefault("size_hint", (None, None))
        kwargs.setdefault("size", (dp(46), dp(46)))
        super().__init__(**kwargs)
        self.pct = max(0.0, min(100.0, float(pct or 0.0)))
        self.failed = bool(failed)
        self.complete = bool(complete)
        self.background_normal = ""
        self.background_down = ""
        self.background_color = (0, 0, 0, 0)
        self.color = THEME["danger"] if self.failed else (THEME["primary_active"] if self.complete or self.pct > 0 else THEME["muted_text"])
        self.bold = True
        self._draw()
        self.bind(pos=lambda *_: self._draw(), size=lambda *_: self._draw())

    def _draw(self) -> None:
        try:
            self.canvas.before.clear()
            with self.canvas.before:
                cx = self.x + self.width / 2.0
                cy = self.y + self.height / 2.0
                r = min(self.width, self.height) / 2.0 - dp(4)
                Color(*THEME["secondary_active"])
                Line(circle=(cx, cy, r, 0, 360), width=dp(2))
                if self.failed:
                    Color(*THEME["danger"])
                    Line(circle=(cx, cy, r, 0, 360), width=dp(2.4))
                else:
                    Color(*(THEME["primary_active"] if self.complete or self.pct >= 100 else THEME["primary"]))
                    end = max(1.0, 360.0 * self.pct / 100.0)
                    Line(circle=(cx, cy, r, 90, 90 - end), width=dp(2.4))
        except Exception:
            pass



class MixedPickerRow(RecycleDataViewBehavior, BoxLayout):
    full_path = StringProperty("")
    display_name = StringProperty("")
    type_text = StringProperty("")
    size_text = StringProperty("")
    modified_text = StringProperty("")
    attr_text = StringProperty("")
    icon_text = StringProperty("")
    thumb_source = StringProperty("")
    selected = BooleanProperty(False)
    is_dir = BooleanProperty(False)
    is_previewable = BooleanProperty(False)
    on_toggle = ObjectProperty(None, allownone=True)
    on_open = ObjectProperty(None, allownone=True)
    on_request_thumb = ObjectProperty(None, allownone=True)
    on_preview = ObjectProperty(None, allownone=True)

    def __init__(self, **kwargs):
        super().__init__(orientation="horizontal", size_hint_y=None, height=dp(34), spacing=dp(4), padding=(0, dp(2), 0, dp(2)), **kwargs)
        self._last_thumb_source = ""
        self.select_btn = make_button("secondary", text="选择", size_hint_x=None, width=dp(52))
        self.preview_box = FloatLayout(size_hint_x=None, width=dp(38))
        self.preview_img = Image(source="", fit_mode="contain", allow_stretch=True, keep_ratio=True)
        self.preview_label = make_label(text="", halign="center", valign="middle", font_size="10sp", bold=True, color=THEME["muted_text"])
        bind_label_wrap(self.preview_label)
        self.preview_box.add_widget(self.preview_img)
        self.preview_box.add_widget(self.preview_label)
        self.name_btn = make_label(text="", halign="left", valign="middle", size_hint_x=1, color=THEME["text"])
        self.name_btn.shorten = True
        self.name_btn.shorten_from = "right"
        self.name_btn.bind(width=lambda inst, _val: setattr(inst, "text_size", (max(1, inst.width - dp(12)), None)))
        self.size_label = make_label(text="", size_hint_x=None, width=dp(92), halign="right", valign="middle", color=THEME["muted_text"])
        self.type_label = make_label(text="", size_hint_x=None, width=dp(88), halign="left", valign="middle", color=THEME["muted_text"])
        self.modified_label = make_label(text="", size_hint_x=None, width=dp(138), halign="left", valign="middle", color=THEME["muted_text"])
        self.attr_label = make_label(text="", size_hint_x=None, width=dp(58), halign="left", valign="middle", color=THEME["muted_text"])
        self.open_btn = make_button("secondary", text="进入", size_hint_x=None, width=dp(58))
        self.preview_btn = make_button("secondary", text="预览", size_hint_x=None, width=dp(64))
        for lab in (self.type_label, self.size_label, self.modified_label, self.attr_label):
            bind_label_wrap(lab)
        self.preview_box.bind(pos=lambda *_: self._layout_preview(), size=lambda *_: self._layout_preview())
        self.select_btn.bind(on_release=lambda *_: self._toggle())
        self.name_btn.bind(on_touch_down=self._touch_name)
        self.open_btn.bind(on_release=lambda *_: self._open())
        self.preview_btn.bind(on_release=lambda *_: self._preview())
        self.add_widget(self.select_btn)
        self.add_widget(self.preview_box)
        self.add_widget(self.name_btn)
        self.add_widget(self.size_label)
        self.add_widget(self.type_label)
        self.add_widget(self.modified_label)
        self.add_widget(self.attr_label)
        self.add_widget(self.open_btn)
        self.add_widget(self.preview_btn)
        self._sync()

    def _layout_preview(self):
        try:
            self.preview_img.pos = self.preview_box.pos
            self.preview_img.size = self.preview_box.size
            self.preview_label.pos = self.preview_box.pos
            self.preview_label.size = self.preview_box.size
            self.preview_label.text_size = self.preview_box.size
        except Exception:
            pass

    def refresh_view_attrs(self, rv, index, data):
        ret = super().refresh_view_attrs(rv, index, data)
        self._sync()
        return ret

    def _sync(self):
        try:
            self.select_btn.text = "已选" if self.selected else "选择"
            style_button(self.select_btn, "primary" if self.selected else "secondary")
            raw_name = str(self.display_name or "")
            prefix = "" if raw_name == ".." else ("[文件夹] " if self.is_dir else "")
            self.name_btn.text = prefix + truncate_filename(raw_name, 72)
            self.name_btn.text_size = (max(1, self.name_btn.width - dp(12)), None)
            self.type_label.text = str(self.type_text or ("文件夹" if self.is_dir else "文件"))
            self.size_label.text = str(self.size_text or "")
            self.modified_label.text = str(self.modified_text or "")
            self.attr_label.text = str(self.attr_text or "")
            self.open_btn.disabled = not bool(self.is_dir)
            self.open_btn.opacity = 1.0 if self.is_dir else 0.25
            self.preview_btn.disabled = not bool(self.is_previewable)
            self.preview_btn.opacity = 1.0 if self.is_previewable else 0.25
            self.name_btn.font_name = UI_FONT
            self.select_btn.font_name = UI_FONT
            self.open_btn.font_name = UI_FONT
            self.preview_btn.font_name = UI_FONT
            self.preview_label.font_name = UI_FONT
            source = str(self.thumb_source or "")
            if source and Path(source).exists():
                if source != self._last_thumb_source:
                    self.preview_img.source = source
                    self.preview_img.reload()
                    self._last_thumb_source = source
                self.preview_img.opacity = 1.0
                self.preview_label.opacity = 0.0
                self.preview_label.text = ""
            else:
                if self._last_thumb_source:
                    self.preview_img.source = ""
                    self._last_thumb_source = ""
                self.preview_img.opacity = 0.0
                self.preview_label.opacity = 1.0
                self.preview_label.text = str(self.icon_text or ("DIR" if self.is_dir else "FILE"))
            self._layout_preview()
        except Exception:
            pass

    def _toggle(self):
        cb = self.on_toggle
        if callable(cb):
            cb(str(self.full_path or ""))

    def _touch_name(self, _inst, touch):
        try:
            if self.name_btn.collide_point(*touch.pos):
                self._toggle()
                return True
        except Exception:
            pass
        return False

    def _open(self):
        cb = self.on_open
        if callable(cb):
            cb(str(self.full_path or ""))

    def _preview(self):
        cb = self.on_preview
        if callable(cb):
            cb(str(self.full_path or ""))


class ChatMessageBox(BoxLayout):
    def __init__(self, root_owner=None, **kwargs):
        super().__init__(orientation="vertical", **kwargs)
        self.root_owner = root_owner
        self.scroll = ScrollView(size_hint_y=1)
        self.inner = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(4), padding=(dp(10), dp(8), dp(10), dp(8)))
        self.inner.bind(minimum_height=self.inner.setter("height"))
        self.scroll.add_widget(self.inner)
        self.add_widget(self.scroll)
        self.card_widgets: Dict[str, Dict[str, object]] = {}

    def _cu(self, key: str, **kwargs) -> str:
        owner = getattr(self, "root_owner", None)
        if owner is not None and hasattr(owner, "cu"):
            return owner.cu(key, **kwargs)
        lang = "zh"
        value = CHAT_UI_TEXT.get(lang, {}).get(key, key)
        return value.format(**kwargs) if kwargs else value

    def append(self, s: str) -> None:
        def _append(_dt):
            text = str(s or "").rstrip("\n")
            if not text:
                return
            lab = make_label(text=text, halign="left", valign="top", size_hint_y=None, color=THEME["log_text"])
            bind_label_wrap(lab)
            lab.height = dp(24 + 18 * max(1, text.count("\n") + 1))
            self.inner.add_widget(lab)
            self.scroll.scroll_y = 0
        Clock.schedule_once(_append, 0)

    def clear(self) -> None:
        self.inner.clear_widgets()
        self.card_widgets.clear()

    def _card_body_text(self, status: str, detail: str) -> str:
        return "  ".join([part for part in (status, detail) if part]) or status or detail or "-"

    def _card_side(self, data: Dict[str, object]) -> str:
        meta = dict((data or {}).get("meta") or {})
        raw = str(
            data.get("side")
            or data.get("direction")
            or meta.get("side")
            or meta.get("direction")
            or ""
        ).strip().lower()
        if raw in ("outgoing", "right", "mine", "sent"):
            return "outgoing"
        if raw in ("system", "center", "middle"):
            return "system"
        return "incoming"

    def _populate_card_actions(self, action_row: BoxLayout, actions: List[Dict[str, object]], card_id: str) -> None:
        action_row.clear_widgets()
        owner = getattr(self, "root_owner", None)
        for action in actions[:3]:
            label = str(action.get("label") or "")
            style = str(action.get("style") or "secondary")
            action_id = str(action.get("action") or "")
            btn = self._make_card_action_button(style, label)
            btn.disabled = not bool(action_id)
            if owner is not None and hasattr(owner, "handle_chat_card_action") and action_id:
                btn.bind(on_release=lambda _btn, cid=card_id, aid=action_id: owner.handle_chat_card_action(cid, aid))
            action_row.add_widget(btn)

    def _make_card_action_button(self, style: str, label: str):
        if UIRoundedButton is not None and ui_component_color is not None:
            try:
                style_name = str(style or "secondary").strip().lower()
                kwargs = {
                    "text": str(label or ""),
                    "size_hint_y": None,
                    "height": dp(28),
                }
                if style_name in ("primary", "accent", "success"):
                    kwargs.update(
                        bg_normal=ui_component_color("accent"),
                        bg_hover=ui_component_color("accent_hover"),
                        bg_down=ui_component_color("accent_hover"),
                        text_normal=ui_component_color("white"),
                        text_down=ui_component_color("white"),
                        border_color=ui_component_color("accent"),
                    )
                elif style_name in ("danger", "destructive", "reject"):
                    kwargs.update(
                        bg_normal=ui_component_color("surface_muted"),
                        bg_hover=ui_component_color("danger_soft"),
                        bg_down=ui_component_color("danger_soft"),
                        text_normal=ui_component_color("danger"),
                        text_down=ui_component_color("danger"),
                        border_color=ui_component_color("border"),
                    )
                return UIRoundedButton(**kwargs)
            except Exception:
                pass
        return make_button(style, text=label, size_hint_y=None, height=dp(28))

    def _modern_card_status_kind(self, status: str, detail: str) -> str:
        text = f"{status} {detail}".lower()
        if any(token in text for token in ("fail", "error", "reject", "denied", "失败", "拒绝")):
            return "failed"
        if any(token in text for token in ("complete", "completed", "saved", "success", "完成", "已完成")):
            return "success"
        if any(token in text for token in ("wait", "pending", "confirm", "queued", "等待", "确认")):
            return "waiting"
        if any(token in text for token in ("send", "transfer", "receive", "watch", "screen", "active", "start", "传输", "投屏", "观看", "启动")):
            return "accent"
        return "neutral"

    def _modern_card_progress(self, data: Dict[str, object], status: str, detail: str) -> float:
        meta = dict((data or {}).get("meta") or {})
        for key in ("progress", "pct", "percent"):
            try:
                value = data.get(key, meta.get(key))
                if value not in (None, ""):
                    return max(0.0, min(100.0, float(value)))
            except Exception:
                pass
        text = f"{status} {detail}"
        match = re.search(r"([0-9]+(?:\.[0-9]+)?)\s*%", text)
        if match:
            try:
                return max(0.0, min(100.0, float(match.group(1))))
            except Exception:
                pass
        if self._modern_card_status_kind(status, detail) == "success":
            return 100.0
        return 0.0

    def _modern_card_action_callback(self, card_id: str):
        owner = getattr(self, "root_owner", None)

        def _dispatch(action_id: str) -> None:
            if owner is not None and hasattr(owner, "handle_chat_card_action"):
                owner.handle_chat_card_action(card_id, action_id)

        return _dispatch

    def _modern_card_available(self, card_type: str) -> bool:
        if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER):
            return UIFileTransferCard is not None
        if card_type in (CARD_SCREEN_OFFER, CARD_SCREEN_STATE):
            return UIScreenShareCard is not None
        return False

    def _create_modern_card_widget(
        self,
        data: Dict[str, object],
        *,
        card_type: str,
        title: str,
        subtitle: str,
        status: str,
        detail: str,
        actions: List[Dict[str, object]],
        card_id: str,
    ):
        kind = self._modern_card_status_kind(status, detail)
        callback = self._modern_card_action_callback(card_id)
        if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER):
            return UIFileTransferCard(
                title=title or "File",
                filename=subtitle or title or "File",
                detail=detail or self._card_body_text(status, detail),
                status_text=status or "-",
                status=kind,
                progress=self._modern_card_progress(data, status, detail),
                actions=actions,
                on_action=callback,
                size_hint_x=None,
                width=dp(420),
            )
        return UIScreenShareCard(
            title=title or "Screen",
            peer=subtitle or "",
            detail=detail or self._card_body_text(status, detail),
            status_text=status or "-",
            status=kind,
            actions=actions,
            on_action=callback,
            size_hint_x=None,
            width=dp(420),
        )

    def _sync_modern_line_height(self, line, card_widget) -> None:
        try:
            line.height = max(dp(94), float(card_widget.height or card_widget.minimum_height or dp(84)) + dp(10))
        except Exception:
            line.height = dp(116)

    def _add_modern_card(self, data: Dict[str, object]) -> bool:
        card_type = str(data.get("card_type") or data.get("type") or CARD_SYSTEM)
        if not self._modern_card_available(card_type):
            return False
        title = str(data.get("title") or "")
        subtitle = str(data.get("subtitle") or "")
        status = str(data.get("status") or "")
        detail = str(data.get("detail") or "")
        actions = [dict(item) for item in (data.get("actions") or []) if isinstance(item, dict)]
        card_id = str(data.get("card_id") or "")
        try:
            card_widget = self._create_modern_card_widget(
                data,
                card_type=card_type,
                title=title,
                subtitle=subtitle,
                status=status,
                detail=detail,
                actions=actions,
                card_id=card_id,
            )
            if float(card_widget.width or 0) <= 0 or float(card_widget.height or card_widget.minimum_height or 0) <= 0:
                return False
            side = self._card_side(data)
            line = BoxLayout(orientation="horizontal", size_hint_y=None, padding=(0, dp(4), 0, dp(4)))
            self._sync_modern_line_height(line, card_widget)
            card_widget.bind(height=lambda inst, _value, row=line: self._sync_modern_line_height(row, inst))
            if side in ("outgoing", "system"):
                line.add_widget(BoxLayout(size_hint_x=1))
            else:
                line.add_widget(BoxLayout(size_hint_x=None, width=dp(8)))
            line.add_widget(card_widget)
            if side == "system":
                line.add_widget(BoxLayout(size_hint_x=1))
            elif side == "outgoing":
                line.add_widget(BoxLayout(size_hint_x=None, width=dp(8)))
            else:
                line.add_widget(BoxLayout(size_hint_x=1))
            self.inner.add_widget(line)
            if card_id:
                self.card_widgets[card_id] = {
                    "line": line,
                    "card_box": card_widget,
                    "modern_card": card_widget,
                    "side": side,
                    "card_type": card_type,
                }
            self.scroll.scroll_y = 0
            return True
        except Exception:
            return False

    def _update_modern_card(self, widgets: Dict[str, object], data: Dict[str, object]) -> bool:
        card_widget = widgets.get("modern_card")
        if card_widget is None:
            return False
        card_type = str(data.get("card_type") or data.get("type") or CARD_SYSTEM)
        title = str(data.get("title") or "")
        subtitle = str(data.get("subtitle") or "")
        status = str(data.get("status") or "")
        detail = str(data.get("detail") or "")
        actions = [dict(item) for item in (data.get("actions") or []) if isinstance(item, dict)]
        card_id = str(data.get("card_id") or "")
        try:
            if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER) and UIFileTransferCard is not None:
                card_widget.title = title or "File"
                card_widget.filename = subtitle or title or "File"
                card_widget.detail = detail or self._card_body_text(status, detail)
                card_widget.status_text = status or "-"
                card_widget.status = self._modern_card_status_kind(status, detail)
                card_widget.progress = self._modern_card_progress(data, status, detail)
            elif card_type in (CARD_SCREEN_OFFER, CARD_SCREEN_STATE) and UIScreenShareCard is not None:
                card_widget.title = title or "Screen"
                card_widget.peer = subtitle or ""
                card_widget.detail = detail or self._card_body_text(status, detail)
                card_widget.status_text = status or "-"
                card_widget.status = self._modern_card_status_kind(status, detail)
            else:
                return False
            if hasattr(card_widget, "set_actions"):
                card_widget.set_actions(actions, on_action=self._modern_card_action_callback(card_id))
            line = widgets.get("line")
            if line is not None:
                self._sync_modern_line_height(line, card_widget)
            return True
        except Exception:
            return False

    def update_card(self, card: Dict[str, object]) -> None:
        data = dict(card or {})
        card_id = str(data.get("card_id") or "")
        widgets = self.card_widgets.get(card_id)
        if not card_id or not widgets:
            self.add_card(data)
            return
        if self._update_modern_card(widgets, data):
            return
        title = str(data.get("title") or "")
        subtitle = str(data.get("subtitle") or "")
        status = str(data.get("status") or "")
        detail = str(data.get("detail") or "")
        actions = [dict(item) for item in (data.get("actions") or []) if isinstance(item, dict)]
        title_lab = widgets.get("title")
        subtitle_lab = widgets.get("subtitle")
        body_lab = widgets.get("body")
        if title_lab is not None:
            title_lab.text = shorten_middle(title or str(widgets.get("badge") or "INFO"), 44)
        if subtitle_lab is not None:
            subtitle_lab.text = shorten_middle(subtitle, 58)
            subtitle_lab.opacity = 1.0 if subtitle else 0.0
        if body_lab is not None:
            body_lab.text = shorten_middle(self._card_body_text(status, detail), 76)
        line = widgets.get("line")
        card_box = widgets.get("card_box")
        action_row = widgets.get("action_row")
        has_actions = bool(actions)
        if line is not None:
            line.height = dp(132 if has_actions else 106)
        if action_row is not None and card_box is not None:
            if has_actions:
                self._populate_card_actions(action_row, actions, card_id)
                if getattr(action_row, "parent", None) is None:
                    card_box.add_widget(action_row)
            elif getattr(action_row, "parent", None) is not None:
                card_box.remove_widget(action_row)

    def remove_card(self, card_id: str) -> None:
        cid = str(card_id or "")
        widgets = self.card_widgets.pop(cid, None)
        line = widgets.get("line") if widgets else None
        if line is not None and getattr(line, "parent", None) is self.inner:
            self.inner.remove_widget(line)

    def add_date_separator(self, label: str) -> None:
        line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(30), padding=(0, dp(4), 0, dp(4)))
        line.add_widget(BoxLayout())
        chip = BoxLayout(orientation="vertical", size_hint_x=None, width=dp(112), padding=(dp(8), dp(3), dp(8), dp(3)))
        apply_card_background(chip, "secondary", radius=14)
        lab = make_label(text=str(label or ""), halign="center", valign="middle", color=THEME["muted_text"], size_hint_y=None, height=dp(18))
        bind_label_wrap(lab)
        chip.add_widget(lab)
        line.add_widget(chip)
        line.add_widget(BoxLayout())
        self.inner.add_widget(line)

    def add_card(self, card: Dict[str, object]) -> None:
        data = dict(card or {})
        card_type = str(data.get("card_type") or data.get("type") or CARD_SYSTEM)
        title = str(data.get("title") or "")
        subtitle = str(data.get("subtitle") or "")
        status = str(data.get("status") or "")
        detail = str(data.get("detail") or "")
        actions = [dict(item) for item in (data.get("actions") or []) if isinstance(item, dict)]
        card_id = str(data.get("card_id") or "")

        if card_type == CARD_SYSTEM:
            text = detail or title or subtitle or status
            line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(34), padding=(0, dp(4), 0, dp(4)))
            line.add_widget(BoxLayout())
            chip_width = dp(min(520, max(180, 84 + len(text) * 7)))
            chip = BoxLayout(orientation="vertical", size_hint_x=None, width=chip_width, padding=(dp(12), dp(3), dp(12), dp(3)))
            apply_card_background(chip, "secondary", radius=12)
            lab = make_label(text=shorten_middle(text, 70), halign="center", valign="middle", color=THEME["muted_text"], size_hint_y=None, height=dp(20))
            bind_label_wrap(lab)
            chip.add_widget(lab)
            line.add_widget(chip)
            line.add_widget(BoxLayout())
            self.inner.add_widget(line)
            self.scroll.scroll_y = 0
            return

        if self._add_modern_card(data):
            return

        has_actions = bool(actions)
        height = 132 if has_actions else 106
        side = self._card_side(data)
        line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(height), padding=(0, dp(4), 0, dp(4)))
        if side == "outgoing":
            line.add_widget(BoxLayout(size_hint_x=1))
        elif side == "system":
            line.add_widget(BoxLayout(size_hint_x=1))
        else:
            line.add_widget(BoxLayout(size_hint_x=None, width=dp(8)))
        card_box = BoxLayout(orientation="vertical", spacing=dp(5), padding=(dp(14), dp(10), dp(14), dp(10)), size_hint_x=None, width=dp(420))
        bg_style = "panel_bg" if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER) else "secondary"
        apply_card_background(card_box, bg_style, radius=12)
        title_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(24), spacing=dp(8))
        badge_text = {
            CARD_FILE_OFFER: "FILE",
            CARD_FILE_TRANSFER: "FILE",
            CARD_SCREEN_OFFER: "SCREEN",
            CARD_SCREEN_STATE: "SCREEN",
        }.get(card_type, "INFO")
        badge = make_label(text=badge_text, size_hint_x=None, width=dp(62), halign="center", valign="middle", color=THEME["primary_active"], bold=True)
        title_line.add_widget(badge)
        title_lab = make_label(text=shorten_middle(title or badge_text, 44), halign="left", valign="middle", color=THEME["text"], bold=True)
        title_lab.shorten = True
        title_lab.text_size = (dp(320), None)
        title_line.add_widget(title_lab)
        card_box.add_widget(title_line)
        sub_lab = None
        if subtitle:
            sub_lab = make_label(text=shorten_middle(subtitle, 58), size_hint_y=None, height=dp(20), halign="left", valign="middle", color=THEME["muted_text"])
            sub_lab.text_size = (dp(382), None)
            card_box.add_widget(sub_lab)
        body_lab = make_label(text=shorten_middle(self._card_body_text(status, detail), 76), size_hint_y=None, height=dp(28), halign="left", valign="middle", color=THEME["text"])
        body_lab.text_size = (dp(392), None)
        card_box.add_widget(body_lab)
        action_row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(28), spacing=dp(6))
        if has_actions:
            self._populate_card_actions(action_row, actions, card_id)
            card_box.add_widget(action_row)
        line.add_widget(card_box)
        if side == "outgoing":
            line.add_widget(BoxLayout(size_hint_x=None, width=dp(8)))
        elif side == "system":
            line.add_widget(BoxLayout(size_hint_x=1))
        else:
            line.add_widget(BoxLayout(size_hint_x=1))
        self.inner.add_widget(line)
        if card_id:
            self.card_widgets[card_id] = {
                "line": line,
                "card_box": card_box,
                "title": title_lab,
                "subtitle": sub_lab,
                "body": body_lab,
                "action_row": action_row,
                "badge": badge_text,
                "side": side,
            }
        self.scroll.scroll_y = 0

    def _status_state(self, summary: str, mine: bool) -> tuple[str, tuple]:
        if not mine:
            return "", THEME["muted_text"]
        s = str(summary or "").lower()
        if "transferring" in s or "sending_file" in s or "receiving_file" in s or "queued_file" in s:
            return "", THEME["muted_text"]
        if "failed" in s:
            return "!", THEME["danger"]
        m_read = re.search(r"read\s+(\d+)/(\d+)", s)
        if m_read:
            read = int(m_read.group(1)); total = int(m_read.group(2))
            if total > 0 and read >= total:
                return "√√", THEME["primary_active"]
        m_del = re.search(r"delivered\s+(\d+)/(\d+)", s)
        if m_del:
            delivered = int(m_del.group(1)); total = int(m_del.group(2))
            if total > 0 and delivered >= total:
                return "√√", THEME["primary_active"]
            return "√", THEME["primary_active"]
        m_sent = re.search(r"sent\s+(\d+)/(\d+)", s)
        if m_sent:
            return "√", THEME["primary_active"]
        return "√√", THEME["disabled"]

    def _add_footer(self, parent: BoxLayout, *, mine: bool, timestamp: str, summary: str) -> None:
        footer = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(18), spacing=dp(4))
        footer.add_widget(BoxLayout())
        footer.add_widget(make_label(text=str(timestamp or ""), size_hint_x=None, width=dp(38), halign="right", valign="middle", color=THEME["muted_text"]))
        icon, color = self._status_state(summary, mine)
        if icon:
            footer.add_widget(make_label(text=icon, size_hint_x=None, width=dp(22), halign="left", valign="middle", color=color, bold=True))
        parent.add_widget(footer)

    def _add_file_thumbnail(self, parent: BoxLayout, file_path: str, file_name: str) -> None:
        thumb = BoxLayout(size_hint_x=None, width=dp(62), padding=(dp(4), dp(4), dp(4), dp(4)))
        apply_card_background(thumb, "secondary_active", radius=12)
        source = file_path if file_path and os.path.exists(file_path) else ""
        if source and is_image_file_for_preview(source):
            try:
                img = Image(source=source, allow_stretch=True, keep_ratio=True)
                thumb.add_widget(img)
            except Exception:
                thumb.add_widget(make_label(text=file_type_badge(file_name), halign="center", valign="middle", bold=True, color=THEME["primary_active"]))
        else:
            lab = make_label(text=file_type_badge(file_name), halign="center", valign="middle", bold=True, color=THEME["primary_active"])
            bind_label_wrap(lab)
            thumb.add_widget(lab)
        parent.add_widget(thumb)

    def _file_progress_state(self, message_id: str, total_size: int, summary: str, file_path: str = "") -> tuple[float, bool, bool, str]:
        owner = getattr(self, "root_owner", None)
        mid = str(message_id or "")
        prog = getattr(owner, "file_message_progress", {}).get(mid, {}) if owner is not None else {}

        # Persistent transfer_store is the source of truth. The runtime dict is
        # only a fast UI cache and can lag behind completion on the receiver side.
        row = {}
        if owner is not None and getattr(owner, "file_transfer_service", None) is not None:
            try:
                row = owner.file_transfer_service.progress_for_message(mid) or {}
            except Exception:
                row = {}

        if row:
            sent = int(row.get("transferred_bytes") or 0)
            total = int(row.get("total_bytes") or total_size or 0)
            pct = float(row.get("pct") or ((sent * 100.0 / total) if total else 0.0))
            state = str(row.get("status") or "")
            avg = row.get("avg_mbps")
            current = row.get("current_mbps")
            peak = row.get("peak_mbps")
            elapsed = row.get("elapsed_sec")
            eta = str(row.get("eta") or "")
        else:
            sent = int(prog.get("sent") or 0)
            total = int(prog.get("total") or total_size or 0)
            pct = float(prog.get("pct") or ((sent * 100.0 / total) if total else 0.0))
            state = str(prog.get("state") or "")
            avg = prog.get("avg")
            current = prog.get("current")
            peak = prog.get("peak")
            elapsed = prog.get("elapsed")
            eta = str(prog.get("eta") or "")

        # If the received file exists and is playable/openable, the visual card
        # must not remain stuck at an intermediate percentage.
        try:
            if file_path and os.path.exists(file_path):
                actual = os.path.getsize(file_path)
                if actual > 0 and (total <= 0 or actual >= total or pct >= 99.0):
                    total = max(total, actual)
                    sent = total
                    pct = 100.0
                    state = self._cu("received")
                    eta = "0:00"
        except Exception:
            pass

        failed = "failed" in str(summary or "").lower() or "失败" in state or "failed" in state.lower()
        complete = pct >= 99.9 or state in ("completed", "received", self._cu("completed"), self._cu("received")) or "read" in str(summary or "").lower()
        detail = file_progress_detail(
            sent=sent,
            total=total,
            pct=pct,
            avg=avg,
            current=current,
            peak=peak,
            elapsed=elapsed,
            eta=eta,
            complete=complete,
            unknown_size=self._cu("unknown_size"),
        )
        return max(0.0, min(100.0, pct)), bool(failed), bool(complete), detail

    def _modern_text_bubble_width(self, text: str) -> int:
        char_units = 0
        for ch in str(text or ""):
            if ch == "\n":
                char_units += 24
            else:
                char_units += 2 if ord(ch) > 127 else 1
        char_units = max(1, char_units)
        return max(132, min(420, 64 + min(char_units, 52) * 7))

    def _add_modern_text_message(
        self,
        *,
        mine: bool,
        sender: str,
        text: str,
        timestamp: str,
        summary: str = "",
        message_id: str = "",
        show_sender: bool = False,
    ) -> bool:
        if UIMessageBubble is None:
            return False
        try:
            raw_text = str(text or "")
            sender_text = str(sender or "").strip()
            show_sender_label = bool(show_sender and sender_text and not mine)
            bubble_width = self._modern_text_bubble_width(raw_text)
            line = BoxLayout(orientation="horizontal", size_hint_y=None, padding=(0, dp(4), 0, dp(4)))
            bubble = UIMessageBubble(
                direction="outgoing" if mine else "incoming",
                sender=shorten_middle(sender_text, 28) if show_sender_label else "",
                message=raw_text,
                size_hint_x=None,
                width=dp(bubble_width),
            )
            if mine:
                line.add_widget(BoxLayout(size_hint_x=1))
                line.add_widget(bubble)
                line.add_widget(BoxLayout(size_hint_x=None, width=dp(8)))
            else:
                line.add_widget(BoxLayout(size_hint_x=None, width=dp(8)))
                line.add_widget(bubble)
                line.add_widget(BoxLayout(size_hint_x=1))
            self._add_footer(bubble, mine=mine, timestamp=timestamp, summary=summary)
            if mine and "failed" in str(summary or "").lower() and getattr(self, "root_owner", None) is not None:
                retry_btn = make_button("danger", text=self._cu("retry"), size_hint_y=None, height=dp(26), on_release=lambda *_p, mid=message_id: self.root_owner.retry_outgoing_message(mid))
                bubble.add_widget(retry_btn)

            def _sync_height(*_args):
                try:
                    line.height = max(dp(56), float(bubble.height or bubble.minimum_height or 0) + dp(8))
                except Exception:
                    line.height = dp(72)

            bubble.bind(height=_sync_height, minimum_height=_sync_height)
            Clock.schedule_once(lambda _dt: _sync_height(), 0)
            line.opacity = 0.0
            self.inner.add_widget(line)
            Animation(opacity=1.0, d=0.12, t="out_quad").start(line)
            self.scroll.scroll_y = 0
            return True
        except Exception:
            return False


    def add_message(self, *, mine: bool, sender: str, text: str, timestamp: str, summary: str = "", body_type: str = "text", file_path: str = "", message_id: str = "", progress_text: str = "", total_size: int = 0, show_sender: bool = False) -> None:
        is_file = body_type == "file"
        raw_text = str(text or "")
        sender_text = str(sender or "").strip()
        show_sender_label = bool(show_sender and sender_text and not mine)
        if not is_file and self._add_modern_text_message(mine=mine, sender=sender, text=raw_text, timestamp=timestamp, summary=summary, message_id=message_id, show_sender=show_sender):
            return
        if is_file:
            line_height = 158 if show_sender_label else 136
            bubble_width = 430
        else:
            # Text bubble sizing:
            # - minimum must contain footer: time + double-check
            # - short messages stay compact
            # - long messages grow to a reasonable width first, then wrap
            # - height is calculated from wrapped line count, so text is not clipped
            char_units = 0
            for ch in raw_text:
                if ch == "\n":
                    char_units += 24
                else:
                    char_units += 2 if ord(ch) > 127 else 1
            char_units = max(1, char_units)
            min_bubble = 118
            max_bubble = 372
            desired = 52 + min(char_units, 44) * 7
            bubble_width = max(min_bubble, min(max_bubble, desired))
            inner_units_per_line = max(12, int((bubble_width - 34) / 7))
            manual_lines = raw_text.count("\n") + 1
            wrap_lines = max(manual_lines, int((char_units + inner_units_per_line - 1) // inner_units_per_line))
            text_h = max(32, 22 * wrap_lines + 14)
            footer_h = 20
            retry_h = 31 if mine and "failed" in str(summary or "").lower() else 0
            line_height = int(text_h + footer_h + retry_h + 52 + (22 if show_sender_label else 0))
        line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(line_height), padding=(0, dp(3), 0, dp(3)))
        if mine:
            spacer_left = BoxLayout(size_hint_x=1)
            spacer_right = BoxLayout(size_hint_x=None, width=dp(8))
        else:
            spacer_left = BoxLayout(size_hint_x=None, width=dp(8))
            spacer_right = BoxLayout(size_hint_x=1)
        bubble = BoxLayout(orientation="vertical", spacing=dp(7), padding=(dp(16), dp(15), dp(16), dp(13)), size_hint_x=None, width=dp(bubble_width))
        apply_bubble_background(bubble, "bubble_mine" if mine else "bubble_other", mine=mine, radius=16)
        if show_sender_label:
            sender_lab = make_label(text=shorten_middle(sender_text, 28), size_hint_y=None, height=dp(18), halign="left", valign="middle", color=THEME["muted_text"], bold=True)
            sender_lab.text_size = (dp(max(1, bubble_width - 34)), None)
            bubble.add_widget(sender_lab)

        if is_file:
            file_name = os.path.basename(file_path or text) or text or self._cu("file")
            pct, failed, complete, detail_line = self._file_progress_state(message_id, total_size, summary, file_path=file_path)
            content = BoxLayout(orientation="horizontal", spacing=dp(14), size_hint_y=None, height=dp(66), padding=(dp(2), 0, dp(2), 0))
            self._add_file_thumbnail(content, file_path, file_name)
            meta = BoxLayout(orientation="vertical", spacing=dp(5), padding=(dp(2), 0, dp(8), 0))
            name_lab = make_label(text=shorten_middle(file_name, 36), size_hint_y=None, height=dp(24), halign="left", valign="middle", color=THEME["text"], bold=True)
            name_lab.shorten = True
            name_lab.shorten_from = "right"
            name_lab.text_size = (dp(270), None)
            meta.add_widget(name_lab)
            status_lab = make_label(text=shorten_middle(detail_line, 42), size_hint_y=None, height=dp(30), halign="left", valign="middle", color=THEME["muted_text"])
            status_lab.shorten = True
            status_lab.shorten_from = "right"
            status_lab.text_size = (dp(270), None)
            meta.add_widget(status_lab)
            content.add_widget(meta)
            owner = getattr(self, "root_owner", None)
            circle = CircularProgressButton(pct=pct, failed=failed, complete=complete, size_hint=(None, None), size=(dp(46), dp(46)))
            if mine and failed and owner is not None:
                circle.bind(on_release=lambda *_p, mid=message_id: owner.retry_file_message(mid))
            content.add_widget(circle)
            bubble.add_widget(content)
            if not mine:
                btn = make_button("secondary", text=self._cu("open_folder") if file_path else self._cu("waiting_saved"), size_hint_y=None, height=dp(28), on_release=lambda *_p, path=file_path: open_file_location(path))
                btn.disabled = not bool(file_path)
                bubble.add_widget(btn)
            footer_summary = summary if (complete or failed) else "transferring"
            self._add_footer(bubble, mine=mine, timestamp=timestamp, summary=footer_summary)
        else:
            # Label wrapping must be width-only. If height is included in text_size,
            # Kivy can clip long text and wrap too aggressively.
            label_width = max(1, bubble_width - 32)
            lab = make_label(text=raw_text, size_hint_y=None, height=dp(text_h), halign="left", valign="top", color=THEME["text"])
            lab.text_size = (dp(label_width), None)
            lab.bind(width=lambda inst, _val: setattr(inst, "text_size", (max(1, inst.width), None)))
            bubble.add_widget(lab)
            self._add_footer(bubble, mine=mine, timestamp=timestamp, summary=summary)
            if mine and "failed" in str(summary or "").lower() and getattr(self, "root_owner", None) is not None and body_type == "text":
                retry_btn = make_button("danger", text=self._cu("retry"), size_hint_y=None, height=dp(26), on_release=lambda *_p, mid=message_id: self.root_owner.retry_outgoing_message(mid))
                bubble.add_widget(retry_btn)
        line.add_widget(spacer_left); line.add_widget(bubble); line.add_widget(spacer_right)
        self.inner.add_widget(line)
        self.scroll.scroll_y = 0


class RUDPTransferRoot(BoxLayout):
    def __init__(self, app: "RUDPTransferApp", **kwargs):
        super().__init__(orientation="vertical", spacing=dp(8), padding=dp(12), **kwargs)
        self.app = app
        self.lang = app.lang
        self.discovered: List[Dict[str, object]] = []
        self.selected_receiver: Optional[Dict[str, object]] = None
        self.search_in_progress = False
        self.pending_request_popups = set()
        self.seen_request_files = set()
        self.approval_dir = user_data_dir() / "approvals"
        self.approval_dir.mkdir(parents=True, exist_ok=True)
        self.contact_approval_dir = user_data_dir() / "contact_approvals"
        self.contact_approval_dir.mkdir(parents=True, exist_ok=True)
        self.sender_worker = WorkerProcess("sender", self.sender_log, self.sender_exit, self.sender_progress)
        self.receiver_worker = WorkerProcess("receiver", self.receiver_log, self.receiver_exit)
        self.contact_worker = WorkerProcess("sender", self.sender_log, lambda rc: self.sender_log_box.append(f"Contact request exited, rc={rc}\n"))
        self.last_sender_args: Optional[List[str]] = None
        self.last_sender_file: str = ""
        self.last_sender_failure_code: str = ""
        self.chat_store = None
        self.transfer_store = None
        self.contact_service = None
        self.group_service = None
        self.message_service = None
        self.file_transfer_service = None
        self.chat_unlocked = False
        self.basic_mode = False
        self.chat_db_path = str(user_data_dir() / "chat" / "agoralink_chat.db")
        self.chat_password = ""
        self.chat_local_peer_id = os.environ.get("USERNAME") or "local"
        self.chat_nickname = os.environ.get("USERNAME") or "AgoraLinkUser"
        self.gui_config = load_gui_config()
        self.auto_package_multi_selection = bool(self.gui_config.get("auto_package_multi_selection", True))
        self.share_system_audio = bool(self.gui_config.get("screen_share_system_audio", False))
        self.screen_package_info = self._screen_package_capabilities()
        self.screen_backend_notice = ""
        self.screen_backend = self._coerce_screen_backend_for_package(
            self.gui_config.get("screen_backend", self._default_screen_backend()),
            persist=True,
        )
        self.current_chat_mode = "group"
        self.current_group_id = ""
        self.current_peer_id = ""
        self.selected_contact: Optional[Dict[str, object]] = None
        self.selected_device: Optional[Dict[str, object]] = None
        self.selected_group: Optional[Dict[str, object]] = None
        self.selected_group_member_peer_id: str = ""
        self.selected_chat_entry_value: str = ""
        self.file_message_progress: Dict[str, Dict[str, object]] = {}
        self.current_receiving_file_message_id = ""
        self.receiving_file_message_by_conn: Dict[int, str] = {}
        self.file_message_tasks: Dict[str, Dict[str, object]] = {}
        self.chat_runtime_cards: List[Dict[str, object]] = []
        self.pending_transfer_requests: Dict[int, Dict[str, object]] = {}
        self.pending_transfer_popups: Dict[int, Popup] = {}
        self.pending_transfer_decisions: Dict[int, str] = {}
        self.file_packaging_busy = False
        self.live_message_cache: Dict[str, List[Dict[str, object]]] = {}
        self.debug_protocol_lines: List[str] = []
        self.debug_runtime_lines: List[str] = []
        self.last_transfer_state: Dict[str, Dict[str, object]] = {}
        self._transfer_store_write_bytes: Dict[str, int] = {}
        self._transfer_refresh_scheduled = False
        self._last_transfer_card_refresh_ts = 0.0
        self._last_transfer_progress_line = ""
        self.screen_share_session_id = ""
        self.screen_share_peer_id = ""
        self.screen_share_peer_label = ""
        self.screen_share_current_port: Optional[int] = None
        self.screen_share_selected_profile = ""
        self.screen_share_current_audio: Dict[str, object] = {"enabled": False, "mode": "none"}
        self.screen_share_current_backend = str(self.screen_backend or self._default_screen_backend())
        self.current_screen_peer = ""
        self.current_screen_profile = ""
        self.current_screen_port: Optional[int] = None
        self.screen_share_advertised_profiles: List[Dict[str, object]] = []
        self.screen_share_advertised_profiles_ts = 0.0
        self.screen_share_last_status = ""
        self.screen_share_ui_state = "idle"
        self._seen_screen_control_messages = set()
        self._worker_stop_in_progress = set()
        self._screen_stop_in_progress = False
        self._diagnostic_export_in_progress = False
        self._chat_runtime_card_sequence = 0
        self._rendered_chat_message_ids = set()
        self.pending_screen_offers: Dict[str, Dict[str, object]] = {}
        self.pending_screen_offer_popups: Dict[str, Popup] = {}
        # Deduplicate the receiver's two log lines for one chat frame:
        #   CHAT_MESSAGE_JSON:{...}
        #   Chat from sender: text
        # The second line is informational; it must not create another UI message.
        self._recent_chat_json_seen: Dict[Tuple[str, str], float] = {}
        self._chat_render_sig: Optional[Tuple[object, ...]] = None
        self._chat_render_generation = 0
        self.pending_outgoing_contact_requests: Dict[str, Dict[str, object]] = {}
        self._build()
        self.refresh_texts()
        self.refresh_local_ips()
        Clock.schedule_interval(self.poll_approval_requests, 0.5)
        Clock.schedule_interval(self.poll_contact_requests, 0.5)
        Clock.schedule_interval(lambda _dt: self._auto_refresh_current_chat(), 0.5)
        Clock.schedule_once(lambda _dt: self.show_startup_unlock_popup(), 0.2)

    def t(self, key: str, **kwargs) -> str:
        text = I18N[self.lang].get(key, key)
        return text.format(**kwargs) if kwargs else text

    def cu(self, key: str, **kwargs) -> str:
        text = CHAT_UI_TEXT.get(self.lang, CHAT_UI_TEXT.get("zh", {})).get(key, key)
        return text.format(**kwargs) if kwargs else text

    def _modern_button_style(self, role: str) -> Dict[str, object]:
        return modern_button_style(role)

    def _style_modern_or_legacy_button(self, button, role: str) -> None:
        if UIRoundedButton is not None and isinstance(button, UIRoundedButton):
            try:
                for name, value in self._modern_button_style(role).items():
                    setattr(button, name, value)
                button._refresh_button_state(animated=False)
                return
            except Exception:
                pass
        try:
            style_button(button, role)
        except Exception:
            pass

    def _make_modern_or_legacy_button(
        self,
        role: str,
        *,
        text: str = "",
        width: Optional[int] = None,
        on_release=None,
        height: int = 36,
        size_hint_x=None,
        compact: bool = False,
        pill: bool = True,
    ):
        if UIPillButton is not None and ui_component_color is not None:
            try:
                button_cls = UIPillButton if pill else UIRoundedButton
                kwargs: Dict[str, object] = {
                    "text": text,
                    "height": dp(height),
                    "compact": bool(compact),
                    **self._modern_button_style(role),
                }
                if width is not None:
                    kwargs.update(size_hint_x=None, width=dp(width))
                elif size_hint_x is not None:
                    kwargs.update(size_hint_x=size_hint_x)
                btn = button_cls(**kwargs)
                if on_release is not None:
                    btn.bind(on_release=on_release)
                return btn
            except Exception:
                pass
        kwargs = {"text": text, "on_release": on_release}
        if width is not None:
            kwargs.update(size_hint_x=None, width=dp(width))
        elif size_hint_x is not None:
            kwargs.update(size_hint_x=size_hint_x)
        return make_button(role, **kwargs)

    def _stopping_text(self) -> str:
        return "正在停止..." if self.lang == "zh" else "Stopping..."

    def _set_button_busy(self, button, busy: bool, text: Optional[str] = None) -> str:
        previous = str(getattr(button, "text", "") or "") if button is not None else ""
        if button is None:
            return previous
        try:
            button.disabled = bool(busy)
            if text is not None:
                button.text = str(text)
        except Exception:
            pass
        return previous

    def _append_worker_stop_log(self, worker_name: str, text: str) -> None:
        box_name = "receiver_log_box" if str(worker_name or "") == "receiver" else "sender_log_box"
        try:
            log_box = getattr(self, box_name, None)
            if log_box is not None:
                log_box.append(str(text or ""))
        except Exception:
            pass

    def _stop_worker_nonblocking(self, worker_name: str, worker: WorkerProcess, button=None, on_done=None) -> None:
        key = str(worker_name or "worker")
        if key in self._worker_stop_in_progress:
            self._append_worker_stop_log(key, f"{self._stopping_text()}\n")
            return
        try:
            running = bool(worker and worker.is_running())
        except Exception:
            running = False
        if not running:
            if on_done is not None:
                try:
                    on_done(True, "")
                except Exception:
                    pass
            return
        self._worker_stop_in_progress.add(key)
        previous_text = self._set_button_busy(button, True, self._stopping_text())
        self._append_worker_stop_log(key, f"{self._stopping_text()}\n")

        def _run_stop() -> None:
            error = ""
            try:
                worker.stop()
            except Exception as exc:
                error = str(exc)

            def _finish(_dt) -> None:
                self._worker_stop_in_progress.discard(key)
                self._set_button_busy(button, False, previous_text if previous_text else None)
                if error:
                    self._append_worker_stop_log(key, f"Stop failed: {error}\n")
                else:
                    self._append_worker_stop_log(key, "Stopped.\n")
                if on_done is not None:
                    try:
                        on_done(not bool(error), error)
                    except Exception:
                        pass

            Clock.schedule_once(_finish, 0)

        threading.Thread(target=_run_stop, daemon=True).start()

    def _make_modern_input_shell(self, widget, *, height: int = 38):
        if UIRoundedCard is not None and ui_component_color is not None:
            try:
                shell = UIRoundedCard(
                    orientation="horizontal",
                    size_hint_x=1,
                    size_hint_y=None,
                    height=dp(height),
                    padding=(dp(12), 0, dp(12), 0),
                    spacing=0,
                    radius=12,
                    bg_color=ui_component_color("surface_muted"),
                    border_color=ui_component_color("border_soft"),
                )
                widget.size_hint_x = 1
                widget.size_hint_y = None
                widget.height = dp(height)
                for name, value in (
                    ("background_normal", ""),
                    ("background_active", ""),
                    ("background_down", ""),
                    ("background_color", ui_component_color("transparent")),
                    ("foreground_color", ui_component_color("text_primary")),
                    ("cursor_color", ui_component_color("accent")),
                ):
                    try:
                        setattr(widget, name, value)
                    except Exception:
                        pass
                if isinstance(widget, TextInput):
                    widget.padding = (0, dp(9), 0, dp(7))
                    widget.bind(
                        focus=lambda _inst, focused, target=shell: setattr(
                            target,
                            "border_color",
                            ui_component_color("accent" if focused else "border_soft"),
                        )
                    )
                elif isinstance(widget, Spinner):
                    widget.option_cls = ThemedSpinnerOption
                    widget.color = ui_component_color("text_primary")
                    widget.bind(
                        state=lambda _inst, state, target=shell: setattr(
                            target,
                            "border_color",
                            ui_component_color("accent" if state == "down" else "border_soft"),
                        )
                    )
                shell.add_widget(widget)
                return shell
            except Exception:
                pass
        return widget

    def _build(self) -> None:
        Window.minimum_width = 900
        Window.minimum_height = 600

        top = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(46), spacing=dp(10))
        # Keep a blank flexible spacer; the old "AgoraLink 文件传输与聊天"
        # title consumed horizontal/vertical attention after login.
        self.title_label = make_label(text="", halign="left", valign="middle")
        top.add_widget(self.title_label)
        self.lang_btn = self._make_modern_or_legacy_button("secondary", width=90, on_release=lambda *_: self.toggle_lang())
        top.add_widget(self.lang_btn)
        self.online_btn = self._make_modern_or_legacy_button("active", text="Online", width=90, on_release=lambda *_: self.toggle_online())
        top.add_widget(self.online_btn)
        self.enter_chat_btn = self._make_modern_or_legacy_button("primary", text="进入聊天", width=110, on_release=lambda *_: self.show_startup_unlock_popup())
        top.add_widget(self.enter_chat_btn)
        self.settings_btn = self._make_modern_or_legacy_button("secondary", text="设置", width=96, on_release=lambda *_: self.open_settings_popup())
        top.add_widget(self.settings_btn)
        self.debug_btn = self._make_modern_or_legacy_button("secondary", text="诊断", width=112, on_release=lambda *_: self.open_debug_popup())
        top.add_widget(self.debug_btn)
        self.add_widget(top)

        self.local_ip_label = make_label(size_hint_y=None, height=dp(26), halign="left", valign="middle", shorten=True)
        bind_label_wrap(self.local_ip_label)
        self.add_widget(self.local_ip_label)

        # Use explicit tab buttons instead of Kivy TabbedPanel headers.
        # TabbedPanel headers use their own internal button class and may ignore
        # the application font on some Windows/Kivy builds, which causes Chinese
        # text to disappear. Normal Buttons give deterministic font behavior.
        self.tab_bar = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(10))
        self.send_tab_btn = self._make_modern_or_legacy_button("active", size_hint_x=1, height=36, on_release=lambda *_: self.show_page("send"))
        self.recv_tab_btn = self._make_modern_or_legacy_button("secondary", size_hint_x=1, height=36, on_release=lambda *_: self.show_page("recv"))
        self.chat_tab_btn = self._make_modern_or_legacy_button("secondary", size_hint_x=1, height=36, on_release=lambda *_: self.show_page("chat"))
        self.tab_bar.add_widget(self.send_tab_btn)
        self.tab_bar.add_widget(self.recv_tab_btn)
        self.tab_bar.add_widget(self.chat_tab_btn)
        self.add_widget(self.tab_bar)

        # The content host is the only vertically expanding area below the fixed
        # header. This keeps the top controls anchored while the log panel expands
        # or shrinks with the window, avoiding unused space at the bottom.
        self.page_host = BoxLayout(orientation="vertical", size_hint_y=1)
        self.add_widget(self.page_host)
        self.send_page = self._build_send_tab()
        self.recv_page = self._build_recv_tab()
        self.chat_page = self._build_chat_tab()
        self.agora_chat_page = self._build_agora_chat_main()
        self.current_page = ""
        self.show_page("recv")
        apply_ui_font(self)

    def _build_send_tab(self) -> None:
        root = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(8))
        form = GridLayout(cols=1, size_hint_y=None, spacing=dp(6))
        form.bind(minimum_height=form.setter("height"))
        self.receiver_ip = make_input(text="", multiline=False)
        self.receiver_port = make_input(text="9999", multiline=False, input_filter="int")
        self.discovery_port = make_input(text=str(DEFAULT_DISCOVERY_PORT), multiline=False, input_filter="int")
        self.receiver_spinner = style_spinner(Spinner(text="", values=[], font_name=UI_FONT))
        self.receiver_spinner.bind(text=self.on_receiver_selected)
        self.file_input = make_input(text="", multiline=False)
        self.payload_input = make_input(text="1400", multiline=False, input_filter="int")
        self.complete_timeout_input = make_input(text="180", multiline=False, input_filter="float")
        self.request_timeout_input = make_input(text="300", multiline=False, input_filter="float")
        self.manual_hint = make_label(size_hint_y=None, height=dp(30), halign="left", valign="middle", shorten=True)
        bind_label_wrap(self.manual_hint)
        form.add_widget(self.manual_hint)
        form.add_widget(row("", self.receiver_ip))
        form.add_widget(row("", self.receiver_port))
        discovery_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        discovery_line.add_widget(make_label(text="", size_hint_x=None, width=dp(150), halign="right", valign="middle", shorten=True, color=THEME["muted_text"]))
        discovery_line.add_widget(self.discovery_port)
        self.search_btn = make_button("primary", size_hint_x=None, width=dp(150), on_release=lambda *_: self.search_receivers())
        discovery_line.add_widget(self.search_btn)
        form.add_widget(discovery_line)
        form.add_widget(row("", self.receiver_spinner))
        file_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        file_line.add_widget(make_label(text="", size_hint_x=None, width=dp(150), halign="right", valign="middle", shorten=True, color=THEME["muted_text"]))
        file_line.add_widget(self.file_input)
        self.choose_file_btn = make_button("secondary", size_hint_x=None, width=dp(110), on_release=lambda *_: self.choose_file())
        file_line.add_widget(self.choose_file_btn)
        form.add_widget(file_line)
        form.add_widget(row("", self.payload_input))
        form.add_widget(row("", self.complete_timeout_input))
        form.add_widget(row("", self.request_timeout_input))
        root.add_widget(form)
        progress_box = BoxLayout(orientation="vertical", size_hint_y=None, height=dp(95), spacing=dp(4))
        self.progress = ProgressBar(max=100.0, value=0.0, size_hint_y=None, height=dp(24))
        self.progress_label = make_label(size_hint_y=None, height=dp(24), halign="left", valign="middle", shorten=True)
        self.eta_label = make_label(size_hint_y=None, height=dp(24), halign="left", valign="middle", shorten=True)
        self.speed_label = make_label(size_hint_y=None, height=dp(24), halign="left", valign="middle", shorten=True)
        for lab in (self.progress_label, self.eta_label, self.speed_label):
            lab.bind(size=lambda inst, val: setattr(inst, "text_size", val))
        progress_box.add_widget(self.progress)
        progress_box.add_widget(self.progress_label)
        progress_box.add_widget(self.eta_label)
        progress_box.add_widget(self.speed_label)
        root.add_widget(progress_box)
        action = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        self.send_btn = make_button("success", on_release=lambda *_: self.start_sender())
        self.retry_send_btn = make_button("primary", on_release=lambda *_: self.retry_sender())
        self.retry_send_btn.disabled = True
        self.stop_send_btn = make_button("danger", on_release=lambda *_: self._stop_worker_nonblocking("sender", self.sender_worker, self.stop_send_btn))
        self.clear_send_btn = make_button("secondary", on_release=lambda *_: self.sender_log_box.clear())
        action.add_widget(self.send_btn)
        action.add_widget(self.retry_send_btn)
        action.add_widget(self.stop_send_btn)
        action.add_widget(self.clear_send_btn)
        root.add_widget(action)
        self.sender_log_box = LogBox()
        root.add_widget(self.sender_log_box)
        return root

    def _build_recv_tab(self) -> None:
        root = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(8))
        form = GridLayout(cols=1, size_hint_y=None, spacing=dp(6))
        form.bind(minimum_height=form.setter("height"))
        self.bind_input = make_input(text="0.0.0.0", multiline=False)
        self.recv_port = make_input(text="9999", multiline=False, input_filter="int")
        self.recv_discovery_port = make_input(text=str(DEFAULT_DISCOVERY_PORT), multiline=False, input_filter="int")
        self.save_dir_input = make_input(text=str(user_data_dir() / "received"), multiline=False)
        self.allow_peer_input = make_input(text="", multiline=False)
        self.receiver_name_input = make_input(text="", multiline=False)
        self.approval_timeout_input = make_input(text="300", multiline=False, input_filter="float")
        self.once_checkbox = CheckBox(active=False, size_hint_x=None, width=dp(40))
        form.add_widget(row("", self.bind_input))
        form.add_widget(row("", self.recv_port))
        form.add_widget(row("", self.recv_discovery_port))
        save_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        save_line.add_widget(make_label(text="", size_hint_x=None, width=dp(150), halign="right", valign="middle", shorten=True, color=THEME["muted_text"]))
        save_line.add_widget(self.save_dir_input)
        self.choose_dir_btn = make_button("secondary", size_hint_x=None, width=dp(110), on_release=lambda *_: self.choose_dir())
        save_line.add_widget(self.choose_dir_btn)
        form.add_widget(save_line)
        form.add_widget(row("", self.allow_peer_input))
        form.add_widget(row("", self.receiver_name_input))
        form.add_widget(row("", self.approval_timeout_input))
        once_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        self.once_label = make_label(size_hint_x=None, width=dp(150), halign="right", valign="middle", shorten=True, color=THEME["muted_text"])
        once_line.add_widget(self.once_label)
        once_line.add_widget(self.once_checkbox)
        form.add_widget(once_line)
        root.add_widget(form)
        action = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        self.start_recv_btn = make_button("success", on_release=lambda *_: self.start_receiver())
        self.stop_recv_btn = make_button("danger", on_release=lambda *_: self._stop_worker_nonblocking("receiver", self.receiver_worker, self.stop_recv_btn))
        self.firewall_btn = make_button("primary", on_release=lambda *_: self.allow_firewall())
        self.clear_recv_btn = make_button("secondary", on_release=lambda *_: self.receiver_log_box.clear())
        action.add_widget(self.start_recv_btn)
        action.add_widget(self.stop_recv_btn)
        action.add_widget(self.firewall_btn)
        action.add_widget(self.clear_recv_btn)
        root.add_widget(action)
        self.receiver_log_box = LogBox()
        root.add_widget(self.receiver_log_box)
        return root


    def _build_chat_tab(self) -> None:
        root = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(8))
        form = GridLayout(cols=1, size_hint_y=None, spacing=dp(6))
        form.bind(minimum_height=form.setter("height"))
        self.chat_db_input = make_input(text=self.chat_db_path, multiline=False)
        self.chat_password_input = make_input(text="", multiline=False, password=True)
        self.chat_local_peer_input = make_input(text=self.chat_local_peer_id, multiline=False)
        form.add_widget(row("", self.chat_db_input))
        form.add_widget(row("", self.chat_password_input))
        form.add_widget(row("", self.chat_local_peer_input))
        unlock_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        self.unlock_chat_btn = make_button("primary", on_release=lambda *_: self.unlock_chat_db())
        self.refresh_chat_btn = make_button("secondary", on_release=lambda *_: self.refresh_chat_view())
        unlock_line.add_widget(self.unlock_chat_btn)
        unlock_line.add_widget(self.refresh_chat_btn)
        form.add_widget(unlock_line)
        self.chat_group_id_input = make_input(text="group1", multiline=False)
        self.chat_group_title_input = make_input(text="LAN Group", multiline=False)
        form.add_widget(row("", self.chat_group_id_input))
        form.add_widget(row("", self.chat_group_title_input))
        group_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        self.create_group_btn = make_button("success", on_release=lambda *_: self.create_chat_group())
        self.leave_group_btn = make_button("danger", on_release=lambda *_: self.leave_chat_group())
        group_line.add_widget(self.create_group_btn)
        group_line.add_widget(self.leave_group_btn)
        form.add_widget(group_line)
        self.member_peer_input = make_input(text="", multiline=False)
        self.member_ip_input = make_input(text="", multiline=False)
        self.member_port_input = make_input(text="9999", multiline=False, input_filter="int")
        form.add_widget(row("", self.member_peer_input))
        form.add_widget(row("", self.member_ip_input))
        form.add_widget(row("", self.member_port_input))
        member_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        self.add_member_btn = make_button("success", on_release=lambda *_: self.add_chat_member())
        self.remove_member_btn = make_button("danger", on_release=lambda *_: self.remove_chat_member())
        member_line.add_widget(self.add_member_btn)
        member_line.add_widget(self.remove_member_btn)
        form.add_widget(member_line)
        self.group_message_input = make_input(text="", multiline=False)
        msg_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        msg_line.add_widget(make_label(text="", size_hint_x=None, width=dp(150), halign="right", valign="middle", shorten=True, color=THEME["muted_text"]))
        msg_line.add_widget(self.group_message_input)
        self.send_group_btn = make_button("primary", size_hint_x=None, width=dp(150), on_release=lambda *_: self.send_group_message_gui())
        msg_line.add_widget(self.send_group_btn)
        form.add_widget(msg_line)
        root.add_widget(form)
        split = BoxLayout(orientation="horizontal", spacing=dp(8), size_hint_y=1)
        left = BoxLayout(orientation="vertical", spacing=dp(4), size_hint_x=0.38)
        left.add_widget(make_label(text="Members / Receipts", size_hint_y=None, height=dp(24), halign="left"))
        self.chat_members_box = LogBox()
        left.add_widget(self.chat_members_box)
        split.add_widget(left)
        right = BoxLayout(orientation="vertical", spacing=dp(4), size_hint_x=0.62)
        right.add_widget(make_label(text="Messages", size_hint_y=None, height=dp(24), halign="left"))
        self.chat_messages_box = LogBox()
        right.add_widget(self.chat_messages_box)
        split.add_widget(right)
        root.add_widget(split)
        return root


    def _build_agora_chat_main(self) -> BoxLayout:
        root = BoxLayout(orientation="horizontal", spacing=dp(10), padding=dp(12))

        left = BoxLayout(orientation="vertical", size_hint_x=None, width=dp(280), spacing=dp(10), padding=(dp(12), dp(12), dp(12), dp(12)))
        apply_card_background(left, "panel_bg", radius=22)
        self.profile_name_label = make_label(text=self.chat_nickname or "AgoraLink", size_hint_y=None, height=dp(32), halign="left", valign="middle", bold=True)
        bind_label_wrap(self.profile_name_label)
        left.add_widget(self.profile_name_label)
        self.profile_peer_label = make_label(text=self.chat_local_peer_id or "", size_hint_y=None, height=dp(20), halign="left", valign="middle", color=THEME["muted_text"])
        bind_label_wrap(self.profile_peer_label)
        left.add_widget(self.profile_peer_label)
        self.chat_filter_input = make_input(text="", multiline=False, size_hint_y=None, height=dp(38))
        self.chat_filter_input.hint_text = self.cu("search_hint")
        self.chat_filter_input.bind(text=lambda *_: self.refresh_chat_main())
        left.add_widget(self.chat_filter_input)
        self.chat_nav = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(6))
        self.recent_btn = self._make_modern_or_legacy_button("active", text=self.cu("recent"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.set_chat_section("recent"))
        self.groups_btn = self._make_modern_or_legacy_button("secondary", text=self.cu("contacts"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.set_chat_section("contacts"))
        self.devices_btn = self._make_modern_or_legacy_button("secondary", text=self.cu("devices"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.set_chat_section("devices"))
        self.chat_nav.add_widget(self.recent_btn)
        self.chat_nav.add_widget(self.groups_btn)
        self.chat_nav.add_widget(self.devices_btn)
        left.add_widget(self.chat_nav)

        self.chat_items_scroll = ScrollView(size_hint_y=1)
        self.chat_items_box = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(8), padding=(0, dp(2), 0, dp(2)))
        self.chat_items_box.bind(minimum_height=self.chat_items_box.setter("height"))
        self.chat_items_scroll.add_widget(self.chat_items_box)
        left.add_widget(self.chat_items_scroll)

        # Legacy spinner/list widgets are kept for compatibility with older
        # selection code, but are intentionally not added to the layout.  Adding
        # the hidden spinner could leak internal values such as direct::peer::name
        # above the Scan button on some Kivy builds.
        self.chat_list_box = LogBox(size_hint_y=None, height=0, opacity=0)
        self.chat_list_spinner = style_spinner(Spinner(text="", values=[], size_hint_y=None, height=0, font_name=UI_FONT))
        self.chat_list_spinner.disabled = True

        list_actions = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(36), spacing=dp(8))
        self.scan_devices_btn = self._make_modern_or_legacy_button("secondary", text=self.cu("scan_devices"), size_hint_x=1, height=34, compact=True, on_release=lambda *_: self.scan_devices_for_chat())
        list_actions.add_widget(self.scan_devices_btn)
        self.add_contact_btn = self._make_modern_or_legacy_button("primary", text=self.cu("add_contact"), size_hint_x=1, height=34, compact=True, on_release=lambda *_: self.request_or_add_selected_device())
        list_actions.add_widget(self.add_contact_btn)
        left.add_widget(list_actions)
        root.add_widget(left)

        center = BoxLayout(orientation="vertical", spacing=dp(10), padding=(dp(12), dp(10), dp(12), dp(12)))
        apply_card_background(center, "panel_bg", radius=22)
        # The active chat is already highlighted in the left list, so the old
        # center title bar ("一对一：xxx" / "群聊：xxx") is hidden to save space.
        title_bar = BoxLayout(orientation="horizontal", size_hint_y=None, height=0, spacing=dp(8), opacity=0, disabled=True)
        self.current_chat_title = make_button("secondary", text="", size_hint_y=None, height=0, halign="left", valign="middle", bold=True, disabled=True)
        self.current_chat_title.bind(size=lambda inst, _val: setattr(inst, "text_size", (inst.width - dp(16), None)))
        title_bar.add_widget(self.current_chat_title)
        self.online_state_btn = make_button("secondary", text="Online", size_hint_x=None, width=0, opacity=0, disabled=True, on_release=lambda *_: self.toggle_online_state())
        title_bar.add_widget(self.online_state_btn)
        center.add_widget(title_bar)
        self.main_messages_box = ChatMessageBox(root_owner=self)
        center.add_widget(self.main_messages_box)
        self.screen_share_status_label = make_label(
            text=self._screen_share_status_text("idle"),
            size_hint_y=None,
            height=dp(38),
            halign="left",
            valign="middle",
            color=THEME["muted_text"],
        )
        bind_label_wrap(self.screen_share_status_label)
        center.add_widget(self.screen_share_status_label)
        if str(getattr(self, "screen_backend_notice", "") or "").strip():
            Clock.schedule_once(lambda _dt: self._set_screen_share_status(str(self.screen_backend_notice)), 0)
        if UIRoundedCard is not None and ui_component_color is not None:
            input_line = UIRoundedCard(
                orientation="horizontal",
                size_hint_y=None,
                height=dp(56),
                spacing=dp(8),
                padding=(dp(10), dp(8), dp(10), dp(8)),
                radius=20,
                bg_color=ui_component_color("surface_blue"),
                border_color=ui_component_color("border_soft"),
            )
            input_shell = UIRoundedCard(
                orientation="horizontal",
                size_hint_x=1,
                size_hint_y=None,
                height=dp(38),
                padding=(dp(12), 0, dp(12), 0),
                spacing=0,
                radius=12,
                bg_color=ui_component_color("surface"),
                border_color=ui_component_color("border_soft"),
            )
            self.main_message_input = make_input(text="", multiline=False)
            try:
                self.main_message_input.background_normal = ""
                self.main_message_input.background_active = ""
                self.main_message_input.background_color = ui_component_color("transparent")
                self.main_message_input.foreground_color = ui_component_color("text_primary")
                self.main_message_input.cursor_color = ui_component_color("accent")
                self.main_message_input.padding = (0, dp(9), 0, dp(7))
            except Exception:
                pass
            input_shell.add_widget(self.main_message_input)
            input_line.add_widget(input_shell)
            self.main_message_input.bind(
                focus=lambda _inst, focused, shell=input_shell: setattr(
                    shell,
                    "border_color",
                    ui_component_color("accent" if focused else "border_soft"),
                )
            )
        else:
            input_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(46), spacing=dp(8))
            self.main_message_input = make_input(text="", multiline=False)
            input_line.add_widget(self.main_message_input)
        self.main_message_input.hint_text = self.cu("input_hint")
        self.main_message_input.bind(on_text_validate=lambda *_: self.send_current_chat_message())
        self.main_send_btn = self._make_modern_or_legacy_button("primary", text=self.cu("send"), width=78, height=38, on_release=lambda *_: self.send_current_chat_message())
        input_line.add_widget(self.main_send_btn)
        self.main_file_btn = self._make_modern_or_legacy_button("secondary", text=self.cu("send_file"), width=98, height=38, on_release=lambda *_: self.send_file_to_current_chat())
        input_line.add_widget(self.main_file_btn)
        self.main_screen_btn = self._make_modern_or_legacy_button("secondary", text="鎶曞睆", width=106, on_release=lambda *_: Clock.schedule_once(lambda _dt: self._on_screen_share_button(), 0))
        self.main_screen_btn.height = dp(38)
        self.main_screen_btn.width = dp(104)
        input_line.add_widget(self.main_screen_btn)
        self._schedule_screen_share_button_refresh()
        center.add_widget(input_line)
        root.add_widget(center)

        right = BoxLayout(orientation="vertical", size_hint_x=None, width=dp(288), spacing=dp(8), padding=(dp(12), dp(12), dp(12), dp(12)))
        apply_card_background(right, "panel_bg", radius=22)
        self.right_title = make_label(text=self.cu("right_title"), size_hint_y=None, height=dp(30), halign="left", valign="middle", bold=True)
        bind_label_wrap(self.right_title)
        right.add_widget(self.right_title)
        self.right_member_scroll = ScrollView(size_hint_y=None, height=dp(190))
        self.right_member_box = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(6), padding=(0, 0, 0, dp(4)))
        self.right_member_box.bind(minimum_height=self.right_member_box.setter("height"))
        self.right_member_scroll.add_widget(self.right_member_box)
        right.add_widget(self.right_member_scroll)
        self.right_info_box = LogBox(size_hint_y=None, height=dp(120))
        right.add_widget(self.right_info_box)
        self.shared_title = make_label(text=self.cu("shared_files"), size_hint_y=None, height=dp(26), halign="left", valign="middle", bold=True)
        shared_title = self.shared_title
        right.add_widget(shared_title)
        self.shared_files_scroll = ScrollView(size_hint_y=1)
        self.shared_files_box = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(6), padding=(0, 0, 0, dp(4)))
        self.shared_files_box.bind(minimum_height=self.shared_files_box.setter("height"))
        self.shared_files_scroll.add_widget(self.shared_files_box)
        right.add_widget(self.shared_files_scroll)
        self.right_action1 = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(4))
        self.new_group_btn = self._make_modern_or_legacy_button("primary", text=self.cu("new_group"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.open_group_popup())
        self.right_action1.add_widget(self.new_group_btn)
        self.add_member_main_btn = self._make_modern_or_legacy_button("secondary", text=self.cu("add_member"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.add_selected_contact_to_current_group())
        self.right_action1.add_widget(self.add_member_main_btn)
        right.add_widget(self.right_action1)
        self.right_action2 = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(4))
        self.remove_member_main_btn = self._make_modern_or_legacy_button("danger", text=self.cu("remove_member"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.confirm_remove_member())
        self.right_action2.add_widget(self.remove_member_main_btn)
        self.leave_group_main_btn = self._make_modern_or_legacy_button("danger", text=self.cu("leave_group"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.confirm_leave_group())
        self.right_action2.add_widget(self.leave_group_main_btn)
        self.delete_friend_main_btn = self._make_modern_or_legacy_button("danger", text=self.cu("delete_friend"), size_hint_x=1, height=36, compact=True, on_release=lambda *_: self.confirm_delete_contact())
        self.right_action2.add_widget(self.delete_friend_main_btn)
        right.add_widget(self.right_action2)
        self.chat_detail_panel = right
        root.add_widget(right)
        self.detail_panel_visible = False
        Clock.schedule_once(lambda _dt: self._hide_detail_panel(), 0)
        Clock.schedule_once(lambda _dt: self._set_right_action_mode("none"), 0)
        return root

    def _hide_detail_panel(self) -> None:
        try:
            self.detail_panel_visible = False
            panel = self.chat_detail_panel
            panel.disabled = True
            panel.opacity = 0.0
            panel.width = 0
            panel.size_hint_x = None
        except Exception:
            pass

    def _show_detail_panel(self) -> None:
        try:
            self.detail_panel_visible = True
            panel = self.chat_detail_panel
            panel.disabled = False
            panel.opacity = 1.0
            panel.width = dp(288)
            panel.size_hint_x = None
        except Exception:
            pass

    def toggle_detail_panel(self) -> None:
        if getattr(self, "current_chat_mode", "") not in ("direct", "group"):
            return
        if getattr(self, "detail_panel_visible", False):
            self._hide_detail_panel()
        else:
            self._show_detail_panel()
            self.render_current_chat(reason="detail_panel")

    def toggle_online_state(self) -> None:
        try:
            running = self.receiver_worker.is_running()
        except Exception:
            running = False
        if running:
            def _offline_done(_ok, _error) -> None:
                if hasattr(self, "online_state_btn"):
                    self.online_state_btn.text = "Offline"
                    style_button(self.online_state_btn, "secondary")
            self._stop_worker_nonblocking("receiver", self.receiver_worker, getattr(self, "online_state_btn", None), _offline_done)
        else:
            try:
                self.start_receiver()
            except Exception:
                pass
            if hasattr(self, "online_state_btn"):
                self.online_state_btn.text = "Online"
                style_button(self.online_state_btn, "active")

    def _set_widget_visible(self, widget, visible: bool, height: int = 38) -> None:
        try:
            widget.disabled = not bool(visible)
            widget.opacity = 1.0 if visible else 0.0
            widget.height = dp(height) if visible else 0
            widget.size_hint_y = None
        except Exception:
            pass

    def _set_button_visible(self, button, visible: bool) -> None:
        try:
            button.disabled = not bool(visible)
            button.opacity = 1.0 if visible else 0.0
            button.size_hint_x = 1 if visible else None
            button.width = dp(80) if visible else 0
        except Exception:
            pass

    def _set_right_action_mode(self, mode: str) -> None:
        is_group = mode == "group"
        is_owner = bool(is_group and self._is_local_group_owner(getattr(self, "current_group_id", "")))
        is_active_member = bool(is_group and self._is_local_active_group_member(getattr(self, "current_group_id", "")))
        if hasattr(self, "right_action1"):
            self._set_widget_visible(self.right_action1, is_group and is_owner)
        if hasattr(self, "right_action2"):
            self._set_widget_visible(self.right_action2, is_group)
        if hasattr(self, "right_member_scroll"):
            self._set_widget_visible(self.right_member_scroll, is_group, height=210)
        if hasattr(self, "new_group_btn"):
            self._set_button_visible(self.new_group_btn, False)
        if hasattr(self, "add_member_main_btn"):
            self._set_button_visible(self.add_member_main_btn, is_group and is_owner)
        if hasattr(self, "remove_member_main_btn"):
            self._set_button_visible(self.remove_member_main_btn, is_group and is_owner)
        if hasattr(self, "leave_group_main_btn"):
            self._set_button_visible(self.leave_group_main_btn, is_group and is_active_member)
        if hasattr(self, "delete_friend_main_btn"):
            self._set_button_visible(self.delete_friend_main_btn, False)

    def set_chat_section(self, section: str) -> None:
        self.current_chat_section = str(section or "recent")
        for btn, name in [(self.recent_btn, "recent"), (self.groups_btn, "contacts"), (self.devices_btn, "devices")]:
            style_button(btn, "active" if name == self.current_chat_section else "secondary")
        self.refresh_chat_main()

    def _short_fp(self, fp: str) -> str:
        text = str(fp or "")
        return text[:12] + ("…" if len(text) > 12 else "")

    def _chat_entry_display_data(self, label: str, value: str, meta: Dict[str, object]) -> Dict[str, str]:
        meta = meta or {}
        text = str(label or "")
        lines = text.splitlines()
        first = lines[0] if lines else ""
        second = lines[1] if len(lines) > 1 else ""
        title = str(meta.get("title") or "").strip()
        meta_text = str(meta.get("time") or meta.get("meta_text") or "").strip()
        if not title:
            if "    " in first:
                left, right = first.rsplit("    ", 1)
                title = left.strip()
                if not meta_text:
                    meta_text = re.sub(r"\s*\(\d+\)\s*$", "", right).strip()
            else:
                title = first.strip()
        preview = str(meta.get("preview") or second or "").strip()
        status_icon = str(meta.get("status_icon") or "").strip()
        if status_icon and preview and not preview.startswith(status_icon):
            preview = f"{status_icon} {preview}"
        status_text = str(meta.get("status_text") or meta.get("kind") or "").strip()
        badge_text = str(meta.get("badge_text") or "").strip()
        if not badge_text:
            unread = 0
            try:
                unread = int(meta.get("unread_count") or 0)
            except Exception:
                unread = 0
            if unread <= 0:
                match = re.search(r"\((\d+)\)\s*$", first)
                if match:
                    try:
                        unread = int(match.group(1))
                    except Exception:
                        unread = 0
            if unread > 0:
                badge_text = "99+" if unread > 99 else str(unread)
        if not title:
            title = str(value or "").strip() or self.cu("no_chat")
        if not preview:
            preview = self.cu("no_message")
        return {
            "title": title,
            "preview": preview,
            "meta_text": meta_text,
            "status_text": status_text,
            "badge_text": badge_text,
        }

    def _legacy_chat_entry_button(self, label: str, value: str, meta: Dict[str, object], active: bool) -> Button:
        role = "active" if active else "secondary"
        btn = make_button(role, text=str(label), size_hint_y=None, height=dp(62))
        btn.halign = "center"
        btn.valign = "middle"
        btn.bold = bool((meta or {}).get("unread"))
        btn.shorten = False
        btn.bind(size=lambda inst, _val: setattr(inst, "text_size", (inst.width - dp(16), None)))
        return btn

    def _build_chat_entry_widget(self, label: str, value: str, meta: Dict[str, object], active: bool):
        if UIConversationItem is not None:
            try:
                display = self._chat_entry_display_data(label, value, meta)
                return UIConversationItem(
                    title=display["title"],
                    preview=display["preview"],
                    meta_text=display["meta_text"],
                    status_text=display["status_text"],
                    badge_text=display["badge_text"],
                    active=active,
                    size_hint_y=None,
                    height=dp(62),
                )
            except Exception:
                pass
        return self._legacy_chat_entry_button(label, value, meta, active)

    def _set_chat_entry_buttons(self, entries: List[tuple]) -> None:
        if not hasattr(self, "chat_items_box"):
            return
        self.chat_items_box.clear_widgets()
        values = []
        for item in entries:
            if len(item) >= 3:
                label, value, meta = item[0], item[1], item[2] or {}
            else:
                label, value, meta = item[0], item[1], {}
            values.append(value)
            active = str(value or "") == str(getattr(self, "selected_chat_entry_value", "") or "")
            btn = self._build_chat_entry_widget(str(label), str(value or ""), meta, active)
            btn.bind(on_release=lambda _btn, v=value: self._select_chat_entry(v))
            def _right_click(inst, touch, v=value):
                if getattr(touch, "button", "") == "right" and inst.collide_point(*touch.pos):
                    self._open_chat_entry_context_menu(inst, str(v or ""), tuple(touch.pos))
                    return True
                return False
            btn.bind(on_touch_down=_right_click)
            self.chat_items_box.add_widget(btn)
        self.chat_list_spinner.values = values
        if values and self.chat_list_spinner.text not in values:
            self.chat_list_spinner.text = values[0]
        elif not values:
            self.chat_list_spinner.text = ""

    def _close_chat_context_menu(self, *_args) -> None:
        overlay = getattr(self, "_chat_context_overlay", None)
        if overlay is not None:
            try:
                Window.remove_widget(overlay)
            except Exception:
                pass
        self._chat_context_overlay = None

    def _build_context_menu_button(self, label: str, role: str, callback) -> Button:
        btn = Button(
            text=str(label or ""),
            size_hint_y=None,
            height=dp(40),
            font_name=UI_FONT,
            background_normal="",
            background_down="",
            halign="left",
            valign="middle",
        )
        btn.text_size = (dp(190), None)
        btn.padding = (dp(14), 0)
        # The menu item background is never danger-red. Dangerous actions only use red text.
        btn.background_color = THEME.get("menu_bg", THEME["panel_bg"])
        btn.color = THEME.get("menu_danger_text", THEME["danger"]) if role == "danger" else THEME["text"]

        def _sync_text_size(inst, _value):
            try:
                inst.text_size = (max(1, inst.width - dp(28)), None)
            except Exception:
                pass

        def _run(*_):
            self._close_chat_context_menu()
            try:
                callback()
            except Exception as exc:
                try:
                    self.status_text.text = f"context action failed: {exc}"
                except Exception:
                    pass

        btn.bind(size=_sync_text_size)
        btn.bind(on_release=_run)
        return btn

    def _show_context_menu_overlay(self, actions: List[tuple[str, str, object]], pos) -> None:
        self._close_chat_context_menu()
        if not actions:
            return
        item_h = dp(40)
        width = dp(224)
        height = dp(16) + item_h * len(actions)
        overlay = FloatLayout(size=Window.size, pos=(0, 0))
        # Transparent full-window catcher: clicking outside the panel closes the menu.
        catcher = Button(
            text="",
            size_hint=(None, None),
            size=Window.size,
            pos=(0, 0),
            background_normal="",
            background_down="",
            background_color=(0, 0, 0, 0),
        )
        catcher.bind(on_release=lambda *_: self._close_chat_context_menu())
        overlay.add_widget(catcher)

        panel = BoxLayout(
            orientation="vertical",
            spacing=dp(2),
            padding=(dp(8), dp(8), dp(8), dp(8)),
            size_hint=(None, None),
            width=width,
            height=height,
        )
        apply_card_background(panel, "menu_bg", radius=10)
        for label, role, callback in actions:
            panel.add_widget(self._build_context_menu_button(label, role, callback))

        try:
            x, y = float(pos[0]), float(pos[1])
        except Exception:
            x, y = Window.mouse_pos
        margin = dp(8)
        x = max(margin, min(x, Window.width - width - margin))
        y = max(margin, min(y - height, Window.height - height - margin))
        panel.pos = (x, y)
        overlay.add_widget(panel)

        def _resize(_win, w, h):
            try:
                overlay.size = (w, h)
                catcher.size = (w, h)
                px, py = panel.pos
                panel.pos = (max(margin, min(px, w - width - margin)), max(margin, min(py, h - height - margin)))
            except Exception:
                pass

        Window.bind(on_resize=_resize)
        overlay._context_resize_cb = _resize
        self._chat_context_overlay = overlay
        Window.add_widget(overlay)

    def _open_chat_entry_context_menu(self, anchor, value: str, pos=None) -> None:
        value = str(value or "")
        if not value:
            return

        actions: List[tuple[str, str, object]] = []

        def add_action(label: str, callback, role: str = "normal") -> None:
            actions.append((str(label or ""), str(role or "normal"), callback))

        add_action(self.cu("ctx_open_chat"), lambda: self._select_chat_entry(value))

        if value.startswith("direct::") and self.message_service is not None:
            _tag0, pid0, _name0 = value.split("::", 2)
            add_action(
                self.cu("ctx_unpin") if self.message_service.is_pinned("direct", pid0) else self.cu("ctx_pin"),
                lambda pid=pid0: (self.message_service.toggle_pinned("direct", pid), self.refresh_chat_main()),
            )
        elif value.startswith("group::") and self.message_service is not None:
            _tag0, gid0, _title0 = value.split("::", 2)
            add_action(
                self.cu("ctx_unpin") if self.message_service.is_pinned("group", gid0) else self.cu("ctx_pin"),
                lambda gid=gid0: (self.message_service.toggle_pinned("group", gid), self.refresh_chat_main()),
            )

        if value.startswith("direct::"):
            _tag, pid, name = value.split("::", 2)
            add_action(self.cu("ctx_view_profile"), lambda pid=pid: self._show_direct_profile_popup(pid))
            add_action(self.cu("ctx_join_group"), lambda pid=pid: self._add_peer_to_group_popup(pid))
            add_action(self.cu("ctx_rescan_ip"), lambda: self.search_receivers())
            add_action(self.cu("ctx_delete_friend"), lambda v=value: (self._select_chat_entry(v), self.confirm_delete_contact()), role="danger")
        elif value.startswith("group::"):
            add_action(self.cu("ctx_join_group"), lambda v=value: (self._select_chat_entry(v), self.add_selected_contact_to_current_group()))
            add_action(self.cu("ctx_leave_group"), lambda v=value: (self._select_chat_entry(v), self.confirm_leave_group()), role="danger")
        else:
            add_action(self.cu("ctx_add_contact"), lambda v=value: (self._select_chat_entry(v), self.request_or_add_selected_device()))
            add_action(self.cu("ctx_rescan_ip"), lambda: self.search_receivers())

        try:
            menu_pos = tuple(pos) if pos is not None else tuple(anchor.to_window(anchor.x, anchor.y))
        except Exception:
            menu_pos = Window.mouse_pos
        self._show_context_menu_overlay(actions, menu_pos)

    def _show_direct_profile_popup(self, peer_id: str) -> None:
        if self.contact_service is None:
            return
        contact = self.contact_service.find_contact(peer_id)
        if not contact:
            return
        msg = (
            f"{self.cu('contact')}: {contact.get('remark_name') or contact.get('display_name') or contact.get('nickname') or contact.get('peer_id')}\n"
            f"{self.cu('peer_id')}: {contact.get('peer_id')}\n"
            f"{self.cu('fingerprint')}: {self._short_fp(str(contact.get('fingerprint') or ''))}\n"
            f"{self.cu('ip')}: {contact.get('peer_ip')}:{contact.get('peer_port')}\n"
            f"{self.cu('state')}: {contact.get('trust_state')}\n"
        )
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        lab = make_label(text=msg, halign="left", valign="top")
        bind_label_wrap(lab)
        content.add_widget(lab)
        btn = make_button("secondary", text="OK", size_hint_y=None, height=dp(38), on_release=lambda *_: popup.dismiss())
        content.add_widget(btn)
        popup = style_popup(Popup(title=self.cu("friend_info"), content=content, size_hint=(0.6, 0.5), auto_dismiss=True))
        apply_ui_font(content)
        popup.open()


    def _add_peer_to_group_popup(self, peer_id: str) -> None:
        if self.group_service is None or self.contact_service is None:
            return
        groups = [
            g for g in self.group_service.list_groups()
            if self._is_local_group_owner(str(g.get("group_id") or ""))
        ]
        contact = self.contact_service.find_contact(peer_id)
        if not groups or not contact:
            return
        values = [f"{g.get('group_id')} | {g.get('title') or g.get('group_id')}" for g in groups]
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        sp = style_spinner(Spinner(text=values[0], values=values, font_name=UI_FONT, size_hint_y=None, height=dp(38)))
        content.add_widget(sp)
        popup = style_popup(Popup(title=self.cu("ctx_join_group"), content=content, size_hint=(0.6, 0.35), auto_dismiss=False))
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        def _ok(*_):
            gid = sp.text.split("|", 1)[0].strip()
            self.group_service.add_member_from_contact(gid, contact)
            popup.dismiss()
            self.refresh_chat_main()
        buttons.add_widget(make_button("success", text=self.cu("add_member"), on_release=_ok))
        buttons.add_widget(make_button("secondary", text="Cancel", on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()


    def _select_chat_entry(self, value: str) -> None:
        value = str(value or "")
        if not value:
            return
        self.selected_chat_entry_value = value
        self.chat_list_spinner.text = value
        self.on_chat_list_selected(value)
        self.refresh_chat_main()

    def scan_devices_for_chat(self) -> None:
        self.set_chat_section("devices")
        self.search_receivers()

    def _message_preview(self, msg: Dict[str, object]) -> str:
        if self._is_screen_control_chat(msg):
            return self._screen_control_preview()
        body_type = str(msg.get("body_type") or "text")
        raw = str(msg.get("text") or "")
        if body_type == "file":
            try:
                obj = json.loads(raw)
                name = str(obj.get("name") or obj.get("path") or raw)
            except Exception:
                name = raw
            return "[" + self.cu("file") + "] " + shorten_middle(os.path.basename(name) or name, 20)
        raw = raw.replace("\n", " ").strip()
        return shorten_middle(raw, 22) if raw else self.cu("no_message")

    def _message_time(self, msg: Dict[str, object]) -> str:
        try:
            ts = float(msg.get("sent_at") or msg.get("created_at") or 0)
            return time.strftime("%H:%M", time.localtime(ts)) if ts else ""
        except Exception:
            return ""

    def _list_status_icon(self, msg: Dict[str, object], store=None) -> str:
        try:
            if str(msg.get("direction") or "") != "outgoing":
                return ""
            summary = store.receipt_summary(str(msg.get("message_id") or "")) if store is not None else str(msg.get("status") or "")
            icon, _color = self.main_messages_box._status_state(summary, True) if hasattr(self, "main_messages_box") else ("", None)
            return icon
        except Exception:
            return ""

    def _latest_group_preview(self, store, group_id: str) -> Tuple[str, str, float, str, int]:
        try:
            rows = store.list_messages(group_id=group_id, limit=1)
            unread = store.unread_count_group(group_id) if hasattr(store, "unread_count_group") else 0
            if not rows:
                return self.cu("no_message"), "", 0.0, "", unread
            msg = rows[-1]
            return self._message_preview(msg), self._message_time(msg), float(msg.get("created_at") or 0.0), self._list_status_icon(msg, store), unread
        except Exception:
            return self.cu("no_message"), "", 0.0, "", 0

    def _latest_direct_preview(self, store, peer_id: str) -> Tuple[str, str, float, str, int]:
        try:
            conv = store.create_direct_conversation(peer_id)
            rows = store.list_messages(conversation_id=conv, limit=1)
            unread = store.unread_count_direct(peer_id) if hasattr(store, "unread_count_direct") else 0
            if not rows:
                return self.cu("no_message"), "", 0.0, "", unread
            msg = rows[-1]
            return self._message_preview(msg), self._message_time(msg), float(msg.get("created_at") or 0.0), self._list_status_icon(msg, store), unread
        except Exception:
            return self.cu("no_message"), "", 0.0, "", 0

    def _format_chat_list_label(self, name: str, preview: str, tlabel: str, status_icon: str = "", unread: int = 0) -> str:
        right = tlabel or ""
        if unread > 0:
            right = (right + "  " if right else "") + f"({unread})"
        first = str(name or "") + (("    " + right) if right else "")
        second = ((status_icon + " ") if status_icon else "") + str(preview or self.cu("no_message"))
        return f"{first}\n{second}"

    def refresh_chat_main(self) -> None:
        if hasattr(self, "right_info_box"):
            self.right_info_box.clear()
        section = getattr(self, "current_chat_section", "recent")
        store = self.chat_store
        entries: List[tuple[str, str]] = []
        query = str(getattr(getattr(self, "chat_filter_input", None), "text", "") or "").strip().lower()

        if section == "devices":
            devices = sorted(self.discovered, key=lambda x: float(x.get("last_seen") or 0), reverse=True)
            for d in devices:
                name = self._device_display_name(d)
                fp = self._device_fingerprint(d)
                pid = self._device_peer_id(d)
                ip, port = self._receiver_endpoint(d)
                label = f"{name}"
                if query and query not in label.lower() and query not in pid.lower() and query not in fp.lower():
                    continue
                entries.append((
                    label,
                    self._chat_device_value(d),
                    {
                        "title": name,
                        "preview": f"{ip}:{port}" if ip else self.cu("no_message"),
                        "time": self._short_fp(fp),
                        "kind": self.cu("devices"),
                    },
                ))
            if not entries:
                entries.append(("未发现在线设备\n点击下方“扫描设备”", ""))
            self._set_chat_entry_buttons(entries)
            return

        if self.message_service is None or self.group_service is None or self.contact_service is None:
            self._set_chat_entry_buttons([(self.cu("chat_locked"), "")])
            return

        try:
            groups = self.group_service.list_groups()
            contacts = self.contact_service.trusted_contacts()

            if section == "recent":
                recent_items = []
                for g in groups:
                    title = str(g.get("title") or g.get("group_id") or "群聊")
                    gid = str(g.get("group_id") or "")
                    preview, tlabel, ts, status_icon, unread = self._latest_group_preview(store, gid)
                    pinned = store.is_pinned("group", gid) if hasattr(store, "is_pinned") else False
                    label = self._format_chat_list_label(("[PIN] " if pinned else "") + title, preview, tlabel, status_icon, unread)
                    if query and query not in label.lower() and query not in gid.lower():
                        continue
                    recent_items.append((
                        1 if pinned else 0,
                        ts or float(g.get("updated_at") or g.get("created_at") or 0),
                        label,
                        f"group::{gid}::{title}",
                        {
                            "title": ("[PIN] " if pinned else "") + title,
                            "preview": preview,
                            "time": tlabel,
                            "kind": self.cu("group_prefix"),
                            "status_icon": status_icon,
                            "unread": unread > 0,
                            "unread_count": unread,
                            "pinned": pinned,
                        },
                    ))
                for c in contacts:
                    name = str(c.get("remark_name") or c.get("display_name") or c.get("nickname") or c.get("peer_id"))
                    pid = str(c.get("peer_id") or "")
                    preview, tlabel, ts, status_icon, unread = self._latest_direct_preview(store, pid)
                    pinned = store.is_pinned("direct", pid) if hasattr(store, "is_pinned") else False
                    label = self._format_chat_list_label(("[PIN] " if pinned else "") + name, preview, tlabel, status_icon, unread)
                    if query and query not in label.lower() and query not in pid.lower():
                        continue
                    recent_items.append((
                        1 if pinned else 0,
                        ts,
                        label,
                        f"direct::{pid}::{name}",
                        {
                            "title": ("[PIN] " if pinned else "") + name,
                            "preview": preview,
                            "time": tlabel,
                            "kind": self.cu("friend_prefix"),
                            "status_icon": status_icon,
                            "unread": unread > 0,
                            "unread_count": unread,
                            "pinned": pinned,
                        },
                    ))
                for _pin, _ts, label, value, meta in sorted(recent_items, key=lambda x: (int(x[0] or 0), float(x[1] or 0)), reverse=True):
                    entries.append((label, value, meta))
            else:
                # 联系人页：群聊在上，好友在下。
                for g in groups:
                    title = str(g.get("title") or g.get("group_id") or "群聊")
                    gid = str(g.get("group_id") or "")
                    preview, tlabel, _ts, status_icon, unread = self._latest_group_preview(store, gid)
                    pinned = store.is_pinned("group", gid) if hasattr(store, "is_pinned") else False
                    label = self._format_chat_list_label(("[PIN] " if pinned else "") + f"{self.cu('group_prefix')}  {title}", preview, tlabel, status_icon, unread)
                    if query and query not in label.lower() and query not in gid.lower():
                        continue
                    entries.append((
                        label,
                        f"group::{gid}::{title}",
                        {
                            "title": ("[PIN] " if pinned else "") + f"{self.cu('group_prefix')}  {title}",
                            "preview": preview,
                            "time": tlabel,
                            "kind": self.cu("group_prefix"),
                            "status_icon": status_icon,
                            "unread": unread > 0,
                            "unread_count": unread,
                            "pinned": pinned,
                        },
                    ))
                for c in contacts:
                    name = str(c.get("remark_name") or c.get("display_name") or c.get("nickname") or c.get("peer_id"))
                    pid = str(c.get("peer_id") or "")
                    preview, tlabel, _ts, status_icon, unread = self._latest_direct_preview(store, pid)
                    pinned = store.is_pinned("direct", pid) if hasattr(store, "is_pinned") else False
                    label = self._format_chat_list_label(("[PIN] " if pinned else "") + f"{self.cu('friend_prefix')}  {name}", preview, tlabel, status_icon, unread)
                    if query and query not in label.lower() and query not in pid.lower():
                        continue
                    entries.append((
                        label,
                        f"direct::{pid}::{name}",
                        {
                            "title": ("[PIN] " if pinned else "") + f"{self.cu('friend_prefix')}  {name}",
                            "preview": preview,
                            "time": tlabel,
                            "kind": self.cu("friend_prefix"),
                            "status_icon": status_icon,
                            "unread": unread > 0,
                            "unread_count": unread,
                            "pinned": pinned,
                        },
                    ))

            if not entries:
                entries.append((self.cu("no_chat"), ""))
            self._set_chat_entry_buttons(entries)
        except Exception as exc:
            self._set_chat_entry_buttons([(self.cu("refresh_failed", error=exc), "")])

    def _endpoint_for_peer(self, peer_id: str) -> Tuple[str, int]:
        if self.contact_service is None:
            return "", 9999
        return self.contact_service.endpoint_for_peer(str(peer_id or ""))


    def _send_chat_read_to_endpoint(self, *, ip: str, port: int, peer_id: str, message_ids: List[str], group_id: str = "", conversation_id: str = "") -> None:
        ids = [str(x or "").strip() for x in (message_ids or []) if str(x or "").strip()]
        if not ids or not ip or is_unspecified_ip(ip):
            return
        args = [
            "--worker", "sender",
            "--server-ip", ip,
            "--server-port", str(int(port or 9999)),
            "--chat-reader-peer-id", self.chat_local_peer_id,
            "--chat-conversation-id", str(conversation_id or ""),
            "--chat-group-id", str(group_id or ""),
            "--server-pin-file", str(receiver_pin_file(ip, int(port or 9999))),
            "--complete-timeout", "8",
            "--final-ack-timeout", "8",
        ]
        for mid in ids:
            args += ["--chat-read-message-id", mid]
        cmd = ([sys.executable] + args) if FROZEN else ([sys.executable, str(Path(__file__).resolve())] + args)
        def _run():
            try:
                proc = run_no_console(cmd, cwd=str(APP_DIR), capture_output=True, text=True, timeout=20)
                combined = (proc.stdout or "") + ("\n" + proc.stderr if proc.stderr else "")
                if combined.strip():
                    Clock.schedule_once(lambda _dt, s=combined[-2000:]: self.sender_log_box.append(s + ("\n" if not s.endswith("\n") else "")), 0)
            except Exception as exc:
                Clock.schedule_once(lambda _dt, msg=str(exc): self.sender_log_box.append(f"CHAT_READ failed for {peer_id}: {msg}\n"), 0)
        threading.Thread(target=_run, daemon=True).start()

    def _mark_current_chat_read_and_notify(self) -> None:
        if self.message_service is None:
            return
        try:
            if self.current_chat_mode == "direct" and self.current_peer_id:
                by_sender = self.message_service.mark_read_and_collect_receipts(
                    peer_id=self.current_peer_id,
                    local_peer_id=self.chat_local_peer_id,
                )
                if not by_sender:
                    return
                conv = self.message_service.create_direct_conversation(self.current_peer_id) if self.message_service is not None else ""
                for sender, ids in by_sender.items():
                    ip, port = self._endpoint_for_peer(sender)
                    self._send_chat_read_to_endpoint(ip=ip, port=port, peer_id=sender, message_ids=ids, conversation_id=conv)
                Clock.schedule_once(lambda _dt: self.refresh_chat_main(), 0.05)
            elif self.current_chat_mode == "group" and self.current_group_id:
                by_sender = self.message_service.mark_read_and_collect_receipts(
                    group_id=self.current_group_id,
                    local_peer_id=self.chat_local_peer_id,
                )
                if not by_sender:
                    return
                for sender, ids in by_sender.items():
                    ip, port = self._endpoint_for_peer(sender)
                    self._send_chat_read_to_endpoint(ip=ip, port=port, peer_id=sender, message_ids=ids, group_id=self.current_group_id)
                Clock.schedule_once(lambda _dt: self.refresh_chat_main(), 0.05)
        except Exception as exc:
            try:
                self.sender_log_box.append(f"mark read failed: {exc}\n")
            except Exception:
                pass


    def on_chat_list_selected(self, value: str) -> None:
        text = str(value or "")
        if not text:
            return
        if text.startswith("group::"):
            _tag, gid, title = text.split("::", 2)
            self.current_chat_mode = "group"
            self.current_group_id = gid
            self.current_peer_id = ""
            self.current_chat_title.text = ""
        elif text.startswith("direct::"):
            _tag, pid, name = text.split("::", 2)
            self.current_chat_mode = "direct"
            self.current_peer_id = pid
            self.current_group_id = ""
            self.current_chat_title.text = ""
        else:
            # Online device row. Do not auto-save contact.
            self.current_chat_mode = "device"
            self.current_chat_title.text = ""
        self.render_current_chat(reason="switch")

    def _transfer_store_tick(self) -> float:
        if self.file_transfer_service is None:
            return 0.0
        return self.file_transfer_service.max_updated_at()

    def _file_progress_text(self, message_id: str, total_size: int = 0, summary: str = "") -> str:
        mid = str(message_id or "")
        row = {}
        if self.file_transfer_service is not None:
            try:
                row = self.file_transfer_service.progress_for_message(mid) or {}
            except Exception:
                row = {}
        prog = {}
        if row:
            prog = {
                "sent": int(row.get("transferred_bytes") or 0),
                "total": int(row.get("total_bytes") or total_size or 0),
                "pct": float(row.get("pct") or 0.0),
                "avg": float(row.get("avg_mbps") or 0.0),
                "eta": str(row.get("eta") or ""),
                "state": str(row.get("status") or self.cu("receiving")),
            }
        else:
            prog = self.file_message_progress.get(mid, {})
        return file_progress_text(prog, total_size=total_size, summary=summary, translate=self.cu)

    def _chat_card_context_from_message(
        self,
        message_id: str = "",
        *,
        peer_id: str = "",
        group_id: str = "",
        conversation_id: str = "",
    ) -> Dict[str, object]:
        mid = str(message_id or "").strip()
        peer = str(peer_id or "").strip()
        group = str(group_id or "").strip()
        conv = str(conversation_id or "").strip()
        if mid and self.message_service is not None:
            try:
                msg = self.message_service.get_message(mid) or {}
            except Exception:
                msg = {}
            if msg:
                group = group or str(msg.get("group_id") or "").strip()
                conv = conv or str(msg.get("conversation_id") or "").strip()
                sender = str(msg.get("sender_peer_id") or "").strip()
                receiver = str(msg.get("receiver_peer_id") or "").strip()
                local = str(getattr(self, "chat_local_peer_id", "") or "").strip()
                if not peer:
                    if sender and sender != local:
                        peer = sender
                    elif receiver and receiver != local:
                        peer = receiver
        if not group and not conv and not peer:
            if self.current_chat_mode == "group" and self.current_group_id:
                group = str(self.current_group_id or "")
            elif self.current_chat_mode == "direct" and self.current_peer_id:
                peer = str(self.current_peer_id or "")
        if peer and not conv and self.message_service is not None:
            try:
                conv = self.message_service.create_direct_conversation(peer)
            except Exception:
                conv = ""
        meta: Dict[str, object] = {}
        if mid:
            meta["message_id"] = mid
        if peer:
            meta["peer_id"] = peer
        if group:
            meta["group_id"] = group
            meta["chat_mode"] = "group"
            meta["target_id"] = group
        elif conv or peer:
            if conv:
                meta["conversation_id"] = conv
            meta["chat_mode"] = "direct"
            meta["target_id"] = peer or conv
        return meta

    def _chat_message_created_at(self, message_id: str) -> float:
        mid = str(message_id or "").strip()
        if not mid or self.message_service is None:
            return 0.0
        try:
            msg = self.message_service.get_message(mid) or {}
            return float(msg.get("created_at") or msg.get("sent_at") or msg.get("received_at") or 0.0)
        except Exception:
            return 0.0

    def _next_runtime_card_sequence(self) -> int:
        self._chat_runtime_card_sequence = int(getattr(self, "_chat_runtime_card_sequence", 0) or 0) + 1
        return self._chat_runtime_card_sequence

    def _runtime_card_sequence(self, card: Dict[str, object]) -> int:
        meta = dict((card or {}).get("meta") or {})
        for key in ("render_sequence", "sequence"):
            try:
                value = meta.get(key, card.get(key))
                if value not in (None, ""):
                    return int(value)
            except Exception:
                pass
        return 0

    def _runtime_card_sort_key(self, card: Dict[str, object]) -> Tuple[float, int, str]:
        try:
            ts = float((card or {}).get("timestamp") or 0.0)
        except Exception:
            ts = 0.0
        return (ts, self._runtime_card_sequence(card), str((card or {}).get("card_id") or ""))

    def _chat_message_render_key(self, msg: Dict[str, object]) -> str:
        mid = str((msg or {}).get("message_id") or "").strip()
        if mid:
            return "message:" + mid
        sender = str((msg or {}).get("sender_peer_id") or "").strip()
        created = str((msg or {}).get("created_at") or (msg or {}).get("sent_at") or (msg or {}).get("received_at") or "").strip()
        text = str((msg or {}).get("text") or "").strip()
        return "live:" + hashlib.sha1(f"{sender}|{created}|{text}".encode("utf-8", errors="replace")).hexdigest()

    def _is_chat_message_rendered(self, msg: Dict[str, object]) -> bool:
        return self._chat_message_render_key(msg) in getattr(self, "_rendered_chat_message_ids", set())

    def _mark_chat_message_rendered(self, msg: Dict[str, object]) -> None:
        try:
            self._rendered_chat_message_ids.add(self._chat_message_render_key(msg))
        except Exception:
            pass

    def _bump_chat_render_generation(self) -> int:
        self._chat_render_generation = int(getattr(self, "_chat_render_generation", 0) or 0) + 1
        return self._chat_render_generation

    def _sync_chat_render_signature(self) -> None:
        try:
            sig = self._current_chat_signature()
            if sig is not None:
                self._chat_render_sig = sig
        except Exception:
            pass

    def _note_chat_live_update(self) -> None:
        self._bump_chat_render_generation()
        self._sync_chat_render_signature()

    def _message_matches_current_chat(self, msg: Dict[str, object]) -> bool:
        group_id = str((msg or {}).get("group_id") or "").strip()
        if self.current_chat_mode == "group" and self.current_group_id:
            return group_id == str(self.current_group_id or "")
        if self.current_chat_mode != "direct" or not self.current_peer_id:
            return False
        sender = str((msg or {}).get("sender_peer_id") or "").strip()
        receiver = str((msg or {}).get("receiver_peer_id") or "").strip()
        if self.current_peer_id in (sender, receiver):
            return True
        conv = str((msg or {}).get("conversation_id") or "").strip()
        if conv and self.message_service is not None:
            try:
                return conv == self.message_service.create_direct_conversation(self.current_peer_id)
            except Exception:
                return False
        return False

    def _append_text_message_live(self, msg: Dict[str, object]) -> bool:
        data = dict(msg or {})
        if str(data.get("body_type") or "text") != "text":
            return False
        if self._is_screen_control_chat(data) or not self._message_matches_current_chat(data):
            return False
        if self._is_chat_message_rendered(data):
            return False
        try:
            created_at = float(data.get("created_at") or data.get("sent_at") or data.get("received_at") or time.time())
        except Exception:
            created_at = time.time()
        sender_id = str(data.get("sender_peer_id") or "")
        mine = sender_id == str(self.chat_local_peer_id or "")
        timestamp = time.strftime("%H:%M", time.localtime(created_at))
        sender_name = self._display_name_for_peer(sender_id)
        self.main_messages_box.add_message(
            mine=mine,
            sender=sender_name,
            text=str(data.get("text") or ""),
            timestamp=timestamp,
            summary="",
            body_type="text",
            message_id=str(data.get("message_id") or ""),
            show_sender=self.current_chat_mode == "group",
        )
        self._mark_chat_message_rendered(data)
        self._note_chat_live_update()
        return True

    def _chat_card_matches_current(self, card: Dict[str, object]) -> bool:
        meta = dict((card or {}).get("meta") or {})
        if self.current_chat_mode == "group" and self.current_group_id:
            return str(meta.get("group_id") or "") == str(self.current_group_id or "")
        if self.current_chat_mode == "direct" and self.current_peer_id:
            if str(meta.get("peer_id") or "") == str(self.current_peer_id or ""):
                return True
            conv = str(meta.get("conversation_id") or "")
            if conv and self.message_service is not None:
                try:
                    return conv == self.message_service.create_direct_conversation(self.current_peer_id)
                except Exception:
                    return False
        return False

    def _runtime_file_card(self, message_id: str) -> Dict[str, object]:
        card_id = f"file_transfer:{str(message_id or '').strip()}"
        if not card_id.endswith(":"):
            for card in self.chat_runtime_cards or []:
                if str(card.get("card_id") or "") == card_id:
                    return dict(card)
        return {}

    def _has_runtime_file_card(self, message_id: str) -> bool:
        return bool(self._runtime_file_card(message_id))

    def _file_card_direction(self, message_id: str = "", requested: str = "") -> str:
        req = str(requested or "").strip().lower()
        existing = self._runtime_file_card(message_id)
        if existing:
            meta = dict(existing.get("meta") or {})
            old = str(existing.get("direction") or existing.get("side") or meta.get("direction") or meta.get("side") or "").strip().lower()
            if old in ("incoming", "outgoing", "system"):
                return old
        mid = str(message_id or "").strip()
        if mid and self.message_service is not None:
            try:
                msg = self.message_service.get_message(mid) or {}
            except Exception:
                msg = {}
            sender = str(msg.get("sender_peer_id") or "").strip()
            local = str(getattr(self, "chat_local_peer_id", "") or "").strip()
            if sender and local:
                return "outgoing" if sender == local else "incoming"
        if req in ("outgoing", "incoming", "system"):
            return req
        return "incoming"

    def _add_runtime_chat_card(
        self,
        card: Dict[str, object],
        *,
        message_id: str = "",
        peer_id: str = "",
        group_id: str = "",
        conversation_id: str = "",
        replace: bool = True,
    ) -> Dict[str, object]:
        data = dict(card or {})
        meta = dict(data.get("meta") or {})
        ctx = self._chat_card_context_from_message(
            str(message_id or meta.get("message_id") or ""),
            peer_id=str(peer_id or meta.get("peer_id") or ""),
            group_id=str(group_id or meta.get("group_id") or ""),
            conversation_id=str(conversation_id or meta.get("conversation_id") or ""),
        )
        meta.update({k: v for k, v in ctx.items() if v not in (None, "")})
        data["meta"] = meta
        card_type = str(data.get("card_type") or data.get("type") or CARD_SYSTEM)
        data["card_type"] = card_type
        if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER, CARD_SCREEN_OFFER, CARD_SCREEN_STATE):
            direction = str(data.get("direction") or meta.get("direction") or "").strip().lower()
            if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER) and direction not in ("incoming", "outgoing", "system"):
                direction = self._file_card_direction(str(message_id or meta.get("message_id") or ""), direction)
            elif card_type in (CARD_SCREEN_OFFER, CARD_SCREEN_STATE) and direction not in ("incoming", "outgoing", "system"):
                direction = "incoming"
            data["direction"] = direction
            data["side"] = direction
            meta["direction"] = direction
            meta["side"] = direction
        should_update_widget = card_type not in (CARD_FILE_OFFER, CARD_FILE_TRANSFER)
        raw_ts = data.get("timestamp")
        explicit_ts = raw_ts not in (None, "", 0, 0.0, "0")
        try:
            ts = float(raw_ts or 0.0)
        except Exception:
            ts = 0.0
        if ts <= 0:
            ts = self._chat_message_created_at(str(meta.get("message_id") or message_id or "")) or time.time()
            data["timestamp"] = ts
        if not str(data.get("card_id") or "").strip():
            base = str(meta.get("message_id") or f"{ts:.3f}")
            data["card_id"] = f"{card_type}:{base}"
        card_id = str(data.get("card_id") or "")
        if replace and card_id:
            for idx, old in enumerate(list(self.chat_runtime_cards)):
                if str(old.get("card_id") or "") == card_id:
                    old_meta = dict(old.get("meta") or {})
                    old_direction = str(old.get("direction") or old.get("side") or old_meta.get("direction") or old_meta.get("side") or "").strip().lower()
                    if not explicit_ts:
                        try:
                            data["timestamp"] = float(old.get("timestamp") or data.get("timestamp") or ts)
                        except Exception:
                            data["timestamp"] = data.get("timestamp") or ts
                    old_sequence = old_meta.get("render_sequence") or old_meta.get("sequence")
                    if old_sequence not in (None, ""):
                        meta["render_sequence"] = old_sequence
                        data["render_sequence"] = old_sequence
                    if card_type in (CARD_FILE_OFFER, CARD_FILE_TRANSFER, CARD_SCREEN_OFFER, CARD_SCREEN_STATE) and old_direction in ("incoming", "outgoing", "system"):
                        data["direction"] = old_direction
                        data["side"] = old_direction
                        meta["direction"] = old_direction
                        meta["side"] = old_direction
                        data["meta"] = meta
                    self.chat_runtime_cards[idx] = data
                    if should_update_widget:
                        self._update_runtime_chat_card_widget(data)
                    if self._chat_card_matches_current(data):
                        self._note_chat_live_update()
                    return data
        if meta.get("render_sequence") in (None, ""):
            meta["render_sequence"] = self._next_runtime_card_sequence()
        data["render_sequence"] = meta.get("render_sequence")
        data["meta"] = meta
        self.chat_runtime_cards.append(data)
        if len(self.chat_runtime_cards) > 160:
            self.chat_runtime_cards = self.chat_runtime_cards[-160:]
        if should_update_widget:
            self._update_runtime_chat_card_widget(data)
        if self._chat_card_matches_current(data):
            self._note_chat_live_update()
        return data

    def _update_runtime_chat_card_widget(self, card: Dict[str, object]) -> None:
        try:
            data = dict(card or {})
            if not self._chat_card_matches_current(data):
                return
            Clock.schedule_once(lambda _dt, item=data: self.main_messages_box.update_card(item), 0)
        except Exception:
            pass

    def _remove_runtime_chat_card(self, card_id: str) -> None:
        cid = str(card_id or "")
        if not cid:
            return
        self.chat_runtime_cards = [card for card in self.chat_runtime_cards if str(card.get("card_id") or "") != cid]
        try:
            Clock.schedule_once(lambda _dt, item=cid: self.main_messages_box.remove_card(item), 0)
        except Exception:
            pass

    def _current_runtime_chat_cards_sorted(self) -> List[Dict[str, object]]:
        return sorted(
            [dict(card) for card in self.chat_runtime_cards if self._chat_card_matches_current(dict(card))],
            key=self._runtime_card_sort_key,
        )

    def _render_runtime_chat_cards_before(self, cutoff_ts: float, rendered_ids: set) -> None:
        try:
            cutoff = float(cutoff_ts or 0.0)
        except Exception:
            cutoff = 0.0
        for card in self._current_runtime_chat_cards_sorted():
            cid = str(card.get("card_id") or "")
            if cid and cid in rendered_ids:
                continue
            try:
                ts = float(card.get("timestamp") or 0.0)
            except Exception:
                ts = 0.0
            if cutoff > 0 and ts > cutoff:
                continue
            self.main_messages_box.add_card(card)
            if cid:
                rendered_ids.add(cid)

    def _render_remaining_runtime_chat_cards(self, rendered_ids: set) -> None:
        for card in self._current_runtime_chat_cards_sorted():
            cid = str(card.get("card_id") or "")
            if cid and cid in rendered_ids:
                continue
            self.main_messages_box.add_card(card)
            if cid:
                rendered_ids.add(cid)

    def _render_runtime_chat_cards(self) -> None:
        try:
            for card in self._current_runtime_chat_cards_sorted():
                self.main_messages_box.add_card(card)
        except Exception as exc:
            try:
                self.sender_log_box.append(f"chat card render failed: {exc}\n")
            except Exception:
                pass

    def _sync_runtime_chat_cards(self) -> None:
        try:
            for card in self._current_runtime_chat_cards_sorted():
                self.main_messages_box.update_card(card)
        except Exception as exc:
            try:
                self.sender_log_box.append(f"chat card update failed: {exc}\n")
            except Exception:
                pass

    def _file_card_info(
        self,
        message_id: str = "",
        *,
        fallback_name: str = "",
        fallback_path: str = "",
        fallback_size: int = 0,
    ) -> Tuple[str, str, int]:
        name = str(fallback_name or "").strip()
        path = str(fallback_path or "").strip()
        size = int(fallback_size or 0)
        mid = str(message_id or "").strip()
        if mid and self.message_service is not None:
            try:
                msg = self.message_service.get_message(mid) or {}
            except Exception:
                msg = {}
            text = str(msg.get("text") or "")
            if text:
                try:
                    body = json.loads(text)
                    if isinstance(body, dict):
                        name = name or str(body.get("name") or "").strip()
                        path = path or str(body.get("path") or "").strip()
                        size = size or int(body.get("size") or 0)
                except Exception:
                    if not name:
                        name = os.path.basename(text) or text
                    if not path:
                        path = text
        if not name and path:
            name = os.path.basename(path)
        return name or unnamed_file_text(self.lang), path, size

    def _file_peer_label(self, peer_id: str = "", fallback: str = "") -> str:
        peer = str(peer_id or "").strip()
        try:
            label = self._display_name_for_peer(peer)
        except Exception:
            label = ""
        return str(label or fallback or peer or file_transfer_remote_peer_text(self.lang))

    def _file_card_title(self) -> str:
        return file_card_title(self.lang)

    def _multi_file_card_title(self) -> str:
        return multi_file_card_title(self.lang)

    def _multi_file_summary(self, count: int) -> str:
        return multi_file_summary(count, self.lang)

    def _folder_not_supported_text(self) -> str:
        return folder_not_supported_text(self.lang)

    def _file_offer_title(self) -> str:
        return file_offer_title(self.lang)

    def _file_waiting_text(self, peer_label: str = "") -> str:
        return file_waiting_text(peer_label, self.lang)

    def _file_incoming_text(self, peer_label: str = "") -> str:
        return file_incoming_text(peer_label, self.lang)

    def _file_waiting_confirm_text(self) -> str:
        return file_waiting_confirm_text(self.lang)

    def _file_accepted_text(self) -> str:
        return file_accepted_text(self.lang)

    def _file_rejected_local_text(self) -> str:
        return file_rejected_local_text(self.lang)

    def _file_rejected_by_peer_text(self) -> str:
        return file_rejected_by_peer_text(self.lang)

    def _file_completed_text(self) -> str:
        return file_completed_text(self.lang)

    def _file_failed_text(self, reason: object = "") -> str:
        return file_failed_text(reason, self.lang)

    def _file_error_message(self, code: str = "", detail: str = "") -> str:
        return file_error_message(code, detail, lang=self.lang, translate=self.t)

    def _file_resume_text(self, offset: int = 0) -> str:
        return file_resume_text(offset, self.lang)

    def _file_size_detail(self, size: int = 0, *, peer_label: str = "", path: str = "", prefix: str = "") -> str:
        return file_size_detail(size, peer_label=peer_label, path=path, prefix=prefix, lang=self.lang)

    def _add_file_offer_chat_card(
        self,
        *,
        message_id: str = "",
        peer_id: str = "",
        group_id: str = "",
        conversation_id: str = "",
        file_name: str = "",
        file_path: str = "",
        total_size: int = 0,
        status: str = "Waiting",
        detail: str = "",
        actions: Optional[List[Dict[str, object]]] = None,
        direction: str = "incoming",
    ) -> None:
        direction = self._file_card_direction(message_id, direction)
        name, _path, size = self._file_card_info(
            message_id,
            fallback_name=file_name,
            fallback_path=file_path,
            fallback_size=total_size,
        )
        display_name = truncate_filename(name, 48)
        detail_text = detail or self._file_size_detail(size)
        if display_name != name:
            detail_text = (detail_text + "  " if detail_text else "") + f"name: {name}"
        card = make_card(
            CARD_FILE_OFFER,
            title=self._file_offer_title(),
            subtitle=display_name,
            status=status,
            detail=detail_text,
            direction=direction,
            side=direction,
            actions=actions or [],
            card_id=f"file_transfer:{message_id or name}",
            meta={"direction": direction, "side": direction},
        )
        self._add_runtime_chat_card(card, message_id=message_id, peer_id=peer_id, group_id=group_id, conversation_id=conversation_id)

    def _add_file_transfer_chat_card(
        self,
        *,
        message_id: str,
        peer_id: str = "",
        group_id: str = "",
        conversation_id: str = "",
        direction: str = "",
        transferred: int = 0,
        total: int = 0,
        pct: float = 0.0,
        avg: float = 0.0,
        eta: str = "",
        status: str = "",
        error: str = "",
        saved_path: str = "",
        detail: str = "",
        actions: Optional[List[Dict[str, object]]] = None,
        package_count: int = 0,
    ) -> None:
        direction = self._file_card_direction(message_id, direction)
        name, _path, size = self._file_card_info(message_id, fallback_size=total)
        display_name = truncate_filename(name, 48)
        try:
            ctx = dict(self.file_message_tasks.get(str(message_id or ""), {}) or {})
            package_count = int(package_count or ctx.get("package_file_count") or 0)
        except Exception:
            package_count = int(package_count or 0)
        title_text = self._file_card_title()
        if package_count > 1:
            title_text = self._multi_file_card_title()
            display_name = self._multi_file_summary(package_count)
        total = int(total or size or 0)
        status_text = str(status or "").strip() or "Transferring"
        detail_text = file_transfer_card_detail(
            lang=self.lang,
            transferred=transferred,
            total=total,
            pct=pct,
            avg=avg,
            eta=eta,
            detail=detail,
            saved_path=saved_path,
            error=error,
            display_name=display_name,
            original_name=name,
        )
        card_actions = list(actions or [])
        if saved_path:
            card_actions.append({"label": "打开所在文件夹" if self.lang == "zh" else "Open folder", "action": f"open_folder:{saved_path}", "style": "secondary"})
        if direction == "outgoing" and message_id and is_failed_status(status_text, failed_label=self.cu("failed"), lang=self.lang):
            card_actions.append({"label": "继续传输" if self.lang == "zh" else "Resume", "action": f"retry_file:{message_id}", "style": "danger"})
        card = make_card(
            CARD_FILE_TRANSFER,
            title=title_text,
            subtitle=display_name,
            status=status_text,
            detail=detail_text or ("Incoming" if direction == "incoming" else "Outgoing"),
            direction=direction,
            side=direction,
            actions=card_actions,
            card_id=f"file_transfer:{message_id}",
            meta={"direction": direction, "side": direction},
        )
        self._add_runtime_chat_card(card, message_id=message_id, peer_id=peer_id, group_id=group_id, conversation_id=conversation_id)

    def _schedule_file_transfer_chat_card(self, **kwargs) -> None:
        data = dict(kwargs)
        Clock.schedule_once(lambda _dt, data=data: self._add_file_transfer_chat_card(**data), 0)

    def _run_transfer_store_write(self, label: str, operation) -> None:
        def _run() -> None:
            try:
                operation()
            except Exception as exc:
                self._append_debug_line(f"{label} failed: {exc}", protocol=True)

        threading.Thread(target=_run, daemon=True).start()

    def _add_screen_chat_card(
        self,
        card_type: str,
        *,
        session_id: str = "",
        peer_id: str = "",
        title: str = "Screen share",
        subtitle: str = "",
        status: str = "",
        detail: str = "",
        profile: str = "",
        port: object = "",
        actions: Optional[List[Dict[str, object]]] = None,
        direction: str = "",
    ) -> None:
        sid = str(session_id or self.screen_share_session_id or "screen").strip()
        card_direction = str(direction or "").strip().lower()
        if card_direction not in ("incoming", "outgoing", "system"):
            card_direction = "incoming"
        meta = {
            "session_id": sid,
            "profile": str(profile or "").strip(),
            "port": "" if port in (None, "") else str(port),
            "peer_label": str(subtitle or "").strip(),
            "direction": card_direction,
            "side": card_direction,
        }
        card = make_card(
            card_type,
            title=title,
            subtitle=subtitle,
            status=status,
            detail=detail,
            direction=card_direction,
            side=card_direction,
            actions=actions or [],
            card_id=f"screen_share:{sid}",
            meta=meta,
        )
        self._add_runtime_chat_card(card, peer_id=peer_id or self.screen_share_peer_id or self.current_peer_id)

    def _screen_audio_enabled(self) -> bool:
        return bool(getattr(self, "share_system_audio", False))

    def _screen_package_capabilities(self) -> Dict[str, object]:
        try:
            runtime = getattr(self.app, "screen_runtime", None) or ScreenRuntime()
            return dict(runtime.screen_package_info())
        except Exception:
            return {
                "package_flavor": "unknown",
                "native_lite": False,
                "rust_native_available": False,
                "bundled_ffmpeg_available": False,
                "screen_backend_default": SCREEN_BACKEND_FFMPEG,
                "native_screen_video_only": True,
            }

    def _screen_package_snapshot(self) -> Dict[str, object]:
        info = dict(getattr(self, "screen_package_info", None) or {})
        if not info:
            info = self._screen_package_capabilities()
            self.screen_package_info = dict(info)
        return info

    def _native_lite_package(self) -> bool:
        return bool(self._screen_package_snapshot().get("native_lite"))

    def _default_screen_backend(self) -> str:
        default = str(self._screen_package_snapshot().get("screen_backend_default") or "").strip().lower()
        return default if default in SCREEN_BACKEND_VALUES else SCREEN_BACKEND_FFMPEG

    def _normalize_screen_backend(self, backend: object, default: object = None) -> str:
        fallback = str(default or self._default_screen_backend() or SCREEN_BACKEND_FFMPEG).strip().lower()
        if fallback not in SCREEN_BACKEND_VALUES:
            fallback = SCREEN_BACKEND_FFMPEG
        value = str(backend or fallback).strip().lower()
        if value not in SCREEN_BACKEND_VALUES:
            return fallback
        return value

    def _coerce_screen_backend_for_package(self, backend: object, *, persist: bool = False) -> str:
        value = self._normalize_screen_backend(backend)
        package = self._screen_package_snapshot()
        if bool(package.get("native_lite")) and value == SCREEN_BACKEND_FFMPEG and not bool(package.get("bundled_ffmpeg_available")):
            value = SCREEN_BACKEND_RUST
            self.screen_backend_notice = NATIVE_LITE_FFMPEG_UNAVAILABLE_MESSAGE
            if persist:
                try:
                    self.gui_config["screen_backend"] = value
                    save_gui_config(self.gui_config)
                except Exception:
                    pass
        return value

    def _screen_backend(self) -> str:
        value = self._coerce_screen_backend_for_package(getattr(self, "screen_backend", self._default_screen_backend()))
        self.screen_backend = value
        return value

    def _screen_backend_label(self, backend: object = None) -> str:
        value = self._normalize_screen_backend(self._screen_backend() if backend is None else backend)
        return "Rust Native" if value == SCREEN_BACKEND_RUST else "FFmpeg"

    def _screen_backend_from_control(self, control: Dict[str, object], payload: Optional[Dict[str, object]] = None, default: object = SCREEN_BACKEND_FFMPEG) -> str:
        payload = dict(payload or control.get("payload") or {})
        return self._normalize_screen_backend(payload.get("backend") or control.get("backend") or default)

    def _screen_audio_for_backend(self, backend: object, audio: Dict[str, object]) -> Dict[str, object]:
        backend_name = self._normalize_screen_backend(backend)
        if backend_name == SCREEN_BACKEND_RUST and bool((audio or {}).get("enabled")):
            message = NATIVE_LITE_VIDEO_ONLY_MESSAGE if self._native_lite_package() else "Rust native backend currently supports video only"
            return {
                "enabled": False,
                "mode": "none",
                "state": "video_only",
                "error": message,
            }
        return dict(audio or {"enabled": False, "mode": "none"})

    def _screen_audio_config(self, enabled: Optional[bool] = None) -> Dict[str, object]:
        use_audio = self._screen_audio_enabled() if enabled is None else bool(enabled)
        if not use_audio:
            return {"enabled": False, "mode": "none"}
        return {
            "enabled": True,
            "mode": "system",
            "codec": "aac",
            "sample_rate": 48000,
            "channels": 2,
            "bitrate": 128000,
        }

    def _screen_audio_from_control(self, control: Dict[str, object], payload: Optional[Dict[str, object]] = None) -> Dict[str, object]:
        payload = dict(payload or control.get("payload") or {})
        value = payload.get("audio", control.get("audio"))
        if isinstance(value, dict):
            enabled = bool(value.get("enabled"))
            if enabled:
                return {
                    "enabled": True,
                    "mode": "system",
                    "codec": "aac",
                    "sample_rate": int(value.get("sample_rate") or 48000),
                    "channels": int(value.get("channels") or 2),
                    "bitrate": int(value.get("bitrate") or 128000),
                }
            return {"enabled": False, "mode": "none"}
        return {"enabled": False, "mode": "none"}

    def _screen_audio_from_runtime_state(self, state: Optional[Dict[str, object]], fallback: Optional[Dict[str, object]] = None) -> Dict[str, object]:
        data = dict(fallback or {"enabled": False, "mode": "none"})
        try:
            state_dict = dict(state or {})
            runtime_audio = dict(state_dict.get("audio_config") or {})
            if runtime_audio:
                data.update(runtime_audio)
            if "audio_enabled" in state_dict:
                data["enabled"] = bool(state_dict.get("audio_enabled"))
            audio_state = str(state_dict.get("audio_state") or "").strip()
            if audio_state:
                data["state"] = audio_state
        except Exception:
            pass
        return data

    def _screen_audio_text(self, audio: object = None) -> str:
        return screen_audio_text(audio, self.lang)

    def _screen_detail_text(self, profile: object = "", port: object = "", audio: object = None) -> str:
        return screen_detail_text(profile, port, audio, lang=self.lang)

    def _screen_start_failed_text(self, reason: object) -> str:
        return screen_start_failed_text(reason, self.lang)

    def _screen_stop_failed_text(self, reason: object) -> str:
        return screen_stop_failed_text(reason, self.lang)

    def _screen_stopped_text(self) -> str:
        return screen_stopped_text(self.lang)

    def _screen_rejected_by_peer_text(self, peer_label: str) -> str:
        return screen_rejected_by_peer_text(peer_label, self.lang)

    def _screen_rejected_local_text(self) -> str:
        return screen_rejected_local_text(self.lang)

    def _screen_offer_title(self) -> str:
        return screen_offer_title(self.lang)

    def handle_chat_card_action(self, card_id: str, action_id: str) -> None:
        try:
            action = str(action_id or "")
            if action == "stop_screen":
                self.stop_screen_share_from_chat()
                return
            if action.startswith("accept_screen:"):
                session_id = action.split(":", 1)[1]
                control = dict(self.pending_screen_offers.get(session_id) or {})
                if control:
                    pop = self.pending_screen_offer_popups.pop(session_id, None)
                    if pop is not None:
                        try:
                            pop.dismiss()
                        except Exception:
                            pass
                    self._accept_screen_offer(control)
                return
            if action.startswith("reject_screen:"):
                session_id = action.split(":", 1)[1]
                control = dict(self.pending_screen_offers.get(session_id) or {})
                if control:
                    pop = self.pending_screen_offer_popups.pop(session_id, None)
                    if pop is not None:
                        try:
                            pop.dismiss()
                        except Exception:
                            pass
                    self._reject_screen_offer(control, "user_rejected")
                return
            if action.startswith("retry_file:"):
                self.retry_file_message(action.split(":", 1)[1])
                return
            if action.startswith("file_accept:"):
                self._decide_transfer_request(action.split(":", 1)[1], True)
                return
            if action.startswith("file_reject:"):
                self._decide_transfer_request(action.split(":", 1)[1], False)
                return
            if action.startswith("open_folder:"):
                open_file_location(action.split(":", 1)[1])
                return
            self._append_debug_line(f"unknown chat card action: {card_id} {action}", protocol=False)
        except Exception as exc:
            try:
                self.sender_log_box.append(f"chat card action failed: {exc}\n")
            except Exception:
                pass

    def _default_transfer_policy(self, req: Dict[str, object]) -> str:
        if bool((req or {}).get("resume_available")):
            return "resume"
        if bool((req or {}).get("conflict")):
            return "rename"
        return "overwrite"

    def _decide_transfer_request(self, conn_id: object, accepted: bool, policy: str = "") -> None:
        try:
            conn = int(conn_id or 0)
        except Exception:
            conn = 0
        if conn <= 0:
            return
        req = dict(self.pending_transfer_requests.get(conn) or {})
        if not req:
            return
        current_decision = str(self.pending_transfer_decisions.get(conn) or "pending")
        if current_decision in ("accepted", "rejected"):
            return
        self.pending_transfer_decisions[conn] = "accepted" if accepted else "rejected"
        selected_policy = str(policy or self._default_transfer_policy(req) or "overwrite")
        if accepted and selected_policy == "cancel":
            accepted = False
            reason = "file_exists_cancelled"
        else:
            reason = "accepted" if accepted else "rejected"
        approval_dir = self.approval_dir
        approval_dir.mkdir(parents=True, exist_ok=True)
        target = approval_dir / (f"{conn}.accept" if accepted else f"{conn}.reject")
        request_path = approval_dir / f"{conn}.request.json"
        try:
            payload = {"accepted": bool(accepted), "reason": reason, "file_policy": selected_policy}
            target.write_text(json.dumps(payload, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
            try:
                request_path.unlink()
            except Exception:
                pass
            self.seen_request_files.discard(str(request_path))
            self.receiver_log_box.append(("Accepted" if accepted else "Rejected") + f" transfer request conn_id={conn}\n")
        except Exception as exc:
            self.pending_transfer_decisions[conn] = "pending"
            self.receiver_log_box.append(f"Failed to write approval file: {exc}\n")
            return

        mid = str(req.get("chat_message_id") or self.receiving_file_message_by_conn.get(conn) or "")
        total = int(req.get("size") or 0)
        peer_id = str(req.get("chat_sender_peer_id") or req.get("sender_peer_id") or "")
        if mid:
            if self.file_transfer_service is not None:
                self._run_transfer_store_write(
                    "transfer request status update",
                    lambda mid=mid, peer_id=peer_id, total=total, req=dict(req), accepted=accepted, selected_policy=selected_policy: self.file_transfer_service.upsert_incoming_task(
                        chat_message_id=mid,
                        peer_id=peer_id,
                        conversation_id=str(req.get("chat_conversation_id") or ""),
                        group_id=str(req.get("chat_group_id") or ""),
                        file_name=str(req.get("name") or ""),
                        remote_path=str(req.get("save_path") or ""),
                        total_bytes=total,
                        status="accepted" if accepted else "rejected",
                    ),
                )
            if accepted:
                status_text = self._file_accepted_text()
                detail = self._file_size_detail(total, peer_label=self._file_peer_label(peer_id, str(req.get("sender") or "")))
                if selected_policy == "resume":
                    detail = self._file_size_detail(
                        total,
                        peer_label=self._file_peer_label(peer_id, str(req.get("sender") or "")),
                        prefix=self._file_resume_text(int(req.get("resume_offset") or 0)),
                    )
            else:
                status_text = self._file_rejected_local_text()
                detail = str(reason or "")
            self._add_file_offer_chat_card(
                message_id=mid,
                peer_id=peer_id,
                group_id=str(req.get("chat_group_id") or ""),
                conversation_id=str(req.get("chat_conversation_id") or ""),
                file_name=str(req.get("name") or ""),
                file_path=str(req.get("save_path") or ""),
                total_size=total,
                status=status_text,
                detail=detail,
                actions=[],
            )
        self.pending_request_popups.discard(conn)
        self.pending_transfer_requests.pop(conn, None)
        self.pending_transfer_decisions.pop(conn, None)
        pop = self.pending_transfer_popups.pop(conn, None)
        if pop is not None:
            try:
                pop.dismiss()
            except Exception:
                pass
        self._schedule_transfer_card_refresh(force=True)

    def _show_transfer_request_card(self, req: Dict[str, object]) -> None:
        data = dict(req or {})
        try:
            conn = int(data.get("conn_id") or 0)
        except Exception:
            conn = 0
        mid = str(data.get("chat_message_id") or "")
        total = int(data.get("size") or 0)
        peer_id = str(data.get("chat_sender_peer_id") or data.get("sender_peer_id") or "")
        peer_label = self._file_peer_label(peer_id, str(data.get("sender") or ""))
        if conn > 0:
            self.pending_transfer_requests[conn] = data
            self.pending_transfer_decisions.setdefault(conn, "pending")
        detail_prefix = ""
        if bool(data.get("resume_available")):
            detail_prefix = self._file_resume_text(int(data.get("resume_offset") or 0))
        elif bool(data.get("conflict")):
            detail_prefix = "文件已存在" if self.lang == "zh" else "File exists"
        decision = str(self.pending_transfer_decisions.get(conn) or "pending")
        actions = []
        if conn > 0 and decision == "pending":
            actions = [
                {"label": self.t("accept"), "action": f"file_accept:{conn}", "style": "success"},
                {"label": self.t("reject"), "action": f"file_reject:{conn}", "style": "danger"},
            ]
        self._add_file_offer_chat_card(
            message_id=mid,
            peer_id=peer_id,
            group_id=str(data.get("chat_group_id") or ""),
            conversation_id=str(data.get("chat_conversation_id") or ""),
            file_name=str(data.get("name") or ""),
            file_path=str(data.get("save_path") or ""),
            total_size=total,
            status=self._file_waiting_confirm_text(),
            detail=self._file_size_detail(total, peer_label=peer_label, prefix=detail_prefix),
            actions=actions,
        )

    def _latest_outgoing_file_message_id(self) -> str:
        best_mid = ""
        best_ts = -1.0
        for mid, task in (self.file_message_tasks or {}).items():
            try:
                ts = float((task or {}).get("created_at") or 0.0)
            except Exception:
                ts = 0.0
            if ts >= best_ts:
                best_mid = str(mid or "")
                best_ts = ts
        return best_mid

    def _update_latest_file_card_error(self, code: str, detail: str = "") -> None:
        mid = self._latest_outgoing_file_message_id()
        if not mid:
            return
        ctx = dict(self.file_message_tasks.get(mid) or {})
        recipients = [dict(r) for r in (ctx.get("recipients") or [])]
        peer_id = str((recipients[0] or {}).get("peer_id") or "") if recipients else ""
        total = int(ctx.get("total") or 0)
        error_text = self._file_error_message(code, detail)
        self._add_file_transfer_chat_card(
            message_id=mid,
            peer_id=peer_id,
            group_id=str(ctx.get("group_id") or ""),
            conversation_id=str(ctx.get("conversation_id") or ""),
            direction="outgoing",
            transferred=int(self.file_message_progress.get(mid, {}).get("sent") or 0),
            total=total or int(self.file_message_progress.get(mid, {}).get("total") or 0),
            pct=float(self.file_message_progress.get(mid, {}).get("pct") or 0.0),
            avg=float(self.file_message_progress.get(mid, {}).get("avg") or 0.0),
            eta=str(self.file_message_progress.get(mid, {}).get("eta") or ""),
            status=self._file_failed_text(error_text),
            error=error_text,
        )

    def _current_chat_signature(self) -> Optional[Tuple[object, ...]]:
        store = self.chat_store
        if store is None:
            return None
        try:
            if self.current_chat_mode == "group" and self.current_group_id:
                row = store.db.conn.execute(
                    "SELECT COUNT(*) AS n, COALESCE(MAX(created_at),0) AS t FROM messages WHERE group_id=?",
                    (self.current_group_id,),
                ).fetchone()
                return ("group", self.current_group_id, int(row["n"] or 0), float(row["t"] or 0))
            if self.current_chat_mode == "direct" and self.current_peer_id:
                conv = self.message_service.create_direct_conversation(self.current_peer_id)
                row = store.db.conn.execute(
                    "SELECT COUNT(*) AS n, COALESCE(MAX(created_at),0) AS t FROM messages WHERE conversation_id=?",
                    (conv,),
                ).fetchone()
                return ("direct", conv, int(row["n"] or 0), float(row["t"] or 0))
        except Exception:
            return None
        return None

    def _auto_refresh_current_chat(self) -> None:
        sig = self._current_chat_signature()
        if sig is not None and sig != getattr(self, "_chat_render_sig", None):
            self._force_chat_refresh(reason="history_sync")

    def _force_chat_refresh(self, reason: str = "manual_refresh", generation: Optional[int] = None) -> None:
        if generation is not None and generation != getattr(self, "_chat_render_generation", 0):
            return
        self._chat_render_sig = None
        self.refresh_chat_main()
        self.render_current_chat(reason=reason, allow_clear=True)

    def _date_separator_label(self, ts: float) -> str:
        try:
            dt = time.localtime(float(ts or time.time()))
            date_key = time.strftime("%Y-%m-%d", dt)
            today_key = time.strftime("%Y-%m-%d", time.localtime())
            yesterday_key = time.strftime("%Y-%m-%d", time.localtime(time.time() - 86400))
            if date_key == today_key:
                return self.cu("today")
            if date_key == yesterday_key:
                return self.cu("yesterday")
            return date_key
        except Exception:
            return ""

    def _group_record_by_id(self, group_id: str) -> Dict[str, object]:
        if self.group_service is None:
            return {}
        gid = str(group_id or "")
        try:
            for g in self.group_service.list_groups():
                if str(g.get("group_id") or "") == gid:
                    return dict(g)
        except Exception:
            pass
        return {}

    def _local_group_member(self, group_id: str, members: Optional[List[Dict[str, object]]] = None) -> Dict[str, object]:
        local_id = str(getattr(self, "chat_local_peer_id", "") or "")
        if not local_id:
            return {}
        if members is None and self.group_service is not None:
            try:
                members = self.group_service.members(str(group_id or ""), include_inactive=True)
            except Exception:
                members = []
        for m in members or []:
            if str(m.get("peer_id") or "") == local_id:
                return dict(m)
        return {}

    def _member_role_label(self, member: Dict[str, object]) -> str:
        role = str((member or {}).get("role") or "member")
        if role == "owner":
            return "群主" if self.lang == "zh" else "Owner"
        return "成员" if self.lang == "zh" else "Member"

    def _member_state_label(self, member: Dict[str, object]) -> str:
        state = str((member or {}).get("member_state") or "active")
        if self.lang != "zh":
            return state
        return {"active": "正常", "left": "已退出", "removed": "已移除"}.get(state, state)

    def _is_local_active_group_member(self, group_id: str, members: Optional[List[Dict[str, object]]] = None) -> bool:
        member = self._local_group_member(group_id, members)
        return str(member.get("member_state") or "") == "active"

    def _is_local_group_owner(self, group_id: str, members: Optional[List[Dict[str, object]]] = None) -> bool:
        local_id = str(getattr(self, "chat_local_peer_id", "") or "")
        if not local_id:
            return False
        member = self._local_group_member(group_id, members)
        if str(member.get("role") or "") == "owner" and str(member.get("member_state") or "active") == "active":
            return True
        group = self._group_record_by_id(group_id)
        return bool(str(group.get("creator_peer_id") or "") == local_id and self._is_local_active_group_member(group_id, members))

    def _display_name_for_peer(self, peer_id: str, members: Optional[List[Dict[str, object]]] = None) -> str:
        pid = str(peer_id or "")
        if not pid:
            return ""
        for m in members or []:
            if str(m.get("peer_id") or "") == pid:
                return str(m.get("display_name") or m.get("nickname") or pid)
        if self.contact_service is not None:
            try:
                contact = self.contact_service.find_contact(pid)
                if contact:
                    return str(contact.get("remark_name") or contact.get("display_name") or contact.get("nickname") or pid)
            except Exception:
                pass
        return pid

    def _format_member_detail(self, member: Dict[str, object]) -> str:
        name = str(member.get("display_name") or member.get("nickname") or member.get("peer_id") or "").strip()
        removable = (
            self._is_local_group_owner(self.current_group_id)
            and str(member.get("member_state") or "active") == "active"
            and str(member.get("role") or "member") != "owner"
            and str(member.get("peer_id") or "") != str(self.chat_local_peer_id or "")
        )
        action_hint = ""
        if removable:
            action_hint = "群主可移除此成员" if self.lang == "zh" else "Owner can remove this member"
        role_key = "角色" if self.lang == "zh" else "Role"
        return (
            f"{self.cu('nickname')}: {name}\n"
            f"{self.cu('peer_id')}: {member.get('peer_id') or ''}\n"
            f"{self.cu('fingerprint')}: {self._short_fp(str(member.get('fingerprint') or ''))}\n"
            f"{self.cu('endpoint')}: {member.get('peer_ip') or ''}:{member.get('peer_port') or 9999}\n"
            f"{self.cu('member_state')}: {self._member_state_label(member)}\n"
            f"{role_key}: {self._member_role_label(member)}\n"
            + (f"{action_hint}\n" if action_hint else "")
        )

    def show_group_member_detail(self, member: Dict[str, object]) -> None:
        self.selected_group_member_peer_id = str(member.get("peer_id") or "")
        if hasattr(self, "right_info_box"):
            self.right_info_box.text.text = self._format_member_detail(member)
        if hasattr(self, "right_title"):
            self.right_title.text = self.cu("member_detail")
        self._set_right_action_mode("group")

    def _render_group_member_buttons(self, members: List[Dict[str, object]]) -> None:
        if not hasattr(self, "right_member_box"):
            return
        self.right_member_box.clear_widgets()
        active = []
        inactive = []
        for m in members or []:
            if str(m.get("member_state") or "active") == "active":
                active.append(m)
            else:
                inactive.append(m)
        for m in active + inactive:
            name = str(m.get("display_name") or m.get("nickname") or m.get("peer_id") or "").strip()
            if not name:
                continue
            role = str(m.get("role") or "member")
            state = str(m.get("member_state") or "active")
            suffix = " " + self._member_role_label(m) if role == "owner" else ""
            if state != "active":
                suffix += f" {self._member_state_label(m)}"
            text = shorten_middle(name + suffix, 28)
            role_style = "active" if role == "owner" and state == "active" else "secondary"
            btn = make_button(role_style, text=text, size_hint_y=None, height=dp(38), halign="left", valign="middle")
            if str(m.get("peer_id") or "") == str(getattr(self, "selected_group_member_peer_id", "") or ""):
                style_button(btn, "primary")
            btn.bind(size=lambda inst, _val: setattr(inst, "text_size", (inst.width - dp(12), None)))
            btn.bind(on_release=lambda _btn, member=dict(m): self.show_group_member_detail(member))
            self.right_member_box.add_widget(btn)

    def render_current_chat(self, reason: str = "live", allow_clear: Optional[bool] = None, generation: Optional[int] = None) -> None:
        if generation is not None and generation != getattr(self, "_chat_render_generation", 0):
            return
        if allow_clear is None:
            allow_clear = str(reason or "") in {
                "switch",
                "initial",
                "manual_refresh",
                "history",
                "history_sync",
                "detail_panel",
                "group_manage",
                "contact_manage",
            }
        if not allow_clear:
            self._sync_chat_render_signature()
            return
        self._bump_chat_render_generation()
        self.main_messages_box.clear()
        self._rendered_chat_message_ids = set()
        self.right_info_box.clear()
        if hasattr(self, "shared_files_box"):
            self.shared_files_box.clear_widgets()
        if hasattr(self, "profile_name_label"):
            self.profile_name_label.text = str(self.chat_nickname or self.chat_local_peer_id or "AgoraLink")
        if hasattr(self, "profile_peer_label"):
            self.profile_peer_label.text = str(self.chat_local_peer_id or "")
        if hasattr(self, "online_state_btn"):
            running = False
            try:
                running = self.receiver_worker.is_running()
            except Exception:
                running = False
            self.online_state_btn.text = "Online" if running else "Offline"
            style_button(self.online_state_btn, "active" if running else "secondary")
        self._chat_render_sig = self._current_chat_signature()
        store = self.chat_store
        if self.message_service is None or self.group_service is None or self.contact_service is None:
            self.main_messages_box.add_card(system_card("Chat database is locked."))
            return
        self._mark_current_chat_read_and_notify()
        rendered_runtime_card_ids = set()
        try:
            if self.current_chat_mode == "group" and self.current_group_id:
                members = self.group_service.members(self.current_group_id, include_inactive=True)
                member_ids = {str(m.get("peer_id") or "") for m in members}
                if str(getattr(self, "selected_group_member_peer_id", "") or "") not in member_ids:
                    self.selected_group_member_peer_id = ""
                try:
                    self._show_detail_panel()
                except Exception:
                    pass
                file_cards = []
                seen_message_ids = set()
                last_date_key = ""
                last_sender = ""
                last_ts_for_grouping = 0.0
                for msg in self.message_service.list_messages(group_id=self.current_group_id, limit=200):
                    if self._is_screen_control_chat(msg):
                        mid_hidden = str(msg.get('message_id') or '')
                        if mid_hidden:
                            seen_message_ids.add(mid_hidden)
                        continue
                    summary = self.message_service.receipt_summary(str(msg.get('message_id') or ''))
                    mine_msg = str(msg.get('sender_peer_id') or '') == self.chat_local_peer_id
                    msg_created_ts = float(msg.get("created_at") or time.time())
                    msg_date_key = time.strftime("%Y-%m-%d", time.localtime(msg_created_ts))
                    if msg_date_key != last_date_key:
                        self.main_messages_box.add_date_separator(self._date_separator_label(msg_created_ts))
                        last_date_key = msg_date_key
                        last_sender = ""
                        last_ts_for_grouping = 0.0
                        last_sender = ""
                        last_ts_for_grouping = 0.0
                    self._render_runtime_chat_cards_before(msg_created_ts, rendered_runtime_card_ids)
                    ts_source = msg.get('sent_at') if mine_msg else msg.get('received_at')
                    ts = time.strftime("%H:%M", time.localtime(float(ts_source or msg_created_ts)))
                    body_type = str(msg.get('body_type') or 'text')
                    text = str(msg.get('text') or '')
                    file_path = ''
                    mid = str(msg.get('message_id') or '')
                    if body_type == 'file':
                        try:
                            obj = json.loads(text)
                            file_path = str(obj.get('path') or '')
                            total_size = int(obj.get('size') or 0)
                            text = str(obj.get('name') or file_path or text)
                        except Exception:
                            file_path = text
                            total_size = 0
                        if not file_path:
                            file_path = self._file_path_from_transfer_store(mid)
                    else:
                        total_size = 0
                    progress_text = self._file_progress_text(mid, total_size, summary) if body_type == 'file' else ''
                    if body_type == 'file':
                        file_cards.append((text, file_path, total_size, ts))
                        if self._has_runtime_file_card(mid):
                            if mid:
                                seen_message_ids.add(mid)
                            continue
                    sender_id = str(msg.get('sender_peer_id') or '')
                    compact = (sender_id == last_sender and (msg_created_ts - last_ts_for_grouping) <= 300 and body_type != 'file')
                    sender_name = self._display_name_for_peer(sender_id, members)
                    self.main_messages_box.add_message(mine=mine_msg, sender=sender_name, text=text, timestamp=ts, summary=summary, body_type=body_type, file_path=file_path, message_id=mid, progress_text=progress_text, total_size=total_size, show_sender=True)
                    self._mark_chat_message_rendered(msg)
                    last_sender = sender_id
                    last_ts_for_grouping = msg_created_ts
                for live in self._live_messages_for_current_chat(seen_message_ids):
                    mid = str(live.get("message_id") or "")
                    if mid:
                        seen_message_ids.add(mid)
                    if self._is_screen_control_chat(live):
                        continue
                    msg_created_ts = float(live.get("created_at") or time.time())
                    self._render_runtime_chat_cards_before(msg_created_ts, rendered_runtime_card_ids)
                    ts = time.strftime("%H:%M", time.localtime(msg_created_ts))
                    body_type = str(live.get("body_type") or "text")
                    text_live = str(live.get("text") or "")
                    file_path = ""
                    total_size = 0
                    if body_type == "file":
                        try:
                            obj_live = json.loads(text_live)
                            file_path = str(obj_live.get("path") or self._file_path_from_transfer_store(mid) or "")
                            total_size = int(obj_live.get("size") or 0)
                            text_live = str(obj_live.get("name") or file_path or text_live)
                        except Exception:
                            pass
                    progress_text = self._file_progress_text(mid, total_size, "") if body_type == "file" else ""
                    if body_type == "file" and self._has_runtime_file_card(mid):
                        continue
                    live_sender_id = str(live.get("sender_peer_id") or "")
                    self.main_messages_box.add_message(mine=False, sender=self._display_name_for_peer(live_sender_id, members), text=text_live, timestamp=ts, summary="", body_type=body_type, file_path=file_path, message_id=mid, progress_text=progress_text, total_size=total_size, show_sender=True)
                    self._mark_chat_message_rendered(live)
                self._render_remaining_runtime_chat_cards(rendered_runtime_card_ids)
                active_count = sum(1 for m in members if str(m.get("member_state") or "active") == "active")
                total_count = len(members)
                if hasattr(self, "right_title"):
                    self.right_title.text = f"群成员（{active_count}/{total_count}）"
                self._set_right_action_mode("group")
                if hasattr(self, "right_member_scroll"):
                    self._set_widget_visible(self.right_member_scroll, True, height=210)
                self._render_group_member_buttons(members)
                local_member = self._local_group_member(self.current_group_id, members)
                if local_member:
                    role_text = self._member_role_label(local_member)
                    state_text = self._member_state_label(local_member)
                else:
                    role_text = "非成员" if self.lang == "zh" else "Not a member"
                    state_text = "-"
                can_manage = self._is_local_group_owner(self.current_group_id, members)
                manage_text = "可添加和移除成员" if (self.lang == "zh" and can_manage) else ("Can add and remove members" if can_manage else ("仅可查看和退出群" if self.lang == "zh" else "Can view and leave only"))
                self.right_info_box.text.text = f"群成员：{active_count}/{total_count}\n我的角色：{role_text}\n我的状态：{state_text}\n权限：{manage_text}\n点击成员可查看详情。"
                for name, path, size, tsf in reversed(file_cards[-8:]):
                    self._add_shared_file_entry(name, path, size, tsf)
            elif self.current_chat_mode == "direct" and self.current_peer_id:
                try:
                    self._hide_detail_panel()
                except Exception:
                    pass
                conv = self.message_service.create_direct_conversation(self.current_peer_id)
                file_cards = []
                seen_message_ids = set()
                last_date_key = ""
                last_sender = ""
                last_ts_for_grouping = 0.0
                for msg in self.message_service.list_messages(conversation_id=conv, limit=200):
                    if self._is_screen_control_chat(msg):
                        mid_hidden = str(msg.get('message_id') or '')
                        if mid_hidden:
                            seen_message_ids.add(mid_hidden)
                        continue
                    summary = self.message_service.receipt_summary(str(msg.get('message_id') or ''))
                    mine_msg = str(msg.get('sender_peer_id') or '') == self.chat_local_peer_id
                    msg_created_ts = float(msg.get("created_at") or time.time())
                    msg_date_key = time.strftime("%Y-%m-%d", time.localtime(msg_created_ts))
                    if msg_date_key != last_date_key:
                        self.main_messages_box.add_date_separator(self._date_separator_label(msg_created_ts))
                        last_date_key = msg_date_key
                    self._render_runtime_chat_cards_before(msg_created_ts, rendered_runtime_card_ids)
                    ts_source = msg.get('sent_at') if mine_msg else msg.get('received_at')
                    ts = time.strftime("%H:%M", time.localtime(float(ts_source or msg_created_ts)))
                    body_type = str(msg.get('body_type') or 'text')
                    text = str(msg.get('text') or '')
                    file_path = ''
                    mid = str(msg.get('message_id') or '')
                    if body_type == 'file':
                        try:
                            obj = json.loads(text)
                            file_path = str(obj.get('path') or '')
                            total_size = int(obj.get('size') or 0)
                            text = str(obj.get('name') or file_path or text)
                        except Exception:
                            file_path = text
                            total_size = 0
                        if not file_path:
                            file_path = self._file_path_from_transfer_store(mid)
                    else:
                        total_size = 0
                    progress_text = self._file_progress_text(mid, total_size, summary) if body_type == 'file' else ''
                    if body_type == 'file':
                        file_cards.append((text, file_path, total_size, ts))
                        if self._has_runtime_file_card(mid):
                            if mid:
                                seen_message_ids.add(mid)
                            continue
                    sender_id = str(msg.get('sender_peer_id') or '')
                    compact = (sender_id == last_sender and (msg_created_ts - last_ts_for_grouping) <= 300 and body_type != 'file')
                    if mid:
                        seen_message_ids.add(mid)
                    self.main_messages_box.add_message(mine=mine_msg, sender=sender_id, text=text, timestamp=ts, summary=summary, body_type=body_type, file_path=file_path, message_id=mid, progress_text=progress_text, total_size=total_size)
                    self._mark_chat_message_rendered(msg)
                    last_sender = sender_id
                    last_ts_for_grouping = msg_created_ts
                for live in self._live_messages_for_current_chat(seen_message_ids):
                    mid = str(live.get("message_id") or "")
                    if mid:
                        seen_message_ids.add(mid)
                    if self._is_screen_control_chat(live):
                        continue
                    msg_created_ts = float(live.get("created_at") or time.time())
                    self._render_runtime_chat_cards_before(msg_created_ts, rendered_runtime_card_ids)
                    ts = time.strftime("%H:%M", time.localtime(msg_created_ts))
                    body_type = str(live.get("body_type") or "text")
                    text_live = str(live.get("text") or "")
                    file_path = ""
                    total_size = 0
                    if body_type == "file":
                        try:
                            obj_live = json.loads(text_live)
                            file_path = str(obj_live.get("path") or self._file_path_from_transfer_store(mid) or "")
                            total_size = int(obj_live.get("size") or 0)
                            text_live = str(obj_live.get("name") or file_path or text_live)
                        except Exception:
                            pass
                    progress_text = self._file_progress_text(mid, total_size, "") if body_type == "file" else ""
                    if body_type == "file" and self._has_runtime_file_card(mid):
                        continue
                    self.main_messages_box.add_message(mine=False, sender=str(live.get("sender_peer_id") or ""), text=text_live, timestamp=ts, summary="", body_type=body_type, file_path=file_path, message_id=mid, progress_text=progress_text, total_size=total_size)
                    self._mark_chat_message_rendered(live)
                self._render_remaining_runtime_chat_cards(rendered_runtime_card_ids)
                contact_text = ""
                seen_contact = False
                for c in self.contact_service.list_contacts(trusted_only=False):
                    if str(c.get("peer_id") or "") == self.current_peer_id and not seen_contact:
                        seen_contact = True
                        if hasattr(self, "right_title"):
                            self.right_title.text = self.cu("friend_info")
                        self._set_right_action_mode("direct")
                        if hasattr(self, "right_member_scroll"):
                            self._set_widget_visible(self.right_member_scroll, False)
                        contact_text = (
                            f"{self.cu('contact')}: {c.get('remark_name') or c.get('display_name') or c.get('nickname') or c.get('peer_id')}\n"
                            f"{self.cu('peer_id')}: {c.get('peer_id')}\n"
                            f"{self.cu('fingerprint')}: {self._short_fp(str(c.get('fingerprint') or ''))}\n"
                            f"{self.cu('ip')}: {c.get('peer_ip')}:{c.get('peer_port')}\n"
                            f"{self.cu('state')}: {c.get('trust_state')}\n"
                        )
                        break
                self.right_info_box.text.text = contact_text
                for name, path, size, tsf in reversed(file_cards[-8:]):
                    self._add_shared_file_entry(name, path, size, tsf)
        except Exception as exc:
            self.main_messages_box.add_card(system_card(self.cu("refresh_failed", error=exc)))



    def _add_shared_file_entry(self, file_name: str, file_path: str, total_size: int = 0, timestamp: str = "") -> None:
        if not hasattr(self, "shared_files_box"):
            return
        row = BoxLayout(orientation="vertical", size_hint_y=None, height=dp(88), spacing=dp(2), padding=(dp(8), dp(6), dp(8), dp(6)))
        apply_card_background(row, "secondary", radius=12)
        title = make_label(text=shorten_middle(str(file_name or self.cu("file")), 20), size_hint_y=None, height=dp(20), halign="left", color=THEME["text"], bold=True)
        title.shorten = True
        title.shorten_from = "right"
        info_text = (f"{format_file_size(int(total_size or 0))}" if total_size else "") + (f"   {timestamp}" if timestamp else "")
        info = make_label(text=info_text, size_hint_y=None, height=dp(18), halign="left", color=THEME["muted_text"])
        row.add_widget(title)
        row.add_widget(info)
        btn = make_button("secondary", text=self.cu("open_folder") if file_path else self.cu("not_saved"), size_hint_y=None, height=dp(28), on_release=lambda *_p, path=file_path: open_file_location(path))
        btn.disabled = not bool(file_path)
        row.add_widget(btn)
        self.shared_files_box.add_widget(row)


    def show_page(self, page: str) -> None:
        if page == self.current_page:
            return
        self.page_host.clear_widgets()
        if page == "recv":
            self._set_old_mode_chrome(True)
            self.page_host.add_widget(self.recv_page)
            self.current_page = "recv"
        elif page == "agora_chat":
            self._set_old_mode_chrome(False)
            self.page_host.add_widget(self.agora_chat_page)
            self.current_page = "agora_chat"
        elif page == "chat":
            self._set_old_mode_chrome(True)
            self.page_host.add_widget(self.chat_page)
            self.current_page = "chat"
        else:
            self._set_old_mode_chrome(True)
            self.page_host.add_widget(self.send_page)
            self.current_page = "send"
        self._refresh_tab_button_state()

    def _set_old_mode_chrome(self, visible: bool) -> None:
        if not hasattr(self, "tab_bar"):
            return
        self.tab_bar.disabled = not bool(visible)
        self.tab_bar.opacity = 1.0 if visible else 0.0
        self.tab_bar.height = dp(44) if visible else 0
        self.tab_bar.size_hint_y = None

    def _refresh_tab_button_state(self) -> None:
        # Keep both buttons enabled so users can always click them; use text mark
        # rather than disabled styling because disabled Kivy buttons may render
        # text poorly with some fonts.
        if not hasattr(self, "send_tab_btn"):
            return
        send_text = self.t("send_tab")
        recv_text = self.t("recv_tab")
        chat_text = "聊天主界面" if self.current_page == "agora_chat" else self.t("chat_tab")
        self.send_tab_btn.text = ("● " + send_text) if self.current_page == "send" else send_text
        self.recv_tab_btn.text = ("● " + recv_text) if self.current_page == "recv" else recv_text
        self.chat_tab_btn.text = ("● " + chat_text) if self.current_page in ("chat", "agora_chat") else chat_text
        style_button(self.send_tab_btn, "active" if self.current_page == "send" else "secondary")
        style_button(self.recv_tab_btn, "active" if self.current_page == "recv" else "secondary")
        style_button(self.chat_tab_btn, "active" if self.current_page in ("chat", "agora_chat") else "secondary")

    def refresh_texts(self) -> None:
        self.title_label.text = ""
        self.lang_btn.text = self.t("toggle_lang")
        self.send_tab_btn.text = self.t("send_tab")
        self.recv_tab_btn.text = self.t("recv_tab")
        self.chat_tab_btn.text = self.t("chat_tab")
        self.manual_hint.text = self.t("manual_hint")
        # Row labels are kept by position; recreate label texts through parent children.
        self._set_row_label(self.receiver_ip, self.t("receiver_ip"))
        self._set_row_label(self.receiver_port, self.t("port"))
        self._set_discovery_line_label(self.discovery_port, self.t("discovery_port"))
        self._set_row_label(self.receiver_spinner, self.t("choose_receiver"))
        self._set_discovery_line_label(self.file_input, self.t("file"))
        self._set_row_label(self.payload_input, self.t("payload"))
        self._set_row_label(self.complete_timeout_input, self.t("complete_timeout"))
        self._set_row_label(self.request_timeout_input, self.t("request_timeout"))
        self.search_btn.text = self.t("search")
        self.choose_file_btn.text = self.t("choose_file")
        self.send_btn.text = self.t("send")
        self.retry_send_btn.text = self.t("retry")
        self.stop_send_btn.text = self.t("stop")
        self.clear_send_btn.text = self.t("clear")
        self._set_row_label(self.bind_input, self.t("bind"))
        self._set_row_label(self.recv_port, self.t("port"))
        self._set_row_label(self.recv_discovery_port, self.t("discovery_port"))
        self._set_discovery_line_label(self.save_dir_input, self.t("save_dir"))
        self._set_row_label(self.allow_peer_input, self.t("allow_peer"))
        self._set_row_label(self.receiver_name_input, self.t("receiver_name"))
        self._set_row_label(self.approval_timeout_input, self.t("approval_timeout"))
        self.once_label.text = self.t("once")
        self.choose_dir_btn.text = self.t("choose_dir")
        self.start_recv_btn.text = self.t("start_recv")
        self.stop_recv_btn.text = self.t("stop_recv")
        self.firewall_btn.text = self.t("firewall")
        self.clear_recv_btn.text = self.t("clear")
        if hasattr(self, "chat_db_input"):
            self._set_row_label(self.chat_db_input, self.t("chat_db"))
            self._set_row_label(self.chat_password_input, self.t("chat_password"))
            self._set_row_label(self.chat_local_peer_input, self.t("local_peer_id"))
            self.unlock_chat_btn.text = self.t("unlock_chat")
            self.refresh_chat_btn.text = self.t("refresh_chat")
            self._set_row_label(self.chat_group_id_input, self.t("group_id"))
            self._set_row_label(self.chat_group_title_input, self.t("group_title"))
            self.create_group_btn.text = self.t("create_group")
            self.leave_group_btn.text = self.t("leave_group")
            self._set_row_label(self.member_peer_input, self.t("member_peer_id"))
            self._set_row_label(self.member_ip_input, self.t("member_ip"))
            self._set_row_label(self.member_port_input, self.t("member_port"))
            self.add_member_btn.text = self.t("add_member")
            self.remove_member_btn.text = self.t("remove_member")
            self._set_discovery_line_label(self.group_message_input, self.t("chat_message"))
            self.send_group_btn.text = self.t("send_group_msg")
        self._refresh_tab_button_state()
        self.update_progress_labels(0, 0, 0.0, self.t("unknown"), 0.0)

        if hasattr(self, "recent_btn"):
            self.recent_btn.text = self.cu("recent")
            self.groups_btn.text = self.cu("contacts")
            self.devices_btn.text = self.cu("devices")
            self.chat_filter_input.hint_text = self.cu("search_hint")
            self.scan_devices_btn.text = self.cu("scan_devices")
            if hasattr(self, "add_contact_btn"):
                self.add_contact_btn.text = self.cu("add_contact")
            if hasattr(self, "current_chat_title") and self.current_chat_mode not in ("group", "direct", "device"):
                self.current_chat_title.text = ""
            if hasattr(self, "main_message_input"):
                self.main_message_input.hint_text = self.cu("input_hint")
            if hasattr(self, "main_send_btn"):
                self.main_send_btn.text = self.cu("send")
            if hasattr(self, "main_file_btn"):
                self.main_file_btn.text = self.cu("send_file")
            if hasattr(self, "main_screen_btn"):
                self._schedule_screen_share_button_refresh()
            if hasattr(self, "settings_btn"):
                self.settings_btn.text = "设置" if self.lang == "zh" else "Settings"
            if hasattr(self, "debug_btn"):
                self.debug_btn.text = "诊断" if self.lang == "zh" else "Diagnostics"
            if hasattr(self, "right_title"):
                if self.current_chat_mode == "group":
                    self.right_title.text = self.cu("group_members_title")
                elif self.current_chat_mode == "direct":
                    self.right_title.text = self.cu("friend_info")
                else:
                    self.right_title.text = self.cu("right_title")
            if hasattr(self, "shared_title"):
                self.shared_title.text = self.cu("shared_files")
            for attr, key in [("new_group_btn", "new_group"), ("add_member_main_btn", "add_member"), ("remove_member_main_btn", "remove_member"), ("leave_group_main_btn", "leave_group"), ("delete_friend_main_btn", "delete_friend")]:
                if hasattr(self, attr):
                    getattr(self, attr).text = self.cu(key)
            self.refresh_chat_main()
            self.render_current_chat(reason="initial")

    def _set_row_label(self, widget, text: str) -> None:
        parent = widget.parent
        if parent and parent.children:
            # BoxLayout children are stored reverse order; label was added before widget.
            for child in parent.children:
                if isinstance(child, Label):
                    child.text = text
                    break

    def _set_discovery_line_label(self, widget, text: str) -> None:
        parent = widget.parent
        if parent and parent.children:
            for child in parent.children:
                if isinstance(child, Label):
                    child.text = text
                    break

    def toggle_lang(self) -> None:
        self.lang = "en" if self.lang == "zh" else "zh"
        self.app.lang = self.lang
        self.refresh_texts()
        self.refresh_local_ips()

    def refresh_local_ips(self) -> None:
        try:
            ips = get_local_ip_candidates()
        except Exception:
            ips = []
        text = ", ".join(ips) if ips else self.t("unknown")
        self.local_ip_label.text = self.t("local_ip", ips=text)

    def choose_file(self) -> None:
        self._native_file_dialog(select_dir=False, callback=lambda path: self._set_file(path))

    def _set_file(self, path: str) -> None:
        self.file_input.text = path
        try:
            size = os.path.getsize(path)
            self.update_progress_labels(0, size, 0.0, self.t("unknown"), 0.0)
        except Exception:
            pass

    def choose_dir(self) -> None:
        self._native_file_dialog(select_dir=True, callback=lambda path: setattr(self.save_dir_input, "text", path))

    def _native_file_dialog(self, select_dir: bool, callback) -> None:
        """Use the operating system file dialog first.

        On Windows this gives users the familiar Explorer-style picker and
        avoids Kivy FileChooser path-encoding issues with Chinese file names. If
        Tkinter is unavailable in a packaged build, fall back to the built-in
        Kivy chooser.
        """
        try:
            import tkinter as tk
            from tkinter import filedialog

            root = tk.Tk()
            root.withdraw()
            try:
                root.attributes("-topmost", True)
            except Exception:
                pass
            if select_dir:
                selected = filedialog.askdirectory(parent=root, title=self.t("choose_dir"))
            else:
                selected = filedialog.askopenfilename(parent=root, title=self.t("choose_file"))
            root.destroy()
            if selected:
                callback(str(selected))
            return
        except Exception:
            self.sender_log_box.append(self.t("native_dialog_failed") + "\n")
            self._file_popup(select_dir=select_dir, callback=callback)

    def _file_popup(self, select_dir: bool, callback) -> None:
        chooser = FileChooserListView(path=str(Path.home()), dirselect=select_dir)
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(8))
        content.add_widget(chooser)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        popup = style_popup(Popup(title=self.t("browse"), title_font=UI_FONT, content=content, size_hint=(0.9, 0.9)))
        def _select(_btn):
            selected = chooser.selection
            if selected:
                callback(selected[0])
                popup.dismiss()
        buttons.add_widget(make_button("primary", text=self.t("select"), on_release=_select))
        buttons.add_widget(make_button("secondary", text=self.t("cancel"), on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def show_startup_unlock_popup(self) -> None:
        if UIRoundedCard is not None and ui_component_color is not None:
            content = UIRoundedCard(
                orientation="vertical",
                spacing=dp(10),
                padding=(dp(18), dp(18), dp(18), dp(18)),
                radius=24,
                bg_color=ui_component_color("surface"),
                border_color=ui_component_color("border_soft"),
            )
        else:
            content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
            apply_card_background(content, "panel_bg", radius=22)
        title = make_label(
            text="AgoraLink",
            size_hint_y=None,
            height=dp(32),
            bold=True,
            halign="left",
            valign="middle",
            color=ui_component_color("text_primary") if ui_component_color is not None else THEME["text"],
        )
        content.add_widget(title)
        db_input = make_input(text=self.chat_db_path, multiline=False)
        mode_values = ["登录", "注册", "仅收发"]
        db_exists = Path(self.chat_db_path).expanduser().exists()
        mode_spinner = style_spinner(Spinner(text=("登录" if db_exists else "注册"), values=mode_values, font_name=UI_FONT, size_hint_y=None, height=dp(38)))
        password_input = make_input(text="", multiline=False, password=True)
        confirm_input = make_input(text="", multiline=False, password=True)
        nick_input = make_input(text=self.chat_nickname, multiline=False)

        def login_row(label_text: str, control) -> BoxLayout:
            box = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(10))
            lab = make_label(
                text=label_text,
                size_hint_x=None,
                width=dp(112),
                halign="right",
                valign="middle",
                shorten=True,
                color=THEME["muted_text"],
            )
            bind_label_wrap(lab)
            box.add_widget(lab)
            box.add_widget(control)
            return box

        content.add_widget(login_row("启动模式", self._make_modern_input_shell(mode_spinner)))
        content.add_widget(login_row("聊天数据库", self._make_modern_input_shell(db_input)))
        content.add_widget(login_row("密码", self._make_modern_input_shell(password_input)))
        confirm_row = login_row("确认密码", self._make_modern_input_shell(confirm_input))
        nick_row = login_row("昵称", self._make_modern_input_shell(nick_input))
        content.add_widget(confirm_row)
        content.add_widget(nick_row)
        hint = make_label(text="登录：输入已有密码。注册：首次创建聊天库。仅收发：进入旧接收页，不启用聊天。", size_hint_y=None, height=dp(48), halign="left", valign="middle", color=THEME["muted_text"])
        bind_label_wrap(hint)
        content.add_widget(hint)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(8))
        popup = style_popup(Popup(title="AgoraLink 启动", content=content, size_hint=(0.62, 0.76), auto_dismiss=False))

        def set_hint(text: str, *, error: bool = False) -> None:
            hint.text = str(text or "")
            hint.color = (
                ui_component_color("danger")
                if error and ui_component_color is not None
                else (THEME["danger"] if error else THEME["muted_text"])
            )

        def _sync_mode(*_):
            mode = mode_spinner.text
            is_register = mode == "注册"
            password_input.disabled = mode == "仅收发"
            confirm_input.disabled = not is_register
            nick_input.disabled = not is_register
            confirm_row.opacity = 1.0 if is_register else 0.35
            nick_row.opacity = 1.0 if is_register else 0.35
            if mode == "登录":
                set_hint("输入已注册聊天库密码。无需确认密码。")
            elif mode == "注册":
                set_hint("首次创建聊天库时需要密码、确认密码和昵称。")
            else:
                set_hint("仅收发模式：进入旧接收页，保留设备发现、文件发送和文件接收，不启用聊天。")
        mode_spinner.bind(text=lambda *_: _sync_mode())
        _sync_mode()

        def _enter_basic(*_):
            self.basic_mode = True
            self.chat_unlocked = False
            popup.dismiss()
            self.show_page("recv")

        def _submit(_btn=None):
            mode = mode_spinner.text
            if mode == "仅收发":
                _enter_basic()
                return
            db_path = db_input.text.strip() or self.chat_db_path
            pwd = password_input.text
            confirm = confirm_input.text
            nick = nick_input.text.strip() or "AgoraLinkUser"
            exists_now = Path(db_path).expanduser().exists()
            if not pwd:
                set_hint("密码不能为空。", error=True)
                return
            if mode == "登录":
                if not exists_now:
                    set_hint("聊天库不存在，请切换到注册。", error=True)
                    return
            if mode == "注册":
                if exists_now:
                    set_hint("聊天库已存在。请登录，或先使用重置。", error=True)
                    return
                if pwd != confirm:
                    set_hint("注册时密码和确认密码必须一致。", error=True)
                    return
            try:
                self.unlock_chat_with(db_path, pwd, nick)
                popup.dismiss()
            except Exception as exc:
                set_hint(f"解锁失败: {exc}", error=True)

        def _reset(_btn=None):
            db_path = Path(db_input.text.strip() or self.chat_db_path).expanduser()
            content2 = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
            content2.add_widget(make_label(text=f"确认直接删除并重建聊天库？\n旧消息正文将不可恢复。\n{db_path}", halign="left", valign="middle"))
            pop2 = style_popup(Popup(title="重置聊天库", content=content2, size_hint=(0.55, 0.38), auto_dismiss=False))
            line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
            def _do_reset(*_):
                for suffix in ("", "-wal", "-shm"):
                    try:
                        Path(str(db_path) + suffix).unlink(missing_ok=True)
                    except Exception:
                        pass
                pop2.dismiss()
                mode_spinner.text = "注册"
                set_hint("聊天库已删除。请选择注册并输入新密码和昵称。")
            line.add_widget(self._make_modern_or_legacy_button("danger", text="确认重置", size_hint_x=1, height=36, on_release=_do_reset))
            line.add_widget(self._make_modern_or_legacy_button("secondary", text="取消", size_hint_x=1, height=36, on_release=lambda *_: pop2.dismiss()))
            content2.add_widget(line)
            apply_ui_font(content2)
            pop2.open()

        buttons.add_widget(self._make_modern_or_legacy_button("primary", text="进入", size_hint_x=1, height=38, on_release=_submit))
        buttons.add_widget(self._make_modern_or_legacy_button("secondary", text="仅收发", size_hint_x=1, height=38, on_release=_enter_basic))
        buttons.add_widget(self._make_modern_or_legacy_button("danger", text="忘记密码/重置", size_hint_x=1, height=38, on_release=_reset))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def _purge_debug_live_messages(self) -> None:
        """Remove old live_* rows created by the temporary Chat-from fallback.

        These rows were diagnostic artifacts. They duplicate real msg_* messages
        and can also cause CHAT_READ foreign-key errors on the sender side.
        """
        try:
            if self.chat_store is None:
                return
            conn = self.chat_store.db.conn
            conn.execute("DELETE FROM message_receipts WHERE message_id LIKE 'live_%'")
            conn.execute("DELETE FROM messages WHERE message_id LIKE 'live_%'")
            conn.commit()
        except Exception as exc:
            try:
                self._append_debug_line(f"purge live_* messages failed: {exc}", protocol=True)
            except Exception:
                pass

    def _refresh_services(self) -> None:
        """Wire service-layer adapters after chat/transfer stores change.

        The UI should call these adapters for contact, message and file-transfer
        business operations instead of reaching into storage directly.  This keeps
        the Kivy layer replaceable by a future desktop/mobile UI.
        """
        try:
            from app_services import ContactService, GroupService, MessageService, FileTransferService
            self.contact_service = ContactService(self.chat_store)
            self.group_service = GroupService(self.chat_store)
            self.message_service = MessageService(self.chat_store)
            self.file_transfer_service = FileTransferService(self.transfer_store, self.chat_store)
            self._purge_debug_live_messages()
        except Exception:
            self.contact_service = None
            self.group_service = None
            self.message_service = None
            self.file_transfer_service = None

    def unlock_chat_with(self, db_path: str, password: str, nickname: str) -> None:
        from chat_store import ChatStore
        from transfer_store import TransferStore
        # First use a temporary peer id; after opening, persist a stable local id in meta.
        initial_peer = re.sub(r"[^A-Za-z0-9_.-]+", "_", nickname.strip()) or "local"
        self.chat_db_path = str(Path(db_path).expanduser().resolve())
        self.chat_password = str(password or "")
        if self.chat_store is not None:
            try:
                self.chat_store.close()
            except Exception:
                pass
        if self.transfer_store is not None:
            try:
                self.transfer_store.close()
            except Exception:
                pass
        self.chat_store = ChatStore(self.chat_db_path, self.chat_password, my_peer_id=initial_peer)
        self.transfer_store = TransferStore(self.chat_db_path)
        self._refresh_services()
        profile = self.chat_store.get_local_profile()
        if not profile.get("peer_id") or profile.get("peer_id") == "local":
            import hashlib, secrets
            peer_id = "peer_" + hashlib.sha256((nickname + secrets.token_hex(8)).encode("utf-8")).hexdigest()[:24]
        else:
            peer_id = str(profile.get("peer_id"))
        self.chat_store.set_local_profile(nickname=nickname, peer_id=peer_id)
        self.chat_local_peer_id = peer_id
        self.chat_nickname = nickname
        self.chat_unlocked = True
        self.basic_mode = False
        self.receiver_name_input.text = nickname
        try:
            self.enter_chat_btn.disabled = True
            self.enter_chat_btn.opacity = 0.0
            self.enter_chat_btn.width = 0
            self.enter_chat_btn.size_hint_x = None
        except Exception:
            pass
        self.show_page("agora_chat")
        self.refresh_chat_main()
        self.start_receiver(auto=True)

    def toggle_online(self) -> None:
        if self.receiver_worker.is_running():
            def _offline_done(_ok, _error) -> None:
                self.online_btn.text = "Offline"
                style_button(self.online_btn, "secondary")
            self._stop_worker_nonblocking("receiver", self.receiver_worker, getattr(self, "online_btn", None), _offline_done)
        else:
            self.start_receiver(auto=True)

    def open_settings_popup(self) -> None:
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(10))
        theme_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        theme_line.add_widget(make_label(text="主题", size_hint_x=None, width=dp(90), color=THEME["muted_text"]))
        theme_spinner = style_spinner(Spinner(text=getattr(self, "theme_mode", "跟随系统"), values=["跟随系统", "浅色", "深色"], font_name=UI_FONT))
        theme_line.add_widget(theme_spinner)
        content.add_widget(theme_line)
        package_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        package_line.add_widget(make_label(text=self.cu("multi_auto_package_setting"), size_hint_x=None, width=dp(220), color=THEME["muted_text"], halign="left", valign="middle"))
        bind_label_wrap(package_line.children[0])
        package_checkbox = CheckBox(active=bool(getattr(self, "auto_package_multi_selection", True)), size_hint_x=None, width=dp(42))
        package_line.add_widget(package_checkbox)
        content.add_widget(package_line)
        audio_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        audio_label_text = "共享系统音频" if self.lang == "zh" else "Share system audio"
        audio_line.add_widget(make_label(text=audio_label_text, size_hint_x=None, width=dp(220), color=THEME["muted_text"], halign="left", valign="middle"))
        bind_label_wrap(audio_line.children[0])
        audio_checkbox = CheckBox(active=bool(getattr(self, "share_system_audio", False)), size_hint_x=None, width=dp(42))
        audio_line.add_widget(audio_checkbox)
        content.add_widget(audio_line)
        backend_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        backend_line.add_widget(make_label(text="Screen backend", size_hint_x=None, width=dp(220), color=THEME["muted_text"], halign="left", valign="middle"))
        bind_label_wrap(backend_line.children[0])
        backend_spinner = style_spinner(
            Spinner(text=self._screen_backend(), values=list(SCREEN_BACKEND_VALUES), font_name=UI_FONT)
        )
        backend_line.add_widget(backend_spinner)
        content.add_widget(backend_line)
        if self._native_lite_package():
            native_lite_note = make_label(
                text="Native Lite: Rust native video backend is the default. FFmpeg/system audio requires the Full package.",
                size_hint_y=None,
                height=dp(42),
                halign="left",
                valign="middle",
                color=THEME["muted_text"],
            )
            bind_label_wrap(native_lite_note)
            content.add_widget(native_lite_note)
        note = make_label(
            text="诊断日志已移到“诊断”窗口。普通设置只保留日常选项。",
            size_hint_y=None,
            height=dp(46),
            halign="left",
            valign="middle",
            color=THEME["muted_text"],
        )
        bind_label_wrap(note)
        content.add_widget(note)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        popup = style_popup(Popup(title="设置", content=content, size_hint=(0.58, 0.42)))
        def _apply_theme(*_):
            self.theme_mode = theme_spinner.text
            self.apply_theme_mode(self.theme_mode)
            old_audio = bool(getattr(self, "share_system_audio", False))
            old_backend = self._screen_backend()
            self.auto_package_multi_selection = bool(package_checkbox.active)
            self.share_system_audio = bool(audio_checkbox.active)
            self.screen_backend = self._coerce_screen_backend_for_package(backend_spinner.text, persist=False)
            if backend_spinner.text != self.screen_backend:
                backend_spinner.text = self.screen_backend
            try:
                self.gui_config["auto_package_multi_selection"] = bool(self.auto_package_multi_selection)
                self.gui_config["screen_share_system_audio"] = bool(self.share_system_audio)
                self.gui_config["screen_backend"] = str(self.screen_backend)
                save_gui_config(self.gui_config)
            except Exception:
                pass
            if old_audio != bool(self.share_system_audio) and self._screen_share_button_active():
                self._set_screen_share_status("共享系统音频设置将在下次投屏生效" if self.lang == "zh" else "System audio setting applies to the next screen share")
            if old_backend != self.screen_backend and self._screen_share_button_active():
                self._set_screen_share_status("Screen backend setting applies to the next screen share")
            if str(getattr(self, "screen_backend_notice", "") or "").strip():
                self._set_screen_share_status(str(self.screen_backend_notice))
        buttons.add_widget(make_button("primary", text="应用", on_release=_apply_theme))
        export_btn = make_button("secondary", text="导出诊断包")
        export_btn.bind(on_release=lambda *_: self.export_diagnostic_logs_async(log_box=self.sender_log_box, button=export_btn))
        buttons.add_widget(export_btn)
        buttons.add_widget(make_button("secondary", text="防火墙", on_release=lambda *_: self.allow_firewall()))
        buttons.add_widget(make_button("secondary", text="关闭", on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def _chat_diagnostic_summary(self) -> Dict[str, object]:
        summary: Dict[str, object] = {
            "app": APP_NAME,
            "release": APP_VERSION,
            "python": sys.version,
            "platform": sys.platform,
            "frozen": FROZEN,
            "app_dir": str(APP_DIR),
            "package_flavor": str(self._screen_package_snapshot().get("package_flavor") or ""),
            "rust_native_available": bool(self._screen_package_snapshot().get("rust_native_available")),
            "bundled_ffmpeg_available": bool(self._screen_package_snapshot().get("bundled_ffmpeg_available")),
            "screen_backend_default": str(self._screen_package_snapshot().get("screen_backend_default") or ""),
            "native_screen_video_only": bool(self._screen_package_snapshot().get("native_screen_video_only", True)),
            "user_data_dir": str(user_data_dir()),
            "chat_unlocked": bool(getattr(self, "chat_unlocked", False)),
            "local_peer_id": str(getattr(self, "chat_local_peer_id", "") or ""),
            "nickname": str(getattr(self, "chat_nickname", "") or ""),
            "current_chat_mode": str(getattr(self, "current_chat_mode", "") or ""),
            "current_group_id": str(getattr(self, "current_group_id", "") or ""),
            "current_peer_id": str(getattr(self, "current_peer_id", "") or ""),
            "sender_running": bool(self.sender_worker.is_running()) if hasattr(self, "sender_worker") else False,
            "receiver_running": bool(self.receiver_worker.is_running()) if hasattr(self, "receiver_worker") else False,
            "runtime_log_lines_in_memory": len(getattr(self, "debug_runtime_lines", []) or []),
            "protocol_log_lines_in_memory": len(getattr(self, "debug_protocol_lines", []) or []),
        }
        try:
            if self.contact_service is not None:
                contacts = self.contact_service.list_contacts(trusted_only=False)
                summary["contacts_count"] = len(contacts)
                summary["trusted_contacts_count"] = len([c for c in contacts if str(c.get("trust_state") or "") == "trusted"])
        except Exception as exc:
            summary["contacts_error"] = str(exc)
        try:
            if self.group_service is not None:
                groups = self.group_service.list_groups()
                summary["groups_count"] = len(groups)
                summary["groups"] = []
                for g in groups:
                    gid = str(g.get("group_id") or "")
                    members = self.group_service.members(gid, include_inactive=True)
                    local_member = self._local_group_member(gid, members)
                    summary["groups"].append({
                        "group_id": gid,
                        "title": str(g.get("title") or ""),
                        "creator_peer_id": str(g.get("creator_peer_id") or ""),
                        "group_state": str(g.get("group_state") or ""),
                        "active_members": len([m for m in members if str(m.get("member_state") or "active") == "active"]),
                        "total_members": len(members),
                        "local_role": self._member_role_label(local_member) if local_member else "",
                        "local_state": self._member_state_label(local_member) if local_member else "",
                    })
        except Exception as exc:
            summary["groups_error"] = str(exc)
        try:
            if self.transfer_store is not None:
                row = self.transfer_store.conn.execute("SELECT COUNT(*) AS n FROM file_transfers").fetchone()
                summary["file_transfers_count"] = int(row["n"] or 0)
        except Exception as exc:
            summary["file_transfers_error"] = str(exc)
        return summary

    def _export_diagnostic_logs_payload(self) -> str:
        return export_diagnostic_bundle(
            screen_runtime=self._screen_runtime(),
            extra_json={
                "chat_state_summary.json": self._chat_diagnostic_summary(),
            },
            extra_text={
                "gui_runtime_recent.log": "\n".join(getattr(self, "debug_runtime_lines", [])[-1000:]),
                "gui_protocol_recent.log": "\n".join(getattr(self, "debug_protocol_lines", [])[-1000:]),
            },
        )

    def export_diagnostic_logs(self) -> str:
        try:
            return self._export_diagnostic_logs_payload()
        except Exception as exc:
            try:
                self.sender_log_box.append(f"Diagnostics export failed: {exc}\n")
            except Exception:
                pass
            return ""

    def export_diagnostic_logs_async(self, *, log_box=None, button=None) -> None:
        if self._diagnostic_export_in_progress:
            try:
                if log_box is not None:
                    log_box.append("\nDiagnostics export is already running.\n")
            except Exception:
                pass
            return
        self._diagnostic_export_in_progress = True
        previous_text = self._set_button_busy(button, True, "Exporting...")
        try:
            if log_box is not None:
                log_box.append("\nExporting diagnostics...\n")
        except Exception:
            pass

        def _run_export() -> None:
            path = ""
            error = ""
            try:
                path = self._export_diagnostic_logs_payload()
            except Exception as exc:
                error = str(exc)

            def _finish(_dt) -> None:
                self._diagnostic_export_in_progress = False
                self._set_button_busy(button, False, previous_text if previous_text else None)
                if path:
                    try:
                        if log_box is not None:
                            log_box.append(f"\nDiagnostics exported: {path}\n")
                        else:
                            self.sender_log_box.append(f"Diagnostics exported: {path}\n")
                    except Exception:
                        pass
                else:
                    message = f"Diagnostics export failed: {error or 'unknown'}\n"
                    try:
                        if log_box is not None:
                            log_box.append("\n" + message)
                        else:
                            self.sender_log_box.append(message)
                    except Exception:
                        pass

            Clock.schedule_once(_finish, 0)

        threading.Thread(target=_run_export, daemon=True).start()

    def open_ui_preview(self) -> None:
        script = APP_DIR / "ui_preview.py"
        if FROZEN:
            try:
                self.sender_log_box.append("UI Preview is available in source mode only.\n")
            except Exception:
                pass
            return
        if not script.exists():
            try:
                self.sender_log_box.append(f"UI preview not found: {script}\n")
            except Exception:
                pass
            return
        env = os.environ.copy()
        env["AGORALINK_UI_PREVIEW_HOLD"] = "1"
        try:
            popen_no_console(
                [sys.executable, "-B", str(script)],
                cwd=str(APP_DIR),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env=env,
            )
        except Exception as exc:
            try:
                self.sender_log_box.append(f"UI preview failed: {exc}\n")
            except Exception:
                pass

    def _screen_runtime(self) -> ScreenRuntime:
        runtime = getattr(self.app, "screen_runtime", None)
        if runtime is None:
            runtime = ScreenRuntime()
            self.app.screen_runtime = runtime
        return runtime

    def _screen_stopping_status_text(self) -> str:
        return "正在停止投屏..." if self.lang == "zh" else "Stopping screen share..."

    def _stop_screen_runtime_nonblocking(self, *, status_label: Optional[Label] = None, update_chat_ui: bool = True, on_done=None) -> bool:
        if self._screen_stop_in_progress:
            if status_label is not None:
                status_label.text = self._screen_stopping_status_text()
            return False
        self._screen_stop_in_progress = True
        if update_chat_ui:
            self._set_screen_share_ui_state("stopping")
            self._set_screen_share_status(self._screen_stopping_status_text())
        if status_label is not None:
            status_label.text = self._screen_stopping_status_text()

        def _run_stop() -> None:
            state: Dict[str, object] = {}
            error = ""
            try:
                state = dict(self._screen_runtime().stop())
            except Exception as exc:
                error = str(exc)
                try:
                    self._screen_runtime().last_error = error
                except Exception:
                    pass

            def _finish(_dt) -> None:
                self._screen_stop_in_progress = False
                if on_done is not None:
                    try:
                        on_done(state, error)
                    except Exception as callback_exc:
                        self._set_screen_share_status(self._screen_stop_failed_text(str(callback_exc)))
                        self._set_screen_share_ui_state("idle")

            Clock.schedule_once(_finish, 0)

        threading.Thread(target=_run_stop, daemon=True).start()
        return True

    def _choose_screen_receive_port(self, backend: object = None) -> Optional[int]:
        backend_name = self._normalize_screen_backend(self._screen_backend() if backend is None else backend)
        if backend_name != SCREEN_BACKEND_RUST:
            return find_available_udp_port(SCREEN_PORT_CANDIDATES)
        for port in RUST_SCREEN_PORT_CANDIDATES:
            status = udp_port_status(port)
            if status.get("available"):
                return int(port)
            error_text = str(status.get("error") or "")
            if "10013" in error_text or "access permissions" in error_text.lower():
                self._append_debug_line("Port denied by Windows policy; trying another high port", protocol=False)
        return None

    def _screen_receive_ports_busy_message(self, backend: object = None) -> str:
        backend_name = self._normalize_screen_backend(self._screen_backend() if backend is None else backend)
        return RUST_SCREEN_PORTS_BUSY_MESSAGE if backend_name == SCREEN_BACKEND_RUST else SCREEN_PORTS_BUSY_MESSAGE

    def _screen_peer_label(self, peer_id: str = "", fallback_host: str = "") -> str:
        pid = str(peer_id or "").strip()
        fallback = str(fallback_host or "").strip()
        label = ""
        if pid:
            try:
                label = str(self._display_name_for_peer(pid) or "").strip()
            except Exception:
                label = ""
        if label and label != pid:
            return label
        if fallback and not is_unspecified_ip(fallback):
            return fallback
        return label or fallback or pid or ("对方" if self.lang == "zh" else "Remote")

    def _current_screen_peer_label(self, state: Optional[Dict[str, object]] = None) -> str:
        label = str(getattr(self, "screen_share_peer_label", "") or getattr(self, "current_screen_peer", "") or "").strip()
        if label:
            return label
        try:
            runtime_label = str(dict(state or self._screen_runtime().get_state()).get("peer_label") or "").strip()
            if runtime_label:
                return runtime_label
        except Exception:
            pass
        peer_id = str(getattr(self, "screen_share_peer_id", "") or "").strip()
        ui_state = str(getattr(self, "screen_share_ui_state", "idle") or "idle")
        if not peer_id and ui_state in self._screen_share_active_states():
            peer_id = str(getattr(self, "current_peer_id", "") or "").strip()
        return self._screen_peer_label(peer_id) if peer_id else ""

    def _set_current_screen_context(
        self,
        *,
        peer_label: Optional[str] = None,
        profile: Optional[str] = None,
        port: Optional[int] = None,
        audio: Optional[Dict[str, object]] = None,
        backend: Optional[str] = None,
    ) -> None:
        if peer_label is not None:
            self.screen_share_peer_label = str(peer_label or "").strip()
            self.current_screen_peer = self.screen_share_peer_label
        if profile is not None:
            self.screen_share_selected_profile = str(profile or "").strip()
            self.current_screen_profile = self.screen_share_selected_profile
        if port is not None:
            self.screen_share_current_port = int(port)
            self.current_screen_port = int(port)
        if audio is not None:
            self.screen_share_current_audio = dict(audio or {"enabled": False, "mode": "none"})
        if backend is not None:
            self.screen_share_current_backend = self._normalize_screen_backend(backend)

    def _clear_current_screen_context(self) -> None:
        self.screen_share_peer_label = ""
        self.screen_share_current_port = None
        self.screen_share_selected_profile = ""
        self.screen_share_current_audio = {"enabled": False, "mode": "none"}
        self.screen_share_current_backend = self._screen_backend()
        self.current_screen_peer = ""
        self.current_screen_profile = ""
        self.current_screen_port = None

    def _screen_port_from_accept(self, control: Dict[str, object], payload: Dict[str, object]) -> int:
        for raw in (control.get("screen_port"), payload.get("screen_port"), payload.get("port")):
            if raw in (None, ""):
                continue
            try:
                value = int(raw)
            except Exception:
                continue
            if 1 <= value <= 65535:
                return value
        return DEFAULT_SCREEN_PORT

    def _current_screen_port_text(self, state: Optional[Dict[str, object]] = None) -> str:
        port = getattr(self, "screen_share_current_port", None)
        if port in (None, ""):
            port = getattr(self, "current_screen_port", None)
        if port in (None, ""):
            try:
                port = dict(state or self._screen_runtime().get_state()).get("port")
            except Exception:
                port = None
        return "" if port in (None, "") else str(port)

    def _format_udp_port_diagnostics(self, state: Optional[Dict[str, object]] = None) -> str:
        try:
            main = udp_port_status(MAIN_UDP_PORT)
            receiver_running = False
            try:
                receiver_running = bool(getattr(self, "receiver_worker", None) and self.receiver_worker.is_running())
            except Exception:
                receiver_running = False
            if main.get("available"):
                main_status = "可用"
            elif receiver_running:
                main_status = "本机 AgoraLink 接收端正在使用"
            else:
                main_status = "已被占用，请关闭旧的 AgoraLink 或修改配置后重启"
            backend = self._screen_backend()
            screen_port_candidates = RUST_SCREEN_PORT_CANDIDATES if backend == SCREEN_BACKEND_RUST else SCREEN_PORT_CANDIDATES
            screen_port_range = "55000-55999" if backend == SCREEN_BACKEND_RUST else "50020-50025"
            screen_statuses = udp_ports_status(screen_port_candidates)
            occupied = [str(item.get("port")) for item in screen_statuses if not item.get("available")]
            occupied_text = ", ".join(occupied) if occupied else "无"
            current_port = self._current_screen_port_text(state) or "无"
            selected_profile = self._current_screen_profile_name(state) or "无"
            current_encoder = self._current_screen_encoder(selected_profile)
            if backend == SCREEN_BACKEND_RUST:
                profile_ids = [DEFAULT_SCREEN_PROFILE]
            else:
                profile_ids = [str(item.get("id") or item.get("name") or "") for item in self._screen_advertised_profiles()]
            profiles_text = ", ".join([item for item in profile_ids if item]) or "无"
            return (
                f"UDP 9999: {main_status}\n"
                f"screen backend: {self._screen_backend_label(backend)}\n"
                f"UDP {screen_port_range} occupied: {occupied_text}\n"
                f"本机可发送 profiles: {profiles_text}\n"
                f"当前 selected_profile: {selected_profile}\n"
                f"当前 encoder: {current_encoder or '无'}\n"
                f"当前投屏实际端口: {current_port}"
            )
        except Exception as exc:
            return f"UDP 端口检测失败: {exc}"

    def _format_screen_runtime_state(self, state: Dict[str, object]) -> str:
        deps = {}
        try:
            deps = self._screen_runtime().check_dependencies()
        except Exception as exc:
            deps = {"error": str(exc)}
        peer_label = self._current_screen_peer_label(state)
        selected_profile = self._current_screen_profile_name(state) or str(state.get("profile") or "")
        screen_port = self._current_screen_port_text(state)
        backend = str(state.get("backend") or self._screen_backend())
        native_stats = dict(state.get("native_stats") or {})
        return (
            f"screen_state: {state.get('state') or ''}    running: {bool(state.get('running'))}\n"
            f"backend: {self._screen_backend_label(backend)}\n"
            f"mode: {state.get('mode') or ''}    host: {state.get('host') or ''}\n"
            f"peer_label: {peer_label}\n"
            f"selected_profile: {selected_profile}    screen_port: {screen_port}\n"
            f"audio: {state.get('audio_state') or 'video_only'}\n"
            f"ffmpeg_path: {deps.get('ffmpeg_path') or ''}\n"
            f"ffplay_path: {deps.get('ffplay_path') or ''}\n"
            f"rust_native_path: {deps.get('rust_native_path') or deps.get('native_media_path') or ''}\n"
            f"native_stats: fps={native_stats.get('fps') or native_stats.get('fps_render') or ''} mbps={native_stats.get('mbps') or ''} frames_sent={native_stats.get('frames_sent') or ''} frames_rendered={native_stats.get('frames_rendered') or ''} packets_lost={native_stats.get('packets_lost_estimate') or ''} decoder_errors={native_stats.get('decoder_errors') or ''}\n"
            f"last_error: {state.get('last_error') or ''}\n"
            f"{self._format_udp_port_diagnostics(state)}"
        )

    def _diagnostic_section_title(self, text: str, subtitle: str = "") -> BoxLayout:
        box = BoxLayout(orientation="vertical", size_hint_y=None, height=dp(46 if subtitle else 28), spacing=dp(2))
        title = make_label(
            text=str(text or ""),
            size_hint_y=None,
            height=dp(24),
            halign="left",
            valign="middle",
            bold=True,
            color=ui_component_color("text_primary") if ui_component_color is not None else THEME["text"],
        )
        bind_label_wrap(title)
        box.add_widget(title)
        if subtitle:
            sub = make_label(
                text=str(subtitle or ""),
                size_hint_y=None,
                height=dp(18),
                halign="left",
                valign="middle",
                color=THEME["muted_text"],
                shorten=True,
            )
            bind_label_wrap(sub)
            box.add_widget(sub)
        return box

    def _diagnostic_status_card(self, title: str, status: str, detail: str, kind: str = "neutral"):
        try:
            card = UIRoundedCard(
                orientation="vertical",
                size_hint_y=None,
                height=dp(78),
                spacing=dp(6),
                padding=(dp(12), dp(10), dp(12), dp(8)),
                radius=16,
                bg_color=ui_component_color("surface"),
                border_color=ui_component_color("danger_soft" if kind in ("danger", "failed") else "border_soft"),
            )
            header = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(24), spacing=dp(8))
            title_label = make_label(
                text=str(title or ""),
                size_hint_x=1,
                halign="left",
                valign="middle",
                bold=True,
                shorten=True,
                color=ui_component_color("text_primary") if ui_component_color is not None else THEME["text"],
            )
            bind_label_wrap(title_label)
            badge_kind = "failed" if kind == "danger" else ("warning" if kind == "warning" else ("accent" if kind == "accent" else "neutral"))
            badge = UIStatusBadge(text=str(status or ""), status=badge_kind, max_width=dp(120))
            header.add_widget(title_label)
            header.add_widget(badge)
            detail_label = make_label(
                text=str(detail or "-"),
                size_hint_y=None,
                height=dp(30),
                halign="left",
                valign="top",
                color=ui_component_color("danger" if kind == "danger" else "text_secondary") if ui_component_color is not None else THEME["muted_text"],
                shorten=True,
                shorten_from="right",
            )
            bind_label_wrap(detail_label)
            card.add_widget(header)
            card.add_widget(detail_label)
            return card
        except Exception:
            card = BoxLayout(orientation="vertical", size_hint_y=None, height=dp(78), spacing=dp(4), padding=dp(10))
            apply_card_background(card, "panel_bg", radius=16)
            card.add_widget(make_label(text=f"{title}  {status}", size_hint_y=None, height=dp(24), halign="left", valign="middle", bold=True))
            detail_label = make_label(text=str(detail or "-"), size_hint_y=None, height=dp(30), halign="left", valign="top", color=THEME["muted_text"])
            bind_label_wrap(detail_label)
            card.add_widget(detail_label)
            return card

    def _recent_error_summary(self, max_lines: int = 4) -> str:
        lines = list(getattr(self, "debug_runtime_lines", []) or []) + list(getattr(self, "debug_protocol_lines", []) or [])
        markers = ("error", "failed", "fail", "traceback", "exception", "错误", "失败", "异常")
        picked = []
        for line in reversed(lines):
            text = str(line or "").strip()
            if text and any(marker in text.lower() for marker in markers):
                picked.append(text)
            if len(picked) >= max_lines:
                break
        return "\n".join(reversed(picked)) if picked else "暂无最近错误。"

    def _tail_text(self, lines: List[str], *, max_lines: int = 24, max_chars: int = 5000, fallback: str = "暂无日志。") -> str:
        text = "\n".join([str(item or "") for item in (lines or [])[-max_lines:]]).strip()
        return text[-max_chars:] if text else fallback

    def _diagnostic_snapshot(self) -> Dict[str, object]:
        try:
            runtime_state = dict(self._screen_runtime().get_state())
        except Exception as exc:
            runtime_state = {"state": "error", "last_error": str(exc)}
        try:
            deps = dict(self._screen_runtime().check_dependencies())
        except Exception as exc:
            deps = {"ok": False, "error": str(exc)}
        package_info = dict(deps or {})
        try:
            package_info.update(self._screen_package_snapshot())
        except Exception:
            pass
        try:
            receiver_running = bool(getattr(self, "receiver_worker", None) and self.receiver_worker.is_running())
        except Exception:
            receiver_running = False
        try:
            main_port = udp_port_status(MAIN_UDP_PORT)
        except Exception as exc:
            main_port = {"available": False, "occupied": False, "error": str(exc), "port": MAIN_UDP_PORT}
        try:
            backend = self._screen_backend()
            ports = RUST_SCREEN_PORT_CANDIDATES if backend == SCREEN_BACKEND_RUST else SCREEN_PORT_CANDIDATES
            screen_ports = udp_ports_status(ports)
        except Exception:
            screen_ports = []
        return {
            "runtime_state": runtime_state,
            "dependencies": deps,
            "package_flavor": str(package_info.get("package_flavor") or ""),
            "rust_native_available": bool(package_info.get("rust_native_available") or package_info.get("rust_native_ok") or package_info.get("native_media_ok")),
            "bundled_ffmpeg_available": bool(package_info.get("bundled_ffmpeg_available")),
            "screen_backend_default": str(package_info.get("screen_backend_default") or self._default_screen_backend()),
            "native_screen_video_only": bool(package_info.get("native_screen_video_only", True)),
            "screen_backend": self._screen_backend(),
            "receiver_running": receiver_running,
            "main_port": main_port,
            "screen_ports": screen_ports,
            "recent_errors": self._recent_error_summary(),
        }

    def _diagnostic_package_summary(self, snapshot: Dict[str, object]) -> Tuple[str, str, str]:
        flavor = str(snapshot.get("package_flavor") or "unknown")
        rust_ok = bool(snapshot.get("rust_native_available"))
        bundled_ffmpeg = bool(snapshot.get("bundled_ffmpeg_available"))
        default_backend = self._normalize_screen_backend(snapshot.get("screen_backend_default") or self._default_screen_backend())
        video_only = bool(snapshot.get("native_screen_video_only"))
        detail = (
            f"package_flavor={flavor}; rust_native_available={str(rust_ok).lower()}; "
            f"bundled_ffmpeg_available={str(bundled_ffmpeg).lower()}; "
            f"screen_backend_default={default_backend}; "
            f"native_screen_video_only={str(video_only).lower()}"
        )
        if flavor == "native_lite":
            return "Native Lite", detail, "neutral" if rust_ok and default_backend == SCREEN_BACKEND_RUST else "warning"
        return "Package", detail, "neutral"

    def _diagnostic_udp_9999_summary(self, snapshot: Dict[str, object]) -> Tuple[str, str, str]:
        main = dict(snapshot.get("main_port") or {})
        receiver_running = bool(snapshot.get("receiver_running"))
        if main.get("available"):
            return "正常", "UDP 9999：当前可用", "neutral"
        if receiver_running and main.get("occupied"):
            return "正常", "UDP 9999：本机 AgoraLink 接收端正在使用", "neutral"
        if main.get("occupied"):
            return "异常", MAIN_UDP_PORT_BUSY_MESSAGE, "danger"
        return "需检查", f"UDP 9999 检测失败：{main.get('error') or '未知错误'}", "warning"

    def _diagnostic_screen_ports_summary(self, snapshot: Dict[str, object]) -> Tuple[str, str, str]:
        statuses = list(snapshot.get("screen_ports") or [])
        backend = self._normalize_screen_backend(snapshot.get("screen_backend") or self._screen_backend())
        port_range = "55000-55999" if backend == SCREEN_BACKEND_RUST else "50020-50025"
        if not statuses:
            return "需检查", f"投屏端口 {port_range} 状态暂不可用。", "warning"
        occupied = [str(item.get("port")) for item in statuses if not item.get("available")]
        available = [str(item.get("port")) for item in statuses if item.get("available")]
        current = self._current_screen_port_text(dict(snapshot.get("runtime_state") or {})) or "无"
        if len(available) == 0:
            return "异常", f"投屏端口 {port_range} 均被占用，无法启动接收端。", "danger"
        if occupied:
            return "可用", f"可用：{', '.join(available)}；占用：{', '.join(occupied)}；当前：{current}", "warning"
        return "正常", f"{port_range} 均可用；当前投屏端口：{current}", "neutral"

    def _diagnostic_dependencies_summary(self, snapshot: Dict[str, object]) -> Tuple[str, str, str]:
        deps = dict(snapshot.get("dependencies") or {})
        backend = self._normalize_screen_backend(snapshot.get("screen_backend") or self._screen_backend())
        if backend == SCREEN_BACKEND_RUST:
            if bool(deps.get("rust_native_ok") or deps.get("native_media_ok")):
                return "正常", "Rust native media executable found.", "neutral"
            return "异常", "Rust native media executable not found", "danger"
        ffmpeg_ok = bool(deps.get("ffmpeg_ok"))
        ffplay_ok = bool(deps.get("ffplay_ok"))
        if ffmpeg_ok and ffplay_ok:
            return "正常", "FFmpeg 与 ffplay 已找到。", "neutral"
        missing = []
        if not ffmpeg_ok:
            missing.append("ffmpeg")
        if not ffplay_ok:
            missing.append("ffplay")
        hint = deps.get("install_hint") or "winget install --id Gyan.FFmpeg -e"
        return "异常", f"缺少 {', '.join(missing)}。安装命令：{hint}", "danger"

    def _diagnostic_runtime_summary(self, snapshot: Dict[str, object]) -> Tuple[str, str, str]:
        state = dict(snapshot.get("runtime_state") or {})
        runtime_state = str(state.get("state") or "idle")
        running = bool(state.get("running"))
        mode = str(state.get("mode") or "-")
        profile = self._current_screen_profile_name(state) or str(state.get("profile") or "-")
        port = self._current_screen_port_text(state) or "-"
        if runtime_state == "error":
            return "异常", str(state.get("last_error") or "screen runtime error"), "danger"
        if running:
            return "运行中", f"{runtime_state} / {mode}，profile: {profile}，port: {port}", "accent"
        return "空闲", f"screen runtime: {runtime_state}", "neutral"

    def _diagnostic_receiver_summary(self, snapshot: Dict[str, object]) -> Tuple[str, str, str]:
        if bool(snapshot.get("receiver_running")):
            return "运行中", "AgoraLink 接收端正在运行。", "accent"
        return "未运行", "接收端未运行；需要收文件或聊天时请启动接收端。", "warning"

    def _refresh_screen_runtime_status(self, status_label: Label) -> None:
        try:
            state = self._screen_runtime().get_state()
            status_label.text = self._format_screen_runtime_state(state)
        except Exception as exc:
            status_label.text = f"screen runtime error: {exc}"

    def _schedule_screen_runtime_status(self, status_label: Label) -> None:
        Clock.schedule_once(lambda _dt: self._refresh_screen_runtime_status(status_label), 0)

    def _screen_share_button_text(self, active: bool) -> str:
        return screen_share_button_text(active, self.lang)

    def _screen_share_active_states(self) -> set:
        return screen_share_active_states()

    def _screen_share_status_text(
        self,
        key: str,
        detail: str = "",
        *,
        peer_label: Optional[str] = None,
        profile: Optional[str] = None,
        port: Optional[object] = None,
    ) -> str:
        name = str(peer_label or "").strip() or self._current_screen_peer_label()
        profile_text = str(profile or "").strip() or self._current_screen_profile_name() or "-"
        port_text = str(port or "").strip() or self._current_screen_port_text() or "-"
        return screen_share_status_text(
            key,
            detail,
            lang=self.lang,
            peer_label=name,
            profile=profile_text,
            port=port_text,
        )

    def _screen_share_button_active(self) -> bool:
        ui_state = str(getattr(self, "screen_share_ui_state", "idle") or "idle")
        if ui_state in ("pending_offer", "pending_accept", "stopping"):
            return True
        try:
            state = self._screen_runtime().get_state()
            runtime_state = str(state.get("state") or "")
            running = bool(state.get("running"))
            return bool(running and runtime_state in ("sending", "receiving"))
        except Exception:
            return ui_state in self._screen_share_active_states()

    def _refresh_screen_share_button(self) -> None:
        if not hasattr(self, "main_screen_btn"):
            return
        ui_state = str(getattr(self, "screen_share_ui_state", "idle") or "idle")
        active = self._screen_share_button_active()
        if not active and ui_state not in ("pending_offer", "pending_accept", "stopping"):
            self.screen_share_ui_state = "idle"
        self.main_screen_btn.text = self._stopping_text() if ui_state == "stopping" else self._screen_share_button_text(active)
        self.main_screen_btn.disabled = ui_state == "stopping"
        self.main_screen_btn.width = dp(104)
        self._style_modern_or_legacy_button(self.main_screen_btn, "danger" if active else "secondary")

    def _schedule_screen_share_button_refresh(self) -> None:
        try:
            Clock.schedule_once(lambda _dt: self._refresh_screen_share_button(), 0)
        except Exception:
            pass

    def _set_screen_share_ui_state(self, state: str) -> None:
        self.screen_share_ui_state = str(state or "idle")
        self._schedule_screen_share_button_refresh()

    def _screen_runtime_error_detail(self, state: Optional[Dict[str, object]] = None) -> str:
        try:
            current = dict(state or self._screen_runtime().get_state())
            detail = str(current.get("last_error") or current.get("error") or "").strip()
            if not detail:
                returncode = current.get("returncode")
                if returncode not in (None, "", 0):
                    detail = f"returncode={returncode}"
            return detail or "unknown"
        except Exception as exc:
            return str(exc)

    def _on_screen_share_button(self) -> None:
        try:
            if self._screen_share_button_active():
                self.stop_screen_share_from_chat()
            else:
                self.send_screen_share_offer()
        except Exception as exc:
            self._set_screen_share_status(f"Screen button failed: {exc}")
            self._clear_current_screen_context()
            self._set_screen_share_ui_state("idle")

    def _screen_debug_start_receiver(self, status_label: Label) -> None:
        try:
            backend = self._screen_backend()
            screen_port = self._choose_screen_receive_port(backend)
            if screen_port is None:
                self._clear_current_screen_context()
                busy_message = self._screen_receive_ports_busy_message(backend)
                try:
                    self._screen_runtime().last_error = busy_message
                except Exception:
                    pass
                status_label.text = busy_message + "\n" + self._format_udp_port_diagnostics()
                return
            selected_profile = self._screen_preferred_profile()
            peer_label = "Debug"
            audio_config = self._screen_audio_config(False)
            state = self._screen_runtime().start_receiver(
                port=screen_port,
                profile=selected_profile,
                peer_label=peer_label,
                selected_profile=selected_profile,
                screen_port=screen_port,
                audio=audio_config,
                backend=backend,
            )
            if str(state.get("state") or "") == "receiving":
                self._set_current_screen_context(peer_label=peer_label, profile=selected_profile, port=screen_port, audio=audio_config, backend=backend)
            else:
                self._clear_current_screen_context()
            self._append_debug_line(f"screen receiver start requested backend={backend} port={screen_port} profile={selected_profile}", protocol=False)
        except Exception as exc:
            self._clear_current_screen_context()
            try:
                self._screen_runtime().last_error = str(exc)
            except Exception:
                pass
            status_label.text = f"screen receiver start failed: {exc}"
            return
        self._schedule_screen_runtime_status(status_label)

    def _screen_debug_start_sender(self, host_input: TextInput, status_label: Label) -> None:
        try:
            host = str(host_input.text or "").strip()
            if not host:
                status_label.text = "screen sender start failed: target IP is required"
                return
            backend = self._screen_backend()
            selected_profile = self._screen_preferred_profile()
            peer_label = host
            audio_config = self._screen_audio_for_backend(backend, self._screen_audio_config())
            debug_port = int(getattr(self, "screen_share_current_port", None) or (RUST_SCREEN_PORT_CANDIDATES[0] if backend == SCREEN_BACKEND_RUST else DEFAULT_SCREEN_PORT))
            state = self._screen_runtime().start_sender(
                host=host,
                port=debug_port,
                profile=selected_profile,
                peer_label=peer_label,
                selected_profile=selected_profile,
                screen_port=debug_port,
                system_audio=bool(audio_config.get("enabled")),
                audio=audio_config,
                backend=backend,
            )
            if str(state.get("state") or "") == "sending":
                self._set_current_screen_context(peer_label=peer_label, profile=selected_profile, port=debug_port, audio=self._screen_audio_from_runtime_state(state, audio_config), backend=backend)
                self._schedule_screen_audio_fallback_ui_update(
                    session_id=str(getattr(self, "screen_share_session_id", "") or "debug_screen"),
                    peer_id=str(getattr(self, "current_peer_id", "") or ""),
                    peer_label=peer_label,
                    profile=selected_profile,
                    port=debug_port,
                    offered_audio=audio_config,
                )
            self._append_debug_line(f"screen sender start requested backend={backend} host={host} port={debug_port} profile={selected_profile}", protocol=False)
        except Exception as exc:
            self._clear_current_screen_context()
            try:
                self._screen_runtime().last_error = str(exc)
            except Exception:
                pass
            status_label.text = f"screen sender start failed: {exc}"
            return
        self._schedule_screen_runtime_status(status_label)

    def _screen_debug_stop(self, status_label: Label, on_done=None) -> None:
        def _finish_stop(_state: Dict[str, object], error: str) -> None:
            if error:
                try:
                    self._screen_runtime().last_error = error
                except Exception:
                    pass
                status_label.text = f"screen runtime stop failed: {error}"
            else:
                self._clear_current_screen_context()
                self._append_debug_line("screen runtime stop requested", protocol=False)
                self._schedule_screen_runtime_status(status_label)
            if on_done is not None:
                try:
                    on_done()
                except Exception:
                    pass

        self._stop_screen_runtime_nonblocking(status_label=status_label, on_done=_finish_stop)

    def _schedule_screen_audio_fallback_ui_update(
        self,
        *,
        session_id: str,
        peer_id: str,
        peer_label: str,
        profile: str,
        port: object,
        offered_audio: Dict[str, object],
    ) -> None:
        if not bool((offered_audio or {}).get("enabled")):
            return

        def _check(_dt) -> None:
            try:
                state = dict(self._screen_runtime().get_state())
                audio_state = str(state.get("audio_state") or "").strip()
                if audio_state not in ("fallback_video_only", "audio_failed", "audio_failed_video_only"):
                    return
                audio_detail = self._screen_audio_from_runtime_state(state, offered_audio)
                audio_text = self._screen_audio_text(audio_detail)
                status_text = self._screen_share_status_text("sending", peer_label=peer_label, profile=profile, port=port)
                if peer_id:
                    self._add_screen_chat_card(
                        CARD_SCREEN_STATE,
                        session_id=session_id,
                        peer_id=peer_id,
                        title=self._screen_offer_title(),
                        subtitle=peer_label,
                        status=audio_text,
                        detail=self._screen_detail_text(profile, port, audio_detail),
                        profile=profile,
                        port=port,
                        actions=[{"label": "Stop", "action": "stop_screen", "style": "danger"}],
                        direction="outgoing",
                    )
                self._set_current_screen_context(peer_label=peer_label, profile=profile, port=int(port), audio=audio_detail)
                self._set_screen_share_status(f"{status_text} · {audio_text}")
                self._append_debug_line(str(state.get("last_error") or audio_text), protocol=False)
            except Exception as exc:
                self._append_debug_line(f"screen audio fallback UI update failed: {exc}", protocol=False)

        Clock.schedule_once(_check, 1.3)
        Clock.schedule_once(_check, 2.8)

    def _screen_advertised_profiles(self, force: bool = False) -> List[Dict[str, object]]:
        now = time.time()
        cached = getattr(self, "screen_share_advertised_profiles", []) or []
        if cached and not force and now - float(getattr(self, "screen_share_advertised_profiles_ts", 0.0) or 0.0) < 60.0:
            return [dict(item) for item in cached]
        try:
            deps = self._screen_runtime().check_dependencies()
            ffmpeg_path = str(deps.get("ffmpeg_path") or "")
            profiles = self._get_advertised_profiles_no_console(ffmpeg_path=ffmpeg_path, runtime_seconds=0.75)
            self.screen_share_advertised_profiles = [dict(item) for item in profiles]
            self.screen_share_advertised_profiles_ts = now
            return [dict(item) for item in profiles]
        except Exception as exc:
            self._append_debug_line(f"screen advertised profiles failed: {exc}", protocol=False)
            return [dict(item) for item in cached]

    def _get_advertised_profiles_no_console(self, *, ffmpeg_path: str, runtime_seconds: float) -> List[Dict[str, object]]:
        original_run = subprocess.run

        def _run_without_console(args, *run_args, **kwargs):
            return run_no_console(args, *run_args, run_factory=original_run, **kwargs)

        subprocess.run = _run_without_console
        try:
            return get_advertised_profiles(ffmpeg_path=ffmpeg_path, runtime_seconds=runtime_seconds)
        finally:
            subprocess.run = original_run

    def _screen_preferred_profile(self) -> str:
        value = ""
        try:
            value = str((self.gui_config or {}).get("screen_preferred_profile") or "").strip()
        except Exception:
            value = ""
        if value:
            return profile_id_from_info(value, DEFAULT_SCREEN_PROFILE)
        profiles = self._screen_advertised_profiles()
        if profiles:
            return profile_id_from_info(profiles[0], DEFAULT_SCREEN_PROFILE)
        return DEFAULT_SCREEN_PROFILE

    def _screen_profile_dict(self, profile_name: object = DEFAULT_SCREEN_PROFILE) -> Dict[str, object]:
        return profile_info(profile_name)

    def _current_screen_profile_name(self, state: Optional[Dict[str, object]] = None) -> str:
        profile = str(getattr(self, "screen_share_selected_profile", "") or getattr(self, "current_screen_profile", "") or "").strip()
        if profile:
            return profile_id_from_info(profile, DEFAULT_SCREEN_PROFILE)
        try:
            runtime_profile = dict(state or self._screen_runtime().get_state()).get("profile")
            if runtime_profile:
                return profile_id_from_info(runtime_profile, DEFAULT_SCREEN_PROFILE)
        except Exception:
            pass
        return ""

    def _current_screen_encoder(self, profile_name: object) -> str:
        name = profile_id_from_info(profile_name, default="")
        profile = PROFILES_BY_NAME.get(name)
        return str(profile.encoder) if profile is not None else ""

    def _offered_screen_profiles(self, control: Dict[str, object], payload: Dict[str, object]) -> List[Dict[str, object]]:
        raw = control.get("profiles")
        if not isinstance(raw, list):
            raw = payload.get("profiles")
        if not isinstance(raw, list):
            return []
        return [dict(item) for item in raw if isinstance(item, dict)]

    def _preferred_screen_profile_from_offer(self, control: Dict[str, object], payload: Dict[str, object]) -> str:
        raw = control.get("preferred_profile") or payload.get("preferred_profile") or payload.get("profile_name") or DEFAULT_SCREEN_PROFILE
        return profile_id_from_info(raw, DEFAULT_SCREEN_PROFILE)

    def _legacy_screen_profile_from_offer(self, payload: Dict[str, object]) -> Dict[str, object]:
        profile = payload.get("profile")
        if isinstance(profile, dict):
            return dict(profile)
        return self._screen_profile_dict(payload.get("profile_name") or DEFAULT_SCREEN_PROFILE)

    def _selected_profile_from_accept(self, control: Dict[str, object], payload: Dict[str, object]) -> str:
        for raw in (
            control.get("selected_profile"),
            payload.get("selected_profile"),
            control.get("selected_profile_info"),
            payload.get("selected_profile_info"),
        ):
            if raw in (None, ""):
                continue
            return profile_id_from_info(raw, DEFAULT_SCREEN_PROFILE)
        return DEFAULT_SCREEN_PROFILE

    def _local_screen_host(self) -> str:
        try:
            for ip in get_local_ip_candidates():
                ip = str(ip or "").strip()
                if ip and not ip.startswith("127.") and not is_unspecified_ip(ip):
                    return ip
            ips = get_local_ip_candidates()
            return str(ips[0] or "127.0.0.1") if ips else "127.0.0.1"
        except Exception:
            return "127.0.0.1"

    def _set_screen_share_status(self, text: str) -> None:
        self.screen_share_last_status = str(text or "")
        def _apply(_dt):
            try:
                if hasattr(self, "screen_share_status_label"):
                    self.screen_share_status_label.text = self.screen_share_last_status
            except Exception:
                pass
        Clock.schedule_once(_apply, 0)
        self._schedule_screen_share_button_refresh()

    def _update_screen_share_status_from_runtime(self, prefix: str = "") -> None:
        try:
            state = self._screen_runtime().get_state()
            runtime_state = str(state.get("state") or "idle")
            if runtime_state in ("sending", "receiving"):
                self.screen_share_ui_state = runtime_state
            elif runtime_state == "error":
                self.screen_share_ui_state = "idle"
            elif str(getattr(self, "screen_share_ui_state", "idle") or "") not in ("pending_offer", "pending_accept"):
                self.screen_share_ui_state = "idle"
            if runtime_state == "sending":
                text = self._screen_share_status_text("sending")
            elif runtime_state == "receiving":
                text = self._screen_share_status_text("receiving")
            elif runtime_state == "error":
                text = self._screen_share_status_text("startup_failed", self._screen_runtime_error_detail(state))
            else:
                text = self._screen_share_status_text("idle")
            self._set_screen_share_status((str(prefix or "").strip() + "  " + text).strip())
        except Exception as exc:
            self._set_screen_share_status(self._screen_share_status_text("startup_failed", str(exc)))

    def _screen_control_payload_text(self, control: Dict[str, object]) -> str:
        return SCREEN_CONTROL_TEXT_PREFIX + json.dumps(control, ensure_ascii=False, separators=(",", ":"))

    def _screen_control_json_from_chat(self, obj: Dict[str, object]) -> str:
        body_type = str((obj or {}).get("body_type") or "text")
        if body_type and body_type != "text":
            return ""
        text = str((obj or {}).get("text") or (obj or {}).get("body") or "")
        if not text.startswith(SCREEN_CONTROL_TEXT_PREFIX):
            return ""
        return text[len(SCREEN_CONTROL_TEXT_PREFIX):]

    def _is_screen_control_chat(self, obj: Dict[str, object]) -> bool:
        return bool(self._screen_control_json_from_chat(obj))

    def _screen_control_preview(self) -> str:
        return "投屏控制" if self.lang == "zh" else "Screen control"

    def _send_screen_control_to_peer(self, peer_id: str, control: Dict[str, object]) -> bool:
        peer_id = str(peer_id or "").strip()
        if self.message_service is None:
            self._set_screen_share_status(self._screen_share_status_text("startup_failed", "chat is locked"))
            return False
        if not peer_id:
            self._set_screen_share_status(self._screen_share_status_text("startup_failed", "missing peer"))
            return False
        try:
            control_type = str((control or {}).get("type") or "")
            text = self._screen_control_payload_text(control)
            msg, contact = self.message_service.create_direct_text(peer_id, text)
        except Exception as exc:
            self._set_screen_share_status(self._screen_share_status_text("startup_failed", str(exc)))
            if str((control or {}).get("type") or "") == SCREEN_SHARE_OFFER:
                self.screen_share_session_id = ""
                self.screen_share_peer_id = ""
                self._clear_current_screen_context()
                self._set_screen_share_ui_state("idle")
            return False

        mid = str(msg.get("message_id") or "")
        conv_id = str(msg.get("conversation_id") or "")
        created_at = float(msg.get("created_at") or time.time())

        def _mark_sent(pid: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_sent(mid, pid)
            except Exception as exc:
                self._append_debug_line(f"screen mark_sent failed: {exc}", protocol=True)

        def _mark_delivered(pid: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_delivered(mid, pid)
            except Exception as exc:
                self._append_debug_line(f"screen mark_delivered failed: {exc}", protocol=True)

        def _mark_failed(pid: str, err: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_failed(mid, pid, error=err)
            except Exception as exc:
                self._append_debug_line(f"screen mark_failed failed: {exc}; original={err}", protocol=True)

        def _run() -> None:
            ip = str((contact or {}).get("peer_ip") or "")
            try:
                port = int((contact or {}).get("peer_port") or 9999)
            except Exception:
                port = 9999
            if not ip or is_unspecified_ip(ip):
                ip2, port2 = self._endpoint_for_peer(peer_id)
                ip = ip or ip2
                port = port2 or port
            if not ip or is_unspecified_ip(ip):
                def _invalid_endpoint(_dt):
                    _mark_failed(peer_id, "invalid_endpoint")
                    if control_type == SCREEN_SHARE_OFFER:
                        session_id = str((control or {}).get("session_id") or "")
                        peer_label = self._screen_peer_label(peer_id)
                        status_text = self._screen_start_failed_text("invalid endpoint")
                        self._add_screen_chat_card(
                            CARD_SCREEN_STATE,
                            session_id=session_id,
                            peer_id=peer_id,
                            title=self._screen_offer_title(),
                            subtitle=peer_label,
                            status=status_text,
                            detail="invalid endpoint",
                            direction="outgoing",
                        )
                        self.screen_share_session_id = ""
                        self.screen_share_peer_id = ""
                        self._clear_current_screen_context()
                        self._set_screen_share_ui_state("idle")
                    self._set_screen_share_status(self._screen_start_failed_text("invalid endpoint"))
                Clock.schedule_once(_invalid_endpoint, 0)
                return
            Clock.schedule_once(lambda _dt: _mark_sent(peer_id), 0)
            try:
                ok = self._send_chat_to_endpoint(
                    ip=ip,
                    port=port,
                    peer_id=peer_id,
                    text=text,
                    message_id=mid,
                    conversation_id=conv_id,
                    created_at=created_at,
                    body_type="text",
                )
            except Exception as exc:
                ok = False
                self._append_debug_line(f"screen control send failed: {exc}", protocol=True)
            if ok:
                Clock.schedule_once(lambda _dt: _mark_delivered(peer_id), 0)
            else:
                def _send_failed(_dt):
                    _mark_failed(peer_id, "screen_control_send_failed")
                    if control_type == SCREEN_SHARE_OFFER:
                        session_id = str((control or {}).get("session_id") or "")
                        peer_label = self._screen_peer_label(peer_id)
                        status_text = self._screen_start_failed_text("screen control send failed")
                        self._add_screen_chat_card(
                            CARD_SCREEN_STATE,
                            session_id=session_id,
                            peer_id=peer_id,
                            title=self._screen_offer_title(),
                            subtitle=peer_label,
                            status=status_text,
                            detail="screen control send failed",
                            direction="outgoing",
                        )
                        self.screen_share_session_id = ""
                        self.screen_share_peer_id = ""
                        self._clear_current_screen_context()
                        self._set_screen_share_ui_state("idle")
                    self._set_screen_share_status(self._screen_start_failed_text("screen control send failed"))
                Clock.schedule_once(_send_failed, 0)
        threading.Thread(target=_run, daemon=True).start()
        return True

    def send_screen_share_offer(self) -> None:
        ui_state = str(getattr(self, "screen_share_ui_state", "idle") or "idle")
        if ui_state in ("pending_offer", "pending_accept"):
            self._set_screen_share_status(self._screen_share_status_text(ui_state))
            return
        if self._screen_share_button_active():
            return
        if self.current_chat_mode != "direct" or not self.current_peer_id:
            self._set_screen_share_status("Screen share requires a direct contact")
            self._set_screen_share_ui_state("idle")
            return
        try:
            session_id = "screen_" + secrets.token_hex(12)
            backend = self._screen_backend()
            if backend == SCREEN_BACKEND_RUST:
                preferred_profile = DEFAULT_SCREEN_PROFILE
                profile = self._screen_profile_dict(preferred_profile)
                profiles = [dict(profile)]
            else:
                profiles = self._screen_advertised_profiles(force=True)
                preferred_profile = self._screen_preferred_profile()
                profile = self._screen_profile_dict(preferred_profile)
            if not profiles:
                reason = "没有可用的本机投屏档位"
                status_text = self._screen_start_failed_text(reason)
                self._set_screen_share_status(status_text)
                if self.current_peer_id:
                    peer_label = self._screen_peer_label(self.current_peer_id)
                    self._add_screen_chat_card(
                        CARD_SCREEN_STATE,
                        session_id=session_id,
                        peer_id=str(self.current_peer_id or ""),
                        title=self._screen_offer_title(),
                        subtitle=peer_label,
                        status=status_text,
                        detail=reason,
                        direction="outgoing",
                    )
                self._set_screen_share_ui_state("idle")
                return
            audio_config = self._screen_audio_for_backend(backend, self._screen_audio_config())
            if backend == SCREEN_BACKEND_RUST and bool(self._screen_audio_config().get("enabled")):
                self._set_screen_share_status(str(audio_config.get("error") or NATIVE_LITE_VIDEO_ONLY_MESSAGE))
            peer_id = str(self.current_peer_id or "")
            peer_label = self._screen_peer_label(peer_id)
            self._append_debug_line(
                "OFFER advertised profiles: " + ", ".join(str(item.get("id") or item.get("name") or "") for item in profiles),
                protocol=False,
            )
            offer = make_offer(
                session_id,
                self.chat_local_peer_id,
                peer_id,
                self._local_screen_host(),
                DEFAULT_SCREEN_PORT,
                preferred_profile,
                profile,
                profiles=profiles,
                preferred_profile=preferred_profile,
                audio=audio_config,
                backend=backend,
            )
            self.screen_share_session_id = session_id
            self.screen_share_peer_id = peer_id
            self._clear_current_screen_context()
            self.screen_share_peer_label = peer_label
            self.current_screen_peer = peer_label
            self.screen_share_current_audio = dict(audio_config)
            self.screen_share_current_backend = backend
            if self._send_screen_control_to_peer(peer_id, offer):
                status_text = self._screen_share_status_text("pending_offer", peer_label=peer_label)
                self._set_screen_share_ui_state("pending_offer")
                self._set_screen_share_status(status_text)
                self._add_screen_chat_card(
                    CARD_SCREEN_OFFER,
                    session_id=session_id,
                    peer_id=peer_id,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=status_text,
                    detail=self._screen_detail_text(preferred_profile, DEFAULT_SCREEN_PORT, audio_config),
                    profile=preferred_profile,
                    port=DEFAULT_SCREEN_PORT,
                    direction="outgoing",
                )
            else:
                status_text = self._screen_start_failed_text("screen control send failed")
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=session_id,
                    peer_id=peer_id,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=status_text,
                    detail=self._screen_detail_text(preferred_profile, DEFAULT_SCREEN_PORT, audio_config),
                    profile=preferred_profile,
                    port=DEFAULT_SCREEN_PORT,
                    direction="outgoing",
                )
                self.screen_share_session_id = ""
                self.screen_share_peer_id = ""
                self._clear_current_screen_context()
                self._set_screen_share_ui_state("idle")
        except Exception as exc:
            self._set_screen_share_status(self._screen_start_failed_text(str(exc)))
            self.screen_share_session_id = ""
            self.screen_share_peer_id = ""
            self._clear_current_screen_context()
            self._set_screen_share_ui_state("idle")

    def stop_screen_share_from_chat(self) -> None:
        peer_id = str(self.screen_share_peer_id or self.current_peer_id or "").strip()
        session_id = str(self.screen_share_session_id or ("screen_" + secrets.token_hex(12)))
        peer_label = self._current_screen_peer_label() or self._screen_peer_label(peer_id)
        profile_name = self._current_screen_profile_name()
        port_text = self._current_screen_port_text()
        audio_detail = dict(getattr(self, "screen_share_current_audio", {}) or {"enabled": False, "mode": "none"})

        def _finish_stop(state: Dict[str, object], error: str) -> None:
            stop_status_text = self._screen_stopped_text()
            if error:
                stop_status_text = self._screen_stop_failed_text(error)
            elif str(state.get("state") or "") == "error":
                stop_status_text = self._screen_stop_failed_text(self._screen_runtime_error_detail(state))
            self._set_screen_share_status(stop_status_text)
            if peer_id:
                try:
                    stop_msg = make_stop(session_id, self.chat_local_peer_id, peer_id, reason="user_stop")
                    self._send_screen_control_to_peer(peer_id, stop_msg)
                except Exception as exc:
                    self._set_screen_share_status(f"Screen stop notify failed: {exc}")
            if peer_id:
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=session_id,
                    peer_id=peer_id,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=stop_status_text,
                    detail=self._screen_detail_text(profile_name, port_text, audio_detail),
                    profile=profile_name,
                    port=port_text,
                    direction="outgoing",
                )
            self.screen_share_session_id = ""
            self.screen_share_peer_id = ""
            self._clear_current_screen_context()
            self._set_screen_share_ui_state("idle")

        self._stop_screen_runtime_nonblocking(on_done=_finish_stop)

    def _parse_screen_control_from_chat(self, obj: Dict[str, object]) -> Optional[Dict[str, object]]:
        try:
            text = self._screen_control_json_from_chat(obj)
            if not text:
                return None
            return parse_screen_control_message(text)
        except Exception:
            return None

    def _handle_screen_control_from_chat(self, obj: Dict[str, object]) -> None:
        control = self._parse_screen_control_from_chat(obj)
        if not control:
            return
        try:
            sender = str(control.get("sender_peer_id") or "")
            receiver = str(control.get("receiver_peer_id") or "")
            if sender == self.chat_local_peer_id:
                return
            if receiver and receiver != self.chat_local_peer_id:
                return
            key = str(obj.get("message_id") or "") or f"{control.get('type')}:{control.get('session_id')}:{sender}"
            if key in self._seen_screen_control_messages:
                return
            self._seen_screen_control_messages.add(key)
            message_type = str(control.get("type") or "")
            if message_type == SCREEN_SHARE_OFFER:
                payload = dict(control.get("payload") or {})
                peer_label = self._screen_peer_label(sender, str(payload.get("host") or ""))
                session_id = str(control.get("session_id") or "")
                profile_name = profile_id_from_info(payload.get("preferred_profile") or payload.get("profile_name") or DEFAULT_SCREEN_PROFILE, DEFAULT_SCREEN_PROFILE)
                offer_port = payload.get("port") or DEFAULT_SCREEN_PORT
                audio_config = self._screen_audio_from_control(control, payload)
                backend = self._screen_backend_from_control(control, payload, SCREEN_BACKEND_FFMPEG)
                self.pending_screen_offers[session_id] = dict(control)
                status_text = f"收到 {peer_label} 的投屏邀请" if self.lang == "zh" else f"Screen share invitation from {peer_label}"
                self._add_screen_chat_card(
                    CARD_SCREEN_OFFER,
                    session_id=session_id,
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=status_text,
                    detail=self._screen_detail_text(profile_name, offer_port, audio_config) + f"  backend: {self._screen_backend_label(backend)}",
                    profile=profile_name,
                    port=offer_port,
                    actions=[
                        {"label": "接受", "action": f"accept_screen:{session_id}", "style": "success"},
                        {"label": "拒绝", "action": f"reject_screen:{session_id}", "style": "danger"},
                    ],
                )
                self._set_screen_share_status(status_text)
            elif message_type == SCREEN_SHARE_ACCEPT:
                self._handle_screen_accept(control)
            elif message_type == SCREEN_SHARE_REJECT:
                reason = str(dict(control.get("payload") or {}).get("reason") or "")
                peer_label = self._screen_peer_label(sender)
                status_text = self._screen_rejected_by_peer_text(peer_label)
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=str(control.get("session_id") or ""),
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=status_text,
                    detail=reason,
                )
                self.screen_share_session_id = ""
                self.screen_share_peer_id = ""
                self._set_screen_share_ui_state("idle")
                self._set_screen_share_status(status_text)
                self._clear_current_screen_context()
            elif message_type == SCREEN_SHARE_STOP:
                self._handle_screen_stop(control)
            elif message_type == SCREEN_SHARE_STATE:
                payload = dict(control.get("payload") or {})
                peer_label = self._screen_peer_label(sender)
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=str(control.get("session_id") or ""),
                    peer_id=sender,
                    title="Screen share state",
                    subtitle=peer_label,
                    status=str(payload.get("state") or ""),
                    detail=str(payload.get("detail") or ""),
                )
                self._set_screen_share_status(f"Remote screen state: {payload.get('state') or ''} {payload.get('detail') or ''}")
        except Exception as exc:
            self._set_screen_share_status(self._screen_share_status_text("startup_failed", str(exc)))

    def _show_screen_offer_popup(self, control: Dict[str, object]) -> None:
        sender = str(control.get("sender_peer_id") or "")
        session_id = str(control.get("session_id") or "")
        payload = dict(control.get("payload") or {})
        sender_host = str(payload.get("host") or "").strip()
        peer_label = self._screen_peer_label(sender, sender_host)
        profile_name = profile_id_from_info(payload.get("preferred_profile") or payload.get("profile_name") or DEFAULT_SCREEN_PROFILE, DEFAULT_SCREEN_PROFILE)
        audio_config = self._screen_audio_from_control(control, payload)
        content = BoxLayout(orientation="vertical", spacing=dp(10), padding=dp(12))
        label = make_label(
            text=f"收到投屏邀请\n来自: {peer_label}\n推荐 profile: {profile_name}\n{self._screen_audio_text(audio_config)}",
            halign="left",
            valign="top",
        )
        bind_label_wrap(label)
        content.add_widget(label)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        popup = style_popup(Popup(title="投屏邀请", content=content, size_hint=(0.52, 0.36), auto_dismiss=False))
        if session_id:
            self.pending_screen_offer_popups[session_id] = popup
        buttons.add_widget(make_button("success", text="接受", on_release=lambda *_: (self._accept_screen_offer(control), popup.dismiss())))
        buttons.add_widget(make_button("danger", text="拒绝", on_release=lambda *_: (self._reject_screen_offer(control, "user_rejected"), popup.dismiss())))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def _accept_screen_offer(self, control: Dict[str, object]) -> None:
        sender = str(control.get("sender_peer_id") or "")
        session_id = str(control.get("session_id") or "")
        payload = dict(control.get("payload") or {})
        sender_host = str(payload.get("host") or "").strip()
        peer_label = self._screen_peer_label(sender, sender_host)
        backend = self._screen_backend_from_control(control, payload, SCREEN_BACKEND_FFMPEG)
        audio_config = self._screen_audio_for_backend(backend, self._screen_audio_from_control(control, payload))
        try:
            if session_id:
                self.pending_screen_offers.pop(session_id, None)
                pop = self.pending_screen_offer_popups.pop(session_id, None)
                if pop is not None:
                    try:
                        pop.dismiss()
                    except Exception:
                        pass
            self._set_screen_share_ui_state("pending_accept")
            self.screen_share_peer_label = peer_label
            self.current_screen_peer = peer_label
            starting_text = "正在启动接收端" if self.lang == "zh" else "Starting screen receiver"
            self._set_screen_share_status(starting_text)
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=session_id,
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=peer_label,
                status=starting_text,
                detail=self._screen_detail_text(self._preferred_screen_profile_from_offer(control, payload), payload.get("port") or DEFAULT_SCREEN_PORT, audio_config) + f"  backend: {self._screen_backend_label(backend)}",
            )
            offered_profiles = self._offered_screen_profiles(control, payload)
            if backend == SCREEN_BACKEND_RUST:
                selected_profile_info = self._legacy_screen_profile_from_offer(payload)
            elif offered_profiles:
                local_profiles = self._screen_advertised_profiles(force=True)
                user_preferred = str((self.gui_config or {}).get("screen_preferred_profile") or "").strip()
                preferred = user_preferred or self._preferred_screen_profile_from_offer(control, payload)
                selected_profile_info = choose_advertised_profile(
                    offered_profiles,
                    local_profiles,
                    preferred_profile=preferred,
                )
                if selected_profile_info is None:
                    self._clear_current_screen_context()
                    failed_text = self._screen_start_failed_text("没有可用的共同投屏档位")
                    self._set_screen_share_status(failed_text)
                    self._set_screen_share_ui_state("idle")
                    self._add_screen_chat_card(
                        CARD_SCREEN_STATE,
                        session_id=session_id,
                        peer_id=sender,
                        title=self._screen_offer_title(),
                        subtitle=peer_label,
                        status=failed_text,
                        detail="没有可用的共同投屏档位",
                    )
                    self._reject_screen_offer(control, "没有可用的共同投屏档位", update_status=False)
                    return
            else:
                selected_profile_info = self._legacy_screen_profile_from_offer(payload)
            selected_profile_name = profile_id_from_info(selected_profile_info, DEFAULT_SCREEN_PROFILE)
            self._append_debug_line(f"ACCEPT selected profile: {selected_profile_name}", protocol=False)
            screen_port = self._choose_screen_receive_port(backend)
            if screen_port is None:
                self._clear_current_screen_context()
                busy_message = self._screen_receive_ports_busy_message(backend)
                failed_text = self._screen_start_failed_text(busy_message)
                self._set_screen_share_status(failed_text)
                self._set_screen_share_ui_state("idle")
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=session_id,
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=failed_text,
                    detail=busy_message,
                    profile=selected_profile_name,
                )
                self._reject_screen_offer(control, busy_message, update_status=False)
                return
            state = self._screen_runtime().start_receiver(
                port=screen_port,
                profile=selected_profile_name,
                peer_label=peer_label,
                selected_profile=selected_profile_name,
                screen_port=screen_port,
                audio=audio_config,
                backend=backend,
            )
            if str(state.get("state") or "") != "receiving":
                reason = self._screen_runtime_error_detail(self._screen_runtime().get_state())
                failed_text = self._screen_start_failed_text(reason)
                self._set_screen_share_status(failed_text)
                self._set_screen_share_ui_state("idle")
                self._clear_current_screen_context()
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=session_id,
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=failed_text,
                    detail=self._screen_detail_text(selected_profile_name, screen_port, audio_config),
                    profile=selected_profile_name,
                    port=screen_port,
                )
                self._reject_screen_offer(control, reason, update_status=False)
                return
            accept = make_accept(
                session_id,
                self.chat_local_peer_id,
                sender,
                self._local_screen_host(),
                screen_port,
                selected_profile_info,
                audio=audio_config,
                backend=backend,
            )
            self.screen_share_session_id = session_id
            self.screen_share_peer_id = sender
            self._set_current_screen_context(peer_label=peer_label, profile=selected_profile_name, port=screen_port, audio=audio_config, backend=backend)
            self._set_screen_share_ui_state("receiving")
            status_text = self._screen_share_status_text("receiving", peer_label=peer_label, profile=selected_profile_name, port=screen_port)
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=session_id,
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=peer_label,
                status=status_text,
                detail=self._screen_detail_text(selected_profile_name, screen_port, audio_config),
                profile=selected_profile_name,
                port=screen_port,
                actions=[{"label": "Stop", "action": "stop_screen", "style": "danger"}],
            )
            self._send_screen_control_to_peer(sender, accept)
            self._append_debug_line(f"receiver actually used port/profile: {screen_port}/{selected_profile_name}", protocol=False)
            self._set_screen_share_status(status_text)
        except Exception as exc:
            self._clear_current_screen_context()
            failed_text = self._screen_start_failed_text(str(exc))
            self._set_screen_share_status(failed_text)
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=session_id,
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=peer_label,
                status=failed_text,
                detail=str(exc),
            )
            self._set_screen_share_ui_state("idle")

    def _reject_screen_offer(self, control: Dict[str, object], reason: str, update_status: bool = True) -> None:
        sender = str(control.get("sender_peer_id") or "")
        session_id = str(control.get("session_id") or "")
        try:
            if session_id:
                self.pending_screen_offers.pop(session_id, None)
                pop = self.pending_screen_offer_popups.pop(session_id, None)
                if pop is not None:
                    try:
                        pop.dismiss()
                    except Exception:
                        pass
            reject = make_reject(session_id, self.chat_local_peer_id, sender, reason)
            self._send_screen_control_to_peer(sender, reject)
            if update_status:
                status_text = self._screen_rejected_local_text()
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=session_id,
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=self._screen_peer_label(sender),
                    status=status_text,
                    detail=str(reason or ""),
                )
            self._clear_current_screen_context()
            self._set_screen_share_ui_state("idle")
            if update_status:
                self._set_screen_share_status(self._screen_rejected_local_text())
        except Exception as exc:
            self._clear_current_screen_context()
            self._set_screen_share_status(self._screen_stop_failed_text(str(exc)))
            self._set_screen_share_ui_state("idle")

    def _handle_screen_accept(self, control: Dict[str, object]) -> None:
        sender = str(control.get("sender_peer_id") or "")
        payload = dict(control.get("payload") or {})
        audio_field_present = isinstance(payload.get("audio", control.get("audio")), dict)
        audio_config = self._screen_audio_from_control(control, payload)
        if not audio_field_present and not bool(audio_config.get("enabled")):
            audio_config = dict(getattr(self, "screen_share_current_audio", {}) or self._screen_audio_config())
        backend = self._screen_backend_from_control(control, payload, getattr(self, "screen_share_current_backend", SCREEN_BACKEND_FFMPEG))
        audio_config = self._screen_audio_for_backend(backend, audio_config)
        try:
            host = str(payload.get("host") or "").strip()
            if not host or is_unspecified_ip(host):
                host, _port = self._endpoint_for_peer(sender)
            if not host or is_unspecified_ip(host):
                failed_text = self._screen_start_failed_text("missing receiver IP")
                self._set_screen_share_status(failed_text)
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=str(control.get("session_id") or ""),
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=self._screen_peer_label(sender),
                    status=failed_text,
                    detail="Cannot start sender without receiver IP",
                )
                self.screen_share_session_id = ""
                self.screen_share_peer_id = ""
                self._clear_current_screen_context()
                self._set_screen_share_ui_state("idle")
                return
            screen_port = self._screen_port_from_accept(control, payload)
            selected_profile = self._selected_profile_from_accept(control, payload)
            peer_label = self._screen_peer_label(sender, host)
            self.screen_share_session_id = str(control.get("session_id") or "")
            self.screen_share_peer_id = sender
            self._set_current_screen_context(peer_label=peer_label, profile=selected_profile, port=screen_port, audio=audio_config, backend=backend)
            starting_text = self._screen_share_status_text("pending_accept", peer_label=peer_label, profile=selected_profile, port=screen_port)
            self._set_screen_share_status(starting_text)
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=str(control.get("session_id") or ""),
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=peer_label,
                status=starting_text,
                detail=self._screen_detail_text(selected_profile, screen_port, audio_config) + f"  backend: {self._screen_backend_label(backend)}",
                profile=selected_profile,
                port=screen_port,
            )
            state = self._screen_runtime().start_sender(
                host=host,
                port=screen_port,
                profile=selected_profile,
                peer_label=peer_label,
                selected_profile=selected_profile,
                screen_port=screen_port,
                system_audio=bool(audio_config.get("enabled")),
                audio=audio_config,
                backend=backend,
            )
            if str(state.get("state") or "") != "sending":
                reason = self._screen_runtime_error_detail(self._screen_runtime().get_state())
                self.screen_share_session_id = ""
                self.screen_share_peer_id = ""
                self._clear_current_screen_context()
                self._set_screen_share_ui_state("idle")
                failed_text = self._screen_start_failed_text(reason)
                self._set_screen_share_status(failed_text)
                self._add_screen_chat_card(
                    CARD_SCREEN_STATE,
                    session_id=str(control.get("session_id") or ""),
                    peer_id=sender,
                    title=self._screen_offer_title(),
                    subtitle=peer_label,
                    status=failed_text,
                    detail=self._screen_detail_text(selected_profile, screen_port, audio_config),
                    profile=selected_profile,
                    port=screen_port,
                )
                return
            runtime_audio = self._screen_audio_from_runtime_state(state, audio_config)
            self._set_current_screen_context(peer_label=peer_label, profile=selected_profile, port=screen_port, audio=runtime_audio, backend=backend)
            self._append_debug_line(f"sender actually used backend/profile: {backend}/{selected_profile}", protocol=False)
            self._set_screen_share_ui_state("sending")
            status_text = self._screen_share_status_text("sending", peer_label=peer_label, profile=selected_profile, port=screen_port)
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=str(control.get("session_id") or ""),
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=peer_label,
                status=status_text,
                detail=self._screen_detail_text(selected_profile, screen_port, runtime_audio),
                profile=selected_profile,
                port=screen_port,
                actions=[{"label": "Stop", "action": "stop_screen", "style": "danger"}],
            )
            self._schedule_screen_audio_fallback_ui_update(
                session_id=str(control.get("session_id") or ""),
                peer_id=sender,
                peer_label=peer_label,
                profile=selected_profile,
                port=screen_port,
                offered_audio=audio_config,
            )
            self._set_screen_share_status(status_text)
        except Exception as exc:
            self._clear_current_screen_context()
            failed_text = self._screen_start_failed_text(str(exc))
            self._set_screen_share_status(failed_text)
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=str(control.get("session_id") or ""),
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=self._screen_peer_label(sender),
                status=failed_text,
                detail=str(exc),
            )
            self._set_screen_share_ui_state("idle")

    def _handle_screen_stop(self, control: Dict[str, object]) -> None:
        sender = str(control.get("sender_peer_id") or "")
        peer_label = self._current_screen_peer_label() or self._screen_peer_label(sender)
        profile_name = self._current_screen_profile_name()
        port_text = self._current_screen_port_text()
        audio_detail = dict(getattr(self, "screen_share_current_audio", {}) or {"enabled": False, "mode": "none"})

        def _finish_stop(_state: Dict[str, object], error: str) -> None:
            self.screen_share_session_id = ""
            self.screen_share_peer_id = ""
            stopped_text = self._screen_stop_failed_text(error) if error else self._screen_stopped_text()
            self._add_screen_chat_card(
                CARD_SCREEN_STATE,
                session_id=str(control.get("session_id") or ""),
                peer_id=sender,
                title=self._screen_offer_title(),
                subtitle=peer_label,
                status=stopped_text,
                detail=self._screen_detail_text(profile_name, port_text, audio_detail),
                profile=profile_name,
                port=port_text,
            )
            self._clear_current_screen_context()
            self._set_screen_share_status(stopped_text)
            self._set_screen_share_ui_state("idle")

        self._stop_screen_runtime_nonblocking(on_done=_finish_stop)

    def open_debug_popup(self) -> None:
        content = BoxLayout(orientation="vertical", spacing=dp(12), padding=dp(14))
        header = BoxLayout(orientation="vertical", size_hint_y=None, height=dp(54), spacing=dp(2))
        title = make_label(
            text="诊断",
            size_hint_y=None,
            height=dp(28),
            halign="left",
            valign="middle",
            bold=True,
            color=ui_component_color("text_primary") if ui_component_color is not None else THEME["text"],
        )
        intro = make_label(
            text="状态摘要优先显示；原始日志和路径信息放在下方详细区域。",
            size_hint_y=None,
            height=dp(24),
            halign="left",
            valign="middle",
            color=THEME["muted_text"],
            shorten=True,
        )
        bind_label_wrap(title)
        bind_label_wrap(intro)
        header.add_widget(title)
        header.add_widget(intro)
        content.add_widget(header)

        scroll = ScrollView(size_hint_y=1)
        body = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(10), padding=(0, 0, 0, dp(6)))
        body.bind(minimum_height=body.setter("height"))
        scroll.add_widget(body)

        summary_grid = GridLayout(cols=2, size_hint_y=None, spacing=dp(10))
        summary_grid.bind(minimum_height=summary_grid.setter("height"))
        summary_grid.height = dp(250)

        def _populate_summary() -> None:
            summary_grid.clear_widgets()
            snapshot = self._diagnostic_snapshot()
            screen_port_title = "投屏端口 55000-55999" if self._screen_backend() == SCREEN_BACKEND_RUST else "投屏端口 50020-50025"
            cards = [
                ("Package",) + self._diagnostic_package_summary(snapshot),
                ("接收端状态",) + self._diagnostic_receiver_summary(snapshot),
                ("UDP 9999 状态",) + self._diagnostic_udp_9999_summary(snapshot),
                (screen_port_title,) + self._diagnostic_screen_ports_summary(snapshot),
                ("Screen backend dependencies",) + self._diagnostic_dependencies_summary(snapshot),
                ("screen runtime",) + self._diagnostic_runtime_summary(snapshot),
            ]
            recent_errors = str(snapshot.get("recent_errors") or "").strip()
            cards.append(("最近错误摘要", "异常" if recent_errors != "暂无最近错误。" else "正常", recent_errors.splitlines()[-1] if recent_errors else "暂无最近错误。", "danger" if recent_errors != "暂无最近错误。" else "neutral"))
            for title_text, status_text, detail_text, kind in cards:
                summary_grid.add_widget(self._diagnostic_status_card(title_text, status_text, detail_text, kind))
            rows = (len(cards) + 1) // 2
            summary_grid.height = rows * dp(78) + max(0, rows - 1) * dp(10)

        body.add_widget(self._diagnostic_section_title("Summary", "关键状态先看这里；正常状态保持低噪声显示。"))
        body.add_widget(summary_grid)
        _populate_summary()

        screen_target_input = make_input(text="", hint_text="Target receiver IP", multiline=False, size_hint_x=1)
        screen_status_label = make_label(
            text="",
            size_hint_y=None,
            height=dp(116),
            halign="left",
            valign="top",
            color=THEME["muted_text"],
        )
        bind_label_wrap(screen_status_label)

        body.add_widget(self._diagnostic_section_title("Screen", "FFmpeg/UDP 投屏调试入口；视频流仍不经过 RUDP 文件队列。"))
        if UIRoundedCard is not None and ui_component_color is not None:
            screen_card = UIRoundedCard(
                orientation="vertical",
                size_hint_y=None,
                height=dp(218),
                spacing=dp(8),
                padding=(dp(12), dp(10), dp(12), dp(10)),
                radius=16,
                bg_color=ui_component_color("surface"),
                border_color=ui_component_color("border_soft"),
            )
        else:
            screen_card = BoxLayout(orientation="vertical", size_hint_y=None, height=dp(218), spacing=dp(8), padding=dp(10))
            apply_card_background(screen_card, "panel_bg", radius=16)
        screen_target_row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(8))
        screen_target_row.add_widget(make_label(text="Target IP", size_hint_x=None, width=dp(92), halign="right", valign="middle", color=THEME["muted_text"]))
        screen_target_row.add_widget(self._make_modern_input_shell(screen_target_input))
        screen_card.add_widget(screen_target_row)
        screen_buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(36), spacing=dp(8))

        def _refresh_screen_details(*_):
            self._schedule_screen_runtime_status(screen_status_label)
            Clock.schedule_once(lambda _dt: _populate_summary(), 0.05)

        screen_buttons.add_widget(self._make_modern_or_legacy_button("secondary", text="Receive screen", size_hint_x=1, height=34, compact=True, on_release=lambda *_: Clock.schedule_once(lambda _dt: (self._screen_debug_start_receiver(screen_status_label), _populate_summary()), 0)))
        screen_buttons.add_widget(self._make_modern_or_legacy_button("secondary", text="Send screen", size_hint_x=1, height=34, compact=True, on_release=lambda *_: Clock.schedule_once(lambda _dt: (self._screen_debug_start_sender(screen_target_input, screen_status_label), _populate_summary()), 0)))
        screen_buttons.add_widget(self._make_modern_or_legacy_button("danger", text="Stop screen", size_hint_x=1, height=34, compact=True, on_release=lambda *_: Clock.schedule_once(lambda _dt: self._screen_debug_stop(screen_status_label, on_done=_populate_summary), 0)))
        screen_buttons.add_widget(self._make_modern_or_legacy_button("secondary", text="Refresh", size_hint_x=1, height=34, compact=True, on_release=_refresh_screen_details))
        screen_card.add_widget(screen_buttons)
        screen_card.add_widget(screen_status_label)
        self._schedule_screen_runtime_status(screen_status_label)
        body.add_widget(screen_card)

        snapshot = self._diagnostic_snapshot()
        network_detail = self._format_udp_port_diagnostics(dict(snapshot.get("runtime_state") or {}))
        body.add_widget(self._diagnostic_section_title("Network", "端口检测只绑定临时 socket，检测后立即释放。"))
        network_log = LogBox(size_hint_y=None, height=dp(112))
        network_log.append(network_detail)
        body.add_widget(network_log)

        body.add_widget(self._diagnostic_section_title("Logs", "默认只显示最近摘要；完整日志请导出诊断包。"))
        log = LogBox(size_hint_y=None, height=dp(152))
        protocol_text = self._tail_text(list(getattr(self, "debug_protocol_lines", []) or []), max_lines=36, max_chars=6000, fallback="暂无协议日志。")
        runtime_text = self._tail_text(list(getattr(self, "debug_runtime_lines", []) or []), max_lines=28, max_chars=5000, fallback="暂无运行日志。")
        error_text = self._recent_error_summary(max_lines=6)
        log.append(
            "最近错误摘要：\n"
            + error_text
            + "\n\n=== Protocol tail ===\n"
            + protocol_text
            + "\n\n=== Runtime tail ===\n"
            + runtime_text
            + "\n"
        )
        body.add_widget(log)
        content.add_widget(scroll)

        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(8))
        popup = style_popup(Popup(title="诊断", content=content, size_hint=(0.82, 0.84)))

        export_btn = self._make_modern_or_legacy_button("primary", text="导出诊断包", size_hint_x=1, height=38)
        export_btn.bind(on_release=lambda *_: self.export_diagnostic_logs_async(log_box=log, button=export_btn))
        buttons.add_widget(export_btn)
        buttons.add_widget(self._make_modern_or_legacy_button("secondary", text="UI Preview", width=112, height=38, on_release=lambda *_: self.open_ui_preview()))
        buttons.add_widget(self._make_modern_or_legacy_button("secondary", text="关闭", width=88, height=38, on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def apply_theme_mode(self, mode: str) -> None:
        self.theme_mode = str(mode or "跟随系统")
        if self.theme_mode == "深色":
            Window.clearcolor = (0.08, 0.09, 0.11, 1)
        else:
            Window.clearcolor = THEME["window_bg"]

    def _receiver_endpoint(self, rec: Dict[str, object]) -> tuple[str, int]:
        ip = str(rec.get("endpoint_ip") or rec.get("ip") or "").strip()
        if not ip:
            ips = rec.get("ips") or []
            if isinstance(ips, (list, tuple)) and ips:
                ip = str(ips[0] or "").strip()
        try:
            port = int(rec.get("endpoint_port") or rec.get("port") or 9999)
        except Exception:
            port = 9999
        return ip, port

    def on_receiver_selected(self, _spinner, value: str) -> None:
        self.selected_receiver = None
        for rec in self.discovered:
            label = self._receiver_label(rec)
            if label == value:
                self.selected_receiver = rec
                ip, port = self._receiver_endpoint(rec)
                if ip:
                    self.receiver_ip.text = ip
                    self.receiver_port.text = str(port)
                    self.sender_log_box.append(f"Selected receiver: {ip}:{port}\n")
                return

    def _receiver_label(self, rec: Dict[str, object]) -> str:
        name = str(rec.get("name") or rec.get("hostname") or "receiver")
        ip, port = self._receiver_endpoint(rec)
        return f"{name}  {ip}:{port}"

    def _refresh_known_contact_endpoints(self, found: List[Dict[str, object]]) -> None:
        if self.contact_service is None:
            return
        self.contact_service.update_known_endpoints(
            found or [],
            endpoint_getter=self._receiver_endpoint,
            peer_id_getter=self._device_peer_id,
            fingerprint_getter=self._device_fingerprint,
            nickname_getter=self._device_display_name,
            invalid_ip_checker=is_unspecified_ip,
        )


    def search_receivers(self) -> None:
        """Search LAN receivers without blocking the Kivy UI thread."""
        if getattr(self, "search_in_progress", False):
            return

        self.search_in_progress = True
        self.search_btn.disabled = True
        self.search_btn.text = self.t("search_working")
        self.sender_log_box.append(self.t("searching") + "\n")

        def _finish(found=None, error: Optional[str] = None):
            found = found or []
            local_ips = set(get_local_ip_candidates())
            my_ids = {str(getattr(self, "chat_local_peer_id", "") or ""), str(getattr(self, "chat_fingerprint", "") or "")}
            cleaned = []
            seen_ids = set()
            for item in found:
                if not isinstance(item, dict):
                    continue
                endpoint_ip = str(item.get("endpoint_ip") or item.get("ip") or "")
                pid = str(item.get("peer_id") or "")
                fp = str(item.get("fingerprint") or item.get("identity_fingerprint") or "")
                if (pid and pid in my_ids) or (fp and fp in my_ids):
                    continue
                # Hide obvious self-responses from local virtual/network adapters.
                if endpoint_ip in local_ips and (pid or fp):
                    continue
                key = fp or pid or f"{endpoint_ip}:{item.get('port') or item.get('endpoint_port') or 9999}"
                if key in seen_ids:
                    continue
                seen_ids.add(key)
                cleaned.append(item)
            found = cleaned
            ts_now = time.time()
            for item in found:
                if isinstance(item, dict):
                    item.setdefault("last_seen", ts_now)
            self.discovered = found
            try:
                self._refresh_known_contact_endpoints(found)
            except Exception:
                pass
            self.selected_receiver = None
            values = [self._receiver_label(x) for x in found]
            self.receiver_spinner.values = values
            self.receiver_spinner.text = values[0] if values else self.t("no_receiver")
            if found:
                self.on_receiver_selected(self.receiver_spinner, values[0])
            if error:
                self.sender_log_box.append(f"Discovery failed: {error}\n")
            else:
                self.sender_log_box.append(self.t("search_done", n=len(found)) + "\n")
                if not found:
                    self.sender_log_box.append(self.t("search_none") + "\n")
            self.search_in_progress = False
            self.search_btn.disabled = False
            self.search_btn.text = self.t("search")
            try:
                self.refresh_chat_main()
            except Exception:
                pass
            style_button(self.search_btn, "primary")

        def _run():
            try:
                discovery_port = int(self.discovery_port.text or DEFAULT_DISCOVERY_PORT)
                transfer_port = int(self.receiver_port.text or 9999)
                manual_ip = self.receiver_ip.text.strip()
                found = discover_receivers(
                    discovery_port=discovery_port,
                    timeout=20.0,
                    extra_ports=[transfer_port],
                    manual_targets=[manual_ip] if manual_ip else None,
                    max_probe_hosts=2048,
                )
            except Exception as exc:
                Clock.schedule_once(lambda _dt, msg=str(exc): _finish([], msg), 0)
                return
            Clock.schedule_once(lambda _dt, result=found: _finish(result, None), 0)

        threading.Thread(target=_run, daemon=True).start()


    def unlock_chat_db(self) -> None:
        try:
            from chat_store import ChatStore
            from transfer_store import TransferStore
            self.chat_db_path = self.chat_db_input.text.strip() or str(user_data_dir() / "chat" / "agoralink_chat.db")
            self.chat_password = self.chat_password_input.text
            self.chat_local_peer_id = self.chat_local_peer_input.text.strip() or "local"
            if not self.chat_password:
                self.chat_messages_box.append(self.t("chat_locked") + "\n")
                return
            if self.chat_store is not None:
                try:
                    self.chat_store.close()
                except Exception:
                    pass
            self.chat_store = ChatStore(self.chat_db_path, self.chat_password, my_peer_id=self.chat_local_peer_id)
            self.transfer_store = TransferStore(self.chat_db_path)
            self._refresh_services()
            self.chat_messages_box.append(self.t("chat_unlocked") + f" {self.chat_db_path}\n")
            self.refresh_chat_view()
        except Exception as exc:
            self.chat_messages_box.append(f"Chat unlock failed: {exc}\n")

    def _require_chat_store(self):
        if self.chat_store is None:
            self.chat_messages_box.append(self.t("chat_locked") + "\n")
            return None
        return self.chat_store

    def create_chat_group(self) -> None:
        if self.group_service is None:
            return
        gid = self.chat_group_id_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        title = self.chat_group_title_input.text.strip() or gid
        self.group_service.create_group(gid, title)
        self.chat_messages_box.append(f"Group saved: {gid}\n")
        self.refresh_chat_view()


    def add_chat_member(self) -> None:
        if self.group_service is None:
            return
        gid = self.chat_group_id_input.text.strip()
        pid = self.member_peer_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        if not pid:
            self.chat_messages_box.append(self.t("need_member") + "\n")
            return
        if not self._is_local_group_owner(gid):
            self.chat_messages_box.append("只有群主可以添加成员。\n")
            return
        try:
            port = int(self.member_port_input.text.strip() or "9999")
        except Exception:
            port = 9999
        self.group_service.add_member_manual(
            gid, pid,
            peer_ip=self.member_ip_input.text.strip(),
            peer_port=port,
            display_name=pid,
        )
        self.chat_messages_box.append(f"Member saved: {pid}\n")
        self.refresh_chat_view()


    def remove_chat_member(self) -> None:
        if self.group_service is None:
            return
        gid = self.chat_group_id_input.text.strip()
        pid = self.member_peer_input.text.strip()
        if not gid or not pid:
            self.chat_messages_box.append(self.t("need_member") + "\n")
            return
        if not self._is_local_group_owner(gid):
            self.chat_messages_box.append("只有群主可以移除成员。\n")
            return
        self.group_service.remove_member(gid, pid, removed=True)
        self.chat_messages_box.append(f"Member removed: {pid}\n")
        self.refresh_chat_view()


    def leave_chat_group(self) -> None:
        if self.group_service is None:
            return
        gid = self.chat_group_id_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        self.group_service.leave_group_local(gid, self.chat_local_peer_id)
        self.chat_messages_box.append(f"Left group: {gid}\n")
        self.refresh_chat_view()


    def refresh_chat_view(self) -> None:
        if self.group_service is None or self.message_service is None:
            return
        gid = self.chat_group_id_input.text.strip()
        self.chat_members_box.clear()
        self.chat_messages_box.clear()
        try:
            groups = self.group_service.list_groups()
            self.chat_members_box.append("Groups:\n")
            for g in groups:
                self.chat_members_box.append(f"  {g.get('group_id')}  {g.get('title')}  {g.get('group_state')}\n")
            if gid:
                self.chat_members_box.append("\nMembers:\n")
                for m in self.group_service.members(gid, include_inactive=True):
                    self.chat_members_box.append(f"  {m.get('peer_id')}  {m.get('peer_ip')}:{m.get('peer_port')}  {m.get('member_state')}\n")
                self.chat_messages_box.append(f"Messages for {gid}:\n")
                for msg in self.message_service.list_messages(group_id=gid, limit=100):
                    summary = self.message_service.receipt_summary(str(msg.get('message_id') or ''))
                    self.chat_messages_box.append(f"[{msg.get('status')}] {msg.get('sender_peer_id')}: {msg.get('text')}  {summary}\n")
        except Exception as exc:
            self.chat_messages_box.append(f"Chat refresh failed: {exc}\n")



    def send_group_message_gui(self) -> None:
        if self.message_service is None:
            self.chat_messages_box.append(self.t("chat_locked") + "\n")
            return
        gid = self.chat_group_id_input.text.strip()
        text = self.group_message_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        if not text:
            self.chat_messages_box.append(self.t("need_chat_message") + "\n")
            return
        try:
            msg, recipients = self.message_service.create_group_text(gid, text)
        except Exception as exc:
            self.chat_messages_box.append(f"Failed to create message: {exc}\n")
            return
        self.group_message_input.text = ""
        self.chat_messages_box.append(f"Sending group message {msg['message_id']} to {len(recipients)} member(s)...\n")
        self.refresh_chat_view()

        def _run():
            for member in recipients:
                peer_id = str(member.get("peer_id") or "")
                ip = str(member.get("peer_ip") or "")
                try:
                    port = int(member.get("peer_port") or 9999)
                except Exception:
                    port = 9999
                if not ip:
                    self.message_service.mark_failed(str(msg["message_id"]), peer_id, error="missing_member_ip")
                    Clock.schedule_once(lambda _dt, pid=peer_id: self.chat_messages_box.append(f"Failed {pid}: missing IP\n"), 0)
                    continue
                pin_file = str(receiver_pin_file(ip, port))
                args = [
                    "--worker", "sender",
                    "--server-ip", ip,
                    "--server-port", str(port),
                    "--chat-message", str(msg["text"]),
                    "--chat-group-id", str(msg["group_id"]),
                    "--chat-sender-peer-id", self.chat_local_peer_id,
                    "--chat-receiver-peer-id", peer_id,
                    "--chat-message-id", str(msg["message_id"]),
                    "--chat-created-at", str(float(msg["created_at"])),
                    "--chat-db", self.chat_db_path,
                    "--chat-password", self.chat_password,
                    "--server-pin-file", pin_file,
                    "--complete-timeout", "20",
                    "--final-ack-timeout", "20",
                ]
                try:
                    cmd = ([sys.executable] + args) if FROZEN else ([sys.executable, str(Path(__file__).resolve())] + args)
                    proc = run_no_console(cmd, cwd=str(APP_DIR), capture_output=True, text=True, timeout=60)
                    if proc.returncode == 0:
                        self.message_service.mark_delivered(str(msg["message_id"]), peer_id)
                        Clock.schedule_once(lambda _dt, pid=peer_id: self.chat_messages_box.append(f"Delivered to {pid}\n"), 0)
                    else:
                        err = (proc.stderr or proc.stdout or "send_failed")[-500:]
                        self.message_service.mark_failed(str(msg["message_id"]), peer_id, error=err)
                        Clock.schedule_once(lambda _dt, pid=peer_id, e=err: self.chat_messages_box.append(f"Failed {pid}: {e}\n"), 0)
                except Exception as exc:
                    self.message_service.mark_failed(str(msg["message_id"]), peer_id, error=str(exc))
                    Clock.schedule_once(lambda _dt, pid=peer_id, e=str(exc): self.chat_messages_box.append(f"Failed {pid}: {e}\n"), 0)
            Clock.schedule_once(lambda _dt: self.refresh_chat_view(), 0)
        threading.Thread(target=_run, daemon=True).start()


    def _device_display_name(self, rec: Dict[str, object]) -> str:
        return str(rec.get("nickname") or rec.get("receiver_name") or rec.get("name") or rec.get("hostname") or "Unknown")

    def _device_fingerprint(self, rec: Dict[str, object]) -> str:
        return str(rec.get("fingerprint") or rec.get("identity_fingerprint") or rec.get("peer_id") or "")

    def _device_peer_id(self, rec: Dict[str, object]) -> str:
        return str(rec.get("peer_id") or self._device_fingerprint(rec) or "")

    def _chat_device_value(self, rec: Dict[str, object]) -> str:
        ip, port = self._receiver_endpoint(rec)
        name = self._device_display_name(rec)
        fp = self._device_fingerprint(rec)
        pid = self._device_peer_id(rec)
        return f"{name} | {ip}:{port} | {self._short_fp(fp)} | {pid}"

    def _find_discovered_by_chat_value(self, value: str) -> Optional[Dict[str, object]]:
        text = str(value or "")
        for rec in self.discovered:
            if self._chat_device_value(rec) == text:
                return rec
        return None

    def _handle_contact_response(self, obj: Dict[str, object]) -> None:
        req_id = str(obj.get("request_id") or "")
        accepted = bool(obj.get("accepted"))
        pending = self.pending_outgoing_contact_requests.pop(req_id, {}) if req_id else {}
        if not req_id:
            self.sender_log_box.append("Contact response missing request_id.\n")
            return
        if not accepted:
            self.sender_log_box.append(f"Contact request rejected: {obj.get('reason') or 'rejected'}\n")
            return
        if self.contact_service is None:
            self.sender_log_box.append("Contact accepted, but chat database is not unlocked; contact was not saved locally.\n")
            return
        peer_id = str(obj.get("receiver_peer_id") or pending.get("peer_id") or pending.get("fingerprint") or "").strip()
        nickname = str(obj.get("receiver_nickname") or pending.get("nickname") or pending.get("name") or peer_id).strip()
        fp = str(obj.get("receiver_fingerprint") or pending.get("fingerprint") or peer_id).strip()
        ip = normalize_peer_endpoint_ip(str(pending.get("ip") or ""), fallback=str(obj.get("receiver_ip") or ""))
        try:
            port = int(obj.get("receiver_port") or pending.get("port") or 9999)
        except Exception:
            port = 9999
        if not peer_id:
            self.sender_log_box.append("Contact accepted, but peer_id is empty; contact was not saved.\n")
            return
        try:
            self.contact_service.save_accepted_contact(
                peer_id=peer_id,
                nickname=nickname or peer_id,
                fingerprint=fp or peer_id,
                peer_ip=ip,
                peer_port=port,
                remark_name=str(pending.get("remark_name") or ""),
            )
            self.sender_log_box.append(f"Contact saved: {nickname or peer_id} {ip}:{port}\n")
            self.refresh_chat_main()
        except Exception as exc:
            self.sender_log_box.append(f"Failed to save accepted contact: {exc}\n")


    def request_or_add_selected_device(self) -> None:
        value = str(getattr(self.chat_list_spinner, "text", "") or "")
        rec = self._find_discovered_by_chat_value(value)
        if rec is not None:
            name = self._device_display_name(rec)
            ip, port = self._receiver_endpoint(rec)
            peer_id = self._device_peer_id(rec)
            fingerprint = self._device_fingerprint(rec)
        else:
            if not value or "|" not in value:
                self.chat_list_box.append("请先在在线设备列表中选择设备。\n")
                return
            parts = [x.strip() for x in value.split("|")]
            name = parts[0] if parts else "Unknown"
            endpoint = parts[1] if len(parts) > 1 else ""
            fingerprint = parts[2] if len(parts) > 2 else ""
            peer_id = parts[3] if len(parts) > 3 else fingerprint
            if ":" not in endpoint:
                self.chat_list_box.append("无法解析设备 IP:port。\n")
                return
            ip, port_text = endpoint.rsplit(":", 1)
            try:
                port = int(port_text)
            except Exception:
                port = 9999
        if is_unspecified_ip(ip):
            self.chat_list_box.append("设备地址无效，不能使用 0.0.0.0；请重新扫描在线设备。\n")
            return
        if self.contact_service is None:
            self.chat_list_box.append("请先解锁聊天数据库。\n")
            return
        # Send a real contact request. B side will pop up allow/reject.
        req_id = "contact_req_" + secrets.token_hex(12)
        pin_file = str(receiver_pin_file(ip, port))
        self.pending_outgoing_contact_requests[req_id] = {
            "name": name,
            "nickname": name,
            "peer_id": peer_id,
            "fingerprint": fingerprint or peer_id,
            "ip": ip,
            "port": port,
        }
        args = [
            "--server-ip", ip,
            "--server-port", str(port),
            "--contact-request",
            "--contact-request-id", req_id,
            "--contact-sender-peer-id", self.chat_local_peer_id,
            "--contact-sender-nickname", self.chat_nickname,
            "--contact-sender-fingerprint", self.chat_local_peer_id,
            "--contact-message", "Request to add contact",
            "--server-pin-file", pin_file,
            "--request-timeout", "300",
        ]
        self.sender_log_box.append(f"Sending contact request to {name} {ip}:{port}\n")
        self.contact_worker.start(args)

    def open_group_popup(self) -> None:
        if self.group_service is None:
            self.main_messages_box.append("请先解锁聊天数据库。\n")
            return
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        gid_input = make_input(text="group_" + secrets.token_hex(4), multiline=False, size_hint_y=None, height=dp(38))
        title_input = make_input(text="New Group", multiline=False, size_hint_y=None, height=dp(38))
        content.add_widget(make_label(text="Group ID", size_hint_y=None, height=dp(24), halign="left"))
        content.add_widget(gid_input)
        content.add_widget(make_label(text="Title", size_hint_y=None, height=dp(24), halign="left"))
        content.add_widget(title_input)
        popup = style_popup(Popup(title=self.cu("new_group"), content=content, size_hint=(0.65, 0.45), auto_dismiss=False))
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        def _ok(*_):
            try:
                gid = gid_input.text.strip()
                title = title_input.text.strip() or gid
                self.group_service.create_group(gid, title)
                self.current_chat_mode = "group"
                self.current_group_id = gid
                self.current_peer_id = ""
                self.current_chat_title.text = title
            except Exception as exc:
                self.main_messages_box.append(f"group create failed: {exc}\n")
            popup.dismiss()
            self.refresh_chat_main()
            self.render_current_chat(reason="group_manage")
        buttons.add_widget(make_button("success", text=self.cu("new_group"), on_release=_ok))
        buttons.add_widget(make_button("secondary", text="Cancel", on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()


    def add_selected_contact_to_current_group(self) -> None:
        if self.contact_service is None or self.group_service is None or not self.current_group_id:
            self.main_messages_box.append("请先选择群聊。\n")
            return
        if not self._is_local_group_owner(self.current_group_id):
            self.main_messages_box.append("只有群主可以添加成员。\n")
            return
        contacts = self.contact_service.trusted_contacts()
        if not contacts:
            self.main_messages_box.append("没有已允许的联系人。\n")
            return
        values = [f"{c.get('peer_id')} | {c.get('remark_name') or c.get('display_name') or c.get('peer_id')} | {c.get('peer_ip')}:{c.get('peer_port')}" for c in contacts]
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        sp = style_spinner(Spinner(text=values[0], values=values, font_name=UI_FONT, size_hint_y=None, height=dp(38)))
        content.add_widget(sp)
        popup = style_popup(Popup(title="添加群成员", content=content, size_hint=(0.65, 0.35), auto_dismiss=False))
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        def _ok(*_):
            pid = sp.text.split("|", 1)[0].strip()
            contact = next((c for c in contacts if str(c.get('peer_id')) == pid), None)
            if contact:
                self.group_service.add_member_from_contact(self.current_group_id, contact)
            popup.dismiss()
            self.render_current_chat(reason="group_manage")
        buttons.add_widget(make_button("success", text="加入", on_release=_ok))
        buttons.add_widget(make_button("secondary", text="取消", on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()


    def _confirm_action(self, title: str, msg: str, action) -> None:
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        lab = make_label(text=msg, halign="left", valign="middle")
        bind_label_wrap(lab)
        content.add_widget(lab)
        popup = style_popup(Popup(title=title, content=content, size_hint=(0.52, 0.35), auto_dismiss=False))
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        def _ok(*_):
            popup.dismiss()
            action()
        buttons.add_widget(make_button("danger", text="确认", on_release=_ok))
        buttons.add_widget(make_button("secondary", text="取消", on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def confirm_remove_member(self) -> None:
        if self.group_service is None or not self.current_group_id:
            return
        if not self._is_local_group_owner(self.current_group_id):
            self.right_info_box.append("只有群主可以移除成员。\n")
            return
        members = [
            m for m in self.group_service.members(self.current_group_id, include_inactive=False)
            if str(m.get('peer_id')) != self.chat_local_peer_id and str(m.get("role") or "member") != "owner"
        ]
        if not members:
            self.right_info_box.append("没有可移除的成员。\n")
            return
        pid = str(getattr(self, "selected_group_member_peer_id", "") or "")
        allowed = {str(m.get("peer_id") or "") for m in members}
        if pid not in allowed:
            self.right_info_box.append("请先在右侧选择一个可移除的成员。\n")
            return
        self._confirm_action("移除成员", f"确认移除成员 {pid}？", lambda: (self.group_service.remove_member(self.current_group_id, pid, removed=True), self.render_current_chat(reason="group_manage")))


    def confirm_leave_group(self) -> None:
        if self.group_service is None or not self.current_group_id:
            return
        gid = self.current_group_id
        def _do():
            self.group_service.delete_group_data(gid)
            self.current_group_id = ""
            self.current_chat_mode = ""
            self.current_chat_title.text = ""
            self.refresh_chat_main()
            self.render_current_chat(reason="group_manage")
        self._confirm_action("退出群组", f"确认退出群组 {gid}？\n该群相关成员、消息、回执都会从本机删除。", _do)


    def confirm_delete_contact(self) -> None:
        if self.contact_service is None or self.current_chat_mode != "direct" or not self.current_peer_id:
            self.right_info_box.append("请先选择要删除的联系人。\n")
            return
        pid = self.current_peer_id
        def _do():
            self.contact_service.delete_contact_local(pid)
            self.current_peer_id = ""
            self.current_chat_mode = ""
            self.current_chat_title.text = ""
            self.refresh_chat_main()
            self.render_current_chat(reason="contact_manage")
        self._confirm_action("删除联系人", f"确认删除联系人 {pid}？\n该联系人、一对一聊天记录和相关回执都会从本机删除。", _do)


    def _send_chat_to_endpoint(self, *, ip: str, port: int, peer_id: str, text: str, message_id: str, group_id: str = "", conversation_id: str = "", created_at: float = 0.0, body_type: str = "text") -> bool:
        pin_file = str(receiver_pin_file(ip, port))
        args = [
            "--worker", "sender",
            "--server-ip", ip,
            "--server-port", str(port),
            "--chat-message", text,
            "--chat-group-id", group_id,
            "--chat-conversation-id", conversation_id,
            "--chat-sender-peer-id", self.chat_local_peer_id,
            "--chat-receiver-peer-id", peer_id,
            "--chat-message-id", message_id,
            "--chat-created-at", str(float(created_at or time.time())),
            "--chat-body-type", body_type,
            "--chat-db", self.chat_db_path,
            "--chat-password", self.chat_password,
            "--server-pin-file", pin_file,
            "--complete-timeout", "20",
            "--final-ack-timeout", "20",
        ]
        cmd = ([sys.executable] + args) if FROZEN else ([sys.executable, str(Path(__file__).resolve())] + args)
        proc = run_no_console(cmd, cwd=str(APP_DIR), capture_output=True, text=True, timeout=60)
        combined = (proc.stdout or "") + ("\n" + proc.stderr if proc.stderr else "")
        if combined.strip():
            # Keep the subprocess log available in the debug sender log; this is
            # essential for diagnosing CHAT_ACK and pin/handshake issues.
            log_text = combined[-4000:] + ("\n" if not combined.endswith("\n") else "")
            self._append_debug_line(log_text, protocol=True)
            try:
                Clock.schedule_once(lambda _dt, s=log_text: self.sender_log_box.append(s), 0)
            except Exception:
                pass
        ack_seen = (CHAT_ACK_LOG_PREFIX in combined) or ("CHAT_ACK received" in combined)
        return proc.returncode == 0 or ack_seen

    def retry_outgoing_message(self, message_id: str) -> None:
        mid = str(message_id or "").strip()
        if self.message_service is None or not mid:
            return
        try:
            ctx = self.message_service.retry_text_context(mid, current_peer_id=self.current_peer_id)
            if not ctx:
                return
            if ctx.get("error") == "retry_text_only":
                self.sender_log_box.append(self.cu("retry_text_only") + "\n")
                return
            text_body = str(ctx.get("text") or "")
            group_id = str(ctx.get("group_id") or "")
            conv_id = str(ctx.get("conversation_id") or "")
            created_at = float(ctx.get("created_at") or time.time())
            recipients = [dict(r) for r in (ctx.get("recipients") or [])]

            def _mark_delivered(pid: str) -> None:
                try:
                    if self.message_service is not None:
                        self.message_service.mark_delivered(mid, pid)
                except Exception as exc:
                    self._append_debug_line(f"retry mark_delivered failed: {exc}", protocol=True)

            def _mark_failed(pid: str, err: str) -> None:
                try:
                    if self.message_service is not None:
                        self.message_service.mark_failed(mid, pid, error=err)
                except Exception as exc:
                    self._append_debug_line(f"retry mark_failed failed: {exc}; original={err}", protocol=True)

            def _run():
                for r in recipients:
                    pid = str(r.get("peer_id") or "")
                    ip = str(r.get("peer_ip") or "")
                    try:
                        port = int(r.get("peer_port") or 9999)
                    except Exception:
                        port = 9999
                    if not pid or not ip or is_unspecified_ip(ip):
                        Clock.schedule_once(lambda _dt, peer=pid: _mark_failed(peer, "invalid_endpoint"), 0)
                        continue
                    try:
                        ok = self._send_chat_to_endpoint(ip=ip, port=port, peer_id=pid, text=text_body, message_id=mid, group_id=group_id, conversation_id=conv_id, created_at=created_at, body_type="text")
                    except Exception as exc:
                        ok = False
                        self._append_debug_line(f"retry _send_chat_to_endpoint exception: {exc}", protocol=True)
                    if ok:
                        Clock.schedule_once(lambda _dt, peer=pid: (_mark_delivered(peer), self.refresh_chat_main(), self._sync_chat_render_signature()), 0)
                    else:
                        Clock.schedule_once(lambda _dt, peer=pid: (_mark_failed(peer, "retry_failed"), self.refresh_chat_main(), self._sync_chat_render_signature()), 0)
                Clock.schedule_once(lambda _dt: (self.refresh_chat_main(), self._sync_chat_render_signature()), 0)
            threading.Thread(target=_run, daemon=True).start()
        except Exception as exc:
            try:
                self.sender_log_box.append(f"retry failed: {exc}\n")
            except Exception:
                pass


    def send_current_chat_message(self) -> None:
        if self.message_service is None:
            self.main_messages_box.append("请先解锁聊天数据库。\n")
            return
        text = self.main_message_input.text.strip()
        if not text:
            return
        self.main_message_input.text = ""
        try:
            if self.current_chat_mode == "direct" and self.current_peer_id:
                msg, contact = self.message_service.create_direct_text(self.current_peer_id, text)
                recipients = [contact]
                group_id = ""
                conversation_id = str(msg.get('conversation_id') or '')
            elif self.current_chat_mode == "group" and self.current_group_id:
                if not self._is_local_active_group_member(self.current_group_id):
                    self.main_messages_box.append("你已不在此群，不能发送消息。\n")
                    return
                msg, recipients = self.message_service.create_group_text(self.current_group_id, text)
                group_id = self.current_group_id
                conversation_id = ""
            else:
                self.main_messages_box.append("请先选择联系人或群聊。\n")
                return
        except Exception as exc:
            self.main_messages_box.append(f"消息创建失败: {exc}\n")
            return

        mid = str(msg.get("message_id") or "")
        created_at = float(msg.get("created_at") or time.time())
        live_msg = dict(msg)
        live_msg.setdefault("text", text)
        live_msg.setdefault("body_type", "text")
        live_msg.setdefault("created_at", created_at)
        live_msg.setdefault("group_id", group_id)
        live_msg.setdefault("conversation_id", conversation_id)
        live_msg.setdefault("sender_peer_id", self.chat_local_peer_id)
        self._append_text_message_live(live_msg)

        def _mark_sent(pid: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_sent(mid, pid)
            except Exception as exc:
                self._append_debug_line(f"mark_sent failed: {exc}", protocol=True)

        def _mark_delivered(pid: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_delivered(mid, pid)
            except Exception as exc:
                self._append_debug_line(f"mark_delivered failed: {exc}", protocol=True)

        def _mark_failed(pid: str, err: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_failed(mid, pid, error=err)
            except Exception as exc:
                self._append_debug_line(f"mark_failed failed: {exc}; original={err}", protocol=True)

        def _run():
            for r in recipients:
                peer_id = str(r.get('peer_id') or '')
                ip = str(r.get('peer_ip') or '')
                try:
                    port = int(r.get('peer_port') or 9999)
                except Exception:
                    port = 9999
                if not peer_id or not ip or is_unspecified_ip(ip):
                    Clock.schedule_once(lambda _dt, pid=peer_id: _mark_failed(pid, "invalid_endpoint"), 0)
                    continue

                # Important: SQLite/Kivy operations must stay on the UI thread.
                # The background thread only performs the blocking subprocess send.
                Clock.schedule_once(lambda _dt, pid=peer_id: _mark_sent(pid), 0)
                try:
                    ok = self._send_chat_to_endpoint(
                        ip=ip,
                        port=port,
                        peer_id=peer_id,
                        text=text,
                        message_id=mid,
                        group_id=group_id,
                        conversation_id=conversation_id,
                        created_at=created_at,
                        body_type="text",
                    )
                except Exception as exc:
                    ok = False
                    self._append_debug_line(f"_send_chat_to_endpoint exception: {exc}", protocol=True)
                if ok:
                    Clock.schedule_once(lambda _dt, pid=peer_id: _mark_delivered(pid), 0)
                else:
                    Clock.schedule_once(lambda _dt, pid=peer_id: _mark_failed(pid, "send_failed"), 0)
        threading.Thread(target=_run, daemon=True).start()


    def _run_file_sender_with_progress(self, *, args: List[str], message_id: str, peer_id: str, total_size: int = 0) -> bool:
        cmd = ([sys.executable] + args) if FROZEN else ([sys.executable, str(Path(__file__).resolve())] + args)
        mid = str(message_id or "")
        try:
            if mid:
                self.file_message_progress[mid] = {
                    "sent": 0,
                    "total": int(total_size or 0),
                    "pct": 0.0,
                    "state": self.cu("sending_to", peer=peer_id),
                }
                # Do not write SQLite here from the sender worker thread. The outgoing
                # task was already created before transfer start; progress persistence is
                # handled by TRANSFER_*_JSON on the UI thread.
                self._schedule_transfer_card_refresh(force=True)

            try:
                self.sender_log_box.append(f"Starting file transfer worker: message_id={mid}, peer={peer_id}, size={int(total_size or 0)}\n")
            except Exception:
                pass

            proc = popen_no_console(
                cmd,
                cwd=str(APP_DIR),
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
            assert proc.stdout is not None
            for line in proc.stdout:
                is_progress = self._is_transfer_progress_line(line)
                if not is_progress:
                    try:
                        self.sender_log_box.append(line)
                    except Exception:
                        pass
                self._append_debug_line(line, protocol=any(marker in str(line or "") for marker in (
                    TRANSFER_STARTED_LOG_PREFIX,
                    TRANSFER_PROGRESS_LOG_PREFIX,
                    TRANSFER_SAVED_LOG_PREFIX,
                    TRANSFER_COMPLETE_LOG_PREFIX,
                    TRANSFER_FAILED_LOG_PREFIX,
                    USER_ERROR_LOG_PREFIX,
                    "LAN_STATS ",
                )))
                self._try_parse_user_event(line, "sender")
                tev = self._parse_transfer_event_line(line)
                if tev and mid:
                    try:
                        Clock.schedule_once(lambda _dt, data=tev: self._handle_transfer_event_ui(data, source="sender"), 0)
                    except Exception:
                        pass
                    continue
                m = PROGRESS_RE.search(line)
                if m and mid:
                    sent = int(m.group("sent"))
                    total = int(m.group("total") or total_size or 0)
                    pct = float(m.group("pct") or ((sent * 100.0 / total) if total else 0.0))
                    self.file_message_progress[mid] = {
                        "sent": sent,
                        "total": total,
                        "pct": pct,
                        "avg": float(m.group("avg") or 0.0),
                        "eta": m.group("eta") or "",
                        "state": self.cu("sending_to", peer=peer_id),
                    }
                    self._schedule_file_transfer_chat_card(
                        message_id=mid,
                        peer_id=peer_id,
                        direction="outgoing",
                        transferred=sent,
                        total=total,
                        pct=pct,
                        avg=float(m.group("avg") or 0.0),
                        eta=m.group("eta") or "",
                        status=self.cu("sending"),
                    )
                    self._schedule_transfer_card_refresh(force=False)

            rc = proc.wait(timeout=5)
            if mid:
                final_total = int(total_size or self.file_message_progress.get(mid, {}).get("total") or 0)
                self.file_message_progress[mid] = {
                    "sent": final_total if rc == 0 else int(self.file_message_progress.get(mid, {}).get("sent") or 0),
                    "total": final_total,
                    "pct": 100.0 if rc == 0 else float(self.file_message_progress.get(mid, {}).get("pct") or 0.0),
                    "avg": self.file_message_progress.get(mid, {}).get("avg", 0.0),
                    "eta": "0:00" if rc == 0 else self.file_message_progress.get(mid, {}).get("eta", ""),
                    "state": self.cu("completed") if rc == 0 else self.cu("failed"),
                }
                final_sent = final_total if rc == 0 else int(self.file_message_progress.get(mid, {}).get("sent") or 0)
                final_pct = 100.0 if rc == 0 else float(self.file_message_progress.get(mid, {}).get("pct") or 0.0)
                final_avg = float(self.file_message_progress.get(mid, {}).get("avg") or 0.0)
                final_eta = "0:00" if rc == 0 else str(self.file_message_progress.get(mid, {}).get("eta") or "")
                final_status = self.cu("completed") if rc == 0 else self.cu("failed")
                final_error = "" if rc == 0 else (str(self.last_sender_failure_code or "") or "file_send_failed")
                final_status = final_status if rc == 0 else self._file_failed_text(self._file_error_message(final_error, ""))
                self._schedule_file_transfer_chat_card(
                    message_id=mid,
                    peer_id=peer_id,
                    direction="outgoing",
                    transferred=final_sent,
                    total=final_total,
                    pct=final_pct,
                    avg=final_avg,
                    eta=final_eta,
                    status=final_status,
                    error=final_error,
                )
                if self.file_transfer_service is not None:
                    try:
                        self.file_transfer_service.update_progress(
                            chat_message_id=mid,
                            direction="outgoing",
                            peer_id=peer_id,
                            transferred_bytes=final_total if rc == 0 else int(self.file_message_progress.get(mid, {}).get("sent") or 0),
                            total_bytes=final_total,
                            pct=100.0 if rc == 0 else float(self.file_message_progress.get(mid, {}).get("pct") or 0.0),
                            avg_mbps=float(self.file_message_progress.get(mid, {}).get("avg") or 0.0),
                            eta="0:00" if rc == 0 else str(self.file_message_progress.get(mid, {}).get("eta") or ""),
                            status="completed" if rc == 0 else "failed",
                            error="" if rc == 0 else "file_send_failed",
                        )
                    except Exception as exc:
                        self._append_debug_line(f"final transfer_store update failed: {exc}", protocol=True)
                self._schedule_transfer_card_refresh(force=True)
            return int(rc or 0) == 0
        except Exception as exc:
            try:
                self.sender_log_box.append(f"File transfer worker failed before/while running: {exc}\n")
                self._append_debug_line(f"File transfer worker failed: {exc}", protocol=True)
            except Exception:
                pass
            if mid:
                old = self.file_message_progress.get(mid, {})
                old.update({"state": f'{self.cu("failed")}: {exc}'})
                self.file_message_progress[mid] = old
                self._schedule_file_transfer_chat_card(
                    message_id=mid,
                    peer_id=peer_id,
                    direction="outgoing",
                    transferred=int(old.get("sent") or 0),
                    total=int(old.get("total") or total_size or 0),
                    pct=float(old.get("pct") or 0.0),
                    avg=float(old.get("avg") or 0.0),
                    eta=str(old.get("eta") or ""),
                    status=self.cu("failed"),
                    error=str(exc),
                )
                self._schedule_transfer_card_refresh(force=True)
            return False


    def send_file_to_current_chat(self) -> None:
        if self.current_chat_mode not in ("direct", "group"):
            self.main_messages_box.append("请先选择聊天对象。\n")
            return
        self._mixed_file_folder_dialog(callback=self._handle_mixed_selected_paths)

    def _common_picker_shortcuts(self) -> List[Tuple[str, str]]:
        shortcuts: List[Tuple[str, str]] = []
        home = Path.home()
        candidates = [
            ("主页", home),
            ("桌面", home / "Desktop"),
            ("下载", home / "Downloads"),
            ("文档", home / "Documents"),
            ("图片", home / "Pictures"),
        ]
        for label, p in candidates:
            try:
                if p.exists() and p.is_dir():
                    shortcuts.append((label, str(p)))
            except Exception:
                pass
        if IS_WINDOWS:
            try:
                import ctypes
                mask = int(ctypes.windll.kernel32.GetLogicalDrives())
                for i in range(26):
                    if mask & (1 << i):
                        drive = f"{chr(65 + i)}:\\"
                        shortcuts.append((drive, drive))
            except Exception:
                pass
        return shortcuts

    def _mixed_file_folder_dialog(self, callback) -> None:
        """Explorer-like picker using RecycleView.

        It can select files and folders in one window.  The row renderer avoids
        emoji glyphs so CJK file names render with the registered UI font.
        """
        state = {
            "path": str(Path.home()),
            "selected": set(),
            "refreshing": False,
            "sort_by": "name",
            "sort_reverse": False,
            "query": "",
        }
        content = BoxLayout(orientation="vertical", spacing=dp(6), padding=dp(8))

        top = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(40), spacing=dp(6))
        btn_up = make_button("secondary", text="上一级", size_hint_x=None, width=dp(82))
        btn_home = make_button("secondary", text="主页", size_hint_x=None, width=dp(70))
        btn_refresh = make_button("secondary", text="刷新", size_hint_x=None, width=dp(70))
        path_input = make_input(text=state["path"], multiline=False)
        search_input = make_input(text="", hint_text="过滤当前目录", multiline=False, size_hint_x=None, width=dp(180))
        top.add_widget(btn_up)
        top.add_widget(btn_home)
        top.add_widget(btn_refresh)
        top.add_widget(path_input)
        top.add_widget(search_input)
        content.add_widget(top)

        body = BoxLayout(orientation="horizontal", spacing=dp(8))
        side = BoxLayout(orientation="vertical", size_hint_x=None, width=dp(126), spacing=dp(4))
        file_area = BoxLayout(orientation="vertical", spacing=dp(4))

        header = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(30), spacing=dp(4))
        header.add_widget(make_label(text="", size_hint_x=None, width=dp(52)))
        header.add_widget(make_label(text="图标", size_hint_x=None, width=dp(38), halign="center", valign="middle"))
        btn_sort_name = make_button("secondary", text="名称", halign="center")
        btn_sort_size = make_button("secondary", text="大小", size_hint_x=None, width=dp(92), halign="center")
        btn_sort_type = make_button("secondary", text="类型", size_hint_x=None, width=dp(88), halign="center")
        btn_sort_mtime = make_button("secondary", text="修改时间", size_hint_x=None, width=dp(138), halign="center")
        header.add_widget(btn_sort_name)
        header.add_widget(btn_sort_size)
        header.add_widget(btn_sort_type)
        header.add_widget(btn_sort_mtime)
        header.add_widget(make_label(text="属性", size_hint_x=None, width=dp(58), halign="center", valign="middle"))
        header.add_widget(make_label(text="", size_hint_x=None, width=dp(58)))
        header.add_widget(make_label(text="预览", size_hint_x=None, width=dp(64), halign="center", valign="middle"))
        for child in header.children:
            try:
                child.halign = "center"
                child.valign = "middle"
                child.text_size = (max(1, child.width - dp(8)), child.height)
                child.bind(size=lambda inst, _val: setattr(inst, "text_size", (max(1, inst.width - dp(8)), inst.height)))
                if hasattr(child, "padding"):
                    child.padding = (0, 0)
            except Exception:
                pass
        file_area.add_widget(header)

        rv = RecycleView(size_hint=(1, 1))
        layout = RecycleBoxLayout(
            default_size=(None, dp(34)),
            default_size_hint=(1, None),
            size_hint_y=None,
            orientation="vertical",
            spacing=dp(2),
        )
        layout.bind(minimum_height=layout.setter("height"))
        rv.add_widget(layout)
        rv.viewclass = "MixedPickerRow"
        file_area.add_widget(rv)

        body.add_widget(side)
        body.add_widget(file_area)
        content.add_widget(body)

        bottom = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(6))
        selected_label = make_label(text=self.cu("mixed_picker_selected", n=0), halign="left", valign="middle")
        bind_label_wrap(selected_label)
        mode_label = make_label(
            text=self.cu("mixed_picker_auto_package_on" if bool(getattr(self, "auto_package_multi_selection", True)) else "mixed_picker_auto_package_off"),
            size_hint_x=None,
            width=dp(270),
            color=THEME["muted_text"],
            halign="left",
            valign="middle",
        )
        bind_label_wrap(mode_label)
        btn_clear = make_button("secondary", text=self.cu("mixed_picker_clear"), size_hint_x=None, width=dp(100))
        btn_send = make_button("primary", text=self.cu("mixed_picker_send"), size_hint_x=None, width=dp(120))
        btn_cancel = make_button("secondary", text=self.cu("cancel"), size_hint_x=None, width=dp(80))
        bottom.add_widget(selected_label)
        bottom.add_widget(mode_label)
        bottom.add_widget(btn_clear)
        bottom.add_widget(btn_send)
        bottom.add_widget(btn_cancel)
        content.add_widget(bottom)

        popup = style_popup(Popup(title=self.cu("mixed_picker_title"), content=content, size_hint=(0.90, 0.88)))

        def _selected_sorted() -> List[str]:
            return sorted(list(state["selected"]), key=lambda s: (0 if os.path.isdir(s) else 1, os.path.basename(s).lower()))

        def _update_selected_label() -> None:
            selected_label.text = self.cu("mixed_picker_selected", n=len(state["selected"]))

        def _toggle_path(path: str) -> None:
            if not path:
                return
            try:
                path = str(Path(path).resolve())
            except Exception:
                path = str(path)
            selected = state["selected"]
            if path in selected:
                selected.remove(path)
            else:
                selected.add(path)
            _update_selected_label()
            _refresh()

        def _enter_path(path: str) -> None:
            try:
                p = Path(path).expanduser().resolve()
                if p.exists() and p.is_dir():
                    state["path"] = str(p)
                    path_input.text = str(p)
                    _refresh()
            except Exception as exc:
                try:
                    self.sender_log_box.append(f"open folder failed: {exc}\n")
                except Exception:
                    pass

        def _preview_image(path: str) -> None:
            try:
                p = Path(str(path or "")).expanduser()
                if not p.exists() or not p.is_file() or not is_image_file_for_preview(str(p)):
                    return
                box = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(8))
                img = Image(source=str(p), fit_mode="contain", allow_stretch=True, keep_ratio=True)
                box.add_widget(img)
                row = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
                row.add_widget(make_label(text=p.name, halign="left", valign="middle", shorten=True))
                popup = style_popup(Popup(title="预览", content=box, size_hint=(0.82, 0.82)))
                row.add_widget(make_button("secondary", text="关闭", size_hint_x=None, width=dp(90), on_release=lambda *_: popup.dismiss()))
                box.add_widget(row)
                apply_ui_font(box)
                popup.open()
            except Exception as exc:
                try:
                    self._append_debug_line(f"image preview failed: {exc}", protocol=False)
                except Exception:
                    pass

        def _file_row_dict(p: Path) -> Dict[str, object]:
            try:
                full = str(p.resolve())
            except Exception:
                full = str(p)
            is_dir = p.is_dir()
            try:
                st = p.stat()
                mtime = float(st.st_mtime)
                modified_text = time.strftime("%Y-%m-%d %H:%M", time.localtime(mtime))
                size_num = int(st.st_size) if not is_dir else 0
            except Exception:
                mtime = 0.0
                modified_text = ""
                size_num = 0
            if is_dir:
                type_text = "文件夹"
                size_text = ""
                icon_text = "DIR"
                thumb_source = ""
                attr_text = "D"
            else:
                ext = p.suffix.lower().lstrip(".")
                type_text = ext.upper() if ext else "文件"
                icon_text = file_icon_text(str(p), is_dir=False)
                size_text = format_file_size(size_num) if size_num >= 0 else ""
                thumb_source = ""
                attr_text = "A"
            return {
                "full_path": full,
                "display_name": p.name,
                "type_text": type_text,
                "size_text": size_text,
                "modified_text": modified_text,
                "attr_text": attr_text,
                "icon_text": icon_text,
                "thumb_source": thumb_source,
                "selected": full in state["selected"],
                "is_dir": is_dir,
                "is_previewable": (not is_dir and is_image_file_for_preview(full)),
                "sort_name": p.name.lower(),
                "sort_size": size_num,
                "sort_type": type_text.lower(),
                "sort_mtime": mtime,
                "on_toggle": _toggle_path,
                "on_open": _enter_path,
                "on_request_thumb": None,
                "on_preview": _preview_image,
            }

        def _refresh(path: Optional[str] = None) -> None:
            if path:
                state["path"] = str(path)
                path_input.text = str(path)
            data: List[Dict[str, object]] = []
            try:
                cur = Path(str(state["path"] or str(Path.home()))).expanduser()
                if not cur.exists() or not cur.is_dir():
                    cur = Path.home()
                cur = cur.resolve()
                state["path"] = str(cur)
                path_input.text = str(cur)
                if cur.parent != cur:
                    data.append({
                        "full_path": str(cur.parent),
                        "display_name": "..",
                        "type_text": "上一级",
                        "size_text": "",
                        "modified_text": "",
                        "attr_text": "D",
                        "icon_text": "UP",
                        "thumb_source": "",
                        "selected": False,
                        "is_dir": True,
                        "is_previewable": False,
                        "sort_name": "",
                        "sort_size": 0,
                        "sort_type": "上一级",
                        "sort_mtime": 0.0,
                        "on_toggle": _enter_path,
                        "on_open": _enter_path,
                        "on_request_thumb": None,
                        "on_preview": None,
                    })
                entries: List[Path] = []
                try:
                    for child in cur.iterdir():
                        try:
                            if child.name.startswith("$") and IS_WINDOWS:
                                continue
                            if child.is_dir() or child.is_file():
                                entries.append(child)
                        except Exception:
                            continue
                    query = str(state.get("query") or "").strip().lower()
                    if query:
                        entries = [p for p in entries if query in p.name.lower()]
                    sort_by = str(state.get("sort_by") or "name")
                    reverse = bool(state.get("sort_reverse", False))
                    def _sort_key(p: Path):
                        try:
                            is_dir0 = 0 if p.is_dir() else 1
                            if sort_by == "size":
                                return (is_dir0, p.stat().st_size if p.is_file() else 0, p.name.lower())
                            if sort_by == "type":
                                return (is_dir0, (p.suffix.lower().lstrip(".") if p.is_file() else "文件夹"), p.name.lower())
                            if sort_by == "mtime":
                                return (is_dir0, p.stat().st_mtime, p.name.lower())
                            return (is_dir0, p.name.lower())
                        except Exception:
                            return (1, p.name.lower())
                    entries.sort(key=_sort_key, reverse=reverse)
                except Exception:
                    entries = []
                for child in entries[:4000]:
                    data.append(_file_row_dict(child))
            except Exception as exc:
                data.append({
                    "full_path": "",
                    "display_name": f"无法读取目录: {exc}",
                    "type_text": "错误",
                    "size_text": "",
                    "modified_text": "",
                    "attr_text": "",
                    "icon_text": "ERR",
                    "thumb_source": "",
                    "selected": False,
                    "is_dir": False,
                    "is_previewable": False,
                    "sort_name": "",
                    "sort_size": 0,
                    "sort_type": "错误",
                    "sort_mtime": 0.0,
                    "on_toggle": None,
                    "on_open": None,
                    "on_request_thumb": None,
                    "on_preview": None,
                })
            rv.data = data
            _update_selected_label()

        def _build_shortcuts() -> None:
            side.clear_widgets()
            for label, p in self._common_picker_shortcuts():
                b = make_button("secondary", text=label, size_hint_y=None, height=dp(34))
                b.bind(on_release=lambda _btn, pp=p: _enter_path(pp))
                side.add_widget(b)
            side.add_widget(make_label(text="", size_hint_y=1))

        def _set_sort(key: str) -> None:
            if str(state.get("sort_by") or "") == key:
                state["sort_reverse"] = not bool(state.get("sort_reverse", False))
            else:
                state["sort_by"] = key
                state["sort_reverse"] = False
            _refresh()

        def _filter_changed(_inst, value: str) -> None:
            state["query"] = str(value or "")
            _refresh()

        def _send_selected(*_) -> None:
            paths = _selected_sorted()
            if not paths:
                return
            popup.dismiss()
            callback(paths)

        def _goto_input(*_) -> None:
            _enter_path(path_input.text.strip())

        path_input.bind(on_text_validate=_goto_input)
        search_input.bind(text=_filter_changed)
        btn_sort_name.bind(on_release=lambda *_: _set_sort("name"))
        btn_sort_size.bind(on_release=lambda *_: _set_sort("size"))
        btn_sort_type.bind(on_release=lambda *_: _set_sort("type"))
        btn_sort_mtime.bind(on_release=lambda *_: _set_sort("mtime"))
        btn_up.bind(on_release=lambda *_: _enter_path(str(Path(state["path"]).parent)))
        btn_home.bind(on_release=lambda *_: _enter_path(str(Path.home())))
        btn_refresh.bind(on_release=lambda *_: _refresh())
        btn_clear.bind(on_release=lambda *_: (state["selected"].clear(), _update_selected_label(), _refresh()))
        btn_send.bind(on_release=_send_selected)
        btn_cancel.bind(on_release=lambda *_: popup.dismiss())

        _build_shortcuts()
        _refresh()
        popup.open()

    def _handle_mixed_selected_paths(self, paths: List[str]) -> None:
        items = []
        seen = set()
        has_folder = False
        for p in paths or []:
            sp = str(p or "")
            if not sp or not (os.path.isfile(sp) or os.path.isdir(sp)):
                continue
            if os.path.isdir(sp):
                has_folder = True
                continue
            key = os.path.normcase(os.path.abspath(sp))
            if key in seen:
                continue
            seen.add(key)
            items.append(sp)
        if has_folder:
            self.main_messages_box.append(self._folder_not_supported_text() + "\n")
            return
        if not items:
            return
        if len(items) == 1:
            self._send_file_path_to_current_chat(items[0])
            return
        self._package_paths_and_send(items)

    def _native_multi_file_dialog(self, callback) -> None:
        """Open a native multi-file picker, with a Kivy fallback."""
        try:
            import tkinter as tk
            from tkinter import filedialog

            root = tk.Tk()
            root.withdraw()
            try:
                root.attributes("-topmost", True)
            except Exception:
                pass
            selected = filedialog.askopenfilenames(parent=root, title=self.cu("choose_files_multi"))
            root.destroy()
            paths = [str(p) for p in (selected or []) if p]
            if paths:
                callback(paths)
            return
        except Exception:
            self.sender_log_box.append(self.t("native_dialog_failed") + "\n")
            self._multi_file_popup(callback=callback)

    def _multi_file_popup(self, callback) -> None:
        chooser = FileChooserListView(path=str(Path.home()), multiselect=True)
        popup = Popup(title=self.cu("choose_files_multi"), content=BoxLayout(orientation="vertical"), size_hint=(0.9, 0.9))
        box = popup.content
        box.add_widget(chooser)
        row = BoxLayout(size_hint_y=None, height=dp(48), spacing=dp(8), padding=dp(8))
        ok = Button(text="OK")
        cancel = Button(text=self.cu("cancel"))
        row.add_widget(ok)
        row.add_widget(cancel)
        box.add_widget(row)

        def _ok(*_):
            paths = [str(p) for p in (chooser.selection or []) if p and os.path.isfile(str(p))]
            popup.dismiss()
            if paths:
                callback(paths)

        ok.bind(on_release=_ok)
        cancel.bind(on_release=lambda *_: popup.dismiss())
        popup.open()

    def _native_multi_folder_dialog(self, callback) -> None:
        """Open native Windows Explorer folder picker with multi-select.

        Uses Windows IFileOpenDialog when pywin32 is available. Falls back to the
        built-in Kivy folder chooser when COM is unavailable.
        """
        if IS_WINDOWS:
            try:
                import pythoncom
                from win32com.shell import shell, shellcon

                pythoncom.CoInitialize()
                dialog = pythoncom.CoCreateInstance(
                    shell.CLSID_FileOpenDialog,
                    None,
                    pythoncom.CLSCTX_INPROC_SERVER,
                    shell.IID_IFileOpenDialog,
                )
                options = int(dialog.GetOptions())
                options |= int(getattr(shellcon, "FOS_PICKFOLDERS", 0x20))
                options |= int(getattr(shellcon, "FOS_FORCEFILESYSTEM", 0x40))
                options |= int(getattr(shellcon, "FOS_ALLOWMULTISELECT", 0x200))
                options |= int(getattr(shellcon, "FOS_PATHMUSTEXIST", 0x800))
                dialog.SetOptions(options)
                try:
                    dialog.SetTitle(self.cu("choose_folder_package"))
                except Exception:
                    pass
                try:
                    dialog.Show(None)
                except Exception:
                    # User cancel or COM dialog failure. Do not show fallback on a
                    # normal cancel because that feels like the dialog ignored the user.
                    try:
                        pythoncom.CoUninitialize()
                    except Exception:
                        pass
                    return
                results = dialog.GetResults()
                count = int(results.GetCount())
                paths = []
                sigdn = int(getattr(shellcon, "SIGDN_FILESYSPATH", 0x80058000))
                for i in range(count):
                    try:
                        item = results.GetItemAt(i)
                        p = item.GetDisplayName(sigdn)
                        if p:
                            paths.append(str(p))
                    except Exception:
                        continue
                try:
                    pythoncom.CoUninitialize()
                except Exception:
                    pass
                if paths:
                    callback(paths)
                return
            except Exception as exc:
                try:
                    self.sender_log_box.append(self.t("native_dialog_failed") + f" ({exc})\n")
                except Exception:
                    pass
        self._multi_folder_popup(callback=callback)

    def _multi_folder_popup(self, callback) -> None:
        chooser = FileChooserListView(path=str(Path.home()), dirselect=True, multiselect=True)
        popup = Popup(title=self.cu("choose_folder_package"), content=BoxLayout(orientation="vertical"), size_hint=(0.9, 0.9))
        box = popup.content
        box.add_widget(chooser)
        row = BoxLayout(size_hint_y=None, height=dp(48), spacing=dp(8), padding=dp(8))
        ok = Button(text="OK")
        cancel = Button(text=self.cu("cancel"))
        row.add_widget(ok)
        row.add_widget(cancel)
        box.add_widget(row)

        def _ok(*_):
            paths = [str(p) for p in (chooser.selection or []) if p and os.path.isdir(str(p))]
            popup.dismiss()
            if paths:
                callback(paths)

        ok.bind(on_release=_ok)
        cancel.bind(on_release=lambda *_: popup.dismiss())
        popup.open()

    def _handle_selected_folder_paths(self, paths: List[str]) -> None:
        self.main_messages_box.append(self._folder_not_supported_text() + "\n")

    def _handle_selected_file_paths(self, paths: List[str]) -> None:
        files = []
        seen = set()
        for p in paths or []:
            sp = str(p or "")
            if not sp or not os.path.isfile(sp):
                continue
            key = os.path.normcase(os.path.abspath(sp))
            if key in seen:
                continue
            seen.add(key)
            files.append(sp)
        if not files:
            return
        if len(files) == 1:
            self._send_file_path_to_current_chat(files[0])
            return
        self._package_paths_and_send(files)

    def _package_paths_and_send(self, paths: List[str], suggested_name: str = "") -> None:
        valid = [str(p) for p in (paths or []) if p and os.path.isfile(str(p))]
        if any(p and os.path.isdir(str(p)) for p in (paths or [])):
            self.main_messages_box.append(self._folder_not_supported_text() + "\n")
            return
        if not valid:
            return
        if self.file_packaging_busy:
            self.main_messages_box.append(self.cu("packaging_files", n=len(valid)) + "\n")
            return
        self.file_packaging_busy = True
        package_id = f"package:{int(time.time() * 1000)}"
        card_id = f"file_transfer:{package_id}"
        card = make_card(
            CARD_FILE_TRANSFER,
            title=self._multi_file_card_title(),
            subtitle=self._multi_file_summary(len(valid)),
            status=self.cu("packaging_files", n=len(valid)),
            detail="",
            direction="system",
            side="system",
            actions=[],
            card_id=card_id,
        )
        self._add_runtime_chat_card(
            card,
            peer_id=self.current_peer_id,
            group_id=self.current_group_id if self.current_chat_mode == "group" else "",
        )
        self._schedule_transfer_card_refresh(force=True)

        def _run_package():
            result = package_files_to_zip(valid)

            def _finish(_dt):
                self.file_packaging_busy = False
                if result.get("ok"):
                    zip_path = str(result.get("zip_path") or "")
                    count = int(result.get("file_count") or len(valid))
                    self._remove_runtime_chat_card(card_id)
                    ok = self._send_file_path_to_current_chat(
                        zip_path,
                        delete_after_success=True,
                        package_file_count=count,
                    )
                    if not ok:
                        fail_card = make_card(
                            CARD_FILE_TRANSFER,
                            title=self._multi_file_card_title(),
                            subtitle=self._multi_file_summary(count),
                            status=self.cu("package_failed", error="send failed"),
                            detail=str(zip_path or ""),
                            direction="system",
                            side="system",
                            actions=[],
                            card_id=card_id,
                        )
                        self._add_runtime_chat_card(fail_card, peer_id=self.current_peer_id, group_id=self.current_group_id if self.current_chat_mode == "group" else "")
                    return
                error = str(result.get("error") or "unknown")
                fail_card = make_card(
                    CARD_FILE_TRANSFER,
                    title=self._multi_file_card_title(),
                    subtitle=self._multi_file_summary(len(valid)),
                    status=self.cu("package_failed", error=error),
                    detail=error,
                    direction="system",
                    side="system",
                    actions=[],
                    card_id=card_id,
                )
                self._add_runtime_chat_card(fail_card, peer_id=self.current_peer_id, group_id=self.current_group_id if self.current_chat_mode == "group" else "")
                self._schedule_transfer_card_refresh(force=True)

            Clock.schedule_once(_finish, 0)

        threading.Thread(target=_run_package, daemon=True).start()

    def _send_file_path_to_current_chat(self, path: str, delete_after_success: bool = False, package_file_count: int = 0) -> bool:
        path = str(path or "").strip()
        if not path:
            return False
        message_service = self.message_service
        if message_service is None:
            self.main_messages_box.append("请先解锁聊天数据库。\n")
            return False
        chat_mode = str(self.current_chat_mode or "")
        peer_id = str(self.current_peer_id or "")
        group_id = str(self.current_group_id or "")
        if chat_mode == "direct" and not peer_id:
            self.main_messages_box.append("没有可发送文件的接收对象。\n")
            return False
        if chat_mode == "group" and not group_id:
            self.main_messages_box.append("没有可发送文件的接收对象。\n")
            return False
        if chat_mode not in ("direct", "group"):
            self.main_messages_box.append("没有可发送文件的接收对象。\n")
            return False

        package_count = int(package_file_count or 0)
        prepare_card_id = f"file_prepare:{int(time.time() * 1000)}:{secrets.token_hex(3)}"
        name = os.path.basename(path) or unnamed_file_text(self.lang)
        title = self._multi_file_card_title() if package_count > 1 else self._file_card_title()
        subtitle = self._multi_file_summary(package_count) if package_count > 1 else truncate_filename(name, 48)
        preparing_text = "正在准备发送" if self.lang == "zh" else "Preparing to send"
        prepare_state = {"pending": True}

        def _show_prepare_card(_dt):
            if not prepare_state.get("pending"):
                return
            card = make_card(
                CARD_FILE_TRANSFER,
                title=title,
                subtitle=subtitle,
                status=preparing_text,
                detail=str(path),
                direction="outgoing",
                side="outgoing",
                actions=[],
                card_id=prepare_card_id,
                meta={"direction": "outgoing", "side": "outgoing"},
            )
            self._add_runtime_chat_card(
                card,
                peer_id=peer_id,
                group_id=group_id if chat_mode == "group" else "",
            )
            self._schedule_transfer_card_refresh(force=True)

        def _show_prepare_failed(error: str):
            prepare_state["pending"] = False
            def _finish(_dt):
                card = make_card(
                    CARD_FILE_TRANSFER,
                    title=title,
                    subtitle=subtitle,
                    status=self._file_failed_text(error),
                    detail=f"{error}\n{path}",
                    direction="outgoing",
                    side="outgoing",
                    actions=[],
                    card_id=prepare_card_id,
                    meta={"direction": "outgoing", "side": "outgoing"},
                )
                self._add_runtime_chat_card(
                    card,
                    peer_id=peer_id,
                    group_id=group_id if chat_mode == "group" else "",
                )
                self._schedule_transfer_card_refresh(force=True)

            Clock.schedule_once(_finish, 0)

        Clock.schedule_once(_show_prepare_card, 0)

        def _prepare_and_start() -> None:
            try:
                if not os.path.isfile(path):
                    raise FileNotFoundError(self.cu("retry_file_missing"))
                msg = None
                recipients: List[Dict[str, object]] = []
                if chat_mode == "direct":
                    msg, contact = message_service.create_direct_file(peer_id, path)
                    recipients = [contact] if contact else []
                elif chat_mode == "group":
                    if not self._is_local_active_group_member(group_id):
                        raise RuntimeError("not an active group member")
                    msg, recipients = message_service.create_group_file(group_id, path)
                if not recipients:
                    raise RuntimeError("no file recipients")
                prepare_state["pending"] = False
                Clock.schedule_once(lambda _dt: self._remove_runtime_chat_card(prepare_card_id), 0)
                self._start_file_transfer_for_message(
                    msg=msg,
                    recipients=recipients,
                    path=path,
                    delete_after_success=delete_after_success,
                    package_file_count=package_count,
                )
            except Exception as exc:
                error = str(exc or "file send preparation failed")
                self._append_debug_line(f"file send preparation failed: {error}", protocol=True)
                _show_prepare_failed(error)

        threading.Thread(target=_prepare_and_start, daemon=True).start()
        return True


    def _start_file_transfer_for_message(self, *, msg: Dict[str, object], recipients: List[Dict[str, object]], path: str, delete_after_success: bool = False, package_file_count: int = 0) -> None:
        if self.message_service is None or not msg:
            return
        path = str(path or "")
        chat_message_id = str(msg.get('message_id') or '')
        chat_conversation_id = str(msg.get('conversation_id') or '')
        chat_group_id = str(msg.get('group_id') or (self.current_group_id if self.current_chat_mode == 'group' else ''))
        chat_sender_peer_id = str(msg.get('sender_peer_id') or self.chat_local_peer_id or '')
        created_at = float(msg.get('created_at') or time.time())
        first_peer_id = str((recipients[0] or {}).get("peer_id") or "") if recipients else ""
        first_peer_label = self._file_peer_label(first_peer_id)
        preparing_text = "正在准备发送" if self.lang == "zh" else "Preparing to send"
        self._schedule_file_transfer_chat_card(
            message_id=chat_message_id,
            peer_id=first_peer_id,
            group_id=chat_group_id,
            conversation_id=chat_conversation_id,
            direction="outgoing",
            transferred=0,
            total=0,
            pct=0.0,
            status=preparing_text,
            detail=self._file_size_detail(0, peer_label=first_peer_label, path=path),
            package_count=package_file_count,
        )
        self._schedule_transfer_card_refresh(force=True)

        def _mark_sent(pid: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_sent(chat_message_id, pid)
            except Exception as exc:
                self._append_debug_line(f"file mark_sent failed: {exc}", protocol=True)

        def _mark_delivered(pid: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_delivered(chat_message_id, pid)
            except Exception as exc:
                self._append_debug_line(f"file mark_delivered failed: {exc}", protocol=True)

        def _mark_failed(pid: str, err: str) -> None:
            try:
                if self.message_service is not None:
                    self.message_service.mark_failed(chat_message_id, pid, error=err)
            except Exception as exc:
                self._append_debug_line(f"file mark_failed failed: {exc}; original={err}", protocol=True)

        def _run():
            try:
                if not path or not os.path.isfile(path):
                    raise FileNotFoundError(self.cu("retry_file_missing"))
                file_total = os.path.getsize(path) if os.path.exists(path) else 0
                self._schedule_file_transfer_chat_card(
                    message_id=chat_message_id,
                    peer_id=first_peer_id,
                    group_id=chat_group_id,
                    conversation_id=chat_conversation_id,
                    direction="outgoing",
                    transferred=0,
                    total=file_total,
                    pct=0.0,
                    status=self._file_waiting_text(first_peer_label),
                    detail=self._file_size_detail(file_total, peer_label=first_peer_label, path=path),
                    package_count=package_file_count,
                )
                self._schedule_transfer_card_refresh(force=True)
                if self.file_transfer_service is not None:
                    self.file_transfer_service.remember_runtime_task(
                        self.file_message_tasks,
                        chat_message_id=chat_message_id,
                        path=path,
                        recipients=[dict(r) for r in recipients],
                        conversation_id=chat_conversation_id,
                        group_id=chat_group_id,
                        sender_peer_id=chat_sender_peer_id,
                        created_at=created_at,
                        total_bytes=file_total,
                    )
                    self.file_transfer_service.create_outgoing_tasks(
                        chat_message_id=chat_message_id,
                        recipients=recipients,
                        path=path,
                        total_bytes=file_total,
                        conversation_id=chat_conversation_id,
                        group_id=chat_group_id,
                    )
                    if int(package_file_count or 0) > 1:
                        self.file_message_tasks.setdefault(chat_message_id, {})["package_file_count"] = int(package_file_count or 0)
                else:
                    self.file_message_tasks[chat_message_id] = {
                        "message_id": chat_message_id,
                        "path": path,
                        "recipients": [dict(r) for r in recipients],
                        "conversation_id": chat_conversation_id,
                        "group_id": chat_group_id,
                        "sender_peer_id": chat_sender_peer_id,
                        "created_at": created_at,
                        "total": file_total,
                        "package_file_count": int(package_file_count or 0),
                    }
            except Exception as exc:
                error = str(exc or "file send preparation failed")
                self._append_debug_line(f"file transfer preparation failed: {error}", protocol=True)
                targets = recipients or [{"peer_id": first_peer_id}]
                for r in targets:
                    pid = str((r or {}).get("peer_id") or first_peer_id)
                    self._schedule_file_transfer_chat_card(
                        message_id=chat_message_id,
                        peer_id=pid,
                        group_id=chat_group_id,
                        conversation_id=chat_conversation_id,
                        direction="outgoing",
                        transferred=0,
                        total=0,
                        pct=0.0,
                        status=self._file_failed_text(error),
                        error=error,
                        package_count=package_file_count,
                    )
                    Clock.schedule_once(lambda _dt, p=pid, e=error: (_mark_failed(p, e), self._schedule_transfer_card_refresh(force=True)), 0)
                Clock.schedule_once(lambda _dt, e=error: self.sender_log_box.append(f"file send failed: {e}\n"), 0)
                return
            all_ok = True
            file_meta_text = json.dumps({"kind": "file", "name": os.path.basename(path), "size": file_total, "chat_message_id": chat_message_id}, ensure_ascii=False, separators=(",", ":"))
            for r in recipients:
                peer_id = str(r.get('peer_id') or '')
                ip = str(r.get('peer_ip') or '')
                try:
                    port = int(r.get('peer_port') or 9999)
                except Exception:
                    port = 9999
                if not ip or is_unspecified_ip(ip):
                    Clock.schedule_once(lambda _dt, pid=peer_id: (_mark_failed(pid, 'invalid_endpoint'), self._add_file_transfer_chat_card(
                        message_id=chat_message_id,
                        peer_id=pid,
                        group_id=chat_group_id,
                        conversation_id=chat_conversation_id,
                        direction="outgoing",
                        transferred=0,
                        total=file_total,
                        pct=0.0,
                        status=self._file_failed_text("invalid_endpoint"),
                        error="invalid_endpoint",
                    ), self._schedule_transfer_card_refresh(force=True)), 0)
                    all_ok = False
                    if self.file_transfer_service is not None:
                        self.file_transfer_service.mark_failed(chat_message_id, peer_id=peer_id, direction='outgoing', error='invalid_endpoint')
                    continue
                try:
                    Clock.schedule_once(lambda _dt, pid=peer_id: _mark_sent(pid), 0)
                    self._send_chat_to_endpoint(
                        ip=ip, port=port, peer_id=peer_id, text=file_meta_text,
                        message_id=chat_message_id,
                        group_id=chat_group_id,
                        conversation_id=chat_conversation_id,
                        created_at=created_at,
                        body_type='file',
                    )
                except Exception as exc:
                    self._append_debug_line(f"file chat metadata send failed: {exc}", protocol=True)
                pin_file = str(receiver_pin_file(ip, port))
                args = [
                    "--worker", "sender",
                    "--server-ip", ip,
                    "--server-port", str(port),
                    "--file", path,
                    "--server-pin-file", pin_file,
                    "--request-timeout", "300",
                    "--complete-timeout", "180",
                    "--final-ack-timeout", "180",
                    "--no-progress-timeout", "120",
                    "--stats-interval", "1",
                    "--payload-size", "1400",
                    "--file-read-chunk-mb", "4",
                    "--max-unacked-pkts", "1024",
                    "--adaptive-max-unacked-min", "960",
                    "--adaptive-max-unacked-max", "1536",
                    "--adaptive-max-unacked-step", "64",
                    "--adaptive-eval-interval-sec", "5",
                    "--lan-pacing-burst-pkts", "32",
                    "--lan-pacing-interval-ms", "5",
                    "--reorder-tolerance-pkts", "128",
                    "--chat-message-id", chat_message_id,
                    "--chat-conversation-id", chat_conversation_id,
                    "--chat-group-id", chat_group_id,
                    "--chat-sender-peer-id", chat_sender_peer_id,
                    "--chat-receiver-peer-id", peer_id,
                ]
                self._schedule_file_transfer_chat_card(
                    message_id=chat_message_id,
                    peer_id=peer_id,
                    group_id=chat_group_id,
                    conversation_id=chat_conversation_id,
                    direction="outgoing",
                    transferred=0,
                    total=file_total,
                    pct=0.0,
                    status=self._file_accepted_text(),
                    detail=self._file_size_detail(file_total, peer_label=self._file_peer_label(peer_id)),
                )
                ok = self._run_file_sender_with_progress(args=args, message_id=chat_message_id, peer_id=peer_id, total_size=file_total)
                if ok:
                    Clock.schedule_once(lambda _dt, pid=peer_id: (_mark_delivered(pid), self._schedule_transfer_card_refresh(force=True)), 0)
                else:
                    all_ok = False
                    Clock.schedule_once(lambda _dt, pid=peer_id: (_mark_failed(pid, 'file_send_failed'), self._schedule_transfer_card_refresh(force=True)), 0)
                    if self.file_transfer_service is not None:
                        self.file_transfer_service.mark_failed(chat_message_id, peer_id=peer_id, direction='outgoing', error='file_send_failed')
            if delete_after_success and all_ok:
                try:
                    package_root = temp_dir().resolve()
                    target = Path(path).resolve()
                    if package_root in target.parents and target.suffix.lower() == ".zip":
                        target.unlink(missing_ok=True)
                except Exception as exc:
                    self._append_debug_line(f"temporary package cleanup failed: {exc}", protocol=True)
            Clock.schedule_once(lambda _dt: self._schedule_transfer_card_refresh(force=True), 0)
        threading.Thread(target=_run, daemon=True).start()


    def retry_file_message(self, message_id: str) -> None:
        mid = str(message_id or "")
        if not mid or self.file_transfer_service is None:
            return
        try:
            ctx = self.file_transfer_service.retry_context_from_message(mid, self.file_message_tasks, local_peer_id=self.chat_local_peer_id)
            if not ctx:
                return
            path = str(ctx.get("path") or "")
            recipients = [dict(r) for r in (ctx.get("recipients") or [])]
            if not path or not os.path.isfile(path):
                self.sender_log_box.append(self.cu("retry_file_missing") + "\n")
                return
            if not recipients:
                self.sender_log_box.append("no file recipient\n")
                return
            msg = {
                "message_id": mid,
                "conversation_id": str(ctx.get("conversation_id") or ""),
                "group_id": str(ctx.get("group_id") or ""),
                "sender_peer_id": str(ctx.get("sender_peer_id") or self.chat_local_peer_id or ""),
                "created_at": float(ctx.get("created_at") or time.time()),
                "body_type": "file",
            }
            self._start_file_transfer_for_message(msg=msg, recipients=recipients, path=path)
        except Exception as exc:
            try:
                self.sender_log_box.append(f"file retry failed: {exc}\n")
            except Exception:
                pass


    def start_sender(self) -> None:
        rec = getattr(self, "selected_receiver", None)
        selected_ip = ""
        selected_port = 9999
        if rec is not None and self.receiver_spinner.text == self._receiver_label(rec):
            selected_ip, selected_port = self._receiver_endpoint(rec)

        ip_text = self.receiver_ip.text.strip()
        port_text = self.receiver_port.text.strip() or "9999"
        try:
            port_num = int(port_text)
        except Exception:
            port_num = 9999
            port_text = "9999"

        if selected_ip and ip_text == selected_ip and port_num == int(selected_port):
            ip = selected_ip
            port_num = int(selected_port)
            port_text = str(port_num)
        else:
            # Manual edits take precedence over a stale discovery selection.
            self.selected_receiver = None
            ip = ip_text

        if not ip:
            self.sender_log_box.append(self.t("need_ip") + "\n")
            return
        file_path = self.file_input.text.strip().strip('"')
        if not file_path or not os.path.isfile(file_path):
            self.sender_log_box.append(self.t("need_file") + "\n")
            return
        pin_file = str(receiver_pin_file(ip, port_num))
        self.sender_log_box.append(f"Starting sender to: {ip}:{port_num}\n")
        self.sender_log_box.append(f"Receiver pin file: {pin_file}\n")
        args = [
            "--server-ip", ip,
            "--server-port", str(port_num),
            "--file", file_path,
            "--payload-size", self.payload_input.text.strip() or "1400",
            "--complete-timeout", self.complete_timeout_input.text.strip() or "180",
            "--final-ack-timeout", self.complete_timeout_input.text.strip() or "180",
            "--request-timeout", self.request_timeout_input.text.strip() or "300",
            "--no-progress-timeout", "120",
            "--stats-interval", "0.5",
            "--server-pin-file", pin_file,
        ]
        self.last_sender_args = list(args)
        self.last_sender_file = file_path
        self.last_sender_failure_code = ""
        self.retry_send_btn.disabled = True
        self.progress.value = 0
        try:
            total = os.path.getsize(file_path)
            self.update_progress_labels(0, total, 0.0, self.t("unknown"), 0.0)
        except Exception:
            pass
        self.sender_worker.start(args)
        self.sender_log_box.append(self.t("started") + "\n")
        self.sender_log_box.append(self.t("request_waiting") + "\n")

    def retry_sender(self) -> None:
        if self.sender_worker.is_running():
            self.sender_log_box.append(self.t("running") + "\n")
            return
        if not self.last_sender_args:
            self.sender_log_box.append(self.t("need_file") + "\n")
            return
        if self.last_sender_file and not os.path.isfile(self.last_sender_file):
            self.sender_log_box.append(self.t("need_file") + "\n")
            return
        self.retry_send_btn.disabled = True
        self.last_sender_failure_code = ""
        self.progress.value = 0
        try:
            total = os.path.getsize(self.last_sender_file) if self.last_sender_file else 0
            self.update_progress_labels(0, total, 0.0, self.t("unknown"), 0.0)
        except Exception:
            pass
        self.sender_worker.start(list(self.last_sender_args))
        self.sender_log_box.append(self.t("started") + "\n")
        self.sender_log_box.append(self.t("request_waiting") + "\n")

    def start_receiver(self, auto: bool = False) -> None:
        if self.receiver_worker.is_running():
            if auto:
                return
            self.receiver_log_box.append(self.t("running") + "\n")
            return
        bind_host = self.bind_input.text.strip() or "0.0.0.0"
        port_text = self.recv_port.text.strip() or str(MAIN_UDP_PORT)
        try:
            port_num = int(port_text)
        except Exception:
            port_num = MAIN_UDP_PORT
            port_text = str(MAIN_UDP_PORT)
        port_status = udp_port_status(port_num, bind_host)
        if not port_status.get("available"):
            if port_num == MAIN_UDP_PORT:
                message = MAIN_UDP_PORT_BUSY_MESSAGE
            else:
                message = f"UDP {port_num} 已被占用，请关闭占用程序或修改配置后重启。"
            error = str(port_status.get("error") or "").strip()
            if error:
                message = f"{message} ({error})"
            self.receiver_log_box.append(message + "\n")
            self._append_debug_line(message, protocol=False)
            return
        save_dir = self.save_dir_input.text.strip() or str(user_data_dir() / "received")
        Path(save_dir).mkdir(parents=True, exist_ok=True)
        key_file = str(user_data_dir() / "rudp_receiver_ed25519.key")
        approval_dir = self.approval_dir
        approval_dir.mkdir(parents=True, exist_ok=True)
        self.seen_request_files.clear()
        for pattern in ("*.request.json", "*.accept", "*.reject"):
            for pth in approval_dir.glob(pattern):
                try:
                    pth.unlink()
                except Exception:
                    pass
        approval_timeout_text = self.approval_timeout_input.text.strip() or "300"
        try:
            idle_timeout_text = str(max(90.0, float(approval_timeout_text) + 60.0))
        except Exception:
            idle_timeout_text = "360"
        args = [
            "--bind", bind_host,
            "--port", str(port_num),
            "--save-dir", save_dir,
            "--discovery-port", self.recv_discovery_port.text.strip() or str(DEFAULT_DISCOVERY_PORT),
            "--server-id-key-file", key_file,
            "--require-approval",
            "--approval-dir", str(approval_dir),
            "--approval-timeout", approval_timeout_text,
            "--idle-timeout", idle_timeout_text,
            "--max-unacked-pkts", "1536",
        ]
        if self.chat_store is not None:
            args += ["--chat-db", self.chat_db_path, "--chat-password", self.chat_password, "--chat-local-peer-id", self.chat_local_peer_id, "--chat-local-nickname", self.chat_nickname, "--contact-approval-dir", str(self.contact_approval_dir)]
        allow_peer = self.allow_peer_input.text.strip()
        if allow_peer:
            args += ["--allow-peer-ip", allow_peer]
        name = self.receiver_name_input.text.strip()
        if name:
            args += ["--receiver-name", name]
        if self.once_checkbox.active:
            args.append("--once")
        self.receiver_worker.start(args)
        self.online_btn.text = "Online"
        style_button(self.online_btn, "success")
        self.receiver_log_box.append(self.t("started") + "\n")

    def allow_firewall(self) -> None:
        if not IS_WINDOWS:
            self.receiver_log_box.append("Firewall helper is Windows-only.\n")
            return
        script = RESOURCE_DIR / "allow_firewall_udp_9999_admin.bat"
        if not script.exists():
            self.receiver_log_box.append(f"Firewall script not found: {script}\n")
            return
        try:
            import ctypes
            rc = ctypes.windll.shell32.ShellExecuteW(None, "runas", str(script), None, str(RESOURCE_DIR), 1)
            self.receiver_log_box.append(f"Firewall helper started, ShellExecute rc={rc}.\n")
        except Exception as exc:
            self.receiver_log_box.append(f"Failed to start firewall helper: {exc}\n")

    def _display_user_error(self, code: str, detail: str = "", target: str = "sender") -> None:
        msg = file_error_message(code, detail, lang=self.lang, translate=self.t, detail_separator="\n")
        if target == "receiver":
            self.receiver_log_box.append(msg + "\n")
        else:
            self.sender_log_box.append(msg + "\n")
            self.last_sender_failure_code = str(code or "transfer_failed")
            self.retry_send_btn.disabled = False
            self.sender_log_box.append(self.t("retry_ready") + "\n")
            self._update_latest_file_card_error(str(code or "transfer_failed"), str(detail or ""))

    def _try_parse_user_event(self, text: str, target: str) -> bool:
        marker = USER_ERROR_LOG_PREFIX
        if marker in text:
            payload = text.split(marker, 1)[1].strip()
            try:
                obj = json.loads(payload)
            except Exception:
                return False
            self._display_user_error(str(obj.get("code") or "transfer_failed"), str(obj.get("detail") or obj.get("message") or ""), target=target)
            return True
        marker = USER_STATUS_LOG_PREFIX
        if marker in text:
            payload = text.split(marker, 1)[1].strip()
            try:
                obj = json.loads(payload)
            except Exception:
                return False
            if str(obj.get("code") or "") == "resume_enabled":
                resume_offset = int(obj.get("resume_offset") or 0)
                msg = self.t("resume_enabled", offset=format_file_size(resume_offset))
                if target == "receiver":
                    self.receiver_log_box.append(msg + "\n")
                else:
                    self.sender_log_box.append(msg + "\n")
                    mid = self._latest_outgoing_file_message_id()
                    if mid:
                        ctx = dict(self.file_message_tasks.get(mid) or {})
                        recipients = [dict(r) for r in (ctx.get("recipients") or [])]
                        peer_id = str((recipients[0] or {}).get("peer_id") or "") if recipients else ""
                        total = int(ctx.get("total") or self.file_message_progress.get(mid, {}).get("total") or 0)
                        self._add_file_transfer_chat_card(
                            message_id=mid,
                            peer_id=peer_id,
                            group_id=str(ctx.get("group_id") or ""),
                            conversation_id=str(ctx.get("conversation_id") or ""),
                            direction="outgoing",
                            transferred=resume_offset,
                            total=total,
                            pct=(resume_offset * 100.0 / total) if total else 0.0,
                            status=self._file_resume_text(resume_offset),
                            detail=self._file_size_detail(total, peer_label=self._file_peer_label(peer_id)),
                        )
                return True
        return False

    def sender_log(self, text: str) -> None:
        is_progress = self._is_transfer_progress_line(text)
        self._append_debug_line(text, protocol=any(marker in str(text or "") for marker in (
            CHAT_MESSAGE_LOG_PREFIX, CHAT_ACK_LOG_PREFIX, CHAT_READ_LOG_PREFIX,
            CONTACT_REQUEST_LOG_PREFIX, CONTACT_RESPONSE_LOG_PREFIX, TRANSFER_REQUEST_LOG_PREFIX,
            TRANSFER_STARTED_LOG_PREFIX, TRANSFER_PROGRESS_LOG_PREFIX,
            TRANSFER_SAVED_LOG_PREFIX, TRANSFER_COMPLETE_LOG_PREFIX, TRANSFER_FAILED_LOG_PREFIX,
            USER_ERROR_LOG_PREFIX, USER_STATUS_LOG_PREFIX, "LAN_STATS ", "SUMMARY_STATS ", "WIFI_GUARD ", "ADAPTIVE_WIFI_STATS ",
        )))
        tev = self._parse_transfer_event_line(text)
        if tev:
            Clock.schedule_once(lambda _dt, data=tev: self._handle_transfer_event_ui(data, source="sender"), 0)
        if not is_progress:
            self.sender_log_box.append(text)
        self._try_parse_user_event(text, "sender")
        if CONTACT_RESPONSE_LOG_PREFIX in text:
            try:
                payload = text.split(CONTACT_RESPONSE_LOG_PREFIX, 1)[1].strip()
                obj = json.loads(payload)
                self._handle_contact_response(obj)
            except Exception as exc:
                self.sender_log_box.append(f"Failed to parse contact response: {exc}\n")


    def poll_approval_requests(self, _dt=None) -> bool:
        try:
            self.approval_dir.mkdir(parents=True, exist_ok=True)
            files = sorted(self.approval_dir.glob("*.request.json"), key=lambda p: p.stat().st_mtime)
        except Exception:
            return True
        for pth in files:
            key = str(pth)
            if key in self.seen_request_files:
                continue
            try:
                req = json.loads(pth.read_text(encoding="utf-8"))
            except Exception:
                continue
            conn_id = int(req.get("conn_id") or 0)
            if conn_id in self.pending_request_popups:
                continue
            self.seen_request_files.add(key)
            self.show_transfer_request(req)
        return True

    def poll_contact_requests(self, _dt=None) -> bool:
        try:
            self.contact_approval_dir.mkdir(parents=True, exist_ok=True)
            files = sorted(self.contact_approval_dir.glob("*.request.json"), key=lambda p: p.stat().st_mtime)
        except Exception:
            return True
        for pth in files:
            key = "contact:" + str(pth)
            if key in self.seen_request_files:
                continue
            try:
                req = json.loads(pth.read_text(encoding="utf-8"))
            except Exception:
                continue
            self.seen_request_files.add(key)
            self.show_contact_request(req)
        return True

    def show_contact_request(self, req: Dict[str, object]) -> None:
        req_id = str(req.get("request_id") or "")
        if not req_id:
            return
        name = str(req.get("sender_nickname") or req.get("sender_peer_id") or "Unknown")
        pid = str(req.get("sender_peer_id") or "")
        ip = normalize_peer_endpoint_ip(str(req.get("sender_ip") or ""), fallback=str((req.get("sender_addr") or [""])[0] if isinstance(req.get("sender_addr"), list) else ""))
        try:
            port = int(req.get("sender_port") or 9999)
        except Exception:
            port = 9999
        fp = str(req.get("sender_fingerprint") or pid)
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        msg = f"{name} 请求添加联系人\npeer_id: {pid}\nfingerprint: {fp}\n地址: {ip}:{port}\n\n是否允许？"
        lab = make_label(text=msg, halign="left", valign="middle")
        bind_label_wrap(lab)
        content.add_widget(lab)
        popup = style_popup(Popup(title="联系人申请", content=content, size_hint=(0.62, 0.52), auto_dismiss=False))
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        def _decision(accepted: bool):
            target = self.contact_approval_dir / (f"{req_id}.accept" if accepted else f"{req_id}.reject")
            try:
                target.write_text(json.dumps({"accepted": accepted}, ensure_ascii=False), encoding="utf-8")
            except Exception as exc:
                self.receiver_log_box.append(f"Failed to write contact decision: {exc}\n")
            if accepted and self.contact_service is not None and pid:
                try:
                    self.contact_service.save_accepted_contact(
                        peer_id=pid,
                        nickname=name,
                        fingerprint=fp,
                        peer_ip=ip,
                        peer_port=port,
                    )
                    self.refresh_chat_main()
                except Exception as exc:
                    self.receiver_log_box.append(f"Failed to save contact: {exc}\n")
            popup.dismiss()
        buttons.add_widget(make_button("success", text="允许", on_release=lambda *_: _decision(True)))
        buttons.add_widget(make_button("danger", text="拒绝", on_release=lambda *_: _decision(False)))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def _peer_ip_from_log_obj(self, obj: Dict[str, object]) -> str:
        peer_text = str(obj.get("peer") or obj.get("sender") or "")
        ip = str(obj.get("sender_ip") or "").strip()
        if not ip and peer_text:
            ip = peer_text.split(":", 1)[0].strip()
        return ip

    def _incoming_matches_current_direct(self, obj: Dict[str, object]) -> bool:
        if self.current_chat_mode != "direct" or not self.current_peer_id:
            return False
        if str(obj.get("group_id") or obj.get("chat_group_id") or ""):
            return False

        incoming_ip = self._peer_ip_from_log_obj(obj)
        current_ip, _port = self._endpoint_for_peer(self.current_peer_id)

        # If the sender peer_id matches the current chat, this is a direct match.
        sender = str(obj.get("sender_peer_id") or obj.get("chat_sender_peer_id") or "")
        if sender and sender == self.current_peer_id:
            return True

        # If the incoming IP maps to a known contact, only attach it to that contact.
        # This avoids stealing messages from another peer while still tolerating
        # nickname/fingerprint/peer_id migrations for the current contact.
        if incoming_ip and self.contact_service is not None:
            try:
                for c in self.contact_service.list_contacts(trusted_only=False):
                    contact_ip = str(c.get("peer_ip") or "").strip()
                    contact_pid = str(c.get("peer_id") or "").strip()
                    if contact_ip and contact_ip == incoming_ip:
                        return contact_pid == self.current_peer_id
            except Exception:
                pass

        # If the current contact has the same IP, treat it as the current chat even
        # when the transmitted peer_id differs from the locally stored peer_id.
        if incoming_ip and current_ip and incoming_ip == current_ip:
            return True

        # Recovery fallback: if a direct chat is actively open and the incoming log
        # cannot be mapped to any other known contact, bind it to the visible chat.
        # This keeps the receiver UI usable when old databases saved peer_id as a
        # nickname but new messages use a stable peer_* identity.
        if incoming_ip:
            return True
        if sender:
            try:
                if self.contact_service is not None and self.contact_service.find_contact(sender):
                    return False
            except Exception:
                pass
        return True


    def _normalize_incoming_chat_for_visible_context(self, obj: Dict[str, object]) -> Dict[str, object]:
        fixed = dict(obj or {})
        if self._incoming_matches_current_direct(fixed):
            fixed["sender_peer_id"] = self.current_peer_id
            fixed["receiver_peer_id"] = self.chat_local_peer_id
            if self.message_service is not None:
                fixed["conversation_id"] = self.message_service.create_direct_conversation(self.current_peer_id)
        return fixed

    def _normalize_transfer_request_for_visible_context(self, req: Dict[str, object]) -> Dict[str, object]:
        fixed = dict(req or {})
        if self.current_chat_mode == "direct" and self.current_peer_id and not str(fixed.get("chat_group_id") or ""):
            incoming_ip = str(fixed.get("sender_ip") or "").strip()
            current_ip, _port = self._endpoint_for_peer(self.current_peer_id)
            if (incoming_ip and current_ip and incoming_ip == current_ip) or not str(fixed.get("chat_sender_peer_id") or ""):
                fixed["chat_sender_peer_id"] = self.current_peer_id
                fixed["chat_receiver_peer_id"] = self.chat_local_peer_id
                if self.message_service is not None:
                    fixed["chat_conversation_id"] = self.message_service.create_direct_conversation(self.current_peer_id)
        return fixed

    def _file_path_from_transfer_store(self, message_id: str) -> str:
        if self.file_transfer_service is None:
            return ""
        try:
            row = self.file_transfer_service.progress_for_message(str(message_id or "")) or {}
            for key in ("local_path", "remote_path"):
                value = str(row.get(key) or "")
                if value:
                    return value
        except Exception:
            pass
        return ""

    def _schedule_force_chat_refresh(self, *delays: float, reason: str = "manual_refresh") -> None:
        if not delays:
            delays = (0.0,)
        generation = getattr(self, "_chat_render_generation", 0)
        for delay in delays:
            try:
                Clock.schedule_once(
                    lambda _dt, gen=generation, why=reason: self._force_chat_refresh(reason=why, generation=gen),
                    max(0.0, float(delay or 0.0)),
                )
            except Exception:
                pass

    def _handle_incoming_chat_read_ui(self, obj: Dict[str, object]) -> None:
        try:
            if self.message_service is not None:
                self.message_service.mark_read(str(obj.get("message_id") or ""), str(obj.get("reader_peer_id") or ""))
        except Exception as exc:
            try:
                self.receiver_log_box.append(f"Failed to apply CHAT_READ in UI DB: {exc}\n")
            except Exception:
                pass

    def _handle_incoming_chat_message_ui(self, obj: Dict[str, object]) -> None:
        """Handle a CHAT_MESSAGE log on the Kivy/UI thread.

        The receiver worker reads stdout on a background thread. SQLite objects in
        chat_db.py are created on the UI thread, so writing incoming messages from
        the reader thread can fail silently or be swallowed by the old broad
        exception handler.  All chat DB writes must be performed here on the main
        Kivy thread.
        """
        try:
            obj = self._normalize_incoming_chat_for_visible_context(dict(obj or {}))
            self._mark_chat_json_seen(obj)
            is_screen_control = self._is_screen_control_chat(obj)
            # Cache first so the currently opened chat can display the message
            # even if SQLite saving fails or an old DB has inconsistent peer IDs.
            if not is_screen_control:
                self._cache_live_incoming_message(obj)
            if self.message_service is not None:
                try:
                    self.message_service.save_incoming_message(obj, local_peer_id=self.chat_local_peer_id)
                except Exception as exc:
                    try:
                        self.receiver_log_box.append(f"Failed to save incoming chat message in UI DB: {exc}\n")
                        self._append_debug_line(f"Failed to save incoming chat message in UI DB: {exc}", protocol=True)
                    except Exception:
                        pass
            if is_screen_control:
                self._handle_screen_control_from_chat(obj)
                return
            try:
                self.chat_messages_box.append(f"Incoming {obj.get('group_id')}: {obj.get('sender_peer_id')}: {obj.get('text')}\n")
            except Exception:
                pass
            if str(obj.get("body_type") or "") == "file":
                mid = str(obj.get("message_id") or "")
                total = 0
                body_name = ""
                try:
                    body = json.loads(str(obj.get("text") or ""))
                    total = int(body.get("size") or 0)
                    body_name = str(body.get("name") or "")
                except Exception:
                    pass
                if mid:
                    self.current_receiving_file_message_id = mid
                    self.file_message_progress[mid] = {"sent": 0, "total": total, "pct": 0.0, "avg": 0.0, "eta": "", "state": self.cu("waiting_receive")}
                    if self.file_transfer_service is not None:
                        self._run_transfer_store_write(
                            "incoming file task upsert",
                            lambda mid=mid, obj=dict(obj), body_name=body_name, total=total: self.file_transfer_service.upsert_incoming_task(
                                chat_message_id=mid,
                                peer_id=str(obj.get("sender_peer_id") or ""),
                                conversation_id=str(obj.get("conversation_id") or ""),
                                group_id=str(obj.get("group_id") or ""),
                                file_name=body_name,
                                total_bytes=total,
                                status="queued",
                            ),
                        )
                    self._add_file_offer_chat_card(
                        message_id=mid,
                        peer_id=str(obj.get("sender_peer_id") or ""),
                        group_id=str(obj.get("group_id") or ""),
                        conversation_id=str(obj.get("conversation_id") or ""),
                        file_name=body_name,
                        total_size=total,
                        status=self._file_incoming_text(self._file_peer_label(str(obj.get("sender_peer_id") or ""))),
                        detail=self._file_size_detail(total, peer_label=self._file_peer_label(str(obj.get("sender_peer_id") or ""))),
                    )
                    self._schedule_transfer_card_refresh(force=True)
                return
            self._append_text_message_live(obj)
        except Exception as exc:
            try:
                self.receiver_log_box.append(f"CHAT_MESSAGE UI handler failed: {exc}\n")
            except Exception:
                pass

    def _handle_receiver_saved_file_ui(self, conn: int, saved_path: str) -> None:
        try:
            saved_path = str(saved_path or "").strip()
            if not saved_path:
                return
            mid = str(self.receiving_file_message_by_conn.get(int(conn or 0)) or self.current_receiving_file_message_id or "")
            if mid and self.message_service is not None:
                self.message_service.bind_file_path(mid, saved_path)
            elif self.message_service is not None:
                mid = self.message_service.bind_latest_incoming_file_path(file_name=os.path.basename(saved_path), saved_path=saved_path)
            if mid:
                total = os.path.getsize(saved_path) if os.path.exists(saved_path) else int(self.file_message_progress.get(mid, {}).get("total") or 0)
                self.file_message_progress[mid] = {"sent": total, "total": total, "pct": 100.0, "avg": self.file_message_progress.get(mid, {}).get("avg", 0.0), "eta": "0:00", "state": self.cu("received")}
                self._add_file_transfer_chat_card(
                    message_id=mid,
                    direction="incoming",
                    transferred=total,
                    total=total,
                    pct=100.0,
                    avg=float(self.file_message_progress.get(mid, {}).get("avg") or 0.0),
                    eta="0:00",
                    status=self._file_completed_text(),
                    saved_path=saved_path,
                )
                if self.file_transfer_service is not None:
                    self._run_transfer_store_write(
                        "receiver saved-path bind",
                        lambda mid=mid, saved_path=saved_path: self.file_transfer_service.bind_saved_path(mid, saved_path),
                    )
            self.receiving_file_message_by_conn.pop(int(conn or 0), None)
            if self.current_receiving_file_message_id == mid:
                self.current_receiving_file_message_id = ""
            self._schedule_transfer_card_refresh(force=True)
        except Exception as exc:
            try:
                self.receiver_log_box.append(f"saved-file UI handler failed: {exc}\n")
            except Exception:
                pass

    def _handle_receiver_complete_ui(self, message_id: str = "") -> None:
        try:
            mids = set()
            if message_id:
                mids.add(str(message_id))
            if self.current_receiving_file_message_id:
                mids.add(str(self.current_receiving_file_message_id))
            for _conn, _mid in list(self.receiving_file_message_by_conn.items()):
                if _mid:
                    mids.add(str(_mid))
            for mid in mids:
                saved_path = self._file_path_from_transfer_store(mid)
                if saved_path and self.message_service is not None:
                    self.message_service.bind_file_path(mid, saved_path)
                total = 0
                try:
                    total = os.path.getsize(saved_path) if saved_path and os.path.exists(saved_path) else int(self.file_message_progress.get(mid, {}).get("total") or 0)
                except Exception:
                    total = int(self.file_message_progress.get(mid, {}).get("total") or 0)
                if total:
                    self.file_message_progress[mid] = {
                        "sent": total,
                        "total": total,
                        "pct": 100.0,
                        "avg": self.file_message_progress.get(mid, {}).get("avg", 0.0),
                        "eta": "0:00",
                        "state": self.cu("received"),
                    }
                    self._add_file_transfer_chat_card(
                        message_id=mid,
                        direction="incoming",
                        transferred=total,
                        total=total,
                        pct=100.0,
                        avg=float(self.file_message_progress.get(mid, {}).get("avg") or 0.0),
                        eta="0:00",
                        status=self._file_completed_text(),
                        saved_path=saved_path,
                    )
                    if self.file_transfer_service is not None:
                        self._run_transfer_store_write(
                            "receiver complete progress update",
                            lambda mid=mid, total=total: self.file_transfer_service.update_progress(
                                chat_message_id=mid,
                                direction="incoming",
                                transferred_bytes=total,
                                total_bytes=total,
                                pct=100.0,
                                eta="0:00",
                                status="received",
                            ),
                        )
            self.receiving_file_message_by_conn.clear()
            self.current_receiving_file_message_id = ""
            self._schedule_transfer_card_refresh(force=True)
        except Exception as exc:
            try:
                self.receiver_log_box.append(f"complete UI handler failed: {exc}\n")
            except Exception:
                pass

    def _handle_transfer_request_ui(self, req: Dict[str, object]) -> None:
        try:
            req = self._normalize_transfer_request_for_visible_context(dict(req or {}))
            conn = int(req.get("conn_id") or 0)
            mid = str(req.get("chat_message_id") or "")
            total = int(req.get("size") or 0)
            self._show_transfer_request_card(req)
            if mid and self.message_service is not None:
                try:
                    self.message_service.ensure_incoming_file_placeholder_from_transfer_request(req, local_peer_id=self.chat_local_peer_id)
                except Exception as exc:
                    try:
                        self.receiver_log_box.append(f"Failed to create incoming file card: {exc}\n")
                    except Exception:
                        pass
            if conn > 0 and mid:
                self.receiving_file_message_by_conn[conn] = mid
                self.current_receiving_file_message_id = mid
                self.file_message_progress[mid] = {
                    "sent": 0,
                    "total": total,
                    "pct": 0.0,
                    "avg": 0.0,
                    "eta": "",
                    "state": self.cu("waiting_receive"),
                }
                if self.file_transfer_service is not None:
                    self._run_transfer_store_write(
                        "transfer request task upsert",
                        lambda mid=mid, req=dict(req), total=total: self.file_transfer_service.upsert_incoming_task(
                            chat_message_id=mid,
                            file_name=str(req.get("name") or ""),
                            local_path=str(req.get("save_path") or ""),
                            remote_path=str(req.get("save_path") or ""),
                            total_bytes=total,
                            status="offered",
                        ),
                    )
                self._schedule_transfer_card_refresh(force=True)
            self.show_transfer_request(req)
        except Exception as exc:
            try:
                self.receiver_log_box.append(f"transfer request UI handler failed: {exc}\n")
            except Exception:
                pass

    def _append_debug_line(self, line: str, *, protocol: bool = False) -> None:
        try:
            s = str(line or "").rstrip("\n")
            if not s:
                return
            # TRANSFER_PROGRESS_JSON is high-frequency during large transfers.
            # Keep only the latest copy in memory; do not append it to LogBox/debug lists.
            if TRANSFER_PROGRESS_LOG_PREFIX in s:
                self._last_transfer_progress_line = s
                return
            self.debug_runtime_lines.append(s)
            if len(self.debug_runtime_lines) > 1000:
                self.debug_runtime_lines = self.debug_runtime_lines[-1000:]
            if protocol or any(marker in s for marker in (
                CHAT_MESSAGE_LOG_PREFIX,
                CHAT_ACK_LOG_PREFIX,
                CHAT_READ_LOG_PREFIX,
                CONTACT_REQUEST_LOG_PREFIX,
                CONTACT_RESPONSE_LOG_PREFIX,
                TRANSFER_REQUEST_LOG_PREFIX,
                TRANSFER_STARTED_LOG_PREFIX,
                TRANSFER_SAVED_LOG_PREFIX,
                TRANSFER_COMPLETE_LOG_PREFIX,
                TRANSFER_FAILED_LOG_PREFIX,
                USER_ERROR_LOG_PREFIX,
                USER_STATUS_LOG_PREFIX,
                "LAN_STATS ",
            )):
                self.debug_protocol_lines.append(s)
                if len(self.debug_protocol_lines) > 1000:
                    self.debug_protocol_lines = self.debug_protocol_lines[-1000:]
        except Exception:
            pass


    def _live_cache_key_for_message(self, obj: Dict[str, object]) -> str:
        group_id = str(obj.get("group_id") or "")
        if group_id:
            return "group:" + group_id
        sender = str(obj.get("sender_peer_id") or "")
        if self.current_chat_mode == "direct" and self.current_peer_id:
            sender = self.current_peer_id
        return "direct:" + sender

    def _cache_live_incoming_message(self, obj: Dict[str, object]) -> None:
        try:
            mid = str(obj.get("message_id") or "")
            if not mid:
                return
            key = self._live_cache_key_for_message(obj)
            arr = self.live_message_cache.setdefault(key, [])
            if not any(str(m.get("message_id") or "") == mid for m in arr):
                arr.append(dict(obj))
            if len(arr) > 100:
                self.live_message_cache[key] = arr[-100:]
        except Exception:
            pass

    def _mark_chat_json_seen(self, obj: Dict[str, object]) -> None:
        try:
            sender = str(obj.get("sender_peer_id") or obj.get("chat_sender_peer_id") or "").strip()
            body = str(obj.get("text") or "").strip()
            if not sender or not body:
                return
            now = time.time()
            self._recent_chat_json_seen[(sender, body)] = now
            # Keep the cache small and recent.
            for k, ts in list(self._recent_chat_json_seen.items()):
                if now - float(ts or 0.0) > 8.0:
                    self._recent_chat_json_seen.pop(k, None)
        except Exception:
            pass

    def _was_chat_json_recently_seen(self, sender: str, body: str) -> bool:
        try:
            sender = str(sender or "").strip()
            body = str(body or "").strip()
            if not sender or not body:
                return False
            ts = float(self._recent_chat_json_seen.get((sender, body)) or 0.0)
            return bool(ts and (time.time() - ts) <= 8.0)
        except Exception:
            return False

    def _live_messages_for_current_chat(self, seen_ids: set) -> List[Dict[str, object]]:
        try:
            if self.current_chat_mode == "group" and self.current_group_id:
                key = "group:" + self.current_group_id
            elif self.current_chat_mode == "direct" and self.current_peer_id:
                key = "direct:" + self.current_peer_id
            else:
                return []
            result = []
            for m in self.live_message_cache.get(key, []) or []:
                mid = str(m.get("message_id") or "")
                if mid and mid not in seen_ids:
                    result.append(dict(m))
            return result
        except Exception:
            return []

    def _is_transfer_progress_line(self, text: str) -> bool:
        return TRANSFER_PROGRESS_LOG_PREFIX in str(text or "")

    def _transfer_key_for_event(self, mid: str, direction: str, peer_id: str) -> str:
        return f"{str(mid or '')}:{str(direction or '')}:{str(peer_id or '')}"

    def _should_persist_transfer_event(self, *, mid: str, direction: str, peer_id: str, transferred: int, status: str) -> bool:
        if not mid:
            return False
        if status in ("received", "completed", "failed", "saved"):
            return True
        key = self._transfer_key_for_event(mid, direction, peer_id)
        last = int(self._transfer_store_write_bytes.get(key, 0) or 0)
        if int(transferred or 0) - last >= 16 * 1024 * 1024:
            self._transfer_store_write_bytes[key] = int(transferred or 0)
            return True
        return False

    def _schedule_transfer_card_refresh(self, *, force: bool = False) -> None:
        def _refresh(_dt):
            try:
                self._transfer_refresh_scheduled = False
                self._last_transfer_card_refresh_ts = time.time()
                self._sync_runtime_chat_cards()
            except Exception:
                pass

        min_interval = 0.20
        if force:
            # This method is often called from worker threads. All Kivy widget
            # and canvas updates must be marshalled back to the main thread.
            self._transfer_refresh_scheduled = False
            Clock.schedule_once(_refresh, 0.05)
            return

        if self._transfer_refresh_scheduled:
            return
        now = time.time()
        delay = max(0.0, min_interval - (now - float(self._last_transfer_card_refresh_ts or 0.0)))
        self._transfer_refresh_scheduled = True
        Clock.schedule_once(_refresh, delay)

    def _parse_transfer_event_line(self, text: str) -> Dict[str, object]:
        raw = str(text or "")
        for prefix in (
            TRANSFER_STARTED_LOG_PREFIX,
            TRANSFER_PROGRESS_LOG_PREFIX,
            TRANSFER_SAVED_LOG_PREFIX,
            TRANSFER_COMPLETE_LOG_PREFIX,
            TRANSFER_FAILED_LOG_PREFIX,
        ):
            if prefix in raw:
                try:
                    return json.loads(raw.split(prefix, 1)[1].strip())
                except Exception:
                    return {}
        return {}

    def _handle_transfer_event_ui(self, obj: Dict[str, object], *, source: str = "") -> bool:
        if not isinstance(obj, dict) or not obj:
            return False
        typ = str(obj.get("type") or "")
        if not typ.startswith("TRANSFER_"):
            return False

        mid = str(obj.get("chat_message_id") or "")
        conn = int(obj.get("conn_id") or 0)
        if conn and mid:
            self.receiving_file_message_by_conn[conn] = mid
            if source == "receiver":
                self.current_receiving_file_message_id = mid

        direction = self._file_card_direction(
            mid,
            str(obj.get("direction") or ("incoming" if source == "receiver" else "outgoing")),
        )
        peer_id = str(obj.get("peer_id") or "")
        transferred = int(obj.get("transferred_bytes") or obj.get("bytes_recv") or obj.get("bytes_sent") or 0)
        total = int(obj.get("total_bytes") or obj.get("size") or 0)
        pct = float(obj.get("pct") or ((transferred * 100.0 / total) if total else 0.0))
        current = float(obj.get("current_mbps") or obj.get("interval_mbps") or 0.0)
        avg = float(obj.get("avg_mbps") or 0.0)
        peak = float(obj.get("peak_mbps") or current or 0.0)
        elapsed = float(obj.get("elapsed_sec") or 0.0)
        eta = str(obj.get("eta") or "")
        status = str(obj.get("status") or "").strip().lower() or "transferring"
        if typ == "TRANSFER_STARTED":
            status = "started"
        elif typ == "TRANSFER_SAVED":
            status = "received"
        elif typ == "TRANSFER_COMPLETE":
            status = "received" if direction == "incoming" else "completed"
        elif typ == "TRANSFER_FAILED":
            status = "failed"

        if not mid:
            return True

        state_map = {
            "started": transfer_status_label("started", direction=direction, lang=self.lang, translate=self.cu),
            "transferring": transfer_status_label("transferring", direction=direction, lang=self.lang, translate=self.cu),
            "received": transfer_status_label("received", direction=direction, lang=self.lang, translate=self.cu),
            "completed": transfer_status_label("completed", direction=direction, lang=self.lang, translate=self.cu),
            "failed": self._file_failed_text(str(obj.get("error") or obj.get("reason") or "")),
        }
        latest = {
            "sent": transferred,
            "total": total,
            "pct": pct,
            "avg": avg,
            "current": current,
            "peak": peak,
            "elapsed": elapsed,
            "eta": eta,
            "state": state_map.get(status, status),
            "status": status,
        }
        self.file_message_progress[mid] = latest
        self.last_transfer_state[mid] = dict(obj)
        if len(self.last_transfer_state) > 200:
            # Keep only recent states to prevent unbounded memory growth.
            for k in list(self.last_transfer_state.keys())[:-200]:
                self.last_transfer_state.pop(k, None)
        self._add_file_transfer_chat_card(
            message_id=mid,
            peer_id=peer_id,
            group_id=str(obj.get("chat_group_id") or obj.get("group_id") or ""),
            conversation_id=str(obj.get("chat_conversation_id") or obj.get("conversation_id") or ""),
            direction=direction,
            transferred=transferred,
            total=total,
            pct=pct,
            avg=current or avg,
            eta=eta,
            status=state_map.get(status, status),
            error=str(obj.get("error") or ""),
            saved_path=str(obj.get("save_path") or obj.get("local_path") or ""),
        )

        persist = self._should_persist_transfer_event(
            mid=mid,
            direction=direction,
            peer_id=peer_id,
            transferred=transferred,
            status=status,
        )
        if persist and self.file_transfer_service is not None:
            self._run_transfer_store_write(
                "transfer_store progress update",
                lambda mid=mid, direction=direction, peer_id=peer_id, transferred=transferred, total=total, pct=pct, avg=avg, current=current, peak=peak, elapsed=elapsed, eta=eta, status=status, obj=dict(obj): self.file_transfer_service.update_progress(
                    chat_message_id=mid,
                    direction=direction,
                    peer_id=peer_id,
                    transferred_bytes=transferred,
                    total_bytes=total,
                    pct=pct,
                    avg_mbps=avg,
                    current_mbps=current,
                    peak_mbps=peak,
                    elapsed_sec=elapsed,
                    eta=eta,
                    status=status,
                    error=str(obj.get("error") or ""),
                ),
            )

        saved_path = str(obj.get("save_path") or obj.get("local_path") or "")
        if saved_path and self.message_service is not None and status in ("received", "completed"):
            try:
                self.message_service.bind_file_path(mid, saved_path)
                if self.file_transfer_service is not None:
                    self._run_transfer_store_write(
                        "transfer_store saved-path bind",
                        lambda mid=mid, saved_path=saved_path: self.file_transfer_service.bind_saved_path(mid, saved_path),
                    )
            except Exception as exc:
                self._append_debug_line(f"transfer saved-path bind failed: {exc}", protocol=True)

        if status in ("received", "completed", "failed"):
            if conn > 0:
                self.pending_request_popups.discard(conn)
                self.pending_transfer_requests.pop(conn, None)
                self.pending_transfer_decisions.pop(conn, None)
                pop = self.pending_transfer_popups.pop(conn, None)
                if pop is not None:
                    try:
                        pop.dismiss()
                    except Exception:
                        pass
            self._schedule_transfer_card_refresh(force=True)
        else:
            self._schedule_transfer_card_refresh(force=False)
        return True


    def receiver_log(self, text: str) -> None:
        is_progress = self._is_transfer_progress_line(text)
        self._append_debug_line(text, protocol=any(marker in str(text or "") for marker in (
            CHAT_MESSAGE_LOG_PREFIX, CHAT_ACK_LOG_PREFIX, CHAT_READ_LOG_PREFIX,
            CONTACT_REQUEST_LOG_PREFIX, CONTACT_RESPONSE_LOG_PREFIX, TRANSFER_REQUEST_LOG_PREFIX,
            TRANSFER_STARTED_LOG_PREFIX, TRANSFER_PROGRESS_LOG_PREFIX,
            TRANSFER_SAVED_LOG_PREFIX, TRANSFER_COMPLETE_LOG_PREFIX, TRANSFER_FAILED_LOG_PREFIX,
            USER_ERROR_LOG_PREFIX, USER_STATUS_LOG_PREFIX, "SUMMARY_STATS ", "WIFI_GUARD ",
        )))
        tev = self._parse_transfer_event_line(text)
        if tev:
            Clock.schedule_once(lambda _dt, data=tev: self._handle_transfer_event_ui(data, source="receiver"), 0)
        if not is_progress:
            self.receiver_log_box.append(text)
        self._try_parse_user_event(text, "receiver")
        if CONTACT_REQUEST_LOG_PREFIX in text:
            try:
                payload = text.split(CONTACT_REQUEST_LOG_PREFIX, 1)[1].strip()
                req = json.loads(payload)
                Clock.schedule_once(lambda _dt, data=req: self.show_contact_request(data), 0)
            except Exception:
                pass
        if CONTACT_RESPONSE_LOG_PREFIX in text:
            self.sender_log_box.append(text)
        if CHAT_READ_LOG_PREFIX in text:
            try:
                payload = text.split(CHAT_READ_LOG_PREFIX, 1)[1].strip()
                obj = json.loads(payload)
                Clock.schedule_once(lambda _dt, data=obj: self._handle_incoming_chat_read_ui(data), 0)
            except Exception:
                pass
        if CHAT_MESSAGE_LOG_PREFIX in text:
            try:
                payload = text.split(CHAT_MESSAGE_LOG_PREFIX, 1)[1].strip()
                obj = json.loads(payload)
                Clock.schedule_once(lambda _dt, data=obj: self._handle_incoming_chat_message_ui(data), 0)
            except Exception as exc:
                try:
                    self.receiver_log_box.append(f"Failed to parse CHAT_MESSAGE_JSON: {exc}\n")
                except Exception:
                    pass
        if TRANSFER_REQUEST_LOG_PREFIX in text:
            try:
                payload = text.split(TRANSFER_REQUEST_LOG_PREFIX, 1)[1].strip()
                req = json.loads(payload)
                Clock.schedule_once(lambda _dt, data=req: self.show_transfer_request(data), 0)
            except Exception as exc:
                try:
                    self.receiver_log_box.append(f"Failed to parse TRANSFER_REQUEST_JSON: {exc}\n")
                except Exception:
                    pass
        # Do not parse "Chat from ..." as a message. That line is a human-readable
        # server log emitted after CHAT_MESSAGE_JSON and must not create a second
        # live_* message. If CHAT_MESSAGE_JSON is absent, keep it as a diagnostic
        # problem instead of inventing a UI message.
        m_recv = RECEIVE_PROGRESS_RE.search(text)
        if m_recv and not is_progress:
            try:
                conn = int(m_recv.group("conn") or 0)
                mid = str(self.receiving_file_message_by_conn.get(conn) or self.current_receiving_file_message_id or "")
                if mid:
                    sent = int(m_recv.group("sent"))
                    total = int(m_recv.group("total") or 0)
                    pct = float(m_recv.group("pct") or ((sent * 100.0 / total) if total else 0.0))
                    self.file_message_progress[mid] = {"sent": sent, "total": total, "pct": pct, "avg": float(m_recv.group("avg") or 0.0), "eta": m_recv.group("eta") or "", "state": self.cu("receiving")}
                    self._schedule_file_transfer_chat_card(
                        message_id=mid,
                        direction="incoming",
                        transferred=sent,
                        total=total,
                        pct=pct,
                        avg=float(m_recv.group("avg") or 0.0),
                        eta=m_recv.group("eta") or "",
                        status=self.cu("receiving"),
                    )
                    self._schedule_transfer_card_refresh(force=False)
            except Exception:
                pass
        m_saved = RECEIVED_SAVE_RE.search(text)
        if m_saved and self.message_service is not None:
            try:
                conn = int(m_saved.group("conn") or 0)
                saved_path = str(m_saved.group("path") or "").strip()
                if saved_path:
                    mid = str(self.receiving_file_message_by_conn.get(conn) or self.current_receiving_file_message_id or "")
                    if mid:
                        self.message_service.bind_file_path(mid, saved_path)
                    else:
                        mid = self.message_service.bind_latest_incoming_file_path(file_name=os.path.basename(saved_path), saved_path=saved_path)
                    if mid:
                        total = os.path.getsize(saved_path) if os.path.exists(saved_path) else int(self.file_message_progress.get(mid, {}).get("total") or 0)
                        self.file_message_progress[mid] = {"sent": total, "total": total, "pct": 100.0, "avg": self.file_message_progress.get(mid, {}).get("avg", 0.0), "eta": "0:00", "state": self.cu("received")}
                        self._schedule_file_transfer_chat_card(
                            message_id=mid,
                            direction="incoming",
                            transferred=total,
                            total=total,
                            pct=100.0,
                            avg=float(self.file_message_progress.get(mid, {}).get("avg") or 0.0),
                            eta="0:00",
                            status=self._file_completed_text(),
                            saved_path=saved_path,
                        )
                        if self.file_transfer_service is not None:
                            self._run_transfer_store_write(
                                "receiver log saved-path bind",
                                lambda mid=mid, saved_path=saved_path: self.file_transfer_service.bind_saved_path(mid, saved_path),
                            )
                    self.receiving_file_message_by_conn.pop(conn, None)
                    self.pending_request_popups.discard(conn)
                    self.pending_transfer_requests.pop(conn, None)
                    self.pending_transfer_decisions.pop(conn, None)
                    pop = self.pending_transfer_popups.pop(conn, None)
                    if pop is not None:
                        try:
                            pop.dismiss()
                        except Exception:
                            pass
                    if self.current_receiving_file_message_id == mid:
                        self.current_receiving_file_message_id = ""
                    self._schedule_transfer_card_refresh(force=True)
            except Exception:
                pass
        if "end reason=complete" in text and self.current_receiving_file_message_id:
            Clock.schedule_once(lambda _dt, mid=self.current_receiving_file_message_id: self._handle_receiver_complete_ui(mid), 0)


    def show_transfer_request(self, req: Dict[str, object]) -> None:
        req = self._normalize_transfer_request_for_visible_context(dict(req or {}))
        conn_id = int(req.get("conn_id") or 0)
        try:
            mid = str(req.get("chat_message_id") or "")
            total = int(req.get("size") or 0)
            self._show_transfer_request_card(req)
            if mid and self.message_service is not None:
                try:
                    self.message_service.ensure_incoming_file_placeholder_from_transfer_request(req, local_peer_id=self.chat_local_peer_id)
                except Exception as exc:
                    try:
                        self.receiver_log_box.append(f"Failed to create incoming file card: {exc}\n")
                    except Exception:
                        pass
            if conn_id > 0 and mid:
                self.receiving_file_message_by_conn[conn_id] = mid
                self.current_receiving_file_message_id = mid
                self.file_message_progress.setdefault(mid, {"sent": 0, "total": total, "pct": 0.0, "avg": 0.0, "eta": "", "state": self.cu("waiting_receive")})
                if self.file_transfer_service is not None:
                    self._run_transfer_store_write(
                        "transfer popup task upsert",
                        lambda mid=mid, req=dict(req), total=total: self.file_transfer_service.upsert_incoming_task(
                            chat_message_id=mid,
                            file_name=str(req.get("name") or ""),
                            remote_path=str(req.get("save_path") or ""),
                            total_bytes=total,
                            status="offered",
                        ),
                    )
                self._schedule_transfer_card_refresh(force=True)
        except Exception:
            pass
        if conn_id > 0:
            pop = self.pending_transfer_popups.pop(conn_id, None)
            if pop is not None:
                try:
                    pop.dismiss()
                except Exception:
                    pass
            self.pending_request_popups.discard(conn_id)
        return

    def sender_exit(self, rc) -> None:
        self.sender_log_box.append(f"Process exited, rc={rc}\n")
        if int(rc or 0) == 0:
            self.sender_log_box.append(self.t("transfer_finished") + "\n")
            self.retry_send_btn.disabled = True
        else:
            if not self.last_sender_failure_code:
                self._display_user_error("transfer_failed", "", target="sender")
            else:
                self.retry_send_btn.disabled = False

    def receiver_exit(self, rc) -> None:
        self.receiver_log_box.append(f"Process exited, rc={rc}\n")

    def sender_progress(self, data: Dict[str, object]) -> None:
        def _update(_dt):
            sent = int(data.get("sent") or 0)
            total = int(data.get("total") or 0)
            pct = float(data.get("pct") or 0.0)
            avg = float(data.get("avg") or 0.0)
            eta = str(data.get("eta") or self.t("unknown"))
            self.update_progress_labels(sent, total, pct, eta, avg)
        Clock.schedule_once(_update, 0)

    def update_progress_labels(self, sent: int, total: int, pct: float, eta: str, avg: float) -> None:
        self.progress.value = max(0.0, min(100.0, float(pct)))
        self.progress_label.text = f"{self.t('progress')}: {pct:.2f}%  {format_file_size(sent)} / {format_file_size(total)}"
        self.eta_label.text = self.t("eta", eta=eta or self.t("unknown"))
        self.speed_label.text = self.t("speed", speed=f"{avg:.2f} Mbps") + "    " + self.t("size", size=format_file_size(total))

    def on_stop(self) -> None:
        self.sender_worker.stop()
        self.receiver_worker.stop()
        if self.chat_store is not None:
            try:
                self.chat_store.close()
            except Exception:
                pass
        if self.transfer_store is not None:
            try:
                self.transfer_store.close()
            except Exception:
                pass


class RUDPTransferApp(App):
    lang = StringProperty("zh")
    title = "AgoraLink"
    icon = str(RESOURCE_DIR / "assets" / "app.png")

    def build(self):
        Window.size = (1180, 760)
        Window.clearcolor = THEME["window_bg"]
        self.screen_runtime = ScreenRuntime()
        self.root_widget = RUDPTransferRoot(self)
        return self.root_widget

    def on_stop(self):
        try:
            if hasattr(self, "screen_runtime"):
                self.screen_runtime.stop()
        except Exception:
            pass
        try:
            self.root_widget.on_stop()
        except Exception:
            pass


if __name__ == "__main__":
    RUDPTransferApp().run()
