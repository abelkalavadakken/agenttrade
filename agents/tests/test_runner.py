import json

from agenttrade_agents.brain import RateLimited
from agenttrade_agents.journal import Journal
from agenttrade_agents.runner import Prompts, run, tick
from agenttrade_agents.state import FixedStateSource
from conftest import FakeBrain

PROMPTS = Prompts("obs", "ana", "tra", "rev")
PLACE = {
    "intent_type": "PLACE", "side": "BUY", "time_in_force": "GTC",
    "target_price": "61240.0", "stop_loss": "61100.0", "quantity": "0.01", "reason": "r",
}
NOOP = {
    "intent_type": "NOOP", "side": "SIDE_UNSPECIFIED", "time_in_force": "TIF_UNSPECIFIED",
    "target_price": "0", "stop_loss": "0", "quantity": "0", "reason": "wait",
}


def test_tick_runs_all_four_and_keeps_only_passed(state, limits, tmp_path):
    brain = FakeBrain({
        "observer": {"narrative": "calm"},
        "analyst": {"read": "up", "regime": "trend_up", "bias": "long", "confidence": 0.7},
        "trader": {"intents": [PLACE, NOOP]},
        "reviewer": {"verdicts": [{"index": 0, "pass": True, "reason": "ok"}, {"index": 1, "pass": False, "reason": "no"}]},
    })
    journal = Journal(tmp_path / "j.jsonl")
    out = tick(brain, PROMPTS, FixedStateSource(state), limits, journal)
    assert out == [PLACE]
    assert [t.agent_id for t in brain.turns] == ["observer", "analyst", "trader", "reviewer"]
    assert [t.effort for t in brain.turns] == ["low", "medium", "medium", "high"]
    kinds = [json.loads(l)["kind"] for l in (tmp_path / "j.jsonl").read_text().splitlines()]
    assert kinds == ["state", "observer", "analyst", "trader", "reviewer"]


def test_reviewer_skipped_when_trader_proposes_nothing(state, limits, tmp_path):
    brain = FakeBrain({
        "observer": {"narrative": "calm"},
        "analyst": {"read": "?", "regime": "unclear", "bias": "flat", "confidence": 0.2},
        "trader": {"intents": []},
    })
    assert tick(brain, PROMPTS, FixedStateSource(state), limits, Journal(tmp_path / "j")) == []
    assert "reviewer" not in [t.agent_id for t in brain.turns]


def test_trader_prompt_carries_read_state_and_limits(state, limits, tmp_path):
    brain = FakeBrain({
        "observer": {"narrative": "calm"},
        "analyst": {"read": "up", "regime": "range", "bias": "flat", "confidence": 0.5},
        "trader": {"intents": []},
    })
    tick(brain, PROMPTS, FixedStateSource(state), limits, Journal(tmp_path / "j"))
    prompt = brain.turns[2].user_prompt
    assert "max_order_qty=0.01" in prompt and "bid=61240.0" in prompt and '"bias": "flat"' in prompt


def test_run_sleeps_on_rate_limit_instead_of_retrying_hot(state, limits, tmp_path, monkeypatch):
    slept = []
    monkeypatch.setattr("agenttrade_agents.runner.time.sleep", lambda s: slept.append(s))
    monkeypatch.setattr("agenttrade_agents.runner.time.time", lambda: 1000)
    calls = {"n": 0}

    class Flaky(FakeBrain):
        def think(self, turn):
            calls["n"] += 1
            if calls["n"] == 1:
                raise RateLimited(resets_at=1300, kind="five_hour")
            return super().think(turn)

    brain = Flaky({
        "observer": {"narrative": "c"},
        "analyst": {"read": "?", "regime": "unclear", "bias": "flat", "confidence": 0.1},
        "trader": {"intents": []},
    })
    journal = Journal(tmp_path / "j.jsonl")
    run(brain, PROMPTS, FixedStateSource(state), limits, journal, clock_seconds=60, ticks=1)
    assert slept[0] == 300
    assert any(json.loads(l)["kind"] == "rate_limited" for l in (tmp_path / "j.jsonl").read_text().splitlines())
