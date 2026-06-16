#!/usr/bin/env python3
"""Pure presentation helpers for AgoraLink file-transfer UI text."""

from __future__ import annotations

from typing import Callable, Dict, Mapping

from chat_cards import truncate_filename


def format_file_size(num_bytes: object) -> str:
    try:
        n = float(max(0, int(num_bytes or 0)))
    except Exception:
        n = 0.0
    units = ["B", "KiB", "MiB", "GiB", "TiB"]
    for unit in units:
        if n < 1024.0 or unit == units[-1]:
            return f"{int(n)} B" if unit == "B" else f"{n:.2f} {unit}"
        n /= 1024.0
    return f"{int(n)} B"


def localized_error_key(code: object) -> str:
    value = str(code or "transfer_failed")
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
    return mapping.get(value, "transfer_failed")


def _lang(lang: object) -> str:
    return "zh" if str(lang or "").lower().startswith("zh") else "en"


def unnamed_file_text(lang: object = "en") -> str:
    return "未命名文件" if _lang(lang) == "zh" else "Unnamed file"


def remote_peer_text(lang: object = "en") -> str:
    return "对方" if _lang(lang) == "zh" else "Remote"


def file_card_title(lang: object = "en") -> str:
    return "文件传输" if _lang(lang) == "zh" else "File transfer"


def multi_file_card_title(lang: object = "en") -> str:
    return "多个文件" if _lang(lang) == "zh" else "Multiple files"


def multi_file_summary(count: object, lang: object = "en") -> str:
    try:
        value = int(count or 0)
    except Exception:
        value = 0
    return f"共 {value} 个文件，打包后发送" if _lang(lang) == "zh" else f"{value} files, sent as one ZIP"


def folder_not_supported_text(lang: object = "en") -> str:
    return "不支持文件夹，请选择文件" if _lang(lang) == "zh" else "Folders are not supported. Please select files."


def file_offer_title(lang: object = "en") -> str:
    return "文件邀请" if _lang(lang) == "zh" else "File invitation"


def file_waiting_text(peer_label: object = "", lang: object = "en") -> str:
    name = str(peer_label or "").strip() or ("对方" if _lang(lang) == "zh" else "remote")
    return f"等待 {name} 接受" if _lang(lang) == "zh" else f"Waiting for {name} to accept"


def file_incoming_text(peer_label: object = "", lang: object = "en") -> str:
    name = str(peer_label or "").strip() or ("对方" if _lang(lang) == "zh" else "remote")
    return f"来自 {name} 的文件邀请" if _lang(lang) == "zh" else f"File invitation from {name}"


def file_waiting_confirm_text(lang: object = "en") -> str:
    return "等待确认" if _lang(lang) == "zh" else "Waiting for confirmation"


def file_accepted_text(lang: object = "en") -> str:
    return "已接受，等待传输开始" if _lang(lang) == "zh" else "Accepted, waiting for transfer to start"


def file_rejected_local_text(lang: object = "en") -> str:
    return "已拒绝" if _lang(lang) == "zh" else "Rejected"


def file_rejected_by_peer_text(lang: object = "en") -> str:
    return "对方拒绝接收文件" if _lang(lang) == "zh" else "The receiver rejected this file"


def file_completed_text(lang: object = "en") -> str:
    return "已完成" if _lang(lang) == "zh" else "Completed"


def file_failed_text(reason: object = "", lang: object = "en") -> str:
    detail = str(reason or "").strip()
    if _lang(lang) == "zh":
        return "失败" + (f"：{detail}" if detail else "")
    return "Failed" + (f": {detail}" if detail else "")


def file_error_message(
    code: object = "",
    detail: object = "",
    *,
    lang: object = "en",
    translate: Callable[[str], str],
    detail_separator: str = " ",
) -> str:
    if str(code or "") in ("receiver_rejected", "user_rejected", "file_exists_cancelled"):
        return file_rejected_by_peer_text(lang)
    key = localized_error_key(code)
    detail_text = str(detail or "")
    if key == "transfer_failed":
        return translate("transfer_failed", reason=(detail_text or str(code or "") or translate("unknown")))
    msg = translate(key)
    if detail_text:
        msg = msg + str(detail_separator) + detail_text
    return msg


def file_resume_text(offset: object = 0, lang: object = "en") -> str:
    try:
        value = int(offset or 0)
    except Exception:
        value = 0
    if value > 0:
        return ("续传可用：" if _lang(lang) == "zh" else "Resume available: ") + format_file_size(value)
    return "续传可用" if _lang(lang) == "zh" else "Resume available"


def file_size_detail(
    size: object = 0,
    *,
    peer_label: object = "",
    path: object = "",
    prefix: object = "",
    lang: object = "en",
) -> str:
    parts = []
    if prefix:
        parts.append(str(prefix))
    if peer_label:
        parts.append(("对象: " if _lang(lang) == "zh" else "Peer: ") + str(peer_label))
    try:
        size_value = int(size or 0)
    except Exception:
        size_value = 0
    if size_value > 0:
        parts.append(("大小: " if _lang(lang) == "zh" else "Size: ") + format_file_size(size_value))
    if path:
        parts.append(("保存路径: " if _lang(lang) == "zh" else "Saved: ") + str(path))
    return "  ".join(parts)


