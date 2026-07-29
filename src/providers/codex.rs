use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::{TimeZone, Utc};
use reqwest::header::ACCEPT;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, USER_AGENT, credential, response_json};
use crate::model::Metric;

const BACKEND_URL: &str = "https://chatgpt.com";
const AUTH_URL: &str = "https://auth.openai.com";
const OAUTH_TOKEN_VARIABLE: &str = "CODEX_OAUTH_TOKEN";
const ACCESS_TOKEN_VARIABLE: &str = "CODEX_ACCESS_TOKEN";
const ACCOUNT_ID_VARIABLE: &str = "CODEX_ACCOUNT_ID";
const WEEK_SECONDS: i64 = 7 * 24 * 60 * 60;

pub(super) struct Codex;

impl Provider for Codex {
    async fn fetch(&self, client: &Client, env: &BTreeMap<String, String>) -> Result<Vec<Metric>> {
        fetch_from(client, env, AUTH_URL, BACKEND_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    env: &BTreeMap<String, String>,
    auth_url: &str,
    backend_url: &str,
) -> Result<Vec<Metric>> {
    let (token, account_id) = if env.contains_key(OAUTH_TOKEN_VARIABLE) {
        (
            credential(env, OAUTH_TOKEN_VARIABLE)?,
            env.get(ACCOUNT_ID_VARIABLE)
                .map(String::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
        )
    } else {
        let token = credential(env, ACCESS_TOKEN_VARIABLE)
            .context("set CODEX_OAUTH_TOKEN or CODEX_ACCESS_TOKEN")?;
        if !token.starts_with("at-") {
            bail!(
                "CODEX_ACCESS_TOKEN must be an at- personal access token, use CODEX_OAUTH_TOKEN for a raw OAuth access token"
            );
        }
        let response = whoami_request(client, auth_url, token)
            .send()
            .await
            .context("Codex account discovery request failed")?;
        let identity: Identity =
            response_json("Codex", "token may lack account-discovery access", response).await?;
        (token, Some(identity.chatgpt_account_id))
    };

    let response = usage_request(client, backend_url, token, account_id.as_deref())
        .send()
        .await
        .context("Codex quota request failed")?;
    let usage: UsageResponse = response_json(
        "Codex",
        "token may not have access to this ChatGPT workspace",
        response,
    )
    .await?;

    Ok(vec![parse_weekly_quota(usage)?])
}

fn whoami_request(client: &Client, base_url: &str, token: &str) -> RequestBuilder {
    client
        .get(format!(
            "{}/api/accounts/v1/user-auth-credential/whoami",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT)
}

fn usage_request(
    client: &Client,
    base_url: &str,
    token: &str,
    account_id: Option<&str>,
) -> RequestBuilder {
    let request = client
        .get(format!(
            "{}/backend-api/wham/usage",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT);

    if let Some(account_id) = account_id {
        request.header("ChatGPT-Account-ID", account_id)
    } else {
        request
    }
}

fn parse_weekly_quota(usage: UsageResponse) -> Result<Metric> {
    let rate_limit = usage
        .rate_limit
        .context("Codex returned no rate-limit information")?;
    let weekly = [rate_limit.primary_window, rate_limit.secondary_window]
        .into_iter()
        .flatten()
        .find(|window| window.limit_window_seconds == WEEK_SECONDS)
        .context("Codex returned no 7-day quota window")?;

    if !weekly.used_percent.is_finite() || weekly.used_percent < 0.0 {
        bail!("Codex returned an invalid 7-day percentage");
    }
    let resets_at = Utc
        .timestamp_opt(weekly.reset_at, 0)
        .single()
        .context("Codex returned an invalid 7-day reset timestamp")?
        .fixed_offset();

    Ok(Metric::Window {
        label: "7d".to_owned(),
        used_percent: weekly.used_percent,
        used: None,
        limit: None,
        resets_at,
    })
}

#[derive(Debug, Deserialize)]
struct Identity {
    chatgpt_account_id: String,
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    rate_limit: Option<RateLimit>,
}

#[derive(Debug, Deserialize)]
struct RateLimit {
    primary_window: Option<RateLimitWindow>,
    secondary_window: Option<RateLimitWindow>,
}

#[derive(Debug, Deserialize)]
struct RateLimitWindow {
    used_percent: f64,
    limit_window_seconds: i64,
    reset_at: i64,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use reqwest::Client;
    use serde_json::json;

    use super::{UsageResponse, fetch_from, parse_weekly_quota, usage_request, whoami_request};
    use crate::model::Metric;

    #[test]
    fn selects_the_seven_day_window_by_duration() {
        let response: UsageResponse = serde_json::from_value(json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 23,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 7200,
                    "reset_at": 1_785_343_200
                },
                "secondary_window": {
                    "used_percent": 55,
                    "limit_window_seconds": 604_800,
                    "reset_after_seconds": 252_000,
                    "reset_at": 1_785_592_800
                }
            }
        }))
        .unwrap();

        let metric = parse_weekly_quota(response).unwrap();

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
        assert!((used_percent - 55.0).abs() < f64::EPSILON);
        assert_eq!(
            resets_at,
            Utc.timestamp_opt(1_785_592_800, 0).unwrap().fixed_offset()
        );
    }

    #[test]
    fn builds_usage_and_account_discovery_requests() {
        let client = Client::new();
        let usage = usage_request(
            &client,
            "https://chatgpt.example/",
            "secret",
            Some("account"),
        )
        .build()
        .unwrap();
        let whoami = whoami_request(&client, "https://auth.example/", "secret")
            .build()
            .unwrap();

        assert_eq!(
            usage.url().as_str(),
            "https://chatgpt.example/backend-api/wham/usage"
        );
        assert_eq!(usage.headers()["authorization"], "Bearer secret");
        assert_eq!(usage.headers()["chatgpt-account-id"], "account");
        assert_eq!(
            whoami.url().as_str(),
            "https://auth.example/api/accounts/v1/user-auth-credential/whoami"
        );
    }

    #[tokio::test]
    async fn rejects_non_pat_values_in_the_access_token_variable() {
        let env = BTreeMap::from([(
            "CODEX_ACCESS_TOKEN".to_owned(),
            "oauth-access-token".to_owned(),
        )]);

        let error = fetch_from(
            &Client::new(),
            &env,
            "https://auth.example",
            "https://chatgpt.example",
        )
        .await
        .expect_err("a non-PAT access token should fail");

        assert!(
            error
                .to_string()
                .contains("must be an at- personal access token")
        );
    }
}
