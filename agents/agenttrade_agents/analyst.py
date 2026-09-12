from __future__ import annotations

from typing import Any

from .brain import Brain, Turn
from .schemas import ANALYST


def analyse(brain: Brain, system_prompt: str, narrative: str) -> dict[str, Any]:
    turn = Turn("analyst", system_prompt, narrative, ANALYST, "medium")
    return brain.think(turn).output
