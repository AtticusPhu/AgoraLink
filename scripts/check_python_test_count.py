#!/usr/bin/env python3
"""Fail validation when unittest discovery finds no AgoraLink tests."""

from __future__ import annotations

import ast
import json
import sys
from pathlib import Path


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    test_files = sorted((root / "tests").glob("test_*.py"))
    count = 0
    for test_file in test_files:
        tree = ast.parse(test_file.read_text(encoding="utf-8"), filename=str(test_file))
        count += sum(
            1
            for node in ast.walk(tree)
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name.startswith("test_")
        )
    print(
        json.dumps(
            {
                "type": "PYTHON_TEST_DISCOVERY",
                "count": count,
                "files": len(test_files),
                "method": "ast",
            }
        )
    )
    if count <= 0:
        print("Python unittest discovery found zero tests", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
