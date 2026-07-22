#!/usr/bin/env python3
"""Capture a geometry-only pixel-snap A/B for the settings surface."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

os.environ.setdefault("KIVY_NO_ARGS", "1")
PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--width", type=int, default=1180)
    parser.add_argument("--height", type=int, default=760)
    parser.add_argument("--density", type=float, default=1.0)
    parser.add_argument("--dpi", type=float, default=96.0)
    parser.add_argument("--section", default="network")
    parser.add_argument("--output-dir", required=True)
    return parser.parse_args()


ARGS = parse_args()
os.environ["KIVY_METRICS_DENSITY"] = str(ARGS.density)
os.environ["KIVY_DPI"] = str(ARGS.dpi)

from kivy.config import Config

Config.set("graphics", "width", str(ARGS.width))
Config.set("graphics", "height", str(ARGS.height))
Config.set("graphics", "resizable", "0")
Config.set("input", "mouse", "mouse,multitouch_on_demand")

from kivy.app import App
from kivy.clock import Clock
from kivy.core.text import LabelBase
from kivy.core.window import Window
from kivy.graphics import Color, Rectangle
from kivy.metrics import Metrics, dp
from kivy.uix.behaviors import ButtonBehavior
from kivy.uix.boxlayout import BoxLayout
from kivy.uix.floatlayout import FloatLayout
from kivy.uix.label import Label
from kivy.uix.scrollview import ScrollView
from kivy.uix.spinner import Spinner
from kivy.uix.textinput import TextInput
from kivy.uix.widget import Widget

for font_path in (Path(r"C:\Windows\Fonts\msyh.ttc"), Path(r"C:\Windows\Fonts\msyh.ttf")):
    if font_path.is_file():
        LabelBase.register(name="RUDP_UI", fn_regular=str(font_path))
        break

from ui_form_components import SecondaryPopup, SettingRow, secondary_color
from ui_settings import SettingsCenter
from ui_settings_schema import SETTING_DEFINITIONS


def fixture_context() -> dict[str, object]:
    return {
        "rust_audio_capture_available": True,
        "current_lan_address": "192.168.1.42",
        "receiver_status": "正在运行",
        "receiver_status_kind": "success",
        "firewall_status": "可用",
        "firewall_status_kind": "success",
        "screen_preset_details": {},
        "application_status": "就绪",
        "application_status_kind": "success",
    }


def initial_values() -> dict[str, object]:
    values = {item.key: item.default for item in SETTING_DEFINITIONS if item.persist}
    values.update(
        {
            "language": "zh",
            "theme_mode": "dark",
            "device_display_name": "Attic-Desktop",
            "receiver_port": 9999,
            "discovery_port": 9998,
        }
    )
    return values


def walk(widget: Widget):
    yield widget
    for child in reversed(getattr(widget, "children", ())):
        yield from walk(child)


def is_fractional(value: float) -> bool:
    return abs(float(value) - round(float(value))) > 1e-6


def sample(widget: Widget, role: str) -> dict[str, object]:
    texture_size = getattr(widget, "texture_size", None)
    values = (widget.x, widget.y, widget.width, widget.height)
    return {
        "role": role,
        "class": widget.__class__.__name__,
        "text": str(getattr(widget, "text", ""))[:120],
        "x": float(widget.x),
        "y": float(widget.y),
        "width": float(widget.width),
        "height": float(widget.height),
        "right": float(widget.right),
        "top": float(widget.top),
        "font_size": float(getattr(widget, "font_size", 0) or 0),
        "texture_size": list(texture_size) if texture_size is not None else None,
        "fractional": any(is_fractional(value) for value in values),
    }


def centered_extent(total: float, desired: float, minimum: float = 0.0) -> float:
    total_i = int(round(total))
    minimum_i = int(round(minimum))
    desired_i = max(minimum_i, min(total_i, int(round(desired))))
    if (total_i - desired_i) % 2:
        if desired_i + 1 <= total_i:
            desired_i += 1
        elif desired_i - 1 >= minimum_i:
            desired_i -= 1
    return float(desired_i)


class AuditRoot(FloatLayout):
    def __init__(self, **kwargs) -> None:
        super().__init__(**kwargs)
        with self.canvas.before:
            self._bg_color = Color(*secondary_color("overlay"))
            self._bg = Rectangle(pos=self.pos, size=self.size)
        self.bind(pos=self._sync_bg, size=self._sync_bg)

    def _sync_bg(self, *_args) -> None:
        self._bg.pos = self.pos
        self._bg.size = self.size


class SharpnessABApp(App):
    def build(self):
        Window.size = (ARGS.width, ARGS.height)
        self.output_dir = Path(ARGS.output_dir).resolve()
        self.output_dir.mkdir(parents=True, exist_ok=True)
        self.root_surface = AuditRoot()
        self.settings = SettingsCenter(
            lang="zh",
            initial_values=initial_values(),
            context=fixture_context(),
            on_save=lambda _values: True,
            on_close=lambda: None,
            initial_section=ARGS.section,
        )
        self.popup = SecondaryPopup(
            title="",
            content=self.settings,
            size_hint=(0.94, 0.92),
            auto_dismiss=False,
            separator_height=0,
            background="",
            background_color=(0.035, 0.043, 0.055, 1),
        )
        Clock.schedule_once(lambda _dt: self.popup.open(), 0.1)
        Clock.schedule_once(self._capture_before, 2.2)
        return self.root_surface

    def _capture_before(self, _dt) -> None:
        self._write_capture("ab_original", "ab_geometry_before")
        self._apply_pixel_snap_only()
        Clock.schedule_once(self._capture_after, 0.8)

    def _capture_after(self, _dt) -> None:
        self._write_capture("ab_pixel_snapped", "ab_geometry_after")
        self._write_comparison()
        Clock.schedule_once(lambda _next: self.stop(), 0.2)

    def _apply_pixel_snap_only(self) -> None:
        desired_width = min(round(dp(1180)), round(Window.width - 2 * round(dp(24))))
        desired_height = min(round(dp(820)), round(Window.height - 2 * round(dp(24))))
        width = centered_extent(Window.width, desired_width, round(dp(720)))
        height = centered_extent(Window.height, desired_height, round(dp(560)))
        self.popup.size_hint = (None, None)
        self.popup.size = (width, height)
        self.popup.pos = (
            float(round((Window.width - width) / 2.0)),
            float(round((Window.height - height) / 2.0)),
        )

        scroll = next((item for item in walk(self.settings) if isinstance(item, ScrollView)), None)
        if scroll is not None and scroll.children:
            outer = scroll.children[0]
            if hasattr(outer, "content") and hasattr(outer, "_sync_content_geometry"):
                outer.max_width_dp = 900.0
                outer._sync_content_geometry()
            else:
                content = next(
                    (
                        child
                        for child in outer.children
                        if isinstance(child, BoxLayout) and child.size_hint_x is None
                    ),
                    None,
                )
                if content is None:
                    raise RuntimeError("Pixel-Snap A/B could not locate the centered content column")
                spacers = [child for child in outer.children if child is not content]
                if len(spacers) != 2:
                    raise RuntimeError(
                        "Pixel-Snap A/B expected two legacy centering spacers, "
                        f"found {len(spacers)}"
                    )
                for spacer in spacers:
                    spacer.size_hint_x = None

                def sync_center(instance, _value=None) -> None:
                    total = max(0, int(round(instance.width)))
                    content_width = min(int(round(dp(900))), total)
                    remaining = max(0, total - content_width)
                    left_width = remaining // 2
                    right_width = remaining - left_width
                    content.width = float(content_width)
                    # children are stored in reverse insertion order.
                    spacers[-1].width = float(left_width)
                    spacers[0].width = float(right_width)

                outer.bind(width=sync_center)
                sync_center(outer)

        for row in (item for item in walk(self.settings) if isinstance(item, SettingRow)):
            text_column = next((child for child in row.children if child is not row.control_column), None)
            if text_column is None:
                continue
            row.spacing = float(round(dp(18)))
            text_column.size_hint_x = None
            row.control_column.size_hint_x = None

            def sync_columns(instance, _value=None, left=text_column) -> None:
                gap = int(round(dp(18)))
                usable = max(0, int(round(instance.width)) - gap)
                left_width = int(round(usable * 0.47))
                left_width = max(int(round(dp(260))), min(int(round(dp(380))), left_width))
                left_width = min(usable, left_width)
                left.width = float(left_width)
                instance.control_column.width = float(usable - left_width)

            row.bind(width=sync_columns)
            sync_columns(row)

        self.popup._align_center()
        self.root_surface.do_layout()
        self.settings.do_layout()

    def _metadata(self) -> dict[str, object]:
        all_widgets = list(walk(self.settings))
        rows = [item for item in all_widgets if isinstance(item, SettingRow)]
        scroll = next((item for item in all_widgets if isinstance(item, ScrollView)), None)
        selected: list[dict[str, object]] = [
            sample(self.popup, "popup"),
            sample(self.settings, "popup_content"),
            sample(self.settings.content_host, "content_host"),
        ]
        if scroll is not None:
            selected.append(sample(scroll, "scroll_view"))
            if scroll.children:
                selected.append(sample(scroll.children[0], "scroll_content_outer"))
        for index, row in enumerate(rows[:5]):
            selected.append(sample(row, f"setting_row_{index}"))
            text_column = next((child for child in row.children if child is not row.control_column), None)
            if text_column is not None:
                selected.append(sample(text_column, f"setting_row_{index}_text_column"))
            selected.append(sample(row.control_column, f"setting_row_{index}_control_column"))
        for role, widget_type, limit in (
            ("label", Label, 12),
            ("input", TextInput, 5),
            ("spinner", Spinner, 5),
            ("button", ButtonBehavior, 5),
        ):
            count = 0
            for widget in all_widgets:
                if isinstance(widget, widget_type):
                    selected.append(sample(widget, f"{role}_{count}"))
                    count += 1
                    if count >= limit:
                        break
        fractional_count = sum(1 for item in selected if item["fractional"])
        return {
            "window": {
                "size": list(Window.size),
                "system_size": list(getattr(Window, "system_size", Window.size)),
                "dpi": float(getattr(Window, "dpi", 0) or 0),
            },
            "metrics": {
                "density": float(Metrics.density),
                "dpi": float(Metrics.dpi),
                "fontscale": float(Metrics.fontscale),
            },
            "constraints": {
                "font_changed": False,
                "font_size_changed": False,
                "colors_changed": False,
                "text_changed": False,
                "scrollview_removed": False,
                "control_height_changed": False,
            },
            "sample_count": len(selected),
            "fractional_count": fractional_count,
            "fractional_ratio": fractional_count / len(selected) if selected else 0.0,
            "samples": selected,
        }

    def _write_capture(self, image_name: str, metadata_name: str) -> None:
        target = self.output_dir / f"{image_name}.png"
        if target.exists():
            target.unlink()
        captured = Path(Window.screenshot(name=str(target)))
        captured.replace(target)
        (self.output_dir / f"{metadata_name}.json").write_text(
            json.dumps(self._metadata(), ensure_ascii=False, indent=2),
            encoding="utf-8",
        )

    def _write_comparison(self) -> None:
        before = json.loads((self.output_dir / "ab_geometry_before.json").read_text(encoding="utf-8"))
        after = json.loads((self.output_dir / "ab_geometry_after.json").read_text(encoding="utf-8"))
        report = "\n".join(
            (
                "# Pixel-Snap A/B",
                "",
                "Both images use the same widget instance, font, sizes, colors, copy, controls, and ScrollView.",
                "Only popup extent/position, centered content geometry, and SettingRow column boundaries differ.",
                "",
                f"- Fractional sample ratio before: {before['fractional_ratio']:.6f}",
                f"- Fractional sample ratio after: {after['fractional_ratio']:.6f}",
                f"- Fractional samples before: {before['fractional_count']}/{before['sample_count']}",
                f"- Fractional samples after: {after['fractional_count']}/{after['sample_count']}",
                "- Visual decision: pending original-PNG inspection",
                "",
            )
        )
        (self.output_dir / "ab_comparison.md").write_text(report, encoding="utf-8")


if __name__ == "__main__":
    SharpnessABApp().run()
