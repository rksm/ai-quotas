use std::fmt;

use chrono::{DateTime, FixedOffset};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum Service {
    ClaudeCode,
    Codex,
    OpenaiApi,
    Deepgram,
    Elevenlabs,
}

impl fmt::Display for Service {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenaiApi => "openai-api",
            Self::Deepgram => "deepgram",
            Self::Elevenlabs => "elevenlabs",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub quota_warn: f64,
    pub quota_critical: f64,
    pub balance_critical: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            quota_warn: 70.0,
            quota_critical: 90.0,
            balance_critical: 10.0,
        }
    }
}

impl Thresholds {
    #[must_use]
    pub fn evaluate(self, metric: Metric) -> EvaluatedMetric {
        let level = match &metric {
            Metric::Window { used_percent, .. } => {
                if *used_percent >= self.quota_critical {
                    Level::Critical
                } else if *used_percent >= self.quota_warn {
                    Level::Warn
                } else {
                    Level::Ok
                }
            }
            Metric::Balance { amount, limit, .. } => {
                if let Some(limit) = limit {
                    if (*limit > 0.0 && amount / limit < 0.1) || (*limit <= 0.0 && *amount <= 0.0) {
                        Level::Critical
                    } else {
                        Level::Ok
                    }
                } else if *amount < self.balance_critical {
                    Level::Critical
                } else {
                    Level::Ok
                }
            }
        };

        EvaluatedMetric { metric, level }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Metric {
    Window {
        label: String,
        used_percent: f64,
        used: Option<f64>,
        limit: Option<f64>,
        resets_at: DateTime<FixedOffset>,
    },
    Balance {
        label: String,
        amount: f64,
        currency: String,
        used: Option<f64>,
        limit: Option<f64>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvaluatedMetric {
    pub metric: Metric,
    pub level: Level,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Ok,
    Warn,
    Critical,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{Level, Metric, Thresholds};

    #[test]
    fn evaluates_quota_threshold_boundaries() {
        let thresholds = Thresholds::default();

        assert_eq!(thresholds.evaluate(window(69.999)).level, Level::Ok);
        assert_eq!(thresholds.evaluate(window(70.0)).level, Level::Warn);
        assert_eq!(thresholds.evaluate(window(89.999)).level, Level::Warn);
        assert_eq!(thresholds.evaluate(window(90.0)).level, Level::Critical);
    }

    #[test]
    fn evaluates_currency_balances_below_the_critical_amount() {
        let thresholds = Thresholds::default();

        assert_eq!(
            thresholds.evaluate(balance(9.999, None)).level,
            Level::Critical
        );
        assert_eq!(thresholds.evaluate(balance(10.0, None)).level, Level::Ok);
    }

    #[test]
    fn evaluates_limited_balances_by_remaining_percentage() {
        let thresholds = Thresholds::default();

        assert_eq!(
            thresholds.evaluate(balance(9.999, Some(100.0))).level,
            Level::Critical
        );
        assert_eq!(
            thresholds.evaluate(balance(10.0, Some(100.0))).level,
            Level::Ok
        );
    }

    fn window(used_percent: f64) -> Metric {
        Metric::Window {
            label: "7d".to_owned(),
            used_percent,
            used: None,
            limit: None,
            resets_at: Utc.timestamp_opt(0, 0).unwrap().fixed_offset(),
        }
    }

    fn balance(amount: f64, limit: Option<f64>) -> Metric {
        Metric::Balance {
            label: "balance".to_owned(),
            amount,
            currency: "USD".to_owned(),
            used: None,
            limit,
        }
    }
}
