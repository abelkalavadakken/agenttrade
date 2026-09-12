//! ~/.config/agenttrade/config.toml. This crate owns the schema; the Python
//! reader in agents/ is a strict subset. Risk limits are decimal strings
//! parsed to fixed-point at the instrument's scale. No floats.
//!
//! There is no live venue mode. The parser rejects it. See docs/onboarding.md.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use types::{Price, Qty};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("risk.{field}: {value:?} is not a decimal at scale {scale}")]
    Fixed {
        field: &'static str,
        value: String,
        scale: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Paper,
    Testnet,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub venue: Venue,
    #[serde(default)]
    pub data: Data,
    #[serde(default)]
    pub api: Api,
    #[serde(default)]
    pub agents: Agents,
    #[serde(default)]
    pub risk: RiskText,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Venue {
    pub name: String,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    pub symbol: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    #[serde(default = "default_data_dir")]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Api {
    #[serde(default = "default_listen")]
    pub listen: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agents {
    #[serde(default = "default_clock")]
    pub clock_seconds: u32,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_claude_bin")]
    pub claude_bin: String,
}

/// Limits as written. Strings until an Instrument gives them a scale.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskText {
    #[serde(default = "default_max_position")]
    pub max_position_qty: String,
    #[serde(default = "default_max_order")]
    pub max_order_qty: String,
    #[serde(default = "default_max_loss")]
    pub max_daily_loss: String,
}

/// Limits in fixed-point. Built from RiskText plus an Instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskLimits {
    pub max_position_qty: Qty,
    pub max_order_qty: Qty,
    pub max_daily_loss: Price,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(text)?)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Io(path.to_owned(), e))?;
        Self::parse(&text)
    }

    pub fn risk_limits(&self, instrument: &types::Instrument) -> Result<RiskLimits, ConfigError> {
        let r = &self.risk;
        let qty = |field: &'static str, s: &str| {
            instrument.parse_qty(s).ok_or_else(|| ConfigError::Fixed {
                field,
                value: s.to_owned(),
                scale: instrument.qty_scale,
            })
        };
        let price = |field: &'static str, s: &str| {
            instrument.parse_price(s).ok_or_else(|| ConfigError::Fixed {
                field,
                value: s.to_owned(),
                scale: instrument.price_scale,
            })
        };
        Ok(RiskLimits {
            max_position_qty: qty("max_position_qty", &r.max_position_qty)?,
            max_order_qty: qty("max_order_qty", &r.max_order_qty)?,
            max_daily_loss: price("max_daily_loss", &r.max_daily_loss)?,
        })
    }
}

impl Default for Data {
    fn default() -> Self {
        Self {
            dir: default_data_dir(),
        }
    }
}
impl Default for Api {
    fn default() -> Self {
        Self {
            listen: default_listen(),
        }
    }
}
impl Default for Agents {
    fn default() -> Self {
        Self {
            clock_seconds: default_clock(),
            model: default_model(),
            claude_bin: default_claude_bin(),
        }
    }
}
impl Default for RiskText {
    fn default() -> Self {
        Self {
            max_position_qty: default_max_position(),
            max_order_qty: default_max_order(),
            max_daily_loss: default_max_loss(),
        }
    }
}

fn default_mode() -> Mode {
    Mode::Paper
}
fn default_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("agenttrade")
}
fn default_listen() -> String {
    "127.0.0.1:50051".into()
}
fn default_clock() -> u32 {
    60
}
fn default_model() -> String {
    "opus".into()
}
fn default_claude_bin() -> String {
    "claude".into()
}
fn default_max_position() -> String {
    "0.05".into()
}
fn default_max_order() -> String {
    "0.01".into()
}
fn default_max_loss() -> String {
    "50".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "[venue]\nname = \"kraken\"\nsymbol = \"BTC/USD\"\n";

    fn btc() -> types::Instrument {
        types::Instrument {
            venue: "kraken".into(),
            symbol: "BTC/USD".into(),
            price_scale: 1,
            qty_scale: 8,
            tick: Price(1),
            lot: Qty(1),
        }
    }

    #[test]
    fn minimal_config_defaults_to_paper() {
        let c = Config::parse(MINIMAL).unwrap();
        assert_eq!(c.venue.mode, Mode::Paper);
        assert_eq!(c.agents.clock_seconds, 60);
        assert_eq!(c.agents.model, "opus");
    }

    #[test]
    fn live_mode_is_rejected() {
        let text = "[venue]\nname = \"kraken\"\nmode = \"live\"\nsymbol = \"BTC/USD\"\n";
        assert!(Config::parse(text).is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let text = format!("{MINIMAL}[venue]\napi_key = \"x\"\n");
        assert!(Config::parse(&text).is_err());
    }

    #[test]
    fn risk_limits_parse_to_fixed_point() {
        let c = Config::parse(MINIMAL).unwrap();
        let r = c.risk_limits(&btc()).unwrap();
        assert_eq!(r.max_position_qty, Qty(5_000_000));
        assert_eq!(r.max_order_qty, Qty(1_000_000));
        assert_eq!(r.max_daily_loss, Price(500));
    }

    #[test]
    fn risk_limit_with_too_much_precision_is_an_error() {
        let text = format!("{MINIMAL}[risk]\nmax_daily_loss = \"50.001\"\n");
        let c = Config::parse(&text).unwrap();
        let err = c.risk_limits(&btc()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Fixed {
                field: "max_daily_loss",
                ..
            }
        ));
    }
}
