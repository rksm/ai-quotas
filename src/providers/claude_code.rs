use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, USER_AGENT, credential, response_json};
use crate::model::Metric;

const BASE_URL: &str = "https://api.anthropic.com";
const TOKEN_VARIABLE: &str = "CLAUDE_CODE_OAUTH_TOKEN";

pub(super) struct ClaudeCode;

impl Provider for ClaudeCode {
    async fn fetch(&self, client: &Client, env: &BTreeMap<String, String>) -> Result<Vec<Metric>> {
        fetch_from(client, env, BASE_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    env: &BTreeMap<String, String>,
    base_url: &str,
) -> Result<Vec<Metric>> {
    let token = credential(env, TOKEN_VARIABLE)?;
    let response = usage_request(client, base_url, token)
        .send()
        .await
        .context("Claude Code usage request failed")?;
    let usage: UsageResponse = response_json(
        "Claude Code",
        "OAuth token may lack the required user:profile scope",
        response,
    )
    .await?;
    parse_usage(usage)
}

fn usage_request(client: &Client, base_url: &str, token: &str) -> RequestBuilder {
    client
        .get(format!(
            "{}/api/oauth/usage",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .header("user-agent", USER_AGENT)
}

fn parse_usage(usage: UsageResponse) -> Result<Vec<Metric>> {
    let mut metrics = Vec::with_capacity(3);

    if let Some(metric) = usage
        .five_hour
        .map(|window| metric_from_window("5h", window))
        .transpose()?
        .flatten()
    {
        metrics.push(metric);
    }
    if let Some(metric) = usage
        .seven_day
        .map(|window| metric_from_window("7d", window))
        .transpose()?
        .flatten()
    {
        metrics.push(metric);
    }

    if let Some(limit) = usage.limits.into_iter().flatten().find(is_fable_limit)
        && let Some(metric) = metric_from_limit("fable-week", limit)?
    {
        metrics.push(metric);
    }

    if metrics.is_empty() {
        bail!("Claude Code returned no recognized quota windows");
    }

    Ok(metrics)
}

fn is_fable_limit(limit: &ScopedLimit) -> bool {
    limit.kind == "weekly_scoped"
        && limit
            .scope
            .as_ref()
            .and_then(|scope| scope.model.as_ref())
            .and_then(|model| model.display_name.as_deref())
            .is_some_and(|name| name.eq_ignore_ascii_case("Fable"))
}

fn metric_from_window(label: &str, window: UsageWindow) -> Result<Option<Metric>> {
    metric(label, window.utilization, window.resets_at)
}

fn metric_from_limit(label: &str, limit: ScopedLimit) -> Result<Option<Metric>> {
    metric(label, limit.percent.or(limit.utilization), limit.resets_at)
}

fn metric(
    label: &str,
    used_percent: Option<f64>,
    resets_at: Option<Timestamp>,
) -> Result<Option<Metric>> {
    let (Some(used_percent), Some(resets_at)) = (used_percent, resets_at) else {
        return Ok(None);
    };
    if !used_percent.is_finite() || used_percent < 0.0 {
        bail!("Claude Code returned an invalid percentage for {label}");
    }

    Ok(Some(Metric::Window {
        label: label.to_owned(),
        used_percent,
        used: None,
        limit: None,
        resets_at: resets_at.parse()?,
    }))
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    #[serde(default)]
    limits: Option<Vec<ScopedLimit>>,
}

#[derive(Debug, Deserialize)]
struct UsageWindow {
    utilization: Option<f64>,
    resets_at: Option<Timestamp>,
}

#[derive(Debug, Deserialize)]
struct ScopedLimit {
    kind: String,
    percent: Option<f64>,
    utilization: Option<f64>,
    resets_at: Option<Timestamp>,
    scope: Option<LimitScope>,
}

#[derive(Debug, Deserialize)]
struct LimitScope {
    model: Option<LimitModel>,
}

#[derive(Debug, Deserialize)]
struct LimitModel {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Timestamp {
    Text(String),
    Unix(i64),
}

impl Timestamp {
    fn parse(self) -> Result<DateTime<chrono::FixedOffset>> {
        match self {
            Self::Text(value) => DateTime::parse_from_rfc3339(&value)
                .with_context(|| format!("Claude Code returned invalid reset time {value:?}")),
            Self::Unix(value) => Utc
                .timestamp_opt(value, 0)
                .single()
                .with_context(|| format!("Claude Code returned invalid reset timestamp {value}"))
                .map(|timestamp| timestamp.fixed_offset()),
        }
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;

    use super::{UsageResponse, parse_usage, usage_request};
    use crate::model::Metric;

    #[test]
    fn parses_core_and_fable_windows_in_fixed_order() {
        let response: UsageResponse = serde_json::from_value(json!({
            "five_hour": {
                "utilization": 42.0,
                "resets_at": "2026-07-29T14:00:00+00:00"
            },
            "seven_day": {
                "utilization": 71.0,
                "resets_at": 1_785_592_800
            },
            "limits": [
                {
                    "kind": "weekly_scoped",
                    "percent": 12.0,
                    "resets_at": "2026-08-03T00:00:00+00:00",
                    "scope": {
                        "model": {
                            "id": null,
                            "display_name": "fable"
                        }
                    },
                    "is_active": false
                }
            ]
        }))
        .unwrap();

        let metrics = parse_usage(response).unwrap();

        assert_eq!(labels(&metrics), ["5h", "7d", "fable-week"]);
    }

    #[test]
    fn skips_absent_windows_and_unrelated_scoped_limits() {
        let response: UsageResponse = serde_json::from_value(json!({
            "five_hour": null,
            "seven_day": {
                "utilization": 20,
                "resets_at": "2026-08-01T09:00:00Z"
            },
            "limits": [{
                "kind": "weekly_scoped",
                "percent": 50,
                "resets_at": "2026-08-03T00:00:00Z",
                "scope": {"model": {"display_name": "Sonnet"}}
            }]
        }))
        .unwrap();

        let metrics = parse_usage(response).unwrap();

        assert_eq!(labels(&metrics), ["7d"]);
    }

    #[test]
    fn builds_the_oauth_usage_request() {
        let request = usage_request(&Client::new(), "https://example.com/", "secret")
            .build()
            .unwrap();

        assert_eq!(
            request.url().as_str(),
            "https://example.com/api/oauth/usage"
        );
        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert_eq!(request.headers()["anthropic-beta"], "oauth-2025-04-20");
    }

    fn labels(metrics: &[Metric]) -> Vec<&str> {
        metrics
            .iter()
            .map(|metric| match metric {
                Metric::Window { label, .. } | Metric::Balance { label, .. } => label.as_str(),
            })
            .collect()
    }
}
