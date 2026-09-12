"""One tick of the swarm: state -> observer -> analyst -> trader -> reviewer.
Every stage is journaled before the next one runs. Output is a list of
intents for the Rust risk gate. Submission over gRPC lands with the api
crate; until then `tick` returns them."""

from __future__ import annotations

import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import analyst, observer, reviewer, trader
from .brain import Brain, RateLimited, load_prompt
from .journal import Journal
from .state import StateSource


@dataclass
class Prompts:
    observer: str
    analyst: str
    trader: str
    reviewer: str

    @classmethod
    def load(cls, prompts_dir: Path) -> "Prompts":
        return cls(*(load_prompt(n, prompts_dir) for n in ("observer", "analyst", "trader", "reviewer")))


def tick(
    brain: Brain,
    prompts: Prompts,
    source: StateSource,
    limits: dict[str, str],
    journal: Journal,
) -> list[dict[str, Any]]:
    state = source.get_state()
    journal.write("state", seq=state.sequence_id, text=state.as_text())

    narrative = observer.observe(brain, prompts.observer, state)
    journal.write("observer", seq=state.sequence_id, narrative=narrative)

    read = analyst.analyse(brain, prompts.analyst, narrative)
    journal.write("analyst", seq=state.sequence_id, **read)

    proposed = trader.propose(brain, prompts.trader, read, state, limits)
    journal.write("trader", seq=state.sequence_id, intents=proposed)

    passed = reviewer.review(brain, prompts.reviewer, read, state, limits, proposed)
    journal.write("reviewer", seq=state.sequence_id, passed=passed, dropped=len(proposed) - len(passed))
    return passed


def run(
    brain: Brain,
    prompts: Prompts,
    source: StateSource,
    limits: dict[str, str],
    journal: Journal,
    clock_seconds: int,
    ticks: int | None = None,
) -> None:
    """Slow clock. On a rate limit, sleep until reset and journal it. Never retry hot."""
    n = 0
    while ticks is None or n < ticks:
        started = time.monotonic()
        try:
            tick(brain, prompts, source, limits, journal)
        except RateLimited as e:
            wait = max(60, (e.resets_at or 0) - int(time.time()))
            journal.write("rate_limited", kind_=e.kind, wait_s=wait)
            time.sleep(wait)
            continue
        n += 1
        elapsed = time.monotonic() - started
        time.sleep(max(0.0, clock_seconds - elapsed))
