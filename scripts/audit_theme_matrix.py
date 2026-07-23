#!/usr/bin/env python3
"""Capture the deterministic Light/Dark source UI evidence matrix.

The fixture is intentionally offline: it renders the real AgoraLink UI
components without starting discovery, databases, workers, or media processes.
"""

from __future__ import annotations

import argparse
import binascii
import ctypes
import csv
import json
import os
import struct
import sys
import zlib
from ctypes import wintypes
from pathlib import Path
from typing import Callable, Iterable

os.environ.setdefault("KIVY_NO_ARGS", "1")
PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--settle-sec", type=float, default=0.42)
    parser.add_argument("--width", type=int, default=1280)
    parser.add_argument("--height", type=int, default=720)
    return parser.parse_args()


ARGS = parse_args()

from kivy.config import Config

Config.set("graphics", "width", str(ARGS.width))
Config.set("graphics", "height", str(ARGS.height))
Config.set("graphics", "resizable", "0")
Config.set("input", "mouse", "mouse,multitouch_on_demand")

from kivy.app import App
from kivy.clock import Clock
from kivy.core.text import LabelBase
from kivy.core.window import Window
from kivy.metrics import Metrics, dp
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.floatlayout import FloatLayout
from kivy.uix.widget import Widget

for font_path in (Path(r"C:\Windows\Fonts\msyh.ttc"), Path(r"C:\Windows\Fonts\msyh.ttf")):
    if font_path.is_file():
        LabelBase.register(name="RUDP_UI", fn_regular=str(font_path))
        break

from ui_components import FileTransferCard, MessageBubble, ScreenShareCard
from ui_device_details import ContactDetailsPage
from ui_diagnostics import DiagnosticsPage
from ui_form_components import (
    ErrorStateDialog,
    SecondaryPopup,
    ToastMessage,
    _BackgroundBox,
    _bind_wrapped,
    _label,
    dark_spinner,
    secondary_button,
)
from ui_group_management import GroupManagementPage
from ui_screen_details import ScreenShareDetailsPage
from ui_settings import SettingsCenter
from ui_settings_schema import SETTING_DEFINITIONS
from ui_theme_controller import theme_controller
from ui_transfer_details import FileTransferDetailsPage


class _BitmapInfoHeader(ctypes.Structure):
    _fields_ = (
        ("biSize", wintypes.DWORD),
        ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    )


class _BitmapInfo(ctypes.Structure):
    _fields_ = (("bmiHeader", _BitmapInfoHeader), ("bmiColors", wintypes.DWORD * 3))


class _Rect(ctypes.Structure):
    _fields_ = (("left", wintypes.LONG), ("top", wintypes.LONG), ("right", wintypes.LONG), ("bottom", wintypes.LONG))


def _png_chunk(kind: bytes, payload: bytes) -> bytes:
    checksum = binascii.crc32(kind)
    checksum = binascii.crc32(payload, checksum) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", checksum)


def _write_bgra_png(path: Path, bgra: bytes, width: int, height: int) -> None:
    scanlines = bytearray()
    source_stride = width * 4
    for row_index in range(height):
        row = bgra[row_index * source_stride : (row_index + 1) * source_stride]
        rgb = bytearray(width * 3)
        rgb[0::3] = row[2::4]
        rgb[1::3] = row[1::4]
        rgb[2::3] = row[0::4]
        scanlines.append(0)
        scanlines.extend(rgb)
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    payload = b"\x89PNG\r\n\x1a\n"
    payload += _png_chunk(b"IHDR", header)
    payload += _png_chunk(b"IDAT", zlib.compress(bytes(scanlines), level=6))
    payload += _png_chunk(b"IEND", b"")
    path.write_bytes(payload)


