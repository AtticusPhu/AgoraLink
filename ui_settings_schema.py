#!/usr/bin/env python3
"""Single source of truth for AgoraLink settings navigation and validation."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Callable, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple


Validator = Callable[[object], Optional[str]]

DANGER_ACTION_IDS = frozenset(
    {
        "delete_contact",
        "leave_group",
        "dissolve_group",
        "clear_trust",
        "clear_chat_database",
        "reset_identity",
    }
)


def danger_action_requires_confirmation(action_id: str) -> bool:
    return str(action_id or "").strip().lower() in DANGER_ACTION_IDS


@dataclass(frozen=True)
class SettingChoice:
    value: object
    label_zh: str
    label_en: str

    def label(self, lang: str) -> str:
        return self.label_en if str(lang).lower().startswith("en") else self.label_zh


@dataclass(frozen=True)
class SettingsSectionDefinition:
    key: str
    order: int
    label_zh: str
    label_en: str
    description_zh: str
    description_en: str

    def label(self, lang: str) -> str:
        return self.label_en if str(lang).lower().startswith("en") else self.label_zh

    def description(self, lang: str) -> str:
        return self.description_en if str(lang).lower().startswith("en") else self.description_zh


@dataclass(frozen=True)
class SettingDefinition:
    key: str
    section: str
    order: int
    label_zh: str
    label_en: str
    description_zh: str
    description_en: str
    control_type: str
    default: object = None
    unit_zh: str = ""
    unit_en: str = ""
    minimum: Optional[float] = None
    maximum: Optional[float] = None
    step: Optional[float] = None
    choices: Tuple[SettingChoice, ...] = field(default_factory=tuple)
    advanced: bool = False
    restart_required: bool = False
    sensitive: bool = False
    persist: bool = True
    validator: Optional[Validator] = None
    formatter: Optional[Callable[[object], str]] = None
    visible_when: str = ""
    enabled_when: str = ""

    def label(self, lang: str) -> str:
        return self.label_en if str(lang).lower().startswith("en") else self.label_zh

    def description(self, lang: str) -> str:
        return self.description_en if str(lang).lower().startswith("en") else self.description_zh

    def unit(self, lang: str) -> str:
        return self.unit_en if str(lang).lower().startswith("en") else self.unit_zh

    def choice_label(self, value: object, lang: str) -> str:
        for choice in self.choices:
            if choice.value == value or str(choice.value) == str(value):
                return choice.label(lang)
        return str(value if value is not None else "")

    def choice_value(self, label_or_value: object, lang: str) -> object:
        raw = str(label_or_value if label_or_value is not None else "")
        for choice in self.choices:
            if raw in {str(choice.value), choice.label(lang), choice.label_zh, choice.label_en}:
                return choice.value
        return label_or_value


SETTINGS_SECTIONS: Tuple[SettingsSectionDefinition, ...] = (
    SettingsSectionDefinition("general", 10, "常规", "General", "管理语言、设备名称和应用行为。", "Manage language, device name, and application behavior."),
    SettingsSectionDefinition("network", 20, "网络与发现", "Network & Discovery", "查看此设备的局域网连接和接收状态。", "Review this device's local network connection and receive status."),
    SettingsSectionDefinition("transfer", 30, "文件传输", "File Transfer", "设置文件保存位置和传输偏好。", "Choose where files are saved and how transfers behave."),
    SettingsSectionDefinition("screen", 40, "屏幕共享", "Screen Sharing", "配置屏幕共享的画质、声音与连接选项。", "Configure screen sharing quality, sound, and connection options."),
    SettingsSectionDefinition("privacy", 50, "隐私与安全", "Privacy & Security", "管理受信任设备、聊天数据和确认行为。", "Manage trusted devices, chat data, and confirmation behavior."),
    SettingsSectionDefinition("storage", 60, "存储与诊断", "Storage & Diagnostics", "查看存储位置、运行状态并导出诊断信息。", "Review storage locations, runtime status, and diagnostic exports."),
    SettingsSectionDefinition("about", 70, "关于", "About", "查看版本、构建和内置组件信息。", "Review version, build, and bundled component information."),
)


def _choice(value: object, zh: str, en: str) -> SettingChoice:
    return SettingChoice(value, zh, en)


SETTING_DEFINITIONS: Tuple[SettingDefinition, ...] = (
    SettingDefinition(
        "language", "general", 10, "语言", "Language",
        "选择界面使用的语言。", "Choose the language used by the interface.",
        "select", "zh", choices=(_choice("zh", "简体中文", "Simplified Chinese"), _choice("en", "English", "English")),
    ),
    SettingDefinition(
        "theme_mode", "general", 20, "外观", "Appearance",
        "选择浅色或深色界面，更改会立即生效。", "Choose a light or dark interface. Changes apply immediately.",
        "select", "light", choices=(
            _choice("light", "浅色", "Light"),
            _choice("dark", "深色", "Dark"),
        ),
    ),
    SettingDefinition(
        "device_display_name", "general", 30, "设备显示名称", "Device display name",
        "附近设备和联系人看到的名称。", "The name shown to nearby devices and contacts.",
        "text", "AgoraLink", validator=lambda value: None if str(value or "").strip() else "required",
    ),
    SettingDefinition(
        "receiver_port", "network", 20, "接收端口", "Receive port",
        "文件、聊天和控制消息使用的本地 UDP 端口。", "Local UDP port used for files, chat, and control messages.",
        "number", 9999, unit_zh="端口", unit_en="port", minimum=1024, maximum=65535, step=1, restart_required=True,
    ),
    SettingDefinition(
        "discovery_port", "network", 30, "发现端口", "Discovery port",
        "用于在局域网内查找 AgoraLink 设备。", "Used to find AgoraLink devices on the local network.",
        "number", 9998, unit_zh="端口", unit_en="port", minimum=1024, maximum=65535, step=1,
    ),
    SettingDefinition(
        "lan_discovery_enabled", "network", 40, "允许局域网发现", "Allow LAN discovery",
        "允许此设备查找同一局域网内的 AgoraLink。", "Allow this device to find AgoraLink peers on the same LAN.",
        "toggle", True,
    ),
    SettingDefinition(
        "start_receiver_on_unlock", "network", 50, "启动接收服务", "Start receive service",
        "解锁聊天后自动开始接收服务。", "Start the receive service automatically after chat is unlocked.",
        "toggle", True,
    ),
    SettingDefinition(
        "current_lan_address", "network", 60, "当前局域网地址", "Current LAN address",
        "此设备当前可用的局域网地址。", "LAN addresses currently available to this device.",
        "readonly", "-", persist=False,
    ),
    SettingDefinition(
        "receiver_status", "network", 70, "接收状态", "Receive status",
        "显示接收服务当前是否运行。", "Shows whether the receive service is running.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "firewall_status", "network", 80, "防火墙状态", "Firewall status",
        "重新检测 Windows 防火墙和接收端口。", "Check Windows Firewall and the receive port again.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "bind_address", "network", 100, "监听地址", "Listen address",
        "仅在需要限制本机网络接口时修改。", "Change only when the receive service must use a specific network interface.",
        "text", "0.0.0.0", advanced=True, restart_required=True,
    ),
    SettingDefinition(
        "discovery_timeout_sec", "network", 110, "设备查找等待时间", "Discovery wait time",
        "等待附近设备响应的最长时间。", "Maximum time to wait for nearby devices to respond.",
        "number", 20.0, unit_zh="秒", unit_en="sec", minimum=0.5, maximum=60.0, step=0.5, advanced=True,
    ),
    SettingDefinition(
        "save_directory", "transfer", 10, "保存到", "Save to",
        "接收到的文件默认保存位置。", "Default location for received files.",
        "path", "", validator=lambda value: None if str(value or "").strip() else "required",
    ),
    SettingDefinition(
        "file_conflict_policy", "transfer", 20, "文件已存在时", "When a file already exists",
        "选择接收同名文件时的处理方式。", "Choose how to handle a received file with the same name.",
        "select", "auto", choices=(
            _choice("auto", "根据文件状态询问", "Ask based on file state"),
            _choice("rename", "自动重命名", "Rename automatically"),
            _choice("overwrite", "覆盖现有文件", "Replace existing file"),
            _choice("cancel", "取消接收", "Cancel the transfer"),
        ),
    ),
    SettingDefinition(
        "transfer_resume_enabled", "transfer", 30, "继续未完成传输", "Resume incomplete transfers",
        "检测到未完成文件时允许从已接收位置继续。", "Allow an incomplete file to continue from the received offset.",
        "toggle", True,
    ),
    SettingDefinition(
        "auto_package_multi_selection", "transfer", 40, "多文件自动打包", "Package multiple files automatically",
        "一次选择多个文件时自动创建 ZIP 传输包。", "Create a ZIP transfer package when several files are selected.",
        "toggle", True,
    ),
    SettingDefinition(
        "active_transfer_limit", "transfer", 50, "同时活动任务", "Concurrent transfers",
        "当前版本按顺序处理传输任务。", "This version processes transfer tasks in order.",
        "readonly", 1, unit_zh="个", unit_en="task", persist=False,
    ),
    SettingDefinition(
        "disk_space_status", "transfer", 60, "可用空间", "Available space",
        "接收目录所在磁盘的可用空间。", "Free space on the disk containing the receive folder.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "transfer_payload_size", "transfer", 100, "单个数据包大小", "Packet payload size",
        "较大值可减少包数量；请保持在局域网安全范围内。", "Larger values reduce packet count; keep this within a LAN-safe range.",
        "number", 1400, unit_zh="字节", unit_en="bytes", minimum=576, maximum=1452, step=4, advanced=True,
    ),
    SettingDefinition(
        "transfer_request_timeout_sec", "transfer", 110, "接收确认等待时间", "Receiver confirmation timeout",
        "等待对方接受或拒绝传输的最长时间。", "Maximum time to wait for the peer to accept or reject a transfer.",
        "number", 300, unit_zh="秒", unit_en="sec", minimum=5, maximum=1800, step=5, advanced=True,
    ),
    SettingDefinition(
        "transfer_completion_timeout_sec", "transfer", 120, "完成确认等待时间", "Completion confirmation timeout",
        "文件发送完毕后等待对方完成确认的最长时间。", "Maximum time to wait for final confirmation after sending a file.",
        "number", 180, unit_zh="秒", unit_en="sec", minimum=5, maximum=1800, step=5, advanced=True,
    ),
    SettingDefinition(
        "transfer_window_summary", "transfer", 130, "传输窗口", "Transfer window",
        "发送窗口、节奏和乱序容忍由当前稳定配置管理。", "Send window, pacing, and reordering tolerance use the current stable profile.",
        "readonly", "960–1536 · step 64 · pacing 32/5 ms · reorder 128", advanced=True, persist=False,
    ),
    SettingDefinition(
        "screen_native_preset", "screen", 10, "画质预设", "Quality preset",
        "选择适合网络和设备性能的画质档位。", "Choose a quality level that fits the network and device performance.",
        "select", "r4_default", choices=(
            _choice("stable", "稳定 · 720p30 · 20 Mbps", "Stable · 720p30 · 20 Mbps"),
            _choice("r4_default", "均衡（推荐）· 1080p60 · 22 Mbps", "Balanced (recommended) · 1080p60 · 22 Mbps"),
            _choice("recommended", "流畅高清 · 1080p60 · 50 Mbps", "Smooth HD · 1080p60 · 50 Mbps"),
            _choice("high_quality", "高画质 · 1080p60 · 80 Mbps", "High quality · 1080p60 · 80 Mbps"),
        ),
    ),
    SettingDefinition(
        "screen_preset_summary", "screen", 20, "当前参数", "Current profile",
        "当前预设的分辨率、帧率和目标码率。", "Resolution, frame rate, and target bitrate for the current preset.",
        "readonly", "-", persist=False,
    ),
    SettingDefinition(
        "screen_adaptive_quality", "screen", 30, "自适应画质", "Adaptive quality",
        "根据网络状态自动降低画质以保持流畅。", "Reduce quality automatically when the network becomes unstable.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "screen_share_system_audio", "screen", 50, "共享系统声音", "Share system audio",
        "共享应用、视频和通知等本机播放声音。", "Share sound played by applications, videos, and notifications.",
        "toggle", False, enabled_when="rust_audio_capture_available",
    ),
    SettingDefinition(
        "screen_audio_status", "screen", 60, "声音能力", "Audio capability",
        "显示当前设备是否支持系统声音采集与播放。", "Shows whether this device supports system audio capture and playback.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "screen_port", "screen", 80, "屏幕共享端口", "Screen sharing port",
        "从高位端口范围自动选择，避免受限端口。", "Selected automatically from a high-port range to avoid restricted ports.",
        "readonly", "55000–55999 · 自动", persist=False,
    ),
    SettingDefinition(
        "screen_receiver_status", "screen", 90, "接收状态", "Receive status",
        "屏幕共享接收端的当前状态。", "Current state of the screen sharing receiver.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "screen_engine_status", "screen", 110, "内置媒体引擎", "Built-in media engine",
        "屏幕捕获、硬件编码、解码、Direct3D 11 和声音能力。", "Screen capture, hardware encode/decode, Direct3D 11, and audio capabilities.",
        "readonly", "-", persist=False,
    ),
    SettingDefinition(
        "screen_repair_mode", "screen", 130, "连接恢复", "Connection recovery",
        "丢包时使用快速请求恢复缺失的视频数据。", "Requests missing video data quickly when packets are lost.",
        "readonly", "NACK", advanced=True, persist=False,
    ),
    SettingDefinition(
        "screen_playout_delay_ms", "screen", 140, "播放缓冲", "Playout buffer",
        "为网络抖动预留的播放缓冲时间。", "Playback buffer reserved for network jitter.",
        "readonly", 250, unit_zh="毫秒", unit_en="ms", advanced=True, persist=False,
    ),
    SettingDefinition(
        "screen_native_detail", "screen", 150, "预设技术摘要", "Preset technical summary",
        "当前预设使用的恢复、转换和渲染策略。", "Recovery, conversion, and rendering strategy used by the selected preset.",
        "readonly", "-", advanced=True, persist=False,
    ),
    SettingDefinition(
        "trusted_devices_summary", "privacy", 10, "受信任设备", "Trusted devices",
        "已允许建立加密会话的设备数量。", "Devices allowed to establish encrypted conversations.",
        "readonly", "-", persist=False,
    ),
    SettingDefinition(
        "recent_connection", "privacy", 20, "最近连接", "Recent connection",
        "最近使用的受信任设备。", "Most recently used trusted device.",
        "readonly", "-", persist=False,
    ),
    SettingDefinition(
        "fingerprint_summary", "privacy", 30, "本机身份摘要", "Device identity summary",
        "用于核对本机身份的缩略指纹。", "Short fingerprint used to verify this device's identity.",
        "readonly", "-", persist=False, sensitive=True,
    ),
    SettingDefinition(
        "chat_database_status", "privacy", 50, "聊天数据", "Chat data",
        "聊天数据库的锁定和可用状态。", "Lock and availability state of the chat database.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "chat_data_location", "privacy", 60, "数据位置", "Data location",
        "聊天数据保存在本机应用数据目录。", "Chat data is stored in the local application data folder.",
        "readonly", "-", persist=False, sensitive=True,
    ),
    SettingDefinition(
        "confirm_file_requests", "privacy", 80, "接收文件前询问", "Ask before receiving files",
        "每次收到文件请求时由用户确认。", "Require confirmation for every incoming file request.",
        "readonly", "已启用", persist=False,
    ),
    SettingDefinition(
        "confirm_screen_requests", "privacy", 90, "接收共享前询问", "Ask before receiving screen shares",
        "每次收到屏幕共享邀请时由用户确认。", "Require confirmation for every incoming screen sharing invitation.",
        "readonly", "已启用", persist=False,
    ),
    SettingDefinition(
        "app_data_dir", "storage", 10, "应用数据目录", "Application data folder",
        "配置、身份和本机状态的保存位置。", "Location for configuration, identity, and local state.",
        "readonly", "-", persist=False, sensitive=True,
    ),
    SettingDefinition(
        "downloads_dir", "storage", 20, "下载目录", "Downloads folder",
        "接收到的文件默认保存位置。", "Default location for received files.",
        "readonly", "-", persist=False, sensitive=True,
    ),
    SettingDefinition(
        "temp_dir", "storage", 30, "临时目录", "Temporary folder",
        "多文件打包和临时运行数据的位置。", "Location for multi-file packages and temporary runtime data.",
        "readonly", "-", persist=False, sensitive=True,
    ),
    SettingDefinition(
        "log_size", "storage", 40, "日志占用", "Log usage",
        "当前诊断日志占用空间。", "Space currently used by diagnostic logs.",
        "readonly", "-", persist=False,
    ),
    SettingDefinition(
        "application_status", "storage", 60, "应用状态", "Application status",
        "接收服务、聊天和后台任务摘要。", "Summary of receive service, chat, and background tasks.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "network_status", "storage", 70, "网络状态", "Network status",
        "主要端口和局域网可用性摘要。", "Summary of primary ports and LAN availability.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "native_media_status", "storage", 80, "屏幕共享状态", "Screen sharing status",
        "内置媒体组件和运行状态摘要。", "Summary of built-in media components and runtime state.",
        "status", "-", persist=False,
    ),
    SettingDefinition(
        "diagnostic_technical_summary", "storage", 100, "完整技术信息", "Full technical information",
        "端口、版本、组件校验和最近错误。", "Ports, versions, component verification, and recent errors.",
        "readonly", "-", advanced=True, persist=False, sensitive=True,
    ),
    SettingDefinition("about_version", "about", 10, "版本", "Version", "当前 AgoraLink 版本。", "Current AgoraLink version.", "readonly", "-", persist=False),
    SettingDefinition("about_build_commit", "about", 20, "构建提交", "Build commit", "当前构建对应的源代码提交。", "Source commit used for this build.", "readonly", "-", persist=False),
    SettingDefinition("about_build_date", "about", 30, "构建日期", "Build date", "当前构建的生成日期。", "Date this build was produced.", "readonly", "-", persist=False),
    SettingDefinition("about_package", "about", 40, "软件包", "Package", "当前运行的软件包类型。", "Type of package currently running.", "readonly", "-", persist=False),
    SettingDefinition("about_project", "about", 50, "项目主页", "Project home", "AgoraLink 项目地址。", "AgoraLink project location.", "readonly", "AtticusPhu/AgoraLink", persist=False),
    SettingDefinition("about_license", "about", 60, "许可证", "License", "应用与第三方组件的许可证状态。", "License status for the application and third-party components.", "readonly", "See repository", persist=False),
    SettingDefinition("about_components", "about", 70, "内置组件", "Bundled components", "Python、Kivy 与 Rust native media。", "Python, Kivy, and Rust native media.", "readonly", "-", persist=False),
    SettingDefinition(
        "about_full_technical_info", "about", 100, "完整构建信息", "Full build information",
        "完整路径、运行时和组件校验信息仅在复制时提供。", "Full paths, runtime, and component verification are provided only when copied.",
        "readonly", "-", advanced=True, persist=False, sensitive=True, visible_when="technical_details_visible",
    ),
)


SECTION_BY_KEY: Dict[str, SettingsSectionDefinition] = {item.key: item for item in SETTINGS_SECTIONS}
SETTING_BY_KEY: Dict[str, SettingDefinition] = {item.key: item for item in SETTING_DEFINITIONS}

SECTION_GROUPS: Dict[str, Tuple[Tuple[str, Tuple[str, ...]], ...]] = {
    "general": (
        ("interface", ("language", "theme_mode")),
        ("device", ("device_display_name",)),
    ),
    "network": (
        ("device", ("current_lan_address", "receiver_status")),
        ("lan", ("receiver_port", "discovery_port", "lan_discovery_enabled", "start_receiver_on_unlock", "firewall_status")),
    ),
    "transfer": (
        ("receive_files", ("save_directory", "file_conflict_policy", "transfer_resume_enabled", "disk_space_status")),
        ("send_queue", ("auto_package_multi_selection", "active_transfer_limit")),
    ),
    "screen": (
        ("quality", ("screen_native_preset", "screen_preset_summary", "screen_adaptive_quality")),
        ("sound", ("screen_share_system_audio", "screen_audio_status")),
        ("connection", ("screen_port", "screen_receiver_status")),
        ("media_engine", ("screen_engine_status",)),
    ),
    "privacy": (
        ("trusted_devices", ("trusted_devices_summary", "recent_connection", "fingerprint_summary")),
        ("chat_data", ("chat_database_status", "chat_data_location")),
        ("confirmation", ("confirm_file_requests", "confirm_screen_requests")),
    ),
    "storage": (
        ("storage_locations", ("app_data_dir", "downloads_dir", "temp_dir", "log_size")),
        ("diagnostic_status", ("application_status", "network_status", "native_media_status")),
    ),
    "about": (
        ("product", ("about_version", "about_package", "about_project")),
        ("build", ("about_build_commit", "about_build_date", "about_license", "about_components")),
    ),
}


def ordered_sections() -> Tuple[SettingsSectionDefinition, ...]:
    return tuple(sorted(SETTINGS_SECTIONS, key=lambda item: item.order))


def settings_for_section(section: str, *, include_advanced: bool = True) -> Tuple[SettingDefinition, ...]:
    values = [item for item in SETTING_DEFINITIONS if item.section == section]
    if not include_advanced:
        values = [item for item in values if not item.advanced]
    return tuple(sorted(values, key=lambda item: item.order))


def _coerce_number(definition: SettingDefinition, value: object) -> object:
    if value in (None, ""):
        return definition.default
    number = float(value)
    if definition.step is not None and float(definition.step).is_integer() and float(number).is_integer():
        return int(number)
    return number


def normalize_setting_value(definition: SettingDefinition, value: object) -> object:
    if definition.control_type == "toggle":
        if isinstance(value, str):
            return value.strip().lower() in {"1", "true", "yes", "on", "enabled", "system"}
        return bool(value)
    if definition.control_type == "number":
        return _coerce_number(definition, value)
    if definition.control_type == "select":
        candidate = definition.choice_value(value, "en")
        allowed = {str(choice.value) for choice in definition.choices}
        return candidate if str(candidate) in allowed else definition.default
    if definition.control_type in {"text", "path"}:
        return str(value if value is not None else "")
    return value if value is not None else definition.default


def validate_setting_value(definition: SettingDefinition, value: object) -> Optional[str]:
    try:
        normalized = normalize_setting_value(definition, value)
    except (TypeError, ValueError):
        return "invalid"
    if definition.control_type == "number":
        number = float(normalized)
        if definition.minimum is not None and number < float(definition.minimum):
            return "range"
        if definition.maximum is not None and number > float(definition.maximum):
            return "range"
    if definition.control_type == "select" and definition.choices:
        allowed = {str(choice.value) for choice in definition.choices}
        if str(normalized) not in allowed:
            return "invalid"
    if definition.validator is not None:
        return definition.validator(normalized)
    return None


class SettingsModel:
    """Pure settings state used by both the Kivy page and unit tests."""

    def __init__(
        self,
        values: Optional[Mapping[str, object]] = None,
        *,
        context: Optional[Mapping[str, object]] = None,
    ) -> None:
        self.context: Dict[str, object] = dict(context or {})
        self.values: Dict[str, object] = {
            definition.key: definition.default for definition in SETTING_DEFINITIONS
        }
        for key, value in dict(values or {}).items():
            definition = SETTING_BY_KEY.get(str(key))
            if definition is None:
                continue
            try:
                self.values[definition.key] = normalize_setting_value(definition, value)
            except (TypeError, ValueError):
                self.values[definition.key] = definition.default
        self.errors: Dict[str, str] = {}

    def is_visible(self, definition: SettingDefinition) -> bool:
        if not definition.visible_when:
            return True
        return bool(self.context.get(definition.visible_when))

    def is_enabled(self, definition: SettingDefinition) -> bool:
        if not definition.enabled_when:
            return True
        return bool(self.context.get(definition.enabled_when))

    def set_value(self, key: str, value: object) -> None:
        definition = SETTING_BY_KEY[key]
        self.values[key] = normalize_setting_value(definition, value)
        self.errors.pop(key, None)

    def validate(self, *, section: str = "") -> Dict[str, str]:
        errors: Dict[str, str] = {}
        for definition in SETTING_DEFINITIONS:
            if section and definition.section != section:
                continue
            if not definition.persist or not self.is_visible(definition):
                continue
            error = validate_setting_value(definition, self.values.get(definition.key))
            if error:
                errors[definition.key] = error
        self.errors = errors
        return dict(errors)

    def reset_section(self, section: str) -> None:
        for definition in settings_for_section(section):
            if definition.persist:
                self.values[definition.key] = definition.default
                self.errors.pop(definition.key, None)

    def reset_all(self) -> None:
        for definition in SETTING_DEFINITIONS:
            if definition.persist:
                self.values[definition.key] = definition.default
        self.errors.clear()

    def serializable_values(self) -> Dict[str, object]:
        result: Dict[str, object] = {}
        for definition in SETTING_DEFINITIONS:
            if not definition.persist or not self.is_visible(definition):
                continue
            result[definition.key] = normalize_setting_value(
                definition, self.values.get(definition.key, definition.default)
            )
        return result


def schema_errors() -> List[str]:
    errors: List[str] = []
    seen: set[str] = set()
    section_keys = {section.key for section in SETTINGS_SECTIONS}
    for definition in SETTING_DEFINITIONS:
        if definition.key in seen:
            errors.append(f"duplicate setting key: {definition.key}")
        seen.add(definition.key)
        if definition.section not in section_keys:
            errors.append(f"unknown section for {definition.key}: {definition.section}")
        if not definition.label_zh or not definition.label_en:
            errors.append(f"missing bilingual label: {definition.key}")
        if definition.control_type == "number":
            if definition.minimum is None or definition.maximum is None:
                errors.append(f"numeric range missing: {definition.key}")
            elif float(definition.minimum) > float(definition.maximum):
                errors.append(f"numeric range reversed: {definition.key}")
            elif not (float(definition.minimum) <= float(definition.default) <= float(definition.maximum)):
                errors.append(f"numeric default out of range: {definition.key}")
    return errors


def persisted_setting_keys() -> Tuple[str, ...]:
    return tuple(definition.key for definition in SETTING_DEFINITIONS if definition.persist)


def merge_legacy_config(config: Optional[Mapping[str, object]]) -> Dict[str, object]:
    """Read legacy GUI settings without retaining removed backend fields."""
    source = dict(config or {})
    aliases = {
        "screen_share_audio_mode": "screen_share_system_audio",
        "screen_preset": "screen_native_preset",
        "save_dir": "save_directory",
        "payload_size": "transfer_payload_size",
        "request_timeout": "transfer_request_timeout_sec",
        "complete_timeout": "transfer_completion_timeout_sec",
    }
    migrated: Dict[str, object] = {}
    for key, value in source.items():
        target = aliases.get(key, key)
        if target == "screen_share_system_audio" and key == "screen_share_audio_mode":
            value = str(value or "").strip().lower() == "system"
        if target in SETTING_BY_KEY:
            migrated[target] = value
    return SettingsModel(migrated).serializable_values()


def definitions_contain_forbidden_media_terms() -> bool:
    forbidden = ("ffmpeg", "ffplay", "ffprobe")
    for definition in SETTING_DEFINITIONS:
        text = " ".join(
            (
                definition.key,
                definition.label_zh,
                definition.label_en,
                definition.description_zh,
                definition.description_en,
            )
        ).lower()
        if any(term in text for term in forbidden):
            return True
    return False
