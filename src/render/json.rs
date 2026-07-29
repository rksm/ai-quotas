use chrono::{DateTime, FixedOffset, SecondsFormat};
use serde::Serialize;

use crate::dashboard::{AccountSnapshot, AccountStatus, Dashboard};
use crate::model::{EvaluatedMetric, Level, Metric, Service};

/// Serialize a dashboard as one JSON document.
///
/// # Errors
///
/// Returns an error when a metric contains a number that JSON cannot represent.
pub fn render(dashboard: &Dashboard, pretty: bool) -> Result<String, serde_json::Error> {
    let document = Document {
        generated_at: timestamp(dashboard.generated_at),
        services: dashboard.entries.iter().map(Entry::from).collect(),
    };
    if pretty {
        serde_json::to_string_pretty(&document)
    } else {
        serde_json::to_string(&document)
    }
}

#[derive(Serialize)]
struct Document<'a> {
    generated_at: String,
    services: Vec<Entry<'a>>,
}

#[derive(Serialize)]
struct Entry<'a> {
    service: Service,
    account: &'a str,
    status: OutputStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    metrics: Vec<MetricOutput<'a>>,
}

impl<'a> From<&'a AccountSnapshot> for Entry<'a> {
    fn from(snapshot: &'a AccountSnapshot) -> Self {
        Self {
            service: snapshot.service,
            account: &snapshot.account,
            status: OutputStatus::from(snapshot.status),
            error: snapshot.error.as_deref(),
            metrics: snapshot.metrics.iter().map(MetricOutput::from).collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum OutputStatus {
    Ok,
    Error,
}

impl From<AccountStatus> for OutputStatus {
    fn from(status: AccountStatus) -> Self {
        match status {
            AccountStatus::Ok => Self::Ok,
            AccountStatus::Error | AccountStatus::Stale => Self::Error,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MetricOutput<'a> {
    Window {
        label: &'a str,
        used_percent: f64,
        used: Option<f64>,
        limit: Option<f64>,
        resets_at: String,
        level: Level,
    },
    Balance {
        label: &'a str,
        amount: f64,
        currency: &'a str,
        used: Option<f64>,
        limit: Option<f64>,
        level: Level,
    },
    Cost {
        label: &'a str,
        amount: f64,
        currency: &'a str,
        level: Level,
    },
}

impl<'a> From<&'a EvaluatedMetric> for MetricOutput<'a> {
    fn from(evaluated: &'a EvaluatedMetric) -> Self {
        match &evaluated.metric {
            Metric::Window {
                label,
                used_percent,
                used,
                limit,
                resets_at,
            } => Self::Window {
                label,
                used_percent: *used_percent,
                used: *used,
                limit: *limit,
                resets_at: timestamp(*resets_at),
                level: evaluated.level,
            },
            Metric::Balance {
                label,
                amount,
                currency,
                used,
                limit,
            } => Self::Balance {
                label,
                amount: *amount,
                currency,
                used: *used,
                limit: *limit,
                level: evaluated.level,
            },
            Metric::Cost {
                label,
                amount,
                currency,
            } => Self::Cost {
                label,
                amount: *amount,
                currency,
                level: evaluated.level,
            },
        }
    }
}

fn timestamp(value: DateTime<FixedOffset>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use serde_json::json;

    use super::render;
    use crate::dashboard::{AccountSnapshot, AccountStatus, Dashboard};
    use crate::model::{EvaluatedMetric, Level, Metric, Service};

    #[test]
    fn emits_the_stable_flat_account_schema() {
        let generated_at = DateTime::parse_from_rfc3339("2026-07-29T14:32:07+02:00").unwrap();
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
                            resets_at: DateTime::parse_from_rfc3339("2026-07-29T16:00:00+02:00")
                                .unwrap(),
                        },
                        level: Level::Ok,
                    }],
                    updated_at: Some(generated_at),
                },
                AccountSnapshot {
                    service: Service::OpenaiApi,
                    account: "work".to_owned(),
                    status: AccountStatus::Error,
                    error: Some("unsupported".to_owned()),
                    metrics: Vec::new(),
                    updated_at: None,
                },
            ],
        };

        let output: serde_json::Value =
            serde_json::from_str(&render(&dashboard, false).unwrap()).unwrap();

        assert_eq!(
            output,
            json!({
                "generated_at": "2026-07-29T14:32:07+02:00",
                "services": [
                    {
                        "service": "claude-code",
                        "account": "personal",
                        "status": "ok",
                        "metrics": [{
                            "kind": "window",
                            "label": "5h",
                            "used_percent": 42.0,
                            "used": null,
                            "limit": null,
                            "resets_at": "2026-07-29T16:00:00+02:00",
                            "level": "ok"
                        }]
                    },
                    {
                        "service": "openai-api",
                        "account": "work",
                        "status": "error",
                        "error": "unsupported",
                        "metrics": []
                    }
                ]
            })
        );
    }

    #[test]
    fn emits_balance_and_cost_values_in_compact_jsonl_records() {
        let generated_at = DateTime::parse_from_rfc3339("2026-07-29T14:32:07Z").unwrap();
        let dashboard = Dashboard {
            generated_at,
            entries: vec![
                AccountSnapshot {
                    service: Service::Elevenlabs,
                    account: "main".to_owned(),
                    status: AccountStatus::Stale,
                    error: Some("rate limited".to_owned()),
                    metrics: vec![EvaluatedMetric {
                        metric: Metric::Balance {
                            label: "credits".to_owned(),
                            amount: 81_203.0,
                            currency: "credits".to_owned(),
                            used: Some(18_797.0),
                            limit: Some(100_000.0),
                        },
                        level: Level::Ok,
                    }],
                    updated_at: Some(generated_at),
                },
                AccountSnapshot {
                    service: Service::OpenaiApi,
                    account: "main".to_owned(),
                    status: AccountStatus::Ok,
                    error: None,
                    metrics: vec![EvaluatedMetric {
                        metric: Metric::Cost {
                            label: "month-spend".to_owned(),
                            amount: 7.25,
                            currency: "USD".to_owned(),
                        },
                        level: Level::Ok,
                    }],
                    updated_at: Some(generated_at),
                },
            ],
        };

        let output = render(&dashboard, false).unwrap();

        assert!(!output.contains('\n'));
        assert!(output.contains(r#""status":"error""#));
        assert!(output.contains(r#""amount":81203.0"#));
        assert!(output.contains(r#""used":18797.0"#));
        assert!(output.contains(r#""limit":100000.0"#));
        assert!(output.contains(r#""kind":"cost""#));
        assert!(output.contains(r#""amount":7.25"#));
    }
}