def capture_client_png(path: Path, *, title: str) -> tuple[int, int]:
    """Capture the exact Win32 client area without OpenGL row-stride artifacts."""

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
    user32.FindWindowW.argtypes = (wintypes.LPCWSTR, wintypes.LPCWSTR)
    user32.FindWindowW.restype = wintypes.HWND
    user32.GetWindowThreadProcessId.argtypes = (wintypes.HWND, ctypes.POINTER(wintypes.DWORD))
    user32.GetWindowThreadProcessId.restype = wintypes.DWORD
    user32.IsWindowVisible.argtypes = (wintypes.HWND,)
    user32.IsWindowVisible.restype = wintypes.BOOL
    user32.GetClientRect.argtypes = (wintypes.HWND, ctypes.POINTER(_Rect))
    user32.GetClientRect.restype = wintypes.BOOL
    user32.GetDC.argtypes = (wintypes.HWND,)
    user32.GetDC.restype = wintypes.HDC
    user32.ReleaseDC.argtypes = (wintypes.HWND, wintypes.HDC)
    user32.ReleaseDC.restype = ctypes.c_int
    gdi32.CreateCompatibleDC.argtypes = (wintypes.HDC,)
    gdi32.CreateCompatibleDC.restype = wintypes.HDC
    gdi32.CreateCompatibleBitmap.argtypes = (wintypes.HDC, ctypes.c_int, ctypes.c_int)
    gdi32.CreateCompatibleBitmap.restype = wintypes.HBITMAP
    gdi32.SelectObject.argtypes = (wintypes.HDC, wintypes.HGDIOBJ)
    gdi32.SelectObject.restype = wintypes.HGDIOBJ
    gdi32.BitBlt.argtypes = (
        wintypes.HDC,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        wintypes.HDC,
        ctypes.c_int,
        ctypes.c_int,
        wintypes.DWORD,
    )
    gdi32.BitBlt.restype = wintypes.BOOL
    gdi32.GetDIBits.argtypes = (
        wintypes.HDC,
        wintypes.HBITMAP,
        wintypes.UINT,
        wintypes.UINT,
        wintypes.LPVOID,
        ctypes.POINTER(_BitmapInfo),
        wintypes.UINT,
    )
    gdi32.GetDIBits.restype = ctypes.c_int
    gdi32.DeleteObject.argtypes = (wintypes.HGDIOBJ,)
    gdi32.DeleteObject.restype = wintypes.BOOL
    gdi32.DeleteDC.argtypes = (wintypes.HDC,)
    gdi32.DeleteDC.restype = wintypes.BOOL

    hwnd = user32.FindWindowW(None, title)
    if not hwnd:
        candidates: list[tuple[int, int]] = []
        callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

        @callback_type
        def collect_window(candidate, _lparam):
            process_id = wintypes.DWORD()
            user32.GetWindowThreadProcessId(candidate, ctypes.byref(process_id))
            if process_id.value != os.getpid() or not user32.IsWindowVisible(candidate):
                return True
            candidate_rect = _Rect()
            if user32.GetClientRect(candidate, ctypes.byref(candidate_rect)):
                area = max(0, int(candidate_rect.right - candidate_rect.left)) * max(
                    0, int(candidate_rect.bottom - candidate_rect.top)
                )
                candidates.append((area, int(candidate)))
            return True

        user32.EnumWindows(collect_window, 0)
        if candidates:
            hwnd = wintypes.HWND(max(candidates)[1])
    if not hwnd:
        raise RuntimeError(f"Win32 audit window not found: {title}")
    rect = _Rect()
    if not user32.GetClientRect(hwnd, ctypes.byref(rect)):
        raise ctypes.WinError(ctypes.get_last_error())
    width = int(rect.right - rect.left)
    height = int(rect.bottom - rect.top)
    if width <= 0 or height <= 0:
        raise RuntimeError(f"invalid client extent: {width}x{height}")

    source_dc = user32.GetDC(hwnd)
    memory_dc = gdi32.CreateCompatibleDC(source_dc)
    bitmap = gdi32.CreateCompatibleBitmap(source_dc, width, height)
    old_object = gdi32.SelectObject(memory_dc, bitmap)
    try:
        if not gdi32.BitBlt(memory_dc, 0, 0, width, height, source_dc, 0, 0, 0x00CC0020):
            raise ctypes.WinError(ctypes.get_last_error())
        info = _BitmapInfo()
        info.bmiHeader.biSize = ctypes.sizeof(_BitmapInfoHeader)
        info.bmiHeader.biWidth = width
        info.bmiHeader.biHeight = -height
        info.bmiHeader.biPlanes = 1
        info.bmiHeader.biBitCount = 32
        info.bmiHeader.biCompression = 0
        buffer = ctypes.create_string_buffer(width * height * 4)
        rows = gdi32.GetDIBits(memory_dc, bitmap, 0, height, buffer, ctypes.byref(info), 0)
        if rows != height:
            raise RuntimeError(f"GetDIBits returned {rows}/{height} rows")
        _write_bgra_png(path, buffer.raw, width, height)
    finally:
        if old_object:
            gdi32.SelectObject(memory_dc, old_object)
        if bitmap:
            gdi32.DeleteObject(bitmap)
        if memory_dc:
            gdi32.DeleteDC(memory_dc)
        if source_dc:
            user32.ReleaseDC(hwnd, source_dc)
    return width, height


