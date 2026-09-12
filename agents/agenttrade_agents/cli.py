from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from .brain import ClaudeBrain
from .config import Config, config_dir
from .journal import Journal
from .onboard import onboard
from .runner import Prompts, run, tick
from .state import Features, FixedStateSource, Instrument, Position, State

PROMPTS_DIR = Path(__file__).resolve().parents[2] / "docs" / "prompts"


def demo_state() -> State:
    """A plausible BTC/USD snapshot for running the swarm before the core is wired."""
    inst = Instrument("kraken", "BTC/USD", price_scale=1, qty_scale=8, tick=1, lot=1)
    return State(
        sequence_id=1,
        timestamp_ns=0,
        instrument=inst,
        bid=612400,
        ask=612410,
        position=Position(0, 0, 0),
        features=Features(rsi=54.2, ema_fast=61238.5, ema_slow=61190.2, order_flow_imbalance=31),
        available_equity=100000,
    )


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="agenttrade-agents")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("onboard", help="connect a Claude account and write config")
    t = sub.add_parser("tick", help="run one swarm tick against a demo state and print intents")
    t.add_argument("--config", type=Path, default=config_dir() / "config.toml")
    r = sub.add_parser("run", help="run the swarm on the slow clock against a demo state")
    r.add_argument("--config", type=Path, default=config_dir() / "config.toml")
    r.add_argument("--ticks", type=int, default=None)
    args = p.parse_args(argv)

    if args.cmd == "onboard":
        return onboard()

    cfg = Config.load(args.config) if args.config.exists() else Config()
    brain = ClaudeBrain(cfg.model, cfg.claude_bin)
    prompts = Prompts.load(PROMPTS_DIR)
    journal = Journal(cfg.data / "journal" / "agents.jsonl")
    source = FixedStateSource(demo_state())
    if args.cmd == "tick":
        intents = tick(brain, prompts, source, cfg.limits, journal)
        print(json.dumps(intents, indent=1))
        return 0
    run(brain, prompts, source, cfg.limits, journal, cfg.clock_seconds, args.ticks)
    return 0


if __name__ == "__main__":
    sys.exit(main())
