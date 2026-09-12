from agenttrade_agents.brain import NotSignedIn
from agenttrade_agents.onboard import onboard
from conftest import FakeBrain


def _ask(_q, default):
    return default


def test_missing_claude_code_stops_with_install_hint(tmp_path):
    lines = []
    rc = onboard(out=lines.append, ask=_ask, config_path=tmp_path / "c.toml", which=lambda _: None)
    assert rc == 1
    assert "not found" in lines[0] and not (tmp_path / "c.toml").exists()


def test_signed_in_writes_config_and_warns_about_limits(tmp_path):
    lines = []
    rc = onboard(
        out=lines.append, ask=_ask, config_path=tmp_path / "c.toml",
        brain_factory=lambda c: FakeBrain({"probe": {"ok": True}}),
        which=lambda _: "/usr/local/bin/claude", sign_in_fn=lambda _: (_ for _ in ()).throw(AssertionError("no login needed")),
    )
    assert rc == 0
    text = "\n".join(lines)
    assert "signed in" in text and "usage limits" in text and (tmp_path / "c.toml").exists()
    assert 'mode   = "paper"' in (tmp_path / "c.toml").read_text()


def test_not_signed_in_hands_off_to_claude_code_login_then_verifies_again(tmp_path):
    class Flip(FakeBrain):
        def __init__(self):
            super().__init__({})
            self.n = 0

        def think(self, turn):
            self.n += 1
            if self.n == 1:
                raise NotSignedIn("please log in")
            return FakeBrain({"probe": {"ok": True}}).think(turn)

    logins = []
    lines = []
    rc = onboard(
        out=lines.append, ask=_ask, config_path=tmp_path / "c.toml",
        brain_factory=lambda c: Flip(), which=lambda _: "/x/claude", sign_in_fn=logins.append,
    )
    assert rc == 0 and logins == ["/x/claude"]