def fixture_context() -> dict[str, object]:
    return {
        "package_flavor_label": "Native Lite",
        "app_version": "v0.0.12",
        "python_runtime": sys.version.split()[0],
        "rust_native_available": True,
        "rust_audio_capture_available": True,
        "rust_audio_playback_available": True,
        "native_av_sync_supported": True,
        "bundled_ffmpeg_available": False,
        "native_screen_video_only": False,
        "current_lan_address": "192.168.1.42",
        "receiver_status": "正在运行",
        "receiver_status_kind": "success",
        "firewall_status": "可用",
        "firewall_status_kind": "success",
        "screen_preset_details": {},
        "application_status": "就绪",
        "application_status_kind": "success",
        "app_data_dir": r"C:\Users\Attic\AppData\Roaming\AgoraLink",
        "log_dir": r"C:\Users\Attic\AppData\Roaming\AgoraLink\logs",
    }


def initial_values(lang: str, theme: str) -> dict[str, object]:
    values = {item.key: item.default for item in SETTING_DEFINITIONS if item.persist}
    values.update(
        {
            "language": lang,
            "theme_mode": theme,
            "device_display_name": "Attic-Desktop",
            "receiver_port": 9999,
            "discovery_port": 9998,
            "screen_native_preset": "recommended",
        }
    )
    return values


def walk(widget: Widget):
    yield widget
    for child in reversed(getattr(widget, "children", ())):
        yield from walk(child)


def is_fractional(value: float) -> bool:
    return abs(float(value) - round(float(value))) > 1e-6


def safe_dismiss(item) -> None:
    if item is None:
        return
    try:
        item.dismiss()
    except Exception:
        pass


def settings_page(lang: str, theme: str, section: str) -> SettingsCenter:
    return SettingsCenter(
        lang=lang,
        initial_values=initial_values(lang, theme),
        context=fixture_context(),
        on_save=lambda _values: True,
        on_theme_change=lambda mode: theme_controller.set_mode(mode, persist=False),
        on_close=lambda: None,
        actions={
            "export_diagnostics": lambda: None,
            "copy_technical_info": lambda: None,
            "recheck_network": lambda: None,
        },
        initial_section=section,
    )


def contact_page(lang: str) -> ContactDetailsPage:
    return ContactDetailsPage(
        lang=lang,
        contact={
            "display_name": "Office Desktop" if lang == "en" else "办公室电脑",
            "peer_id": "peer-01A7",
            "peer_ip": "192.168.1.58",
            "peer_port": "9999",
            "online": True,
            "fingerprint": "F1:92:8A:44:0C:77",
            "trust_state": "Trusted" if lang == "en" else "已信任",
            "last_seen": "Just now" if lang == "en" else "刚刚",
        },
        on_close=lambda: None,
        on_message=lambda: None,
        on_send_file=lambda: None,
        on_share_screen=lambda: None,
        on_edit_note=lambda: None,
        on_delete=lambda: None,
    )


def group_page(lang: str) -> GroupManagementPage:
    return GroupManagementPage(
        lang=lang,
        group={"title": "Design Lab" if lang == "en" else "设计工作组", "group_id": "group-24"},
        members=(
            {"display_name": "Attic", "peer_id": "local", "role": "owner", "member_state": "active"},
            {"display_name": "Office PC", "peer_id": "peer-2", "role": "member", "member_state": "active"},
            {"display_name": "Meeting Room", "peer_id": "peer-3", "role": "member", "member_state": "active"},
        ),
        local_peer_id="local",
        can_manage=True,
        on_close=lambda: None,
        on_add_member=lambda: None,
        on_remove_member=lambda _peer: None,
        on_leave=lambda: None,
        on_dissolve=lambda: None,
    )