def transfer_status_label(status: object, *, direction: object = "", lang: object = "en", translate: Callable[[str], str]) -> str:
    value = str(status or "").strip().lower()
    mapping = {
        "queued": translate("pending"),
        "offered": translate("pending"),
        "accepted": translate("pending"),
        "started": file_accepted_text(lang),
        "transferring": translate("receiving") if str(direction or "") == "incoming" else translate("sending"),
        "received": file_completed_text(lang),
        "completed": file_completed_text(lang),
        "failed": file_failed_text("", lang),
    }
    return mapping.get(value, str(status or ""))


def file_progress_text(
    progress: Mapping[str, object],
    *,
    total_size: object = 0,
    summary: object = "",
    translate: Callable[[str], str],
) -> str:
    prog = dict(progress or {})
    if prog:
        sent = int(prog.get("sent") or 0)
        total = int(prog.get("total") or total_size or 0)
        pct = float(prog.get("pct") or ((sent * 100.0 / total) if total else 0.0))
        state_raw = str(prog.get("state") or translate("receiving"))
        status_map = {
            "queued": translate("pending"),
            "offered": translate("pending"),
            "accepted": translate("pending"),
            "transferring": translate("receiving"),
            "completed": translate("completed"),
            "received": translate("received"),
            "failed": translate("failed"),
        }
        state = status_map.get(state_raw, state_raw)
        avg = prog.get("avg")
        eta = str(prog.get("eta") or "")
        rate_part = f"  {float(avg):.2f} Mbps" if avg not in (None, "", 0) else ""
        eta_part = f"  ETA {eta}" if eta and eta != "unknown" else ""
        return f"{state}: {pct:.1f}%  {format_file_size(sent)} / {format_file_size(total)}{rate_part}{eta_part}"
    total = int(total_size or 0)
    if total > 0:
        return f'{translate("state")}: {summary or translate("pending")}  {translate("total_size")}: {format_file_size(total)}'
    return f'{translate("state")}: {summary or translate("pending")}  {translate("total_size")}: {translate("unknown_size")}'


def file_progress_detail(
    *,
    sent: object = 0,
    total: object = 0,
    pct: object = 0.0,
    avg: object = None,
    current: object = None,
    peak: object = None,
    elapsed: object = None,
    eta: object = "",
    complete: bool = False,
    unknown_size: object = "Unknown size",
) -> str:
    sent_value = int(sent or 0)
    total_value = int(total or 0)

    def _mbps(value) -> str:
        try:
            return f"{float(value):.1f}"
        except Exception:
            return "0.0"

    current_part = f"Cur {_mbps(current)} Mbps" if current not in (None, "", 0) else ""
    avg_part = f"Avg {_mbps(avg)} Mbps" if avg not in (None, "", 0) else ""
    peak_part = f"Peak {_mbps(peak)} Mbps" if peak not in (None, "", 0) else ""
    elapsed_part = ""
    try:
        if elapsed not in (None, "", 0):
            elapsed_part = f"{float(elapsed):.1f}s"
    except Exception:
        elapsed_part = ""
    eta_text = str(eta or "")
    eta_part = f"ETA {eta_text}" if eta_text and eta_text != "unknown" else ""
    metric_line = " · ".join([item for item in (current_part, avg_part, peak_part, elapsed_part, eta_part) if item])
    if total_value > 0:
        size_line = f"{format_file_size(sent_value)} / {format_file_size(total_value)}" if sent_value > 0 and not complete else format_file_size(total_value)
        return f"{size_line}  {metric_line}" if metric_line else size_line
    return metric_line or str(unknown_size)


def file_transfer_card_detail(
    *,
    lang: object = "en",
    transferred: object = 0,
    total: object = 0,
    pct: object = 0.0,
    avg: object = 0.0,
    eta: object = "",
    detail: object = "",
    saved_path: object = "",
    error: object = "",
    display_name: object = "",
    original_name: object = "",
) -> str:
    parts = []
    total_value = int(total or 0)
    transferred_value = int(transferred or 0)
    if total_value > 0:
        parts.append(f"{float(pct or 0.0):.1f}%")
        parts.append(f"{format_file_size(transferred_value)} / {format_file_size(total_value)}")
    elif transferred_value:
        parts.append(format_file_size(transferred_value))
    if detail:
        parts.append(str(detail))
    if avg:
        parts.append(f"{float(avg):.2f} Mbps")
    eta_text = str(eta or "")
    if eta_text and eta_text != "unknown":
        parts.append(f"ETA {eta_text}")
    if saved_path:
        parts.append(("保存路径: " if _lang(lang) == "zh" else "Saved: ") + str(saved_path))
    if error:
        parts.append(str(error))
    if not parts:
        parts.append("Waiting")
    if str(display_name or "") != str(original_name or ""):
        parts.append(f"name: {original_name}")
    return "  ".join(parts)


def is_failed_status(status_text: object, *, failed_label: object = "", lang: object = "en") -> bool:
    status = str(status_text or "")
    lower = status.lower()
    label = str(failed_label or "").lower()
    if lower == "failed" or (label and lower == label) or (label and lower.startswith(label)):
        return True
    return _lang(lang) == "zh" and status.startswith("失败")
