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
import re
import secrets
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple

APP_NAME = "AgoraLink"
IS_WINDOWS = os.name == "nt"
FROZEN = bool(getattr(sys, "frozen", False))
APP_DIR = Path(sys.executable).resolve().parent if FROZEN else Path(__file__).resolve().parent
RESOURCE_DIR = Path(getattr(sys, "_MEIPASS", APP_DIR))

PROGRESS_RE = re.compile(
    r"Progress:\s+(?P<sent>\d+)/(?:\s*)?(?P<total>\d+)\s+bytes\s+\((?P<pct>[0-9.]+)%\).*?"
    r"avg=(?P<avg>[0-9.]+)\s+Mbps.*?eta=(?P<eta>[^,\s]+)"
)
COMPLETE_RE = re.compile(r"Transfer complete:\s+(?P<total>\d+)\s+bytes.*?avg=(?P<avg>[0-9.]+)\s+Mbps")


def user_data_dir() -> Path:
    if IS_WINDOWS:
        base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
        path = Path(base) / APP_NAME
    elif sys.platform == "darwin":
        path = Path.home() / "Library" / "Application Support" / APP_NAME
    else:
        path = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local" / "share"))) / APP_NAME
    path.mkdir(parents=True, exist_ok=True)
    return path


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


def format_file_size(num_bytes: int) -> str:
    n = float(max(0, int(num_bytes)))
    units = ["B", "KiB", "MiB", "GiB", "TiB"]
    for unit in units:
        if n < 1024.0 or unit == units[-1]:
            return f"{int(n)} B" if unit == "B" else f"{n:.2f} {unit}"
        n /= 1024.0
    return f"{int(num_bytes)} B"



def localized_error_key(code: str) -> str:
    code = str(code or "transfer_failed")
    mapping = {
        "approval_timeout": "approval_timeout_msg",
        "receiver_approval_timeout": "approval_timeout_msg",
        "receiver_rejected": "receiver_rejected_msg",
        "user_rejected": "receiver_rejected_msg",
        "file_exists_cancelled": "receiver_rejected_msg",
        "save_dir_not_writable": "save_dir_not_writable_msg",
        "save_dir_create_failed": "save_dir_not_writable_msg",
        "save_dir_not_directory": "save_dir_not_writable_msg",
        "disk_space_not_enough": "disk_space_not_enough_msg",
        "network_no_progress": "network_no_progress_msg",
        "receiver_unreachable": "receiver_unreachable_msg",
        "receiver_identity_changed": "receiver_identity_changed_msg",
        "complete_timeout": "complete_timeout_msg",
        "sha256_mismatch": "sha256_mismatch_msg",
        "output_open_failed": "output_open_failed_msg",
    }
    return mapping.get(code, "transfer_failed")


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
        receiver = server.RUDPFileReceiver(args)
        try:
            receiver.start()
        except KeyboardInterrupt:
            return 130
        finally:
            receiver.stop()
        return 0
    print(f"unknown worker role: {role}", file=sys.stderr)
    return 2


if len(sys.argv) >= 2 and sys.argv[1] == "--worker":
    configure_stdio_utf8()
    raise SystemExit(run_worker(sys.argv[1:]))


# GUI imports are intentionally below the worker dispatch.
from kivy.app import App
from kivy.clock import Clock
from kivy.core.window import Window
from kivy.core.text import LabelBase, DEFAULT_FONT
from kivy.metrics import dp
from kivy.properties import StringProperty
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.button import Button
from kivy.uix.checkbox import CheckBox
from kivy.uix.filechooser import FileChooserListView
from kivy.uix.gridlayout import GridLayout
from kivy.uix.label import Label
from kivy.uix.popup import Popup
from kivy.uix.progressbar import ProgressBar
from kivy.uix.scrollview import ScrollView
from kivy.uix.spinner import Spinner
from kivy.uix.tabbedpanel import TabbedPanel, TabbedPanelItem
from kivy.uix.textinput import TextInput
from kivy.uix.dropdown import DropDown