def transfer_page(lang: str) -> FileTransferDetailsPage:
    return FileTransferDetailsPage(
        lang=lang,
        details={
            "file_name": "AgoraLink_release_notes.pdf",
            "progress": 68,
            "status": "Transferring" if lang == "en" else "传输中",
            "size": "148.6 MB",
            "peer": "Office Desktop",
            "speed": "18.4 MB/s",
            "eta": "3 s",
            "saved_path": r"D:\Downloads\AgoraLink",
            "task_id": "transfer-024",
            "hash": "SHA256 6CB8...28A4",
        },
        on_close=lambda: None,
        actions={"open_folder": lambda: None, "cancel": lambda: None},
    )


def screen_page(lang: str) -> ScreenShareDetailsPage:
    return ScreenShareDetailsPage(
        lang=lang,
        details={
            "peer": "Meeting Room Display",
            "status": "Active" if lang == "en" else "投屏中",
            "direction": "Outgoing" if lang == "en" else "发送",
            "quality": "Recommended · 1920×1080 · 60 FPS",
            "audio": "System audio" if lang == "en" else "系统音频",
            "duration": "00:12:48",
            "connection": "Stable" if lang == "en" else "稳定",
            "session_id": "screen-81A2",
            "port": "55134/UDP",
            "profile": "recommended",
        },
        on_close=lambda: None,
        actions={"stop": lambda: None, "settings": lambda: None},
    )


def diagnostics_page(lang: str) -> DiagnosticsPage:
    return DiagnosticsPage(
        lang=lang,
        summary_items=(
            {"title": "Receiver", "status": "Running", "detail": "UDP 9999"},
            {"title": "Rust Native", "status": "Available", "detail": "Video + system audio"},
            {"title": "Network", "status": "Ready", "detail": "192.168.1.42"},
        ),
        technical_sections={
            "package_flavor": "native_lite",
            "screen_backend": "rust",
            "render_backend": "d3d11",
        },
        recent_errors="No recent errors" if lang == "en" else "没有最近错误",
        on_close=lambda: None,
        on_export=lambda: None,
        on_recheck=lambda: None,
    )


def primary_fixture(lang: str) -> Widget:
    zh = lang == "zh"
    switch_target = ("浅色" if zh else "Light") if theme_controller.mode == "dark" else ("深色" if zh else "Dark")
    root = _BackgroundBox(orientation="vertical", background_token="background", spacing=0)
    toolbar = _BackgroundBox(
        orientation="horizontal",
        size_hint_y=None,
        height=dp(52),
        padding=(dp(16), dp(7), dp(16), dp(7)),
        spacing=dp(8),
        background_token="surface",
        border_token="border_soft",
        border_width=1,
    )
    title = _label("AgoraLink", font_size=18, bold=True, halign="left")
    _bind_wrapped(title)
    toolbar.add_widget(title)
    toolbar.add_widget(Widget(size_hint_x=1))
    toolbar.add_widget(secondary_button(switch_target, width=82))
    toolbar.add_widget(secondary_button("设置" if zh else "Settings", width=96))
    root.add_widget(toolbar)

    body = BoxLayout(orientation="horizontal", spacing=dp(12), padding=dp(14))
    sidebar = _BackgroundBox(
        orientation="vertical",
        size_hint_x=None,
        width=dp(244),
        padding=dp(12),
        spacing=dp(8),
        background_token="surface",
        border_token="border_soft",
        border_width=1,
        radius=8,
    )
    sidebar.add_widget(_label("接收设备" if zh else "Receive devices", font_size=14, bold=True, size_hint_y=None, height=dp(30), halign="left"))
    for name, state in (("Office Desktop", "在线" if zh else "Online"), ("Meeting Room", "待机" if zh else "Idle")):
        row = _BackgroundBox(orientation="vertical", size_hint_y=None, height=dp(62), padding=dp(10), background_token="surface_muted", radius=6)
        row.add_widget(_label(name, font_size=13, bold=True, halign="left"))
        row.add_widget(_label(state, color_name="text_muted", font_size=12, halign="left"))
        sidebar.add_widget(row)
    sidebar.add_widget(Widget())
    body.add_widget(sidebar)

    content = _BackgroundBox(
        orientation="vertical",
        padding=dp(16),
        spacing=dp(10),
        background_token="surface",
        border_token="border_soft",
        border_width=1,
        radius=8,
    )
    content.add_widget(_label("仅接受" if zh else "Receive only", font_size=19, bold=True, size_hint_y=None, height=dp(34), halign="left"))
    content.add_widget(_label("等待局域网设备发送内容" if zh else "Waiting for content from LAN devices", color_name="text_secondary", font_size=13, size_hint_y=None, height=dp(28), halign="left"))
    content.add_widget(
        MessageBubble(
            direction="incoming",
            sender="Office Desktop",
            message="连接已建立。" if zh else "Connection established.",
            size_hint_x=None,
            width=dp(720),
        )
    )
    content.add_widget(FileTransferCard(filename="design-assets.zip", detail="68% · 18.4 MB/s", status="active", status_text="传输中" if zh else "Transferring", progress=68))
    content.add_widget(ScreenShareCard(peer="Meeting Room Display", detail="1920×1080 · 60 FPS · Rust Native", status="active", status_text="投屏中" if zh else "Active"))
    content.add_widget(Widget())
    body.add_widget(content)
    root.add_widget(body)
    return root


