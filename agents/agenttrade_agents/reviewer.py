from __future__ import annotations

import json
from typing import Any

from .brain import Brain, Turn
from .schemas import REVIEWER
from .state import State


def review(
    brain: Brain,
    system_prompt: str,
    read: dict[str, Any],
    state: State,
    limits: dict[str, str],
    intents: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    """Returns the intents that passed, in order. Anything the reviewer
    did not explicitly pass is dropped."""
    if not intents:
        return []
    prompt = "\n\n".join(
        [
            "ANALYST READ\n" + json.dumps(read, indent=1),
            "CORE STATE\n" + state.as_text(),
            "RISK LIMITS\n" + "\n".join(f"{k}={v}" for k, v in limits.items()),
            "PROPOSED INTENTS\n" + json.dumps(list(enumerate(intents)), indent=1),
        ]
    )
    turn = Turn("reviewer", system_prompt, prompt, REVIEWER, "high")
    verdicts = brain.think(turn).output["verdicts"]
    passed = {v["index"] for v in verdicts if v["pass"]}
    return [it for i, it in enumerate(intents) if i in passed]
