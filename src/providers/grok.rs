use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use reqwest::header::ACCEPT;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::Value;

use super::{Provider, USER_AGENT, credential, response_json};
use crate::config::{AccountTarget, CredentialSource};
use crate::model::Metric;

const BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
const TOKEN_VARIABLE: &str = "GROK_OAUTH_TOKEN";

pub(super) struct Grok;

impl Provider for Grok {
    async fn fetch(&self, client: &Client, account: &AccountTarget) -> Result<Vec<Metric>> {
        fetch_from(client, &account.credentials, BASE_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    credentials: &CredentialSource,
    base_url: &str,
) -> Result<Vec<Metric>> {
    let token = access_token(credentials).await?;
    let response = billing_request(client, base_url, &token)
        .send()
        .await
        .context("Grok quota request failed")?;
    let billing: BillingResponse = response_json(
        "Grok",
        "OAuth token may not have access to Grok billing",
        response,
    )
    .await?;

    Ok(vec![weekly_metric(billing)?])
}

async fn access_token(credentials: &CredentialSource) -> Result<String> {
    let file = match credentials {
        CredentialSource::Env(env) => {
            return credential(env, TOKEN_VARIABLE).map(str::to_owned);
        }
        CredentialSource::File(file) => file,
    };
    let contents = file.read("Grok").await?;
    let credentials: CredentialsJson = serde_json::from_str(&contents)
        .with_context(|| format!("invalid Grok credentials in {file}"))?;
    let token = credentials
        .access_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .or_else(|| grok_cli_token(&credentials.entries))
        .with_context(|| {
            format!(
                "Grok credentials {file} do not contain a non-empty access_token or Grok CLI key"
            )
        })?;

    Ok(token.trim().to_owned())
}

fn grok_cli_token(entries: &BTreeMap<String, Value>) -> Option<&str> {
    entries
        .iter()
        .filter(|(key, _)| key.starts_with("https://auth.x.ai::"))
        .map(|(_, value)| value)
        .chain(entries.values())
        .filter_map(|entry| entry.get("key").and_then(Value::as_str))
        .find(|token| !token.trim().is_empty())
}

fn billing_request(client: &Client, base_url: &str, token: &str) -> RequestBuilder {
    client
        .get(format!(
            "{}/billing?format=credits",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header("x-xai-token-auth", "xai-grok-cli")
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT)
}

fn weekly_metric(billing: BillingResponse) -> Result<Metric> {
    let config = billing.config.context("Grok returned no billing config")?;
    let period = config
        .current_period
        .context("Grok returned no current billing period")?;
    if !period.kind.to_ascii_uppercase().contains("WEEK") {
        bail!("Grok returned a non-weekly billing period");
    }

    let used_percent = config.credit_usage_percent.unwrap_or(0.0);
    if !used_percent.is_finite() || used_percent < 0.0 {
        bail!("Grok returned an invalid weekly percentage");
    }
    let resets_at = DateTime::parse_from_rfc3339(&period.end)
        .with_context(|| format!("Grok returned invalid reset time {:?}", period.end))?;

    Ok(Metric::Window {
        label: "7d".to_owned(),
        used_percent,
        used: None,
        limit: None,
        resets_at,
    })
}

#[derive(Debug, Deserialize)]
struct CredentialsJson {
    access_token: Option<String>,
    #[serde(flatten)]
    entries: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct BillingResponse {
    config: Option<BillingConfig>,
}

#[derive(Debug, Deserialize)]
struct BillingConfig {
    #[serde(rename = "creditUsagePercent")]
    credit_usage_percent: Option<f64>,
    #[serde(rename = "currentPeriod")]
    current_period: Option<BillingPeriod>,
}

#[derive(Debug, Deserialize)]
struct BillingPeriod {
    #[serde(rename = "type")]
    kind: String,
    end: String,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono::DateTime;
    use reqwest::Client;
    use serde_json::json;

    use super::{BillingResponse, access_token, billing_request, weekly_metric};
    use crate::config::{CredentialSource, CredentialsFile};
    use crate::model::Metric;

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn parses_the_weekly_credit_window() {
        let response: BillingResponse = serde_json::from_value(json!({
            "config": {
                "creditUsagePercent": 42.5,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2026-08-24T14:42:22.854544+00:00",
                    "end": "2026-08-31T14:42:22.854544+00:00"
                }
            }
        }))
        .unwrap();

        let metric = weekly_metric(response).unwrap();

        let Metric::Window {
            label,
            used_percent,
            resets_at,
            ..
        } = metric
        else {
            panic!("expected window metric");
        };
        assert_eq!(label, "7d");
        assert!((used_percent - 42.5).abs() < f64::EPSILON);
        assert_eq!(
            resets_at,
            DateTime::parse_from_rfc3339("2026-08-31T14:42:22.854544+00:00").unwrap()
        );
    }

    #[test]
    fn treats_an_omitted_percentage_as_unused() {
        let response: BillingResponse = serde_json::from_value(json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-31T14:42:22Z"
                }
            }
        }))
        .unwrap();

        let Metric::Window { used_percent, .. } = weekly_metric(response).unwrap() else {
            panic!("expected window metric");
        };

        assert!(used_percent.abs() < f64::EPSILON);
    }

    #[test]
    fn builds_the_billing_request() {
        let request = billing_request(&Client::new(), "https://example.com/v1/", "secret")
            .build()
            .unwrap();

        assert_eq!(
            request.url().as_str(),
            "https://example.com/v1/billing?format=credits"
        );
        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert_eq!(request.headers()["x-xai-token-auth"], "xai-grok-cli");
    }

    #[tokio::test]
    async fn reads_a_grok_cli_token() {
        let path = temporary_path();
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "https://auth.x.ai::client-id": {
                    "key": "cli-token",
                    "auth_mode": "oauth",
                    "user_id": "user"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            access_token(&CredentialSource::File(local_file(&path)))
                .await
                .unwrap(),
            "cli-token"
        );
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn reads_a_cli_proxy_api_token() {
        let path = temporary_path();
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "access_token": "proxy-token",
                "refresh_token": "ignored",
                "type": "xai"
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            access_token(&CredentialSource::File(local_file(&path)))
                .await
                .unwrap(),
            "proxy-token"
        );
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn reads_a_token_from_the_environment_source() {
        let credentials = CredentialSource::Env(BTreeMap::from([(
            "GROK_OAUTH_TOKEN".to_owned(),
            "oauth-token".to_owned(),
        )]));

        assert_eq!(access_token(&credentials).await.unwrap(), "oauth-token");
    }

    fn local_file(path: &Path) -> CredentialsFile {
        CredentialsFile::from(path.to_str().unwrap().to_owned())
    }

    fn temporary_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-quotas-grok-credentials-{}-{}.json",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
