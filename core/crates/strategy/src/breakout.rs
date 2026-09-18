//! Breakout on an N-bar range. Horizon minutes, gated. docs/strategy.md section 6.

use types::{Price, Qty, Side, TimeInForce};

use crate::{set_bounded, Action, Horizon, Order, Param, ParamError, SetupState, State, Strategy};

const BTC: i64 = 100_000_000;
const S: i64 = 1_000_000_000;

pub struct Breakout {
    params: [Param; 5],
    seen_bars: usize,
    setup: SetupState,
}

impl Default for Breakout {
    fn default() -> Self {
        Self::new()
    }
}

impl Breakout {
    pub fn new() -> Self {
        Self {
            params: [
                Param {
                    name: "lookback_bars",
                    value: 20,
                    min: 5,
                    max: 200,
                },
                Param {
                    name: "size",
                    value: 2 * BTC / 100,
                    min: BTC / 10_000,
                    max: 2 * BTC,
                },
                Param {
                    name: "stop_ticks",
                    value: 200,
                    min: 10,
                    max: 20_000,
                },
                Param {
                    name: "volume_mult_bps",
                    value: 15_000,
                    min: 10_000,
                    max: 100_000,
                },
                Param {
                    name: "ttl_ns",
                    value: 120 * S,
                    min: 10 * S,
                    max: 900 * S,
                },
            ],
            seen_bars: 0,
            setup: SetupState::None,
        }
    }

    fn p(&self, name: &str) -> i64 {
        self.param(name).expect("own param")
    }
}

impl Strategy for Breakout {
    fn id(&self) -> &'static str {
        "breakout"
    }

    fn horizon(&self) -> Horizon {
        Horizon::Minutes
    }

    fn params(&self) -> &[Param] {
        &self.params
    }

    fn set_param(&mut self, name: &str, value: i64) -> Result<(), ParamError> {
        set_bounded(&mut self.params, name, value)
    }

    /// Evaluated only when a bar has closed since the last call.
    fn on_event(&mut self, st: &State) -> Option<Action> {
        let bars = st.features.bars;
        if bars.len() == self.seen_bars {
            return None;
        }
        self.seen_bars = bars.len();
        if matches!(self.setup, SetupState::Active { .. }) || !st.position.net_qty.is_zero() {
            return None;
        }
        let n = self.p("lookback_bars") as usize;
        if bars.len() < n + 1 {
            self.setup = SetupState::None;
            return None;
        }
        let last = bars[bars.len() - 1];
        let range = &bars[bars.len() - 1 - n..bars.len() - 1];
        if last.gap || range.iter().any(|b| b.gap) {
            self.setup = SetupState::None;
            return None;
        }
        let high = range.iter().map(|b| b.high.0).max().unwrap();
        let low = range.iter().map(|b| b.low.0).min().unwrap();
        let volume_ok = if st.features.volume_available {
            let avg = range.iter().map(|b| b.volume.0 as i128).sum::<i128>() / n as i128;
            last.volume.0 as i128 * 10_000 >= avg * self.p("volume_mult_bps") as i128
        } else {
            true
        };
        let (bid, ask) = match (st.book.best_bid(), st.book.best_ask()) {
            (Some(b), Some(a)) => (b, a),
            _ => return None,
        };
        let stop_ticks = self.p("stop_ticks");
        let proposed = if last.close.0 > high && volume_ok {
            Some((
                Side::Buy,
                ask.price,
                Price(low - stop_ticks),
                "close above N-bar high",
            ))
        } else if last.close.0 < low && volume_ok {
            Some((
                Side::Sell,
                bid.price,
                Price(high + stop_ticks),
                "close below N-bar low",
            ))
        } else {
            None
        };
        self.setup = match proposed {
            Some((side, price, stop, reason)) => SetupState::Active {
                setup_id: st.sequence_id,
                order: Order {
                    side,
                    price,
                    stop,
                    qty: Qty(self.p("size")),
                    tif: TimeInForce::Gtc,
                    reason: if st.features.volume_available {
                        reason
                    } else {
                        "close beyond N-bar range, volume unavailable"
                    },
                },
            },
            None => SetupState::Forming,
        };
        None
    }

    fn setup_state(&self) -> SetupState {
        self.setup
    }

    fn on_setup_cleared(&mut self) {
        self.setup = SetupState::None;
    }

    fn reset(&mut self) {
        self.seen_bars = 0;
        self.setup = SetupState::None;
    }
}