from file_transfer_common import (
    CHAT_MESSAGE_LOG_PREFIX,
    CHAT_ACK_LOG_PREFIX,
    CONTACT_REQUEST_LOG_PREFIX,
    CONTACT_RESPONSE_LOG_PREFIX,
    DEFAULT_DISCOVERY_PORT,
    TRANSFER_REQUEST_LOG_PREFIX,
    USER_ERROR_LOG_PREFIX,
    USER_STATUS_LOG_PREFIX,
    discover_receivers,
    get_local_ip_candidates,
    is_unspecified_ip,
    normalize_peer_endpoint_ip,
)


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
        candidates.extend([
            win / "msyh.ttc",       # Microsoft YaHei
            win / "msyh.ttf",
            win / "simhei.ttf",
            win / "simsun.ttc",
            win / "Deng.ttf",
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
    # White-first application style. Kivy color values are RGBA floats.
    "window_bg": (0.985, 0.990, 0.995, 1),
    "panel_bg": (1.000, 1.000, 1.000, 1),
    "primary": (0.145, 0.365, 0.760, 1),
    "primary_active": (0.095, 0.255, 0.620, 1),
    "secondary": (0.955, 0.965, 0.985, 1),
    "secondary_active": (0.900, 0.925, 0.970, 1),
    "success": (0.150, 0.560, 0.320, 1),
    "success_active": (0.090, 0.430, 0.230, 1),
    "danger": (0.780, 0.200, 0.200, 1),
    "danger_active": (0.610, 0.130, 0.130, 1),
    "text": (0.070, 0.085, 0.115, 1),
    "muted_text": (0.390, 0.425, 0.500, 1),
    "on_primary": (1.000, 1.000, 1.000, 1),
    "on_secondary": (0.070, 0.085, 0.115, 1),
    "input_bg": (1.000, 1.000, 1.000, 1),
    "input_text": (0.070, 0.085, 0.115, 1),
    "input_cursor": (0.145, 0.365, 0.760, 1),
    "log_bg": (0.985, 0.990, 0.995, 1),
    "log_text": (0.080, 0.095, 0.125, 1),
    "disabled": (0.720, 0.750, 0.800, 1),
}

_BUTTON_ROLES = {
    "primary": ("primary", "on_primary"),
    "active": ("primary_active", "on_primary"),
    "secondary": ("secondary", "on_secondary"),
    "success": ("success", "on_primary"),
    "danger": ("danger", "on_primary"),
    "input": ("input_bg", "input_text"),
}


def style_button(button: Button, role: str = "secondary") -> Button:
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


def style_spinner(spinner: Spinner) -> Spinner:
    spinner.font_name = UI_FONT
    spinner.background_normal = ""
    spinner.background_down = ""
    spinner.background_color = THEME["input_bg"]
    spinner.color = THEME["input_text"]
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
        popup.background_color = THEME["panel_bg"]
    except Exception:
        pass
    try:
        popup.separator_color = THEME["secondary_active"]
    except Exception:
        pass
    return popup


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
        self.proc = subprocess.Popen(cmd, **kwargs)
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
                self.log_callback(line)
                if self.progress_callback is not None:
                    self._try_progress(line)
        finally:
            rc = self.proc.wait() if self.proc is not None else None
            self.exit_callback(rc)

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
        except Exception:
            pass


class LogBox(BoxLayout):
    def __init__(self, **kwargs):
        super().__init__(orientation="vertical", **kwargs)
        self.text = make_input(readonly=True, multiline=True, size_hint_y=1, background_color=THEME["log_bg"], foreground_color=THEME["log_text"], cursor_color=THEME["log_text"])
        self.add_widget(self.text)

    def append(self, s: str) -> None:
        def _append(_dt):
            self.text.text += s
            self.text.cursor = (0, len(self.text.text.splitlines()))
        Clock.schedule_once(_append, 0)

    def clear(self) -> None:
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


class ChatMessageBox(BoxLayout):
    def __init__(self, **kwargs):
        super().__init__(orientation="vertical", **kwargs)
        self.scroll = ScrollView(size_hint_y=1)
        self.inner = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(8), padding=(dp(8), dp(8), dp(8), dp(8)))
        self.inner.bind(minimum_height=self.inner.setter("height"))
        self.scroll.add_widget(self.inner)
        self.add_widget(self.scroll)

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

    def add_message(self, *, mine: bool, sender: str, text: str, timestamp: str, summary: str = "", body_type: str = "text", file_path: str = "") -> None:
        line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(86 if body_type == "file" else 70))
        spacer_left = BoxLayout(size_hint_x=0.24 if mine else 0.02)
        spacer_right = BoxLayout(size_hint_x=0.02 if mine else 0.24)
        bubble = BoxLayout(orientation="vertical", spacing=dp(3), padding=(dp(10), dp(6), dp(10), dp(6)), size_hint_x=0.74)
        header = f"{timestamp}  {'我' if mine else sender}"
        if summary:
            header += f"  {summary}"
        bubble.add_widget(make_label(text=header, size_hint_y=None, height=dp(20), halign="right" if mine else "left", color=THEME["muted_text"]))
        if body_type == "file":
            file_name = os.path.basename(file_path or text) or text or "文件"
            bubble.add_widget(make_label(text=f"[文件] {file_name}", size_hint_y=None, height=dp(24), halign="right" if mine else "left", color=THEME["text"]))
            btn = make_button("secondary", text="打开所在位置", size_hint_y=None, height=dp(30), on_release=lambda *_p, path=file_path: open_file_location(path))
            bubble.add_widget(btn)
        else:
            lab = make_label(text=str(text or ""), size_hint_y=None, height=dp(34), halign="right" if mine else "left", color=THEME["text"])
            bind_label_wrap(lab)
            bubble.add_widget(lab)
        if mine:
            line.add_widget(spacer_left); line.add_widget(bubble); line.add_widget(spacer_right)
        else:
            line.add_widget(spacer_left); line.add_widget(bubble); line.add_widget(spacer_right)
        self.inner.add_widget(line)
        self.scroll.scroll_y = 0


