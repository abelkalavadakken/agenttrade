//! Mean reversion on spread and OFI. Horizon seconds, autonomous.
//! docs/strategy.md section 5.

use types::{Price, Side, TimeInForce};

use crate::{set_bounded, Action, Horizon, Order, Param, ParamError, SetupState, State, Strategy};

const BTC: i64 = 100_000_000;
const S: i64 = 1_000_000_000;

pub struct MrOfi {
    params: [Param; 6],
    entered_ns: Option<i64>,
    entry_ofi_sign: i64,
    cooldown_until_ns: i64,
    /// A flatten is in flight; wait for the position to clear before acting again.
    exiting: bool,
}

impl Default for MrOfi {
    fn default() -> Self {
        Self::new()
    }
}

impl MrOfi {
    pub fn new() -> Self {
        Self {
            params: [
                Param {
                    name: "ofi_threshold",
                    value: 2 * BTC,
                    min: BTC / 10,
                    max: 50 * BTC,
                },
                Param {
                    name: "min_spread_ticks",
                    value: 2,
                    min: 1,
                    max: 100,
                },
                Param {
                    name: "size",
                    value: BTC / 100,
                    min: BTC / 10_000,
                    max: BTC,
                },
                Param {
                    name: "stop_ticks",
                    value: 50,
                    min: 5,
                    max: 5_000,
                },
                Param {
                    name: "hold_ns",
                    value: 30 * S,
                    min: S,
                    max: 600 * S,
                },
                Param {
                    name: "cooldown_ns",
                    value: 60 * S,
                    min: 0,
                    max: 3_600 * S,
                },
            ],
            entered_ns: None,
            entry_ofi_sign: 0,
            cooldown_until_ns: 0,
            exiting: false,
        }
    }

    fn p(&self, name: &str) -> i64 {
        self.param(name).expect("own param")
    }
}

impl Strategy for MrOfi {
    fn id(&self) -> &'static str {
        "mr_ofi"
    }

    fn horizon(&self) -> Horizon {
        Horizon::Seconds
    }

    fn params(&self) -> &[Param] {
        &self.params
    }

    fn set_param(&mut self, name: &str, value: i64) -> Result<(), ParamError> {
        set_bounded(&mut self.params, name, value)
    }

    fn on_event(&mut self, st: &State) -> Option<Action> {
        let f = st.features;
        let ofi = f.order_flow_imbalance;
        let in_position = !st.position.net_qty.is_zero();

        if in_position {
            let entered = self.entered_ns.unwrap_or(st.now_ns);
            let flipped = ofi.signum() != 0 && ofi.signum() != self.entry_ofi_sign;
            let held_long_enough = st.now_ns - entered >= self.p("hold_ns");
            if flipped || held_long_enough {
                self.entered_ns = None;
                self.cooldown_until_ns = st.now_ns + self.p("cooldown_ns");
                return Some(Action::Flatten {
                    reason: if flipped {
                        "ofi flipped"
                    } else {
                        "hold expired"
                    },
                });
            }
            return None;
        }

        if st.open_orders > 0 || f.book_stale || st.book.is_stale() {
            return None;
        }
        if st.now_ns < self.cooldown_until_ns {
            return None;
        }
        let (bid, ask) = (st.book.best_bid()?, st.book.best_ask()?);
        if (ask.price.0 - bid.price.0) < self.p("min_spread_ticks") {
            return None;
        }
        let threshold = self.p("ofi_threshold");
        let (side, price, stop, reason) = if ofi <= -threshold {
            (
                Side::Buy,
                bid.price,
                Price(bid.price.0 - self.p("stop_ticks")),
                "fade sell flow at the bid",
            )
        } else if ofi >= threshold {
            (
                Side::Sell,
                ask.price,
                Price(ask.price.0 + self.p("stop_ticks")),
                "fade buy flow at the ask",
            )
        } else {
            return None;
        };
        self.entered_ns = Some(st.now_ns);
        self.entry_ofi_sign = ofi.signum();
        Some(Action::Place(Order {
            side,
            price,
            stop,
            qty: types::Qty(self.p("size")),
            tif: TimeInForce::Gtc,
            reason,
        }))
    }

    fn setup_state(&self) -> SetupState {
        SetupState::None
    }

    fn reset(&mut self) {
        self.entered_ns = None;
        self.entry_ofi_sign = 0;
        self.cooldown_until_ns = 0;
        self.exiting = false;
    }
}
