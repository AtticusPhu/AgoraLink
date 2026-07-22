from __future__ import annotations

import sys
import unittest
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from ui_geometry import (
    centered_content_geometry,
    popup_geometry,
    setting_row_column_widths,
    snap_centered_extent,
    snap_px,
    split_integer_width,
)


class UIGeometryTests(unittest.TestCase):
    def assert_integer_geometry(self, values) -> None:
        self.assertTrue(all(float(value).is_integer() for value in values), values)

    def test_pixel_snap_rounds_physical_coordinates(self):
        self.assertEqual(snap_px(35.4), 35.0)
        self.assertEqual(snap_px(35.6), 36.0)

    def test_centered_extent_matches_window_parity(self):
        for total in (1180, 1181, 1600, 1601):
            extent = int(snap_centered_extent(total, 1132, 680))
            self.assertEqual((total - extent) % 2, 0)

    def test_centered_extent_never_exceeds_window(self):
        self.assertLessEqual(snap_centered_extent(640, 1180, 680), 640)

    def test_split_widths_are_integer(self):
        self.assert_integer_geometry(split_integer_width(817.2, 18, 0.47))

    def test_split_widths_sum_exactly(self):
        left, right, gap = split_integer_width(817.2, 18, 0.47)
        self.assertEqual(left + right + gap, 817)

    def test_popup_geometry_1180x760_is_integer(self):
        geometry = popup_geometry(
            1180, 760, desired_width=1180, desired_height=820,
            horizontal_margin=24, vertical_margin=24, minimum_width=680, minimum_height=520,
        )
        self.assert_integer_geometry(geometry)
        self.assertEqual(geometry, (24.0, 24.0, 1132.0, 712.0))

    def test_popup_geometry_1181x760_is_integer(self):
        geometry = popup_geometry(
            1181, 760, desired_width=1180, desired_height=820,
            horizontal_margin=24, vertical_margin=24, minimum_width=680, minimum_height=520,
        )
        self.assert_integer_geometry(geometry)
        self.assertEqual((1181 - geometry[2]) % 2, 0)

    def test_popup_geometry_1280x720_is_integer(self):
        geometry = popup_geometry(
            1280, 720, desired_width=1180, desired_height=820,
            horizontal_margin=24, vertical_margin=24, minimum_width=680, minimum_height=520,
        )
        self.assert_integer_geometry(geometry)
        self.assertLessEqual(geometry[2], 1232)
        self.assertLessEqual(geometry[3], 672)

    def test_content_geometry_1599_is_integer(self):
        self.assert_integer_geometry(centered_content_geometry(252, 1299, 900))

    def test_content_geometry_1600_is_integer(self):
        self.assert_integer_geometry(centered_content_geometry(252, 1300, 900))

    def test_content_geometry_1601_is_integer(self):
        self.assert_integer_geometry(centered_content_geometry(252, 1301, 900))

    def test_setting_row_columns_1280_are_integer(self):
        values = setting_row_column_widths(817.2, gap=18, minimum_left=260, maximum_left=380)
        self.assert_integer_geometry(values)
        self.assertEqual(sum(values), 817)

    def test_setting_row_columns_1600_are_integer(self):
        values = setting_row_column_widths(900, gap=18, minimum_left=260, maximum_left=380)
        self.assert_integer_geometry(values)
        self.assertEqual(sum(values), 900)

    def test_setting_row_columns_density_125_are_integer(self):
        values = setting_row_column_widths(900 * 1.25, gap=18 * 1.25, minimum_left=260 * 1.25, maximum_left=380 * 1.25)
        self.assert_integer_geometry(values)
        self.assertEqual(sum(values), round(900 * 1.25))

    def test_setting_row_columns_density_150_are_integer(self):
        values = setting_row_column_widths(900 * 1.5, gap=18 * 1.5, minimum_left=260 * 1.5, maximum_left=380 * 1.5)
        self.assert_integer_geometry(values)
        self.assertEqual(sum(values), round(900 * 1.5))

    def test_scrolling_does_not_change_x_geometry(self):
        before = centered_content_geometry(252, 1301, 900)
        after = centered_content_geometry(252, 1301, 900)
        self.assertEqual(before, after)


if __name__ == "__main__":
    unittest.main()