def menu_fixture(lang: str) -> Widget:
    root = FloatLayout()
    backdrop = primary_fixture(lang)
    root.add_widget(backdrop)
    panel = _BackgroundBox(
        orientation="vertical",
        size_hint=(None, None),
        size=(dp(230), dp(176)),
        pos=(dp(280), dp(380)),
        padding=dp(8),
        spacing=dp(3),
        background_token="menu_bg",
        border_token="border",
        border_width=1,
        radius=8,
    )
    labels = ("查看详情", "发送文件", "移除设备") if lang == "zh" else ("View details", "Send file", "Remove device")
    for index, label in enumerate(labels):
        panel.add_widget(secondary_button(label, variant="danger" if index == 2 else "ghost"))
    root.add_widget(panel)
    return root


def spinner_fixture(lang: str) -> tuple[Widget, Callable[[], None]]:
    root = _BackgroundBox(orientation="vertical", background_token="background", padding=dp(36), spacing=dp(16))
    root.add_widget(_label("主题选择" if lang == "zh" else "Theme selection", font_size=18, bold=True, size_hint_y=None, height=dp(36), halign="left"))
    values = ("浅色", "深色") if lang == "zh" else ("Light", "Dark")
    spinner = dark_spinner(text=values[0], values=values)
    spinner.size_hint = (None, None)
    spinner.size = (dp(320), dp(40))
    root.add_widget(spinner)
    root.add_widget(Widget())

    def open_dropdown() -> None:
        spinner.is_open = True

    return root, open_dropdown


def toast_fixture(lang: str) -> tuple[Widget, Callable[[], None]]:
    root = primary_fixture(lang)
    toast = ToastMessage("主题已切换" if lang == "zh" else "Theme updated", kind="success")

    def show_toast() -> None:
        toast.pos = (Window.width - toast.width - dp(24), dp(24))
        toast.show_in(root, seconds=8)

    return root, show_toast


