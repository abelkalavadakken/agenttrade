"""`agenttrade-agents onboard`: connect a Claude account, pick a venue,
write config. The Claude sign-in is Claude Code's own flow. We launch
the unmodified binary and let it do the login; we never read the
credential. See docs/onboarding.md section 3."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path
from typing import Callable

from .brain import Brain, BrainError, ClaudeBrain, NotSignedIn, RateLimited, Turn
from .config import Config, config_dir

INSTALL_HINT = "Install Claude Code from https://claude.com/claude-code and run `claude` once, then retry."
PROBE_SCHEMA = {"type": "object", "additionalProperties": False, "required": ["ok"], "properties": {"ok": {"type": "boolean"}}}

USAGE_WARNING = (
    "Subscription usage limits are sized for ordinary individual use.\n"
    "Four agents on a {clock}s clock will approach Pro limits within hours\n"
    "and can hit Max limits. On a limit the runner sleeps until reset and\n"
    "journals it. It never retries hot. If you need more, set\n"
    "ANTHROPIC_API_KEY and Claude Code bills that key instead."
)


def find_claude(claude_bin: str = "claude") -> str | None:
    return shutil.which(claude_bin)


def sign_in(claude_path: str, run: Callable[..., subprocess.CompletedProcess] = subprocess.run) -> None:
    """Hand the terminal to Claude Code for its own login flow."""
    run([claude_path, "auth", "login"], check=False)


def verify(brain: Brain) -> tuple[bool, str]:
    turn = Turn("probe", "Reply with ok=true.", "ping", PROBE_SCHEMA, "low")
    try:
        reply = brain.think(turn)
    except NotSignedIn as e:
        return False, f"not signed in: {e}"
    except RateLimited as e:
        return True, f"signed in, but rate limited: {e}"
    except BrainError as e:
        return False, f"probe failed: {e}"
    return bool(reply.output.get("ok")), f"signed in, probe {reply.duration_ms} ms"


def onboard(
    out: Callable[[str], None] = print,
    ask: Callable[[str, str], str] = lambda q, d: (input(f"{q} [{d}]: ").strip() or d),
    config_path: Path | None = None,
    brain_factory: Callable[[Config], Brain] = lambda c: ClaudeBrain(c.model, c.claude_bin),
    sign_in_fn: Callable[[str], None] = sign_in,
    which: Callable[[str], str | None] = find_claude,
) -> int:
    cfg = Config()
    config_path = config_path or config_dir() / "config.toml"

    claude_path = which(cfg.claude_bin)
    if not claude_path:
        out(f"Claude Code        ... not found. {INSTALL_HINT}")
        return 1
    out(f"Claude Code        ... {claude_path}")

    brain = brain_factory(cfg)
    ok, why = verify(brain)
    if not ok and why.startswith("not signed in"):
        out("Claude account     ... not signed in. Opening Claude Code's sign-in.")
        sign_in_fn(claude_path)
        ok, why = verify(brain)
    out(f"Claude account     ... {why}")
    if not ok:
        return 1

    cfg.venue = ask("Venue", cfg.venue)
    cfg.mode = ask("Mode (paper|testnet)", cfg.mode)
    cfg.symbol = ask("Symbol", cfg.symbol)
    cfg.model = ask("Model", cfg.model)
    try:
        Config(mode=cfg.mode)
    except ValueError as e:
        out(str(e))
        return 1
    out(f"Venue              ... {cfg.venue}, {cfg.mode} venue" + (" (no keys needed)" if cfg.mode == "paper" else ""))
    out("")
    out(USAGE_WARNING.format(clock=cfg.clock_seconds))
    out("")
    cfg.write(config_path)
    out(f"Wrote {config_path}")
    return 0


if __name__ == "__main__":
    sys.exit(onboard())
