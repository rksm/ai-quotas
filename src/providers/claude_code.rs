use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, USER_AGENT, credential, response_json};
use crate::config::{AccountTarget, CredentialSource};
use crate::model::Metric;

const BASE_URL: &str = "https://api.anthropic.com";
const TOKEN_VARIABLE: &str = "CLAUDE_CODE_OAUTH_TOKEN";

pub(super) struct ClaudeCode;

impl Provider for ClaudeCode {
    async fn fetch(&self, client: &Client, account: &AccountTarget) -> Result<Vec<Metric>> {
        fetch_from(client, &account.credentials, BASE_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    credentials: &CredentialSource,
    base_url: &str,
) -> Result<Vec<Metric>> {
    let token = access_token(credentials)?;
    let response = usage_request(client, base_url, &token)
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

fn access_token(credentials: &CredentialSource) -> Result<String> {
    let path = match credentials {
        CredentialSource::Env(env) => {
            return credential(env, TOKEN_VARIABLE).map(str::to_owned);
        }
        CredentialSource::File(path) => path,
    };
    let path = expand_home(path)?;
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("could not read Claude Code credentials {}", path.display()))?;
    let credentials: CredentialsFile = serde_json::from_str(&contents)
        .with_context(|| format!("invalid Claude Code credentials in {}", path.display()))?;
    let token = credentials
        .claude_ai_oauth
        .and_then(|oauth| oauth.access_token)
        .with_context(|| {
            format!(
                "Claude Code credentials {} do not contain claudeAiOauth.accessToken",
                path.display()
            )
        })?;
    if token.trim().is_empty() {
        bail!(
            "Claude Code credentials {} contain an empty claudeAiOauth.accessToken",
            path.display()
        );
    }

    Ok(token.trim().to_owned())
}

fn expand_home(path: &Path) -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    expand_home_from(path, home.as_deref())
}

fn expand_home_from(path: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let Some(path_text) = path.to_str() else {
        bail!("Claude Code credentials path is not valid UTF-8");
    };
    let Some(relative) = path_text.strip_prefix("~/") else {
        return Ok(path.to_owned());
    };
    let home = home.context("HOME is not set, cannot expand Claude Code credentials path")?;
    Ok(home.join(relative))
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

    if let Some(limit) = usage
        .limits
        .into_iter()
        .flatten()
        .filter(is_fable_limit)
        .max_by_key(|limit| limit.is_active == Some(true))
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
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(Debug, Deserialize)]
struct ClaudeAiOauth {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
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
    is_active: Option<bool>,
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use reqwest::Client;
    use serde_json::json;

    use super::{UsageResponse, access_token, expand_home_from, parse_usage, usage_request};
    use crate::config::CredentialSource;
    use crate::model::Metric;

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

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
    fn prefers_an_active_fable_limit_but_accepts_an_inactive_one() {
        let response: UsageResponse = serde_json::from_value(json!({
            "limits": [
                {
                    "kind": "weekly_scoped",
                    "percent": 90,
                    "resets_at": "2026-08-03T00:00:00Z",
                    "scope": {"model": {"display_name": "Fable"}},
                    "is_active": false
                },
                {
                    "kind": "weekly_scoped",
                    "percent": 20,
                    "resets_at": "2026-08-03T00:00:00Z",
                    "scope": {"model": {"display_name": "Fable"}},
                    "is_active": true
                }
            ]
        }))
        .unwrap();

        let metrics = parse_usage(response).unwrap();

        let Metric::Window { used_percent, .. } = &metrics[0] else {
            panic!("expected window metric");
        };
        assert!((*used_percent - 20.0).abs() < f64::EPSILON);
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

    #[test]
    fn reads_the_latest_access_token_from_a_managed_credentials_file() {
        let path = temporary_path();
        let credentials = CredentialSource::File(path.clone());
        write_credentials(&path, "first-token");

        assert_eq!(
            access_token(&credentials).unwrap(),
            "first-token".to_owned()
        );

        write_credentials(&path, "refreshed-token");

        assert_eq!(
            access_token(&credentials).unwrap(),
            "refreshed-token".to_owned()
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn reports_invalid_credentials_without_exposing_their_contents() {
        let path = temporary_path();
        let credentials = CredentialSource::File(path.clone());
        fs::write(&path, r#"{"secret":"do-not-repeat"}"#).unwrap();

        let error = access_token(&credentials).expect_err("missing access token should fail");
        let message = format!("{error:#}");

        assert!(message.contains("claudeAiOauth.accessToken"));
        assert!(!message.contains("do-not-repeat"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn expands_a_home_relative_credentials_path() {
        let home = Path::new("/home/example");
        let path = Path::new("~/.claude-work/.credentials.json");

        let expanded = expand_home_from(path, Some(home)).unwrap();

        assert_eq!(
            expanded,
            Path::new("/home/example/.claude-work/.credentials.json")
        );
    }

    fn temporary_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-quotas-claude-credentials-{}-{}.json",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn write_credentials(path: &Path, token: &str) {
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "claudeAiOauth": {
                    "accessToken": token,
                    "refreshToken": "ignored",
                    "expiresAt": 9_999_999_999_999_i64
                },
                "mcpOAuth": {"ignored": true}
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn labels(metrics: &[Metric]) -> Vec<&str> {
        metrics
            .iter()
            .map(|metric| match metric {
                Metric::Window { label, .. }
                | Metric::Balance { label, .. }
                | Metric::Cost { label, .. } => label.as_str(),
            })
            .collect()
    }
}
