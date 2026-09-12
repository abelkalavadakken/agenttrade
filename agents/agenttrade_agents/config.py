"""~/.config/agenttrade/config.toml. Schema is owned by crates/config on
the Rust side; this reader must stay a strict subset of it. A `live`
venue mode is rejected here as it is there."""

from __future__ import annotations

import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

MODES = ("paper", "testnet")


def config_dir() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config"
    return Path(base) / "agenttrade"


def data_dir() -> Path:
    base = os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share"
    return Path(base) / "agenttrade"


@dataclass
class Config:
    venue: str = "kraken"
    mode: str = "paper"
    symbol: str = "BTC/USD"
    data: Path = field(default_factory=data_dir)
    listen: str = "127.0.0.1:50051"
    clock_seconds: int = 60
    model: str = "opus"
    claude_bin: str = "claude"
    limits: dict[str, str] = field(
        default_factory=lambda: {
            "max_position_qty": "0.05",
            "max_order_qty": "0.01",
            "max_daily_loss": "50",
        }
    )

    def __post_init__(self) -> None:
        if self.mode not in MODES:
            raise ValueError(f"venue.mode must be one of {MODES}, got {self.mode!r}. There is no live mode.")

    @classmethod
    def load(cls, path: Path) -> "Config":
        raw = tomllib.loads(path.read_text())
        v, d, a, api = raw.get("venue", {}), raw.get("data", {}), raw.get("agents", {}), raw.get("api", {})
        return cls(
            venue=v.get("name", "kraken"),
            mode=v.get("mode", "paper"),
            symbol=v.get("symbol", "BTC/USD"),
            data=Path(d["dir"]).expanduser() if "dir" in d else data_dir(),
            listen=api.get("listen", "127.0.0.1:50051"),
            clock_seconds=int(a.get("clock_seconds", 60)),
            model=a.get("model", "opus"),
            claude_bin=a.get("claude_bin", "claude"),
            limits={k: str(v) for k, v in raw.get("risk", {}).items()} or cls().limits,
        )

    def dump(self) -> str:
        risk = "\n".join(f'{k} = "{v}"' for k, v in self.limits.items())
        return (
            "[venue]\n"
            f'name   = "{self.venue}"\n'
            f'mode   = "{self.mode}"          # paper | testnet. No live mode exists.\n'
            f'symbol = "{self.symbol}"\n\n'
            "[data]\n"
            f'dir = "{self.data}"\n\n'
            "[api]\n"
            f'listen = "{self.listen}"\n\n'
            "[agents]\n"
            f"clock_seconds = {self.clock_seconds}\n"
            f'model         = "{self.model}"      # Claude Code model alias\n'
            f'claude_bin    = "{self.claude_bin}"    # unmodified Claude Code on PATH\n\n'
            "[risk]\n"
            f"{risk}\n"
        )

    def write(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.dump())
        os.chmod(path, 0o600)
