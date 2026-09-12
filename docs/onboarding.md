# docs/onboarding.md

Design note. The install-and-connect experience, modelled on OpenClaw:
one install, connect your Claude account, connect a test trading
account, watch the agents work. Nothing here changes the core. It adds
a front door.

Status: the Claude connection (section 3), config (section 5), and the
agent swarm runner are built and run live on a subscription login.
See agents/ and crates/config. Everything else is still a proposal.

## 1. The user story

```
$ curl -fsSL https://agenttrade.dev/install | sh     # or: brew install agenttrade
$ agenttrade onboard
  Claude Code        ... found, signed in as you@example.com (Max)  [ok]
  Venue              ... kraken, paper venue (no keys needed)       [ok]
  Symbol             ... BTC/USD
  Model              ... opus
  Wrote ~/.config/agenttrade/config.toml
$ agenttrade start
  feed: kraken connected, book checksum ok
  tape: recording to ~/.local/share/agenttrade/tapes/2026-09-12T10-00.tape
  api: grpc listening on 127.0.0.1:50051
  agents: observer, analyst, trader, reviewer on 60s clock
$ agenttrade watch
  10:01:00  observer   "BTC/USD 61,240. Spread 1 tick. OFI +0.31 over 5m ..."
  10:01:04  analyst    "Short-term buy pressure, no regime change ..."
  10:01:07  trader     intent PlaceOrder buy 0.01 @ 61,235  (limit, post-only)
  10:01:08  reviewer   pass
  10:01:08  risk       ACCEPT  -> paper venue, order open
  10:01:31  exec       fill 0.01 @ 61,235
```

Three commands. Nothing else to learn on day one.

## 2. What OpenClaw does that we copy

| OpenClaw                     | AgentTrade equivalent                          |
|------------------------------|------------------------------------------------|
| `openclaw onboard` wizard    | `agenttrade onboard`                           |
| Gateway daemon               | `agenttrade start` (core + agent runner)       |
| Sign in with Claude account  | Unmodified Claude Code binary, see section 3   |
| Channels (Telegram, etc.)    | `agenttrade watch` terminal stream, section 6  |
| Config in `~/.openclaw`      | `~/.config/agenttrade/config.toml`             |
| Local-first, no cloud relay  | Same. No AgentTrade server. Ever.              |

What we do not copy: the plugin marketplace, multi-channel messaging,
and the web control UI. Those are not day-one. See section 9.

## 3. Claude connection

Requirement: the user signs in with their Claude subscription (Pro,
Max, Team, Enterprise). No API key, no per-token bill.

Anthropic's rules here moved twice in 2026. In April, subscription
credentials were blocked for third-party agents such as OpenClaw. In
May, Anthropic reinstated third-party agent use on paid subscriptions
through a separate pool of "Agent SDK" credits, priced close to API
rates and distinct from the conversational quota. The standing terms
still forbid a third-party app from running its own claude.ai login or
holding Claude session tokens. Sign-in has to happen through
Anthropic's own flow.

So the compliant shape, and the one built, is: the agent brain is the
Claude Agent SDK, which itself runs the unmodified Claude Code binary
and inherits whatever sign-in that binary holds. The user signs in
with their subscription inside Claude Code. We never touch a token.

### The agent brain is the Claude Agent SDK over unmodified Claude Code

Each agent turn is one `query()` through `claude-agent-sdk` with its
own versioned system prompt from docs/prompts/, no tools, a JSON
schema for the output, and its own effort level. The four agents are
four separate calls we orchestrate in Python. That is our swarm on our
terms, not Claude Code subagents. Code: `agents/agenttrade_agents/brain.py`.

What AgentTrade does and does not do:

- Never sees a token. `onboard` runs one probe turn; if Claude Code
  reports it is not signed in, `onboard` hands the terminal to
  `claude auth login` and probes again. Nothing under `~/.claude/` is
  read or copied.
- Never bundles or patches the binary. `onboard` checks that `claude`
  is on PATH and prints Anthropic's install line if not.
- Never disables an auth method. A user with an API key sets
  `ANTHROPIC_API_KEY` and Claude Code bills that instead. Same code.
- Rate limits arrive as SDK events. The runner sleeps until the reset
  time and journals it. It never retries hot.

Measured on this machine, 2026-09-12, Claude Code 2.1.212, model
alias `opus`: probe turn 3.4 s, full four-agent tick 29 s.

### Costs of this design, stated plainly

- Claude Code is a hard install-time dependency. Node is not in our
  layout; it arrives as a dependency of Claude Code, not of us. The
  user installs it from Anthropic, we do not vendor it.
- Each turn pays Claude Code startup plus a full model turn, several
  seconds each. Four turns fit a 60s clock. A fast clock does not.
- Subscription usage limits are sized for ordinary individual use. A
  four-agent loop running around the clock will hit Pro limits and
  possibly Max limits. `onboard` says this. `start` backs off on a
  rate-limit reply and journals it rather than retrying hot. Users who
  need more switch to an API key; that is their decision.
- Anthropic states it may enforce these restrictions without notice.
  If the headless path stops honoring subscription auth, the API key
  path keeps working unchanged. We do not build anything that depends
  on the subscription path staying open.
- Branding: we may say AgentTrade runs Claude Code. We may not use the
  Claude or Claude Code names in the product name or logo.

### Model and effort

Model is a Claude Code alias in config, `opus` by default. Effort is
per agent in code: observer low, analyst and trader medium, reviewer
high. Model IDs are never hardcoded in agent code.

## 4. Trading connection

