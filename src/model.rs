use std::fmt;

use clap::ValueEnum;
use serde::Deserialize;

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
