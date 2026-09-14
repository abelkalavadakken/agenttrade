//! EMA and Wilder RSI over bar closes. Only + - * / so replay is bit-identical.

#[derive(Debug, Clone)]
pub struct Ema {
    alpha: f64,
    value: Option<f64>,
    seed_sum: f64,
    seed_n: u32,
    period: u32,
}

impl Ema {
    pub fn new(period: u32) -> Self {
        Self {
            alpha: 2.0 / (period as f64 + 1.0),
            value: None,
            seed_sum: 0.0,
            seed_n: 0,
            period,
        }
    }

    /// Seeds with the simple average of the first `period` closes, the
    /// textbook convention, then smooths.
    pub fn update(&mut self, close: f64) {
        match self.value {
            Some(v) => self.value = Some((close - v) * self.alpha + v),
            None => {
                self.seed_sum += close;
                self.seed_n += 1;
                if self.seed_n == self.period {
                    self.value = Some(self.seed_sum / self.period as f64);
                }
            }
        }
    }

    pub fn value(&self) -> Option<f64> {
        self.value
    }
}

#[derive(Debug, Clone)]
pub struct Rsi {
    period: u32,
    prev_close: Option<f64>,
    gain_sum: f64,
    loss_sum: f64,
    n: u32,
    avg_gain: Option<f64>,
    avg_loss: Option<f64>,
}

impl Rsi {
    pub fn new(period: u32) -> Self {
        Self {
            period,
            prev_close: None,
            gain_sum: 0.0,
            loss_sum: 0.0,
            n: 0,
            avg_gain: None,
            avg_loss: None,
        }
    }

    pub fn update(&mut self, close: f64) {
        let Some(prev) = self.prev_close.replace(close) else {
            return;
        };
        let change = close - prev;
        let (gain, loss) = if change > 0.0 {
            (change, 0.0)
        } else {
            (0.0, -change)
        };
        let p = self.period as f64;
        match (self.avg_gain, self.avg_loss) {
            (Some(g), Some(l)) => {
                self.avg_gain = Some((g * (p - 1.0) + gain) / p);
                self.avg_loss = Some((l * (p - 1.0) + loss) / p);
            }
            _ => {
                self.gain_sum += gain;
                self.loss_sum += loss;
                self.n += 1;
                if self.n == self.period {
                    self.avg_gain = Some(self.gain_sum / p);
                    self.avg_loss = Some(self.loss_sum / p);
                }
            }
        }
    }

    pub fn value(&self) -> Option<f64> {
        let (g, l) = (self.avg_gain?, self.avg_loss?);
        if l == 0.0 {
            return Some(100.0);
        }
        let rs = g / l;
        Some(100.0 - 100.0 / (1.0 + rs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_seeds_with_sma_then_smooths() {
        let mut e = Ema::new(3);
        e.update(1.0);
        e.update(2.0);
        assert_eq!(e.value(), None);
        e.update(3.0);
        assert_eq!(e.value(), Some(2.0));
        e.update(4.0);
        // alpha 0.5: (4 - 2) * 0.5 + 2
        assert_eq!(e.value(), Some(3.0));
    }

    #[test]
    fn rsi_textbook_series() {
        // 14 periods over 20 closes. Reference computed independently in Python
        // with Wilder's smoothing: SMA seed, then (avg*(p-1)+x)/p.
        let closes = [
            44.34, 44.09, 44.15, 43.61, 44.33, 44.83, 45.10, 45.42, 45.84, 46.08, 45.89, 46.03,
            45.61, 46.28, 46.28, 46.00, 46.03, 46.41, 46.22, 45.64,
        ];
        let mut r = Rsi::new(14);
        for c in closes {
            r.update(c);
        }
        let v = r.value().unwrap();
        assert!((v - 57.915_020_670_085_56).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn rsi_all_gains_is_100() {
        let mut r = Rsi::new(3);
        for c in [1.0, 2.0, 3.0, 4.0, 5.0] {
            r.update(c);
        }
        assert_eq!(r.value(), Some(100.0));
    }
}
