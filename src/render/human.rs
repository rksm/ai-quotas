use std::fmt::Write;

use chrono::{DateTime, FixedOffset, Local, TimeDelta, TimeZone};

use crate::dashboard::{AccountSnapshot, AccountStatus, Dashboard};
use crate::model::{EvaluatedMetric, Level, Metric};

const BAR_WIDTH: usize = 8;

#[must_use]
pub fn render(dashboard: &Dashboard, color: bool) -> String {
    render_in(dashboard, color, &Local)
}

fn render_in<Tz>(dashboard: &Dashboard, color: bool, timezone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let mut output = String::new();
    let service_width = dashboard
        .entries
        .iter()
        .map(|entry| entry.service.to_string().len())
        .max()
        .unwrap_or_default();

    for entry in &dashboard.entries {
        writeln!(
            output,
            "{:<width$}  {}",
            entry.service,
            display_text(&entry.account),
            width = service_width,
        )
        .expect("writing to a String cannot fail");

        match entry.status {
            AccountStatus::Error => {
                writeln!(
                    output,
                    "  ! {}",
                    display_text(entry.error.as_deref().unwrap_or("unknown error"))
                )
                .expect("writing to a String cannot fail");
            }
            AccountStatus::Ok | AccountStatus::Stale => {
                for metric in &entry.metrics {
                    let line = metric_line(metric, dashboard.generated_at, timezone);
                    writeln!(
                        output,
                        "{}",
                        style_metric(
                            &line,
                            metric.level,
                            entry.status == AccountStatus::Stale,
                            color,
                        )
                    )
                    .expect("writing to a String cannot fail");
                }
                if entry.status == AccountStatus::Stale {
                    writeln!(
                        output,
                        "  ! stale {}, {}",
                        stale_age(entry, dashboard.generated_at),
                        display_text(entry.error.as_deref().unwrap_or("unknown error"))
                    )
                    .expect("writing to a String cannot fail");
                }
            }
        }
    }

    output
}

