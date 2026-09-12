from __future__ import annotations

import json
from typing import Any

from .brain import Brain, Turn
from .schemas import TRADER
from .state import State


def propose(
    brain: Brain,
    system_prompt: str,
    read: dict[str, Any],
    state: State,
    limits: dict[str, str],
) -> list[dict[str, Any]]:
    prompt = "\n\n".join(
        [
            "ANALYST READ\n" + json.dumps(read, indent=1),
            "CORE STATE\n" + state.as_text(),
            "RISK LIMITS\n" + "\n".join(f"{k}={v}" for k, v in limits.items()),
        ]
    )
    turn = Turn("trader", system_prompt, prompt, TRADER, "medium")
    return brain.think(turn).output["intents"]
