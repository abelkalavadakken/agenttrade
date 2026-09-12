"""Append-only JSONL. One line per agent turn, written before anything
downstream uses the turn."""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any


class Journal:
    def __init__(self, path: Path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)

    def write(self, kind: str, **fields: Any) -> None:
        record = {"t_ns": time.time_ns(), "kind": kind, **fields}
        with self.path.open("a") as f:
            f.write(json.dumps(record, sort_keys=True) + "\n")