class ThemeMatrixApp(App):
    def build(self):
        self.window_title = f"AgoraLink Theme Matrix {os.getpid()}"
        Window.title = self.window_title
        self.output_dir = Path(ARGS.output_dir).resolve()
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.root_surface = FloatLayout()
        self.active_popup = None
        self.active_spinner_close = None
        self.manifest: list[dict[str, object]] = []
        self.cases = [case for case in self._build_cases() if tuple(case["size"]) == (ARGS.width, ARGS.height)]
        self.case_index = 0
        theme_controller.configure({}, initial_mode="light")
        Clock.schedule_once(self._next_case, 0.25)
        return self.root_surface

    def _build_cases(self) -> list[dict[str, object]]:
        cases: list[dict[str, object]] = []
        for theme in ("light", "dark"):
            for lang in ("zh", "en"):
                for section in ("general", "network", "transfer", "screen", "about"):
                    cases.append({"kind": "settings", "name": f"settings_{section}", "theme": theme, "lang": lang, "size": (1280, 720), "section": section})
        for theme in ("light", "dark"):
            for lang in ("zh", "en"):
                for name in ("contact", "group", "file_details", "screen_details", "diagnostics"):
                    cases.append({"kind": "detail", "name": name, "theme": theme, "lang": lang, "size": (1366, 768)})
        for theme in ("light", "dark"):
            for lang in ("zh", "en"):
                for name in ("primary_receive_only", "context_menu", "confirmation", "error", "spinner", "toast"):
                    cases.append({"kind": "transient", "name": name, "theme": theme, "lang": lang, "size": (1920, 1080) if name == "primary_receive_only" else (1600, 900)})
        for theme in ("light", "dark"):
            for width in (1599, 1600, 1601):
                cases.append({"kind": "settings", "name": f"sharpness_{width}", "theme": theme, "lang": "zh", "size": (width, 900), "section": "general"})
        return cases

    def _cleanup(self) -> None:
        if self.active_spinner_close is not None:
            try:
                self.active_spinner_close()
            except Exception:
                pass
        self.active_spinner_close = None
        safe_dismiss(self.active_popup)
        self.active_popup = None
        self.root_surface.clear_widgets()

    def _next_case(self, _dt) -> None:
        self._cleanup()
        if self.case_index >= len(self.cases):
            self._write_manifest()
            Clock.schedule_once(lambda _next: self.stop(), 0.2)
            return
        case = self.cases[self.case_index]
        theme_controller.set_mode(case["theme"], persist=False)
        post_layout = self._show_case(case)
        if post_layout is not None:
            Clock.schedule_once(lambda _delay: post_layout(), 0.15)
        Clock.schedule_once(lambda _delay: self._capture(case), max(0.25, ARGS.settle_sec))

    def _show_popup(self, page: Widget, *, max_width: int = 1180, max_height: int = 820) -> None:
        popup = SecondaryPopup(
            title="",
            content=page,
            maximum_width=max_width,
            maximum_height=max_height,
            auto_dismiss=False,
            separator_height=0,
            background="",
        )
        self.active_popup = popup
        popup.open()

    def _show_case(self, case: dict[str, object]):
        kind = case["kind"]
        name = case["name"]
        lang = case["lang"]
        theme = case["theme"]
        if kind == "settings":
            self._show_popup(settings_page(lang, theme, case["section"]))
            return None
        if kind == "detail":
            builders = {
                "contact": contact_page,
                "group": group_page,
                "file_details": transfer_page,
                "screen_details": screen_page,
                "diagnostics": diagnostics_page,
            }
            self._show_popup(builders[name](lang), max_width=1080, max_height=780)
            return None
        if name == "primary_receive_only":
            self.root_surface.add_widget(primary_fixture(lang))
            return None
        if name == "context_menu":
            self.root_surface.add_widget(menu_fixture(lang))
            return None
        if name == "spinner":
            surface, callback = spinner_fixture(lang)
            self.root_surface.add_widget(surface)
            self.active_spinner_close = lambda: setattr(next(item for item in walk(surface) if hasattr(item, "is_open")), "is_open", False)
            return callback
        if name == "toast":
            surface, callback = toast_fixture(lang)
            self.root_surface.add_widget(surface)
            return callback
        if name == "confirmation":
            from ui_form_components import ConfirmationDialog

            dialog = ConfirmationDialog(
                lang=lang,
                title="确认停止投屏" if lang == "zh" else "Stop screen sharing?",
                message="当前连接将被安全关闭。" if lang == "zh" else "The current connection will close safely.",
                on_confirm=lambda: None,
                confirm_text="停止" if lang == "zh" else "Stop",
                danger=True,
            )
            self.active_popup = dialog.popup
            dialog.open()
            return None
        dialog = ErrorStateDialog(
            lang=lang,
            title="无法连接到设备" if lang == "zh" else "Unable to connect",
            reason="目标设备暂时不可用。" if lang == "zh" else "The target device is temporarily unavailable.",
            suggestion="检查双方是否位于同一局域网，然后重试。" if lang == "zh" else "Verify both devices are on the same LAN, then retry.",
            technical_details="Connection timed out after 5 seconds.",
            on_retry=lambda: None,
            on_settings=lambda: None,
        )
        self.active_popup = dialog.popup
        dialog.open()
        return None

    def _capture(self, case: dict[str, object]) -> None:
        prefix = f"{self.case_index + 1:02d}_{case['theme']}_{case['lang']}_{case['name']}_{case['size'][0]}x{case['size'][1]}"
        image_path = (self.output_dir / f"{prefix}.png").resolve()
        captured_width, captured_height = capture_client_png(image_path, title=self.window_title)
        all_widgets = list(walk(self.root_surface))
        if self.active_popup is not None and getattr(self.active_popup, "content", None) is not None:
            all_widgets.extend(walk(self.active_popup.content))
        geometry = [
            (float(item.x), float(item.y), float(item.width), float(item.height))
            for item in all_widgets
            if hasattr(item, "x") and hasattr(item, "width")
        ]
        fractional_widgets = []
        for item, values in zip(all_widgets, geometry):
            if not any(is_fractional(value) for value in values):
                continue
            fractional_widgets.append(
                {
                    "class": item.__class__.__name__,
                    "text": str(getattr(item, "text", ""))[:80],
                    "x": values[0],
                    "y": values[1],
                    "width": values[2],
                    "height": values[3],
                }
            )
        fractional = len(fractional_widgets)
        record = {
            **case,
            "index": self.case_index + 1,
            "image": image_path.name,
            "window_width": int(Window.width),
            "window_height": int(Window.height),
            "captured_client_width": captured_width,
            "captured_client_height": captured_height,
            "capture_backend": "win32-bitblt-getdibits",
            "kivy_density": float(Metrics.density),
            "kivy_dpi": float(Metrics.dpi),
            "widget_count": len(geometry),
            "fractional_geometry_count": fractional,
            "fractional_geometry_ratio": (fractional / len(geometry)) if geometry else 0.0,
            "fractional_widgets": fractional_widgets,
            "theme_revision": int(theme_controller.revision),
            "offline_fixture": True,
            "primary_mode_label": "receive_only" if case["name"] == "primary_receive_only" else None,
        }
        (self.output_dir / f"{prefix}.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        self.manifest.append(record)
        self.case_index += 1
        Clock.schedule_once(self._next_case, 0.12)

    def _write_manifest(self) -> None:
        manifest_path = self.output_dir / "screenshot_manifest.json"
        manifest_path.write_text(json.dumps(self.manifest, ensure_ascii=False, indent=2), encoding="utf-8")
        with (self.output_dir / "screenshot_manifest.csv").open("w", newline="", encoding="utf-8-sig") as handle:
            fields = (
                "index",
                "image",
                "kind",
                "name",
                "theme",
                "lang",
                "window_width",
                "window_height",
                "fractional_geometry_count",
                "fractional_geometry_ratio",
                "offline_fixture",
            )
            writer = csv.DictWriter(handle, fieldnames=fields, extrasaction="ignore")
            writer.writeheader()
            writer.writerows(self.manifest)
        summary = {
            "status": "PASS" if len(self.manifest) == len(self.cases) else "FAIL",
            "expected_cases": len(self.cases),
            "captured_cases": len(self.manifest),
            "light_cases": sum(item["theme"] == "light" for item in self.manifest),
            "dark_cases": sum(item["theme"] == "dark" for item in self.manifest),
            "chinese_cases": sum(item["lang"] == "zh" for item in self.manifest),
            "english_cases": sum(item["lang"] == "en" for item in self.manifest),
            "width_matrix": [1599, 1600, 1601],
            "actual_windows_dpi_validation": "MANUAL_REAL_DPI_VALIDATION_REQUIRED",
        }
        (self.output_dir / "matrix_summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    ThemeMatrixApp().run()
