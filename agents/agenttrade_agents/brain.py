"""The model behind every agent.

ClaudeBrain runs on the Claude Agent SDK, which uses the user's own
Claude Code sign-in. A subscription works without an API key. We never
see a credential. See docs/onboarding.md section 3.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from claude_agent_sdk import (
    ClaudeAgentOptions,
    RateLimitEvent,
    ResultMessage,
    query,
)


class BrainError(Exception):
    pass


class RateLimited(BrainError):
    def __init__(self, resets_at: int | None, kind: str | None):
        super().__init__(f"rate limited ({kind}), resets at {resets_at}")
        self.resets_at = resets_at
        self.kind = kind


class NotSignedIn(BrainError):
    pass


@dataclass(frozen=True)
class Turn:
    """One agent call. Prompt text is versioned in docs/prompts/."""

    agent_id: str
    system_prompt: str
    user_prompt: str
    schema: dict[str, Any]
    effort: str


@dataclass(frozen=True)
class Reply:
    output: dict[str, Any]
    cost_usd: float | None
    duration_ms: int
    session_id: str


class Brain(Protocol):
    def think(self, turn: Turn) -> Reply: ...


class ClaudeBrain:
    def __init__(self, model: str, claude_bin: str | None = None):
        self.model = model
        self.claude_bin = claude_bin

    def think(self, turn: Turn) -> Reply:
        return asyncio.run(self._think(turn))

    async def _think(self, turn: Turn) -> Reply:
        options = ClaudeAgentOptions(
            model=self.model,
            system_prompt=turn.system_prompt,
            tools=[],
            allowed_tools=[],
            max_turns=1,
            effort=turn.effort,  # type: ignore[arg-type]
            output_format={"type": "json_schema", "schema": turn.schema},
            permission_mode="dontAsk",
            setting_sources=[],
            cli_path=self.claude_bin,
        )
        result: ResultMessage | None = None
        async for msg in query(prompt=turn.user_prompt, options=options):
            if isinstance(msg, RateLimitEvent):
                info = msg.rate_limit_info
                if info.status == "rejected":
                    raise RateLimited(info.resets_at, info.rate_limit_type)
            elif isinstance(msg, ResultMessage):
                result = msg
        if result is None:
            raise BrainError("no result message")
        if result.is_error:
            raise _classify(result)
        if not isinstance(result.structured_output, dict):
            raise BrainError(f"no structured output: {result.result!r}")
        return Reply(
            output=result.structured_output,
            cost_usd=result.total_cost_usd,
            duration_ms=result.duration_ms,
            session_id=result.session_id,
        )


def _classify(result: ResultMessage) -> BrainError:
    text = " ".join(result.errors or []) + " " + (result.result or "")
    if result.api_error_status in (401, 403) or "log in" in text.lower():
        return NotSignedIn(text.strip())
    if result.api_error_status == 429:
        return RateLimited(None, None)
    return BrainError(text.strip() or result.subtype)


def load_prompt(name: str, prompts_dir: Path) -> str:
    """Prompts are files in docs/prompts named <agent>.v<N>.md. Highest N wins."""
    candidates = sorted(prompts_dir.glob(f"{name}.v*.md"), key=_version)
    if not candidates:
        raise FileNotFoundError(f"no prompt {name}.v*.md in {prompts_dir}")
    return candidates[-1].read_text()


def _version(p: Path) -> int:
    return int(p.name.split(".v")[1].split(".")[0])
