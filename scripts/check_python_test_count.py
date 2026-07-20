#!/usr/bin/env python3
"""Fail validation when unittest discovery finds no AgoraLink tests."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    suite = unittest.defaultTestLoader.discover(
        start_dir=str(root / "tests"),
        pattern="test_*.py",
        top_level_dir=str(root),
    )
    count = suite.countTestCases()
    print(json.dumps({"type": "PYTHON_TEST_DISCOVERY", "count": count}))
    if count <= 0:
        print("Python unittest discovery found zero tests", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
