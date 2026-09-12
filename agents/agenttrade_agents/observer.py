from __future__ import annotations

from .brain import Brain, Turn
from .schemas import OBSERVER
from .state import State


def observe(brain: Brain, system_prompt: str, state: State) -> str:
    turn = Turn("observer", system_prompt, state.as_text(), OBSERVER, "low")
    return brain.think(turn).output["narrative"]