The core already promises: paper venue by default, testnet by flag, no
live keys ever. `onboard` enforces that:

- `venue.mode = "paper"` is the default and needs no keys. Live Kraken
  market data feeds the paper venue. This is the first-run path and
  most users never leave it.
- `venue.mode = "testnet"` is offered only for venues that have one.
  Kraken spot has none; Kraken Futures has a demo environment. Binance
  and Bybit have spot testnets. Each is a feed module and a venue
  adapter, one PR each, after the paper path works end to end.
- There is no `venue.mode = "live"`. The config parser rejects it.
  This is the enforcement of "no live keys, ever". A user who wants
  live trading has to fork.

Testnet keys are written to `~/.config/agenttrade/credentials.toml`
with mode 0600. They never appear in logs, the tape, or the journal.

## 5. Config

`~/.config/agenttrade/config.toml`:

```toml
[venue]
name   = "kraken"
mode   = "paper"          # paper | testnet
symbol = "BTC/USD"

[data]
dir = "~/.local/share/agenttrade"   # tapes/, journal/, state/

[api]
listen = "127.0.0.1:50051"

[agents]
clock_seconds = 60
model         = "opus"      # Claude Code model alias
claude_bin    = "claude"    # unmodified Claude Code on PATH

[risk]
max_position_qty = "0.05"    # string, parsed to fixed-point
max_order_qty    = "0.01"
max_daily_loss   = "50"
```

Risk limits are strings parsed into the same int64 fixed-point the
core uses. No floats in config for anything that reaches state.

The Rust side owns the schema. A `crates/config` crate parses it and
the agent runner receives the resolved values over gRPC via a new
`GetConfig` RPC, so Python never parses TOML. That RPC is a proto
change, so it is its own PR.

## 6. Seeing it work

Day one is the terminal. `agenttrade watch` subscribes to
`StreamEvents` and the journal and prints one line per agent turn,
intent, risk verdict, and fill, as in section 1. Every line is
already on the tape or in the journal, so `watch` is a pure view.

Reasons to start here: it needs no new layer, it works over SSH, and it
forces every interesting event to exist on the event stream first,
which a later dashboard would need anyway.

A web dashboard is a real product question and is deliberately not
decided here. See section 9.

## 7. Packaging

One entrypoint: a new Rust binary `bin/agenttrade` with subcommands
`onboard`, `start`, `watch`, `record`, `replay`. The existing `record`
and `replay` bins become subcommands. `start` spawns the Python agent
runner as a child process via `uv run` and supervises it. The runner
spawns `claude -p` per turn. If Python, uv, or Claude Code is missing,
`onboard` says so and prints the install line. Claude Code is never
vendored, per section 3.

Distribution options, one to be chosen (section 9):

- `cargo install agenttrade` plus `uv tool install agenttrade-agents`.
  Zero packaging work, but two toolchains for the user.
- Homebrew tap that installs the binary and vendors the agent package.
- A curl installer that downloads a release tarball with the binary
  and the agent wheel, and installs `uv` if absent.

Whatever the choice, the agent code ships with the binary at a pinned
version. The core and the agents are one release.

## 8. Build order

Each line is one PR. Nothing in phases 2 to 4 starts until phase 1 is
green, because there is nothing to onboard into until the loop works.

Phase 1, the loop (already planned, not onboarding work):
  book, features, risk, exec paper venue, api, agents. End state: a
  tape in, agent intents out, risk verdicts, paper fills, replay
  deterministic.

Phase 2, config and entrypoint:
  1. `crates/config`: TOML schema, fixed-point parsing, mode enum
     without `live`. Tests reject `live`.
  2. `bin/agenttrade` with `start`, `record`, `replay`. Old bins
     removed.
  3. proto: `GetConfig` RPC. Own PR.
  4. Agent runner reads config over gRPC and takes state from GetState
     instead of the built-in demo state. Runner, prompts, and journal
     exist today; the gRPC client does not.

Phase 3, connect:
  5. Done as `agenttrade-agents onboard`: Claude Code detection, login
     handoff, probe turn, usage-limit warning, config write. Moves
     under `agenttrade onboard` when the Rust entrypoint lands.
  6. Done: rate-limit backoff and journaling in the runner.
  7. Testnet mode for the first venue that has one. Feed module and
     adapter. Own PRs.

Phase 4, watch and ship:
  8. `agenttrade watch`.
  9. Installer for the chosen distribution.
  10. README quickstart: the three commands from section 1, with a
      real transcript.

## 9. Open decisions

Each of these changes the work materially. Settle before phase 2.

1. Viewing surface. Terminal only for v1, or terminal plus a local
   web dashboard. A dashboard adds a layer and a language to the
   repo, which the rules say to ask about.
2. Distribution. cargo+uv, Homebrew tap, or curl installer.
3. Second venue. Which testnet first: Binance spot, Bybit spot, or
   Kraken Futures demo. Determines the second feed module.
4. Agent runner as a child process of `agenttrade start`, or a
   separate `agenttrade agents` command the user runs in another
   terminal. Child process is proposed.

## 10. Dependencies this adds

Per CLAUDE.md, listed before use.

- Claude Code CLI: install-time, user-installed from Anthropic, never
  vendored. The agent brain. Brings Node with it; we do not depend on
  Node directly.
- `toml` (Rust): config parsing in `crates/config`.
- `claude-agent-sdk` (Python): the brain. Listed in CLAUDE.md.
- No `anthropic` Python SDK. Adding it later would only be for an
  API-key-only fast path and needs its own design note.
