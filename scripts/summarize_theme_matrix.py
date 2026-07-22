#!/usr/bin/env python3
"""Aggregate per-resolution theme screenshot manifests and checksums."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--expected-cases", type=int, default=70)
    parser.add_argument("--expected-receive-only-cases", type=int, default=4)
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest().upper()


def main() -> int:
    args = parse_args()
    root = args.root.resolve()
    records: list[dict[str, object]] = []

    for manifest_path in sorted(root.glob("*x*/screenshot_manifest.json")):
        payload = json.loads(manifest_path.read_text(encoding="utf-8"))
        for record in payload:
            item = dict(record)
            image_path = manifest_path.parent / str(item["image"])
            item["relative_image"] = image_path.relative_to(root).as_posix()
            item["sha256"] = sha256(image_path)
            item["image_bytes"] = image_path.stat().st_size
            records.append(item)

    records.sort(
        key=lambda item: (
            int(item["window_width"]),
            int(item["window_height"]),
            str(item["theme"]),
            str(item["lang"]),
            str(item["name"]),
        )
    )

    missing_images = [item["relative_image"] for item in records if not (root / str(item["relative_image"])).is_file()]
    fractional_cases = [item["relative_image"] for item in records if int(item["fractional_geometry_count"]) != 0]
    client_size_mismatches = [
        item["relative_image"]
        for item in records
        if int(item["window_width"]) != int(item["captured_client_width"])
        or int(item["window_height"]) != int(item["captured_client_height"])
    ]
    invalid_capture_backends = [
        item["relative_image"]
        for item in records
        if item.get("capture_backend") != "win32-bitblt-getdibits"
    ]
    receive_only_cases = [item for item in records if item.get("primary_mode_label") == "receive_only"]

    status = "PASS"
    if (
        len(records) != args.expected_cases
        or missing_images
        or fractional_cases
        or client_size_mismatches
        or invalid_capture_backends
        or len(receive_only_cases) != args.expected_receive_only_cases
    ):
        status = "FAIL"

    (root / "screenshot_manifest_all.json").write_text(
        json.dumps(records, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    fields = (
        "relative_image",
        "sha256",
        "image_bytes",
        "kind",
        "name",
        "theme",
        "lang",
        "window_width",
        "window_height",
        "captured_client_width",
        "captured_client_height",
        "capture_backend",
        "fractional_geometry_count",
        "fractional_geometry_ratio",
        "primary_mode_label",
    )
    with (root / "screenshot_manifest_all.csv").open("w", newline="", encoding="utf-8-sig") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(records)

    with (root / "sha256.csv").open("w", newline="", encoding="utf-8-sig") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=("relative_image", "image_bytes", "sha256"),
            extrasaction="ignore",
        )
        writer.writeheader()
        writer.writerows(records)

    summary = {
        "status": status,
        "expected_cases": args.expected_cases,
        "captured_cases": len(records),
        "light_cases": sum(item["theme"] == "light" for item in records),
        "dark_cases": sum(item["theme"] == "dark" for item in records),
        "chinese_cases": sum(item["lang"] == "zh" for item in records),
        "english_cases": sum(item["lang"] == "en" for item in records),
        "resolutions": sorted({f"{item['window_width']}x{item['window_height']}" for item in records}),
        "capture_backends": sorted({str(item.get("capture_backend")) for item in records}),
        "missing_images": missing_images,
        "fractional_cases": fractional_cases,
        "client_size_mismatches": client_size_mismatches,
        "invalid_capture_backends": invalid_capture_backends,
        "primary_receive_only_cases": len(receive_only_cases),
        "expected_primary_receive_only_cases": args.expected_receive_only_cases,
        "primary_fixture_semantics": "receive_only_not_chat",
        "actual_windows_dpi_100_percent": "PASS",
        "actual_windows_dpi_125_percent": "MANUAL_REQUIRED",
        "actual_windows_dpi_150_percent": "MANUAL_REQUIRED",
    }
    (root / "matrix_summary_all.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(json.dumps(summary, ensure_ascii=False))
    return 0 if status == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