class RUDPTransferRoot(BoxLayout):
    def __init__(self, app: "RUDPTransferApp", **kwargs):
        super().__init__(orientation="vertical", spacing=dp(8), padding=dp(10), **kwargs)
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
        self.chat_unlocked = False
        self.basic_mode = False
        self.chat_db_path = str(user_data_dir() / "chat" / "agoralink_chat.db")
        self.chat_password = ""
        self.chat_local_peer_id = os.environ.get("USERNAME") or "local"
        self.chat_nickname = os.environ.get("USERNAME") or "AgoraLinkUser"
        self.current_chat_mode = "group"
        self.current_group_id = ""
        self.current_peer_id = ""
        self.selected_contact: Optional[Dict[str, object]] = None
        self.selected_device: Optional[Dict[str, object]] = None
        self.selected_group: Optional[Dict[str, object]] = None
        self.pending_outgoing_contact_requests: Dict[str, Dict[str, object]] = {}
        self._build()
        self.refresh_texts()
        self.refresh_local_ips()
        Clock.schedule_interval(self.poll_approval_requests, 0.5)
        Clock.schedule_interval(self.poll_contact_requests, 0.5)
        Clock.schedule_once(lambda _dt: self.show_startup_unlock_popup(), 0.2)

    def t(self, key: str, **kwargs) -> str:
        text = I18N[self.lang].get(key, key)
        return text.format(**kwargs) if kwargs else text

    def _build(self) -> None:
        Window.minimum_width = 1000
        Window.minimum_height = 680

        top = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(44), spacing=dp(8))
        self.title_label = make_label(font_size="20sp", bold=True, halign="left", valign="middle")
        bind_label_wrap(self.title_label)
        top.add_widget(self.title_label)
        self.lang_btn = make_button("primary", size_hint_x=None, width=dp(90), on_release=lambda *_: self.toggle_lang())
        top.add_widget(self.lang_btn)
        self.online_btn = make_button("success", text="Online", size_hint_x=None, width=dp(90), on_release=lambda *_: self.toggle_online())
        top.add_widget(self.online_btn)
        self.enter_chat_btn = make_button("primary", text="进入聊天", size_hint_x=None, width=dp(110), on_release=lambda *_: self.show_startup_unlock_popup())
        top.add_widget(self.enter_chat_btn)
        self.settings_btn = make_button("secondary", text="设置/调试", size_hint_x=None, width=dp(110), on_release=lambda *_: self.open_settings_popup())
        top.add_widget(self.settings_btn)
        self.add_widget(top)

        self.local_ip_label = make_label(size_hint_y=None, height=dp(30), halign="left", valign="middle", shorten=True)
        bind_label_wrap(self.local_ip_label)
        self.add_widget(self.local_ip_label)

        # Use explicit tab buttons instead of Kivy TabbedPanel headers.
        # TabbedPanel headers use their own internal button class and may ignore
        # the application font on some Windows/Kivy builds, which causes Chinese
        # text to disappear. Normal Buttons give deterministic font behavior.
        self.tab_bar = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(44), spacing=dp(8))
        self.send_tab_btn = make_button("active", on_release=lambda *_: self.show_page("send"))
        self.recv_tab_btn = make_button("secondary", on_release=lambda *_: self.show_page("recv"))
        self.chat_tab_btn = make_button("secondary", on_release=lambda *_: self.show_page("chat"))
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
        self.payload_input = make_input(text="1300", multiline=False, input_filter="int")
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
        self.stop_send_btn = make_button("danger", on_release=lambda *_: self.sender_worker.stop())
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
        self.stop_recv_btn = make_button("danger", on_release=lambda *_: self.receiver_worker.stop())
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
        root = BoxLayout(orientation="horizontal", spacing=dp(8), padding=dp(8))

        left = BoxLayout(orientation="vertical", size_hint_x=None, width=dp(260), spacing=dp(6))
        self.chat_nav = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(4))
        self.recent_btn = make_button("active", text="最近聊天", on_release=lambda *_: self.set_chat_section("recent"))
        self.groups_btn = make_button("secondary", text="联系人", on_release=lambda *_: self.set_chat_section("contacts"))
        self.devices_btn = make_button("secondary", text="在线设备", on_release=lambda *_: self.set_chat_section("devices"))
        self.chat_nav.add_widget(self.recent_btn)
        self.chat_nav.add_widget(self.groups_btn)
        self.chat_nav.add_widget(self.devices_btn)
        left.add_widget(self.chat_nav)

        self.chat_items_scroll = ScrollView(size_hint_y=1)
        self.chat_items_box = BoxLayout(orientation="vertical", size_hint_y=None, spacing=dp(4), padding=(0, dp(2), 0, dp(2)))
        self.chat_items_box.bind(minimum_height=self.chat_items_box.setter("height"))
        self.chat_items_scroll.add_widget(self.chat_items_box)
        left.add_widget(self.chat_items_scroll)
        # Hidden compatibility holders used by older selection functions.
        self.chat_list_box = LogBox(size_hint_y=None, height=0, opacity=0)
        self.chat_list_spinner = style_spinner(Spinner(text="", values=[], size_hint_y=None, height=0, font_name=UI_FONT))
        self.chat_list_spinner.bind(text=lambda _i, value: self.on_chat_list_selected(value))
        left.add_widget(self.chat_list_box)
        left.add_widget(self.chat_list_spinner)
        list_actions = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(4))
        list_actions.add_widget(make_button("secondary", text="刷新", on_release=lambda *_: self.refresh_chat_main()))
        self.scan_devices_btn = make_button("secondary", text="扫描设备", on_release=lambda *_: self.scan_devices_for_chat())
        list_actions.add_widget(self.scan_devices_btn)
        list_actions.add_widget(make_button("primary", text="加联系人", on_release=lambda *_: self.request_or_add_selected_device()))
        left.add_widget(list_actions)
        root.add_widget(left)

        center = BoxLayout(orientation="vertical", spacing=dp(6))
        self.current_chat_title = make_label(text="请选择会话", size_hint_y=None, height=dp(34), halign="left", valign="middle", bold=True)
        bind_label_wrap(self.current_chat_title)
        center.add_widget(self.current_chat_title)
        self.main_messages_box = ChatMessageBox()
        center.add_widget(self.main_messages_box)
        input_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(6))
        self.main_message_input = make_input(text="", multiline=False)
        input_line.add_widget(self.main_message_input)
        input_line.add_widget(make_button("primary", text="发送", size_hint_x=None, width=dp(90), on_release=lambda *_: self.send_current_chat_message()))
        input_line.add_widget(make_button("secondary", text="发送文件", size_hint_x=None, width=dp(105), on_release=lambda *_: self.send_file_to_current_chat()))
        center.add_widget(input_line)
        root.add_widget(center)

        right = BoxLayout(orientation="vertical", size_hint_x=None, width=dp(290), spacing=dp(6))
        self.right_title = make_label(text="群成员 / 设备信息", size_hint_y=None, height=dp(34), halign="left", valign="middle", bold=True)
        bind_label_wrap(self.right_title)
        right.add_widget(self.right_title)
        self.right_info_box = LogBox()
        right.add_widget(self.right_info_box)
        action1 = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(4))
        action1.add_widget(make_button("success", text="新建群", on_release=lambda *_: self.open_group_popup()))
        action1.add_widget(make_button("secondary", text="加成员", on_release=lambda *_: self.add_selected_contact_to_current_group()))
        right.add_widget(action1)
        action2 = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(4))
        action2.add_widget(make_button("danger", text="移除成员", on_release=lambda *_: self.confirm_remove_member()))
        action2.add_widget(make_button("danger", text="退出群", on_release=lambda *_: self.confirm_leave_group()))
        action2.add_widget(make_button("danger", text="删好友", on_release=lambda *_: self.confirm_delete_contact()))
        right.add_widget(action2)
        root.add_widget(right)
        return root

    def set_chat_section(self, section: str) -> None:
        self.current_chat_section = str(section or "recent")
        for btn, name in [(self.recent_btn, "recent"), (self.groups_btn, "contacts"), (self.devices_btn, "devices")]:
            style_button(btn, "active" if name == self.current_chat_section else "secondary")
        self.refresh_chat_main()

    def _short_fp(self, fp: str) -> str:
        text = str(fp or "")
        return text[:12] + ("…" if len(text) > 12 else "")

    def _set_chat_entry_buttons(self, entries: List[tuple[str, str]]) -> None:
        if not hasattr(self, "chat_items_box"):
            return
        self.chat_items_box.clear_widgets()
        values = []
        for label, value in entries:
            values.append(value)
            btn = make_button("secondary", text=str(label), size_hint_y=None, height=dp(52))
            btn.bind(on_release=lambda _btn, v=value: self._select_chat_entry(v))
            self.chat_items_box.add_widget(btn)
        self.chat_list_spinner.values = values
        if values and self.chat_list_spinner.text not in values:
            self.chat_list_spinner.text = values[0]
        elif not values:
            self.chat_list_spinner.text = ""

    def _select_chat_entry(self, value: str) -> None:
        value = str(value or "")
        if not value:
            return
        self.chat_list_spinner.text = value
        self.on_chat_list_selected(value)

    def scan_devices_for_chat(self) -> None:
        self.set_chat_section("devices")
        self.search_receivers()

    def refresh_chat_main(self) -> None:
        if hasattr(self, "right_info_box"):
            self.right_info_box.clear()
        section = getattr(self, "current_chat_section", "recent")
        store = self.chat_store
        entries: List[tuple[str, str]] = []

        if section == "devices":
            devices = sorted(self.discovered, key=lambda x: float(x.get("last_seen") or 0), reverse=True)
            for d in devices:
                ip, port = self._receiver_endpoint(d)
                name = self._device_display_name(d)
                fp = self._device_fingerprint(d)
                pid = self._device_peer_id(d)
                label = f"{name}\n{ip}:{port}  {self._short_fp(fp)}"
                entries.append((label, self._chat_device_value(d)))
            if not entries:
                entries.append(("未发现在线设备\n点击下方“扫描设备”", ""))
            self._set_chat_entry_buttons(entries)
            return

        if store is None:
            self._set_chat_entry_buttons([("聊天数据库未解锁", "")])
            return

        try:
            groups = store.list_groups()
            contacts = store.list_contacts(trusted_only=True)
            if section == "contacts":
                # 联系人页：群聊在上，好友在下。
                for g in groups:
                    title = str(g.get("title") or g.get("group_id") or "群聊")
                    gid = str(g.get("group_id") or "")
                    entries.append((f"[群聊] {title}\n{gid}", f"group::{gid}::{title}"))
                for c in contacts:
                    name = str(c.get("remark_name") or c.get("display_name") or c.get("nickname") or c.get("peer_id"))
                    pid = str(c.get("peer_id") or "")
                    endpoint = f"{c.get('peer_ip') or ''}:{c.get('peer_port') or 9999}"
                    entries.append((f"[好友] {name}\n{endpoint}", f"direct::{pid}::{name}"))
            else:
                # 最近聊天：当前先按群组/联系人更新时间近似排序，后续可接入 last_message_at 字段。
                for g in groups:
                    title = str(g.get("title") or g.get("group_id") or "群聊")
                    gid = str(g.get("group_id") or "")
                    entries.append((f"[群] {title}\n{gid}", f"group::{gid}::{title}"))
                for c in contacts:
                    name = str(c.get("remark_name") or c.get("display_name") or c.get("nickname") or c.get("peer_id"))
                    pid = str(c.get("peer_id") or "")
                    endpoint = f"{c.get('peer_ip') or ''}:{c.get('peer_port') or 9999}"
                    entries.append((f"[联系人] {name}\n{endpoint}", f"direct::{pid}::{name}"))
            if not entries:
                entries.append(("暂无会话或联系人", ""))
            self._set_chat_entry_buttons(entries)
        except Exception as exc:
            self._set_chat_entry_buttons([(f"刷新失败\n{exc}", "")])

    def on_chat_list_selected(self, value: str) -> None:
        text = str(value or "")
        if not text:
            return
        if text.startswith("group::"):
            _tag, gid, title = text.split("::", 2)
            self.current_chat_mode = "group"
            self.current_group_id = gid
            self.current_peer_id = ""
            self.current_chat_title.text = f"群聊：{title} ({gid})"
        elif text.startswith("direct::"):
            _tag, pid, name = text.split("::", 2)
            self.current_chat_mode = "direct"
            self.current_peer_id = pid
            self.current_group_id = ""
            self.current_chat_title.text = f"一对一：{name}"
        else:
            # Online device row. Do not auto-save contact.
            self.current_chat_mode = "device"
            self.current_chat_title.text = "在线设备：先添加联系人后聊天"
        self.render_current_chat()

    def render_current_chat(self) -> None:
        self.main_messages_box.clear()
        self.right_info_box.clear()
        store = self.chat_store
        if store is None:
            self.main_messages_box.append("聊天数据库未解锁。\n")
            return
        try:
            if self.current_chat_mode == "group" and self.current_group_id:
                for msg in store.list_messages(group_id=self.current_group_id, limit=200):
                    summary = store.receipt_summary(str(msg.get('message_id') or ''))
                    ts = time.strftime("%H:%M", time.localtime(float(msg.get("created_at") or time.time())))
                    body_type = str(msg.get('body_type') or 'text')
                    text = str(msg.get('text') or '')
                    file_path = ''
                    if body_type == 'file':
                        try:
                            obj = json.loads(text)
                            file_path = str(obj.get('path') or '')
                            text = str(obj.get('name') or file_path or text)
                        except Exception:
                            file_path = text
                    self.main_messages_box.add_message(mine=str(msg.get('sender_peer_id') or '') == self.chat_local_peer_id, sender=str(msg.get('sender_peer_id') or ''), text=text, timestamp=ts, summary=summary, body_type=body_type, file_path=file_path)
                members = store.list_group_members(self.current_group_id, include_inactive=True)
                self.right_info_box.append(f"群名: {self.current_group_id}\n成员数量: {len(members)}\n\n")
                for m in members:
                    self.right_info_box.append(f"{m.get('display_name') or m.get('peer_id')}\n{m.get('peer_id')}\n{m.get('peer_ip')}:{m.get('peer_port')}  {m.get('member_state')}\n\n")
            elif self.current_chat_mode == "direct" and self.current_peer_id:
                conv = store.create_direct_conversation(self.current_peer_id)
                for msg in store.list_messages(conversation_id=conv, limit=200):
                    summary = store.receipt_summary(str(msg.get('message_id') or ''))
                    ts = time.strftime("%H:%M", time.localtime(float(msg.get("created_at") or time.time())))
                    body_type = str(msg.get('body_type') or 'text')
                    text = str(msg.get('text') or '')
                    file_path = ''
                    if body_type == 'file':
                        try:
                            obj = json.loads(text)
                            file_path = str(obj.get('path') or '')
                            text = str(obj.get('name') or file_path or text)
                        except Exception:
                            file_path = text
                    self.main_messages_box.add_message(mine=str(msg.get('sender_peer_id') or '') == self.chat_local_peer_id, sender=str(msg.get('sender_peer_id') or ''), text=text, timestamp=ts, summary=summary, body_type=body_type, file_path=file_path)
                for c in store.list_contacts():
                    if str(c.get("peer_id") or "") == self.current_peer_id:
                        self.right_info_box.append(f"联系人: {c.get('remark_name') or c.get('display_name')}\npeer_id: {c.get('peer_id')}\nIP: {c.get('peer_ip')}:{c.get('peer_port')}\n状态: {c.get('trust_state')}\n")
                        break
        except Exception as exc:
            self.main_messages_box.append(f"显示失败: {exc}\n")



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
        self.title_label.text = self.t("title")
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
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        content.add_widget(make_label(text="AgoraLink", size_hint_y=None, height=dp(30), bold=True, halign="left"))
        db_input = make_input(text=self.chat_db_path, multiline=False)
        mode_values = ["登录", "注册", "仅收发"]
        db_exists = Path(self.chat_db_path).expanduser().exists()
        mode_spinner = style_spinner(Spinner(text=("登录" if db_exists else "注册"), values=mode_values, font_name=UI_FONT, size_hint_y=None, height=dp(38)))
        password_input = make_input(text="", multiline=False, password=True)
        confirm_input = make_input(text="", multiline=False, password=True)
        nick_input = make_input(text=self.chat_nickname, multiline=False)
        content.add_widget(row("启动模式", mode_spinner, label_width=120))
        content.add_widget(row("聊天数据库", db_input, label_width=120))
        content.add_widget(row("密码", password_input, label_width=120))
        confirm_row = row("确认密码", confirm_input, label_width=120)
        nick_row = row("昵称", nick_input, label_width=120)
        content.add_widget(confirm_row)
        content.add_widget(nick_row)
        hint = make_label(text="登录：输入已有密码。注册：首次创建聊天库。仅收发：进入旧接收页，不启用聊天。", size_hint_y=None, height=dp(54), halign="left", valign="middle", color=THEME["muted_text"])
        bind_label_wrap(hint)
        content.add_widget(hint)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        popup = style_popup(Popup(title="AgoraLink 启动", content=content, size_hint=(0.64, 0.60), auto_dismiss=False))

        def _sync_mode(*_):
            mode = mode_spinner.text
            is_register = mode == "注册"
            password_input.disabled = mode == "仅收发"
            confirm_input.disabled = not is_register
            nick_input.disabled = not is_register
            confirm_row.opacity = 1.0 if is_register else 0.35
            nick_row.opacity = 1.0 if is_register else 0.35
            if mode == "登录":
                hint.text = "输入已注册聊天库密码。无需确认密码。"
            elif mode == "注册":
                hint.text = "首次创建聊天库时需要密码、确认密码和昵称。"
            else:
                hint.text = "仅收发模式：进入旧接收页，保留设备发现、文件发送和文件接收，不启用聊天。"
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
                hint.text = "密码不能为空。"
                return
            if mode == "登录":
                if not exists_now:
                    hint.text = "聊天库不存在，请切换到注册。"
                    return
            if mode == "注册":
                if exists_now:
                    hint.text = "聊天库已存在。请登录，或先使用重置。"
                    return
                if pwd != confirm:
                    hint.text = "注册时密码和确认密码必须一致。"
                    return
            try:
                self.unlock_chat_with(db_path, pwd, nick)
                popup.dismiss()
            except Exception as exc:
                hint.text = f"解锁失败: {exc}"

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
                hint.text = "聊天库已删除。请选择注册并输入新密码和昵称。"
            line.add_widget(make_button("danger", text="确认重置", on_release=_do_reset))
            line.add_widget(make_button("secondary", text="取消", on_release=lambda *_: pop2.dismiss()))
            content2.add_widget(line)
            apply_ui_font(content2)
            pop2.open()

        buttons.add_widget(make_button("primary", text="进入", on_release=_submit))
        buttons.add_widget(make_button("secondary", text="仅收发", on_release=_enter_basic))
        buttons.add_widget(make_button("danger", text="忘记密码/重置", on_release=_reset))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def unlock_chat_with(self, db_path: str, password: str, nickname: str) -> None:
        from chat_store import ChatStore
        # First use a temporary peer id; after opening, persist a stable local id in meta.
        initial_peer = re.sub(r"[^A-Za-z0-9_.-]+", "_", nickname.strip()) or "local"
        self.chat_db_path = str(Path(db_path).expanduser().resolve())
        self.chat_password = str(password or "")
        if self.chat_store is not None:
            try:
                self.chat_store.close()
            except Exception:
                pass
        self.chat_store = ChatStore(self.chat_db_path, self.chat_password, my_peer_id=initial_peer)
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
        self.show_page("agora_chat")
        self.refresh_chat_main()
        self.start_receiver(auto=True)

    def toggle_online(self) -> None:
        if self.receiver_worker.is_running():
            self.receiver_worker.stop()
            self.online_btn.text = "Offline"
            style_button(self.online_btn, "secondary")
        else:
            self.start_receiver(auto=True)

    def open_settings_popup(self) -> None:
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(10))
        theme_line = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(38), spacing=dp(8))
        theme_line.add_widget(make_label(text="主题", size_hint_x=None, width=dp(90), color=THEME["muted_text"]))
        theme_spinner = style_spinner(Spinner(text=getattr(self, "theme_mode", "跟随系统"), values=["跟随系统", "浅色", "深色"], font_name=UI_FONT))
        theme_line.add_widget(theme_spinner)
        content.add_widget(theme_line)
        log = LogBox(size_hint_y=1)
        log.append("调试日志入口\n\n运行日志：接收端和发送端日志仍在旧页面/本窗口记录。\n传输日志：文件传输进度和错误。\n协议错误：USER_ERROR_JSON / CHAT_ACK_JSON / CONTACT_REQUEST_JSON。\n")
        content.add_widget(log)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        popup = style_popup(Popup(title="设置 / 调试", content=content, size_hint=(0.78, 0.72)))
        def _apply_theme(*_):
            self.theme_mode = theme_spinner.text
            self.apply_theme_mode(self.theme_mode)
        buttons.add_widget(make_button("primary", text="应用主题", on_release=_apply_theme))
        buttons.add_widget(make_button("secondary", text="打开防火墙脚本", on_release=lambda *_: self.allow_firewall()))
        buttons.add_widget(make_button("secondary", text="关闭", on_release=lambda *_: popup.dismiss()))
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
        if self.chat_store is None:
            return
        for rec in found or []:
            if not isinstance(rec, dict):
                continue
            ip, port = self._receiver_endpoint(rec)
            if not ip or is_unspecified_ip(ip):
                continue
            self.chat_store.update_known_endpoint(
                peer_id=self._device_peer_id(rec),
                fingerprint=self._device_fingerprint(rec),
                nickname=self._device_display_name(rec),
                peer_ip=ip,
                peer_port=port,
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
        store = self._require_chat_store()
        if store is None:
            return
        gid = self.chat_group_id_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        title = self.chat_group_title_input.text.strip() or gid
        store.create_group(gid, title)
        self.chat_messages_box.append(f"Group saved: {gid}\n")
        self.refresh_chat_view()

    def add_chat_member(self) -> None:
        store = self._require_chat_store()
        if store is None:
            return
        gid = self.chat_group_id_input.text.strip()
        pid = self.member_peer_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        if not pid:
            self.chat_messages_box.append(self.t("need_member") + "\n")
            return
        try:
            port = int(self.member_port_input.text.strip() or "9999")
        except Exception:
            port = 9999
        store.add_group_member(gid, pid, peer_ip=self.member_ip_input.text.strip(), peer_port=port, display_name=pid, member_state="active")
        self.chat_messages_box.append(f"Member saved: {pid}\n")
        self.refresh_chat_view()

    def remove_chat_member(self) -> None:
        store = self._require_chat_store()
        if store is None:
            return
        gid = self.chat_group_id_input.text.strip()
        pid = self.member_peer_input.text.strip()
        if not gid or not pid:
            self.chat_messages_box.append(self.t("need_member") + "\n")
            return
        store.remove_group_member(gid, pid, removed=True)
        self.chat_messages_box.append(f"Member removed: {pid}\n")
        self.refresh_chat_view()

    def leave_chat_group(self) -> None:
        store = self._require_chat_store()
        if store is None:
            return
        gid = self.chat_group_id_input.text.strip()
        if not gid:
            self.chat_messages_box.append(self.t("need_group") + "\n")
            return
        store.leave_group(gid, self.chat_local_peer_id)
        self.chat_messages_box.append(f"Left group: {gid}\n")
        self.refresh_chat_view()

    def refresh_chat_view(self) -> None:
        store = self.chat_store
        if store is None:
            return
        gid = self.chat_group_id_input.text.strip()
        self.chat_members_box.clear()
        self.chat_messages_box.clear()
        try:
            groups = store.list_groups()
            self.chat_members_box.append("Groups:\n")
            for g in groups:
                self.chat_members_box.append(f"  {g.get('group_id')}  {g.get('title')}  {g.get('group_state')}\n")
            if gid:
                self.chat_members_box.append("\nMembers:\n")
                for m in store.list_group_members(gid, include_inactive=True):
                    self.chat_members_box.append(f"  {m.get('peer_id')}  {m.get('peer_ip')}:{m.get('peer_port')}  {m.get('member_state')}\n")
                self.chat_messages_box.append(f"Messages for {gid}:\n")
                for msg in store.list_messages(group_id=gid, limit=100):
                    summary = store.receipt_summary(str(msg.get('message_id') or ''))
                    self.chat_messages_box.append(f"[{msg.get('status')}] {msg.get('sender_peer_id')}: {msg.get('text')}  {summary}\n")
        except Exception as exc:
            self.chat_messages_box.append(f"Chat refresh failed: {exc}\n")

    def send_group_message_gui(self) -> None:
        store = self._require_chat_store()
        if store is None:
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
            msg, recipients = store.send_group_message(gid, text)
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
                    store.mark_chat_failed(str(msg["message_id"]), peer_id, error="missing_member_ip")
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
                    proc = subprocess.run(cmd, cwd=str(APP_DIR), capture_output=True, text=True, timeout=60)
                    if proc.returncode == 0:
                        store.mark_chat_delivered(str(msg["message_id"]), peer_id)
                        Clock.schedule_once(lambda _dt, pid=peer_id: self.chat_messages_box.append(f"Delivered to {pid}\n"), 0)
                    else:
                        err = (proc.stderr or proc.stdout or "send_failed")[-500:]
                        store.mark_chat_failed(str(msg["message_id"]), peer_id, error=err)
                        Clock.schedule_once(lambda _dt, pid=peer_id, e=err: self.chat_messages_box.append(f"Failed {pid}: {e}\n"), 0)
                except Exception as exc:
                    store.mark_chat_failed(str(msg["message_id"]), peer_id, error=str(exc))
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
        if self.chat_store is None:
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
            self.chat_store.upsert_contact(
                peer_id,
                display_name=nickname or peer_id,
                nickname=nickname or peer_id,
                remark_name=str(pending.get("remark_name") or ""),
                fingerprint=fp or peer_id,
                peer_ip=ip,
                peer_port=port,
                trust_state="trusted",
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
        if self.chat_store is None:
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
        if self.chat_store is None:
            self.main_messages_box.append("请先解锁聊天数据库。\n")
            return
        content = BoxLayout(orientation="vertical", spacing=dp(8), padding=dp(12))
        gid_input = make_input(text="group1", multiline=False)
        title_input = make_input(text="LAN Group", multiline=False)
        content.add_widget(row("群 ID", gid_input, label_width=90))
        content.add_widget(row("群名", title_input, label_width=90))
        popup = style_popup(Popup(title="新建群", content=content, size_hint=(0.52, 0.36), auto_dismiss=False))
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        def _ok(*_):
            try:
                gid = self.chat_store.create_group(gid_input.text.strip(), title_input.text.strip())
                self.current_chat_mode = "group"
                self.current_group_id = gid
                self.current_chat_title.text = f"群聊：{title_input.text.strip() or gid} ({gid})"
                popup.dismiss()
                self.refresh_chat_main()
                self.render_current_chat()
            except Exception as exc:
                self.main_messages_box.append(f"创建群失败: {exc}\n")
        buttons.add_widget(make_button("success", text="保存", on_release=_ok))
        buttons.add_widget(make_button("secondary", text="取消", on_release=lambda *_: popup.dismiss()))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def add_selected_contact_to_current_group(self) -> None:
        if self.chat_store is None or not self.current_group_id:
            self.main_messages_box.append("请先选择群聊。\n")
            return
        contacts = self.chat_store.list_contacts(trusted_only=True)
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
                self.chat_store.add_group_member(
                    self.current_group_id,
                    pid,
                    peer_ip=str(contact.get('peer_ip') or ''),
                    peer_port=int(contact.get('peer_port') or 9999),
                    display_name=str(contact.get('remark_name') or contact.get('display_name') or pid),
                    member_state="active",
                )
            popup.dismiss()
            self.render_current_chat()
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
        if self.chat_store is None or not self.current_group_id:
            return
        members = [m for m in self.chat_store.list_group_members(self.current_group_id, include_inactive=False) if str(m.get('peer_id')) != self.chat_local_peer_id]
        if not members:
            self.right_info_box.append("没有可移除的成员。\n")
            return
        # First non-self member for first UI version; details are shown in right panel.
        pid = str(members[0].get('peer_id') or '')
        self._confirm_action("移除成员", f"确认移除成员 {pid}？", lambda: (self.chat_store.remove_group_member(self.current_group_id, pid, removed=True), self.render_current_chat()))

    def confirm_leave_group(self) -> None:
        if self.chat_store is None or not self.current_group_id:
            return
        gid = self.current_group_id
        def _do():
            self.chat_store.delete_group_data(gid)
            self.current_group_id = ""
            self.current_chat_mode = ""
            self.current_chat_title.text = "请选择会话"
            self.refresh_chat_main()
            self.render_current_chat()
        self._confirm_action("退出群组", f"确认退出群组 {gid}？\n该群相关成员、消息、回执都会从本机删除。", _do)

    def confirm_delete_contact(self) -> None:
        if self.chat_store is None or self.current_chat_mode != "direct" or not self.current_peer_id:
            self.right_info_box.append("请先选择要删除的联系人。\n")
            return
        pid = self.current_peer_id
        def _do():
            self.chat_store.delete_contact(pid, purge_data=True)
            self.current_peer_id = ""
            self.current_chat_mode = ""
            self.current_chat_title.text = "请选择会话"
            self.refresh_chat_main()
            self.render_current_chat()
        self._confirm_action("删除联系人", f"确认删除联系人 {pid}？\n该联系人、一对一聊天记录和相关回执都会从本机删除。", _do)

    def _send_chat_to_endpoint(self, *, ip: str, port: int, peer_id: str, text: str, message_id: str, group_id: str = "", conversation_id: str = "", created_at: float = 0.0) -> bool:
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
            "--chat-db", self.chat_db_path,
            "--chat-password", self.chat_password,
            "--server-pin-file", pin_file,
            "--complete-timeout", "20",
            "--final-ack-timeout", "20",
        ]
        cmd = ([sys.executable] + args) if FROZEN else ([sys.executable, str(Path(__file__).resolve())] + args)
        proc = subprocess.run(cmd, cwd=str(APP_DIR), capture_output=True, text=True, timeout=60)
        combined = (proc.stdout or "") + ("\n" + proc.stderr if proc.stderr else "")
        if combined.strip():
            # Keep the subprocess log available in the debug sender log; this is
            # essential for diagnosing CHAT_ACK and pin/handshake issues.
            try:
                self.sender_log_box.append(combined[-4000:] + ("\n" if not combined.endswith("\n") else ""))
            except Exception:
                pass
        ack_seen = (CHAT_ACK_LOG_PREFIX in combined) or ("CHAT_ACK received" in combined)
        return proc.returncode == 0 or ack_seen

    def send_current_chat_message(self) -> None:
        store = self.chat_store
        if store is None:
            self.main_messages_box.append("请先解锁聊天数据库。\n")
            return
        text = self.main_message_input.text.strip()
        if not text:
            return
        self.main_message_input.text = ""
        if self.current_chat_mode == "direct" and self.current_peer_id:
            msg, contact = store.send_direct_message(self.current_peer_id, text)
            recipients = [contact]
            group_id = ""
            conversation_id = str(msg.get('conversation_id') or '')
        elif self.current_chat_mode == "group" and self.current_group_id:
            msg, recipients = store.send_group_message(self.current_group_id, text)
            group_id = self.current_group_id
            conversation_id = ""
        else:
            self.main_messages_box.append("请先选择联系人或群聊。\n")
            return
        self.render_current_chat()
        def _run():
            for r in recipients:
                peer_id = str(r.get('peer_id') or '')
                ip = str(r.get('peer_ip') or '')
                try:
                    port = int(r.get('peer_port') or 9999)
                except Exception:
                    port = 9999
                if not peer_id or not ip or is_unspecified_ip(ip):
                    store.mark_chat_failed(str(msg['message_id']), peer_id, error="invalid_endpoint")
                    continue
                try:
                    ok = self._send_chat_to_endpoint(ip=ip, port=port, peer_id=peer_id, text=text, message_id=str(msg['message_id']), group_id=group_id, conversation_id=conversation_id, created_at=float(msg['created_at']))
                    if ok:
                        store.mark_chat_delivered(str(msg['message_id']), peer_id)
                    else:
                        store.mark_chat_failed(str(msg['message_id']), peer_id, error="send_failed")
                except Exception as exc:
                    store.mark_chat_failed(str(msg['message_id']), peer_id, error=str(exc))
            Clock.schedule_once(lambda _dt: self.render_current_chat(), 0)
        threading.Thread(target=_run, daemon=True).start()

    def send_file_to_current_chat(self) -> None:
        if self.current_chat_mode not in ("direct", "group"):
            self.main_messages_box.append("请先选择聊天对象。\n")
            return
        self._native_file_dialog(select_dir=False, callback=lambda path: self._send_file_path_to_current_chat(path))

    def _send_file_path_to_current_chat(self, path: str) -> None:
        if not path or not os.path.isfile(path):
            return
        store = self.chat_store
        recipients = []
        msg = None
        if self.current_chat_mode == "direct" and self.current_peer_id and store is not None:
            try:
                msg, contact = store.send_direct_file_message(self.current_peer_id, path)
                recipients = [contact]
            except Exception as exc:
                self.main_messages_box.append(f"文件消息创建失败: {exc}\n")
                return
        elif self.current_chat_mode == "group" and self.current_group_id and store is not None:
            try:
                msg, recipients = store.send_group_file_message(self.current_group_id, path)
            except Exception as exc:
                self.main_messages_box.append(f"文件消息创建失败: {exc}\n")
                return
        if not recipients:
            self.main_messages_box.append("没有可发送文件的接收对象。\n")
            return
        self.render_current_chat()
        def _run():
            for r in recipients:
                peer_id = str(r.get('peer_id') or '')
                ip = str(r.get('peer_ip') or '')
                try:
                    port = int(r.get('peer_port') or 9999)
                except Exception:
                    port = 9999
                if not ip or is_unspecified_ip(ip):
                    if msg and store is not None:
                        store.mark_chat_failed(str(msg.get('message_id')), peer_id, error='invalid_endpoint')
                    continue
                pin_file = str(receiver_pin_file(ip, port))
                args = ["--worker", "sender", "--server-ip", ip, "--server-port", str(port), "--file", path, "--server-pin-file", pin_file, "--request-timeout", "300"]
                cmd = ([sys.executable] + args) if FROZEN else ([sys.executable, str(Path(__file__).resolve())] + args)
                proc = subprocess.run(cmd, cwd=str(APP_DIR), capture_output=True, text=True, timeout=3600)
                if msg and store is not None:
                    if proc.returncode == 0:
                        store.mark_chat_delivered(str(msg.get('message_id')), peer_id)
                    else:
                        store.mark_chat_failed(str(msg.get('message_id')), peer_id, error='file_send_failed')
            Clock.schedule_once(lambda _dt: self.render_current_chat(), 0)
        threading.Thread(target=_run, daemon=True).start()

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
            "--payload-size", self.payload_input.text.strip() or "1300",
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
            "--bind", self.bind_input.text.strip() or "0.0.0.0",
            "--port", self.recv_port.text.strip() or "9999",
            "--save-dir", save_dir,
            "--discovery-port", self.recv_discovery_port.text.strip() or str(DEFAULT_DISCOVERY_PORT),
            "--server-id-key-file", key_file,
            "--require-approval",
            "--approval-dir", str(approval_dir),
            "--approval-timeout", approval_timeout_text,
            "--idle-timeout", idle_timeout_text,
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
        if self.receiver_worker.is_running():
            if auto:
                return
            self.receiver_log_box.append(self.t("running") + "\n")
            return
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
        key = localized_error_key(code)
        if key == "transfer_failed":
            msg = self.t("transfer_failed", reason=(detail or code or self.t("unknown")))
        else:
            msg = self.t(key)
            if detail:
                msg = msg + "\n" + detail
        if target == "receiver":
            self.receiver_log_box.append(msg + "\n")
        else:
            self.sender_log_box.append(msg + "\n")
            self.last_sender_failure_code = str(code or "transfer_failed")
            self.retry_send_btn.disabled = False
            self.sender_log_box.append(self.t("retry_ready") + "\n")

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
                msg = self.t("resume_enabled", offset=format_file_size(int(obj.get("resume_offset") or 0)))
                if target == "receiver":
                    self.receiver_log_box.append(msg + "\n")
                else:
                    self.sender_log_box.append(msg + "\n")
                return True
        return False

    def sender_log(self, text: str) -> None:
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
            if accepted and self.chat_store is not None and pid:
                try:
                    self.chat_store.upsert_contact(pid, display_name=name, nickname=name, fingerprint=fp, peer_ip=ip, peer_port=port, trust_state="trusted")
                    self.refresh_chat_main()
                except Exception as exc:
                    self.receiver_log_box.append(f"Failed to save contact: {exc}\n")
            popup.dismiss()
        buttons.add_widget(make_button("success", text="允许", on_release=lambda *_: _decision(True)))
        buttons.add_widget(make_button("danger", text="拒绝", on_release=lambda *_: _decision(False)))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.open()

    def receiver_log(self, text: str) -> None:
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
        if CHAT_MESSAGE_LOG_PREFIX in text:
            try:
                payload = text.split(CHAT_MESSAGE_LOG_PREFIX, 1)[1].strip()
                obj = json.loads(payload)
                self.chat_messages_box.append(f"Incoming {obj.get('group_id')}: {obj.get('sender_peer_id')}: {obj.get('text')}\n")
                self.refresh_chat_view()
            except Exception:
                pass
        if "end reason=complete" in text:
            self.receiver_log_box.append(self.t("receive_transfer_finished") + "\n")
        elif "end reason=idle_timeout" in text:
            self.receiver_log_box.append(self.t("receive_idle_timeout_msg") + "\n")
        marker = TRANSFER_REQUEST_LOG_PREFIX
        if marker in text:
            payload = text.split(marker, 1)[1].strip()
            try:
                req = json.loads(payload)
            except Exception:
                return
            Clock.schedule_once(lambda _dt, data=req: self.show_transfer_request(data), 0)

    def show_transfer_request(self, req: Dict[str, object]) -> None:
        conn_id = int(req.get("conn_id") or 0)
        if conn_id in self.pending_request_popups:
            return
        self.pending_request_popups.add(conn_id)
        approval_dir = self.approval_dir
        approval_dir.mkdir(parents=True, exist_ok=True)
        request_path = approval_dir / f"{conn_id}.request.json"

        message = self.t(
            "incoming_request",
            sender=str(req.get("sender") or ""),
            name=str(req.get("name") or ""),
            size=format_file_size(int(req.get("size") or 0)),
            path=str(req.get("save_path") or ""),
            sha256=str(req.get("sha256") or ""),
        )
        content = BoxLayout(orientation="vertical", spacing=dp(10), padding=dp(12))
        lbl = make_label(text=message, halign="left", valign="top")
        bind_label_wrap(lbl)
        content.add_widget(lbl)
        policy_spinner = None
        resume_available = bool(req.get("resume_available"))
        if resume_available:
            resume_offset = int(req.get("resume_offset") or 0)
            total_size = int(req.get("size") or 0)
            resume_pct = float(req.get("resume_pct") or ((resume_offset * 100.0 / max(total_size, 1)) if total_size > 0 else 0.0))
            content.add_widget(make_label(
                text=self.t("resume_detected", done=format_file_size(resume_offset), total=format_file_size(total_size), pct=resume_pct),
                size_hint_y=None, height=dp(42), halign="left", valign="middle", color=THEME["muted_text"]
            ))
            policy_spinner = style_spinner(Spinner(
                text=self.t("policy_resume"),
                values=[self.t("policy_resume"), self.t("policy_overwrite"), self.t("policy_cancel")],
                font_name=UI_FONT,
                size_hint_y=None,
                height=dp(38),
            ))
            content.add_widget(policy_spinner)
        elif bool(req.get("conflict")):
            content.add_widget(make_label(text=self.t("file_conflict"), size_hint_y=None, height=dp(28), halign="left", valign="middle", color=THEME["muted_text"]))
            policy_spinner = style_spinner(Spinner(
                text=self.t("policy_rename"),
                values=[self.t("policy_rename"), self.t("policy_overwrite"), self.t("policy_cancel")],
                font_name=UI_FONT,
                size_hint_y=None,
                height=dp(38),
            ))
            content.add_widget(policy_spinner)
        buttons = BoxLayout(orientation="horizontal", size_hint_y=None, height=dp(42), spacing=dp(8))
        popup = style_popup(Popup(title=self.t("incoming_request_title"), title_font=UI_FONT, content=content, size_hint=(0.72, 0.60), auto_dismiss=False))

        def _selected_policy() -> str:
            if policy_spinner is None:
                return "overwrite"
            txt = str(policy_spinner.text or "")
            if txt == self.t("policy_resume"):
                return "resume"
            if txt == self.t("policy_overwrite"):
                return "overwrite"
            if txt == self.t("policy_cancel"):
                return "cancel"
            return "rename"

        def _decision(accepted: bool):
            policy = _selected_policy()
            if accepted and policy == "cancel":
                accepted = False
                reason = "file_exists_cancelled"
            else:
                reason = "accepted" if accepted else "rejected"
            target = approval_dir / (f"{conn_id}.accept" if accepted else f"{conn_id}.reject")
            try:
                payload = {"accepted": bool(accepted), "reason": reason, "file_policy": policy}
                target.write_text(json.dumps(payload, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
                try:
                    request_path.unlink()
                except Exception:
                    pass
                self.seen_request_files.discard(str(request_path))
                self.receiver_log_box.append(("Accepted" if accepted else "Rejected") + f" transfer request conn_id={conn_id}\n")
            except Exception as exc:
                self.receiver_log_box.append(f"Failed to write approval file: {exc}\n")
            self.pending_request_popups.discard(conn_id)
            popup.dismiss()

        buttons.add_widget(make_button("success", text=self.t("accept"), on_release=lambda *_: _decision(True)))
        buttons.add_widget(make_button("danger", text=self.t("reject"), on_release=lambda *_: _decision(False)))
        content.add_widget(buttons)
        apply_ui_font(content)
        popup.bind(on_dismiss=lambda *_: self.pending_request_popups.discard(conn_id))
        popup.open()

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


class RUDPTransferApp(App):
    lang = StringProperty("zh")
    title = "AgoraLink"
    icon = str(RESOURCE_DIR / "assets" / "app.png")

    def build(self):
        Window.size = (1180, 760)
        Window.clearcolor = THEME["window_bg"]
        self.root_widget = RUDPTransferRoot(self)
        return self.root_widget

    def on_stop(self):
        try:
            self.root_widget.on_stop()
        except Exception:
            pass


if __name__ == "__main__":
    RUDPTransferApp().run()
