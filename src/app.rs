use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use reqwest::Client;

use crate::cli::Cli;
use crate::config::{AccountTarget, Config, default_path};
use crate::dashboard::Dashboard;
use crate::engine;
use crate::render::{human, json};

/// Run one configured monitor invocation.
///
/// # Errors
///
/// Returns an error when configuration or output fails.
pub async fn run(cli: Cli) -> Result<ExitCode> {
    let config_path = cli.config.clone().map_or_else(default_path, Ok)?;
    let config = Config::load(&config_path)?;
    let targets = config.select(&cli.services, &cli.accounts)?;
    let client = Client::new();
    let options = OutputOptions {
        json: cli.json,
        watch: cli.watch,
        terminal: io::stdout().is_terminal(),
        interval: cli.interval(),
    };

    if options.watch {
        watch(&client, &targets, options).await
    } else {
        run_once(&client, &targets, options).await
    }
}

async fn run_once(
    client: &Client,
    targets: &[AccountTarget],
    options: OutputOptions,
) -> Result<ExitCode> {
    let refreshes = engine::query(client, targets).await;
    let dashboard = Dashboard::new(Local::now().fixed_offset(), refreshes);
    let output = snapshot_output(&dashboard, options)?;
    write_output(&output)?;

    Ok(if dashboard.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

async fn watch(
    client: &Client,
    targets: &[AccountTarget],
    options: OutputOptions,
) -> Result<ExitCode> {
    let mut dashboard: Option<Dashboard> = None;

    loop {
        let refreshes = engine::query(client, targets).await;
        let generated_at = Local::now().fixed_offset();
        match &mut dashboard {
            Some(dashboard) => dashboard.update(generated_at, refreshes),
            None => dashboard = Some(Dashboard::new(generated_at, refreshes)),
        }

        let output = snapshot_output(
            dashboard
                .as_ref()
                .expect("a refresh always creates a dashboard"),
            options,
        )?;
        write_output(&output)?;

        tokio::select! {
            () = tokio::time::sleep(options.interval) => {}
            signal = tokio::signal::ctrl_c() => {
                signal.context("could not listen for Ctrl-C")?;
                return Ok(ExitCode::SUCCESS);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct OutputOptions {
    json: bool,
    watch: bool,
    terminal: bool,
    interval: Duration,
}

fn snapshot_output(dashboard: &Dashboard, options: OutputOptions) -> Result<String> {
    if options.json {
        let mut output =
            json::render(dashboard, !options.watch).context("could not serialize JSON output")?;
        output.push('\n');
        return Ok(output);
    }

    let mut output = String::new();
    if options.watch && options.terminal {
        output.push_str("\u{1b}[2J\u{1b}[H");
    }
    output.push_str(&human::render(dashboard, options.terminal));
    if options.watch {
        writeln!(
            output,
            "last updated {}, next in {}s",
            dashboard.generated_at.format("%H:%M:%S"),
            options.interval.as_secs()
        )
        .expect("writing to a String cannot fail");
    }
    Ok(output)
}

fn write_output(output: &str) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout
        .write_all(output.as_bytes())
        .context("could not write output")?;
    stdout.flush().context("could not flush output")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::DateTime;

    use super::{OutputOptions, snapshot_output};
    use crate::dashboard::{AccountSnapshot, AccountStatus, Dashboard};
    use crate::model::{EvaluatedMetric, Level, Metric, Service};

    #[test]
    fn frames_watch_json_as_one_plain_jsonl_record() {
        let output = snapshot_output(
            &dashboard(),
            OutputOptions {
                json: true,
                watch: true,
                terminal: true,
                interval: Duration::from_mins(1),
            },
        )
        .unwrap();

        assert_eq!(output.lines().count(), 1);
        assert!(serde_json::from_str::<serde_json::Value>(&output).is_ok());
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains("last updated"));
    }

    #[test]
    fn adds_clear_sequence_and_footer_only_to_terminal_watch_output() {
        let terminal = snapshot_output(
            &dashboard(),
            OutputOptions {
                json: false,
                watch: true,
                terminal: true,
                interval: Duration::from_secs(30),
            },
        )
        .unwrap();
        let piped = snapshot_output(
            &dashboard(),
            OutputOptions {
                terminal: false,
                ..OutputOptions {
                    json: false,
                    watch: true,
                    terminal: true,
                    interval: Duration::from_secs(30),
                }
            },
        )
        .unwrap();

        assert!(terminal.starts_with("\u{1b}[2J\u{1b}[H"));
        assert!(terminal.contains("last updated 14:32:07, next in 30s"));
        assert!(!piped.contains("\u{1b}[2J"));
        assert!(piped.contains("last updated 14:32:07, next in 30s"));
    }

    fn dashboard() -> Dashboard {
        let generated_at = DateTime::parse_from_rfc3339("2026-07-29T14:32:07+02:00").unwrap();
        Dashboard {
            generated_at,
            entries: vec![AccountSnapshot {
                service: Service::Codex,
                account: "main".to_owned(),
                status: AccountStatus::Ok,
                error: None,
                metrics: vec![EvaluatedMetric {
                    metric: Metric::Window {
                        label: "7d".to_owned(),
                        used_percent: 55.0,
                        used: None,
                        limit: None,
                        resets_at: generated_at + chrono::Duration::hours(2),
                    },
                    level: Level::Ok,
                }],
                updated_at: Some(generated_at),
            }],
        }
    }
}
