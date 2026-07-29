use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

use crate::model::Service;

const DEFAULT_INTERVAL_SECONDS: u64 = 60;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Read configuration from this path
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Emit JSON instead of a human-readable table
    #[arg(long)]
    pub json: bool,

    /// Refresh continuously
    #[arg(long)]
    pub watch: bool,

    /// Seconds between refreshes
    #[arg(long, value_name = "SECS", requires = "watch")]
    interval: Option<NonZeroU64>,

    /// Fetch only this service, repeatable
    #[arg(long = "service", value_name = "NAME")]
    pub services: Vec<Service>,

    /// Fetch only accounts with this name, repeatable
    #[arg(long = "account", value_name = "NAME")]
    pub accounts: Vec<String>,
}

impl Cli {
    #[must_use]
    pub fn interval(&self) -> Duration {
        Duration::from_secs(
            self.interval
                .map_or(DEFAULT_INTERVAL_SECONDS, NonZeroU64::get),
        )
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;
    use crate::model::Service;

    #[test]
    fn defaults_to_a_sixty_second_interval() {
        let cli = Cli::try_parse_from(["ai-quotas"]).unwrap();

        assert_eq!(cli.interval().as_secs(), 60);
        assert!(!cli.watch);
        assert!(!cli.json);
    }

    #[test]
    fn accepts_repeatable_filters() {
        let cli = Cli::try_parse_from([
            "ai-quotas",
            "--service",
            "claude-code",
            "--service",
            "codex",
            "--service",
            "runpod",
            "--account",
            "work",
            "--account",
            "personal",
        ])
        .unwrap();

        assert_eq!(
            cli.services,
            [Service::ClaudeCode, Service::Codex, Service::Runpod]
        );
        assert_eq!(cli.accounts, ["work", "personal"]);
    }

    #[test]
    fn rejects_an_interval_without_watch_mode() {
        let error = Cli::try_parse_from(["ai-quotas", "--interval", "5"]).unwrap_err();

        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn rejects_unknown_services() {
        let error = Cli::try_parse_from(["ai-quotas", "--service", "made-up"]).unwrap_err();

        assert_eq!(error.exit_code(), 2);
    }
}
