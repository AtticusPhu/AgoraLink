#!/usr/bin/env python3
"""Physical-pixel geometry helpers shared by AgoraLink Kivy surfaces."""

from __future__ import annotations

from typing import Tuple

from kivy.metrics import dp


def snap_px(value: float) -> float:
    return float(round(float(value)))


def snap_dp(value_dp: float) -> float:
    return snap_px(dp(value_dp))


def snap_rect(x: float, y: float, width: float, height: float) -> Tuple[float, float, float, float]:
    left = round(float(x))
    bottom = round(float(y))
    right = round(float(x) + float(width))
    top = round(float(y) + float(height))
    return float(left), float(bottom), float(right - left), float(top - bottom)


def snap_centered_extent(total: float, desired: float, minimum: float = 0) -> float:
    total_i = max(0, int(round(float(total))))
    minimum_i = max(0, min(total_i, int(round(float(minimum)))))
    desired_i = max(minimum_i, min(total_i, int(round(float(desired)))))
    if (total_i - desired_i) % 2:
        if desired_i + 1 <= total_i:
            desired_i += 1
        elif desired_i - 1 >= minimum_i:
            desired_i -= 1
    return float(desired_i)


def split_integer_width(total: float, gap: float, left_ratio: float) -> Tuple[float, float, float]:
    total_i = max(0, int(round(float(total))))
    gap_i = max(0, min(total_i, int(round(float(gap)))))
    usable = total_i - gap_i
    left = max(0, min(usable, int(round(usable * float(left_ratio)))))
    right = usable - left
    return float(left), float(right), float(gap_i)


def popup_geometry(
    window_width: float,
    window_height: float,
    *,
    desired_width: float,
    desired_height: float,
    horizontal_margin: float,
    vertical_margin: float,
    minimum_width: float = 0,
    minimum_height: float = 0,
) -> Tuple[float, float, float, float]:
    window_width_i = max(0, int(round(float(window_width))))
    window_height_i = max(0, int(round(float(window_height))))
    available_width = max(0, window_width_i - 2 * int(round(float(horizontal_margin))))
    available_height = max(0, window_height_i - 2 * int(round(float(vertical_margin))))
    width = snap_centered_extent(
        window_width_i,
        min(float(desired_width), available_width),
        min(float(minimum_width), available_width),
    )
    height = snap_centered_extent(
        window_height_i,
        min(float(desired_height), available_height),
        min(float(minimum_height), available_height),
    )
    x = snap_px((window_width_i - width) / 2.0)
    y = snap_px((window_height_i - height) / 2.0)
    return x, y, width, height


def setting_row_column_widths(
    total_width: float,
    *,
    gap: float,
    left_ratio: float = 0.47,
    minimum_left: float = 0,
    maximum_left: float | None = None,
) -> Tuple[float, float, float]:
    left, right, gap_i = split_integer_width(total_width, gap, left_ratio)
    usable = int(round(left + right))
    minimum_i = max(0, min(usable, int(round(float(minimum_left)))))
    maximum_i = usable if maximum_left is None else max(minimum_i, min(usable, int(round(float(maximum_left)))))
    left_i = max(minimum_i, min(maximum_i, int(round(left))))
    return float(left_i), float(usable - left_i), gap_i


def centered_content_geometry(container_x: float, container_width: float, desired_width: float) -> Tuple[float, float]:
    available = max(0, int(round(float(container_width))))
    width = min(max(0, int(round(float(desired_width)))), available)
    x = snap_px(float(container_x) + (available - width) / 2.0)
    return x, float(width)