fn metric_line<Tz>(evaluated: &EvaluatedMetric, now: DateTime<FixedOffset>, timezone: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    match &evaluated.metric {
        Metric::Window {
            label,
            used_percent,
            resets_at,
            ..
        } => {
            let reset = resets_at.with_timezone(timezone);
            let local_now = now.with_timezone(timezone);
            let reset_time = if reset.date_naive() == local_now.date_naive() {
                reset.format("%H:%M").to_string()
            } else {
                reset.format("%a %H:%M").to_string()
            };
            format!(
                "  {label:<12} {:>4}% {}  resets {reset_time} ({})",
                concise_number(*used_percent),
                progress_bar(*used_percent),
                relative_time(*resets_at - now),
            )
        }
        Metric::Balance {
            label,
            amount,
            currency,
            limit,
            ..
        } => {
            let value = if currency.eq_ignore_ascii_case("credits") {
                match limit {
                    Some(limit) => {
                        format!("{} / {}", grouped_number(*amount), grouped_number(*limit))
                    }
                    None => grouped_number(*amount),
                }
            } else {
                currency_amount(*amount, &display_text(currency))
            };
            format!("  {label:<12} {value} remaining")
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn progress_bar(used_percent: f64) -> String {
    let filled = (used_percent.clamp(0.0, 100.0) * BAR_WIDTH as f64 / 100.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled))
}

fn concise_number(value: f64) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn grouped_number(value: f64) -> String {
    let precision = usize::from((value - value.round()).abs() >= 0.005) * 2;
    let raw = format!("{value:.precision$}");
    group_decimal(&raw)
}

fn group_decimal(raw: &str) -> String {
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    let (sign, digits) = whole
        .strip_prefix('-')
        .map_or(("", whole), |digits| ("-", digits));
    let mut grouped = String::with_capacity(raw.len() + raw.len() / 3);
    grouped.push_str(sign);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if !fraction.is_empty() {
        grouped.push('.');
        grouped.push_str(fraction);
    }
    grouped
}

fn currency_amount(amount: f64, currency: &str) -> String {
    let amount = grouped_fixed(amount, 2);
    if currency.eq_ignore_ascii_case("USD") {
        format!("${amount}")
    } else {
        format!("{amount} {}", currency.to_ascii_uppercase())
    }
}

fn grouped_fixed(value: f64, precision: usize) -> String {
    let raw = format!("{value:.precision$}");
    group_decimal(&raw)
}

fn relative_time(delta: TimeDelta) -> String {
    if delta >= TimeDelta::zero() {
        format!("in {}", duration(delta))
    } else {
        format!("{} ago", duration(-delta))
    }
}

fn stale_age(entry: &AccountSnapshot, now: DateTime<FixedOffset>) -> String {
    entry.updated_at.map_or_else(
        || "unknown age".to_owned(),
        |updated_at| duration((now - updated_at).max(TimeDelta::zero())),
    )
}

fn duration(delta: TimeDelta) -> String {
    let minutes = delta.num_minutes();
    if minutes < 1 {
        return "<1m".to_owned();
    }

    let days = minutes / (24 * 60);
    let hours = minutes % (24 * 60) / 60;
    let minutes = minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn style_metric(line: &str, level: Level, stale: bool, color: bool) -> String {
    if !color {
        return line.to_owned();
    }

    let mut codes = Vec::with_capacity(2);
    if stale {
        codes.push("2");
    }
    match level {
        Level::Ok => {}
        Level::Warn => codes.push("33"),
        Level::Critical => codes.push("31"),
    }
    if codes.is_empty() {
        line.to_owned()
    } else {
        format!("\u{1b}[{}m{line}\u{1b}[0m", codes.join(";"))
    }
}

fn display_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset};

    use super::{render, render_in};
    use crate::dashboard::{AccountSnapshot, AccountStatus, Dashboard};
    use crate::model::{EvaluatedMetric, Level, Metric, Service};

    #[test]
    fn renders_plain_quota_and_balance_blocks() {
        let generated_at = DateTime::parse_from_rfc3339("2026-07-29T11:50:00+02:00").unwrap();
        let dashboard = Dashboard {
            generated_at,
            entries: vec![
                AccountSnapshot {
                    service: Service::ClaudeCode,
                    account: "personal".to_owned(),
                    status: AccountStatus::Ok,
                    error: None,
                    metrics: vec![EvaluatedMetric {
                        metric: Metric::Window {
                            label: "5h".to_owned(),
                            used_percent: 42.0,
                            used: None,
                            limit: None,
                            resets_at: DateTime::parse_from_rfc3339("2026-07-29T14:00:00+02:00")
                                .unwrap(),
                        },
                        level: Level::Ok,
                    }],
                    updated_at: Some(generated_at),
                },
                AccountSnapshot {
                    service: Service::Deepgram,
                    account: "main".to_owned(),
                    status: AccountStatus::Ok,
                    error: None,
                    metrics: vec![EvaluatedMetric {
                        metric: Metric::Balance {
                            label: "balance".to_owned(),
                            amount: 1_102.1,
                            currency: "USD".to_owned(),
                            used: None,
                            limit: None,
                        },
                        level: Level::Ok,
                    }],
                    updated_at: Some(generated_at),
                },
            ],
        };

        assert_eq!(
            render_in(
                &dashboard,
                false,
                &FixedOffset::east_opt(2 * 60 * 60).unwrap()
            ),
            "\
claude-code  personal
  5h             42% ███░░░░░  resets 14:00 (in 2h 10m)
deepgram     main
  balance      $1,102.10 remaining
"
        );
    }

    #[test]
    fn always_marks_stale_and_failed_accounts_without_color() {
        let generated_at = DateTime::parse_from_rfc3339("2026-07-29T12:05:00+00:00").unwrap();
        let dashboard = Dashboard {
            generated_at,
            entries: vec![
                AccountSnapshot {
                    service: Service::Codex,
                    account: "main".to_owned(),
                    status: AccountStatus::Stale,
                    error: Some("auth error".to_owned()),
                    metrics: vec![EvaluatedMetric {
                        metric: Metric::Window {
                            label: "7d".to_owned(),
                            used_percent: 90.0,
                            used: None,
                            limit: None,
                            resets_at: generated_at,
                        },
                        level: Level::Critical,
                    }],
                    updated_at: Some(generated_at - chrono::Duration::minutes(5)),
                },
                AccountSnapshot {
                    service: Service::OpenaiApi,
                    account: "personal".to_owned(),
                    status: AccountStatus::Error,
                    error: Some("unsupported".to_owned()),
                    metrics: Vec::new(),
                    updated_at: None,
                },
            ],
        };

        let output = render(&dashboard, false);

        assert!(output.contains("! stale 5m, auth error"));
        assert!(output.contains("openai-api  personal\n  ! unsupported"));
        assert!(!output.contains('\u{1b}'));
    }

    #[test]
    fn colors_thresholds_and_dims_stale_values_when_enabled() {
        let generated_at = DateTime::parse_from_rfc3339("2026-07-29T12:05:00+00:00").unwrap();
        let dashboard = Dashboard {
            generated_at,
            entries: vec![AccountSnapshot {
                service: Service::Codex,
                account: "main".to_owned(),
                status: AccountStatus::Stale,
                error: Some("error".to_owned()),
                metrics: vec![EvaluatedMetric {
                    metric: Metric::Balance {
                        label: "balance".to_owned(),
                        amount: 1.0,
                        currency: "USD".to_owned(),
                        used: None,
                        limit: None,
                    },
                    level: Level::Critical,
                }],
                updated_at: Some(generated_at),
            }],
        };

        assert!(render(&dashboard, true).contains("\u{1b}[2;31m"));
    }
}
