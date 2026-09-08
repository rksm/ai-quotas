use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, NaiveDateTime, Utc};
use reqwest::header::ACCEPT;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, USER_AGENT, credential, response_json};
use crate::config::{AccountTarget, CredentialSource};
use crate::model::Metric;

const BASE_URL: &str = "https://openrouter.ai/api/v1";
const TOKEN_VARIABLE: &str = "OPENROUTER_MANAGEMENT_KEY";
const SPEND_PERIODS: [(i64, &str); 3] = [(1, "1d-spend"), (7, "7d-spend"), (30, "30d-spend")];

pub(super) struct OpenRouter;

impl Provider for OpenRouter {
    async fn fetch(&self, client: &Client, account: &AccountTarget) -> Result<Vec<Metric>> {
        fetch_from(client, &account.credentials, BASE_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    credentials: &CredentialSource,
    base_url: &str,
) -> Result<Vec<Metric>> {
    let token = management_key(credentials).await?;
    let credits = api_request(client, base_url, &token, "credits")
        .send()
        .await
        .context("OpenRouter credits request failed")?;
    let credits: CreditsResponse =
        response_json("OpenRouter", "key must be a management key", credits).await?;

    let activity = api_request(client, base_url, &token, "activity")
        .send()
        .await
        .context("OpenRouter activity request failed")?;
    let activity: ActivityResponse =
        response_json("OpenRouter", "key must be a management key", activity).await?;

    metrics(credits, activity, Utc::now().date_naive())
}

async fn management_key(credentials: &CredentialSource) -> Result<String> {
    match credentials {
        CredentialSource::Env(env) => credential(env, TOKEN_VARIABLE).map(str::to_owned),
        CredentialSource::File(file) => {
            let contents = file.read("OpenRouter").await?;
            let token = contents.trim();
            if token.is_empty() {
                bail!("OpenRouter credentials {file} are empty");
            }
            Ok(token.to_owned())
        }
    }
}

fn api_request(client: &Client, base_url: &str, token: &str, endpoint: &str) -> RequestBuilder {
    client
        .get(format!("{}/{endpoint}", base_url.trim_end_matches('/')))
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT)
}

fn metrics(
    credits: CreditsResponse,
    activity: ActivityResponse,
    today: NaiveDate,
) -> Result<Vec<Metric>> {
    let credits = credits.data;
    if !credits.total_credits.is_finite() || credits.total_credits < 0.0 {
        bail!("OpenRouter returned invalid total credits");
    }
    if !credits.total_usage.is_finite() || credits.total_usage < 0.0 {
        bail!("OpenRouter returned invalid total usage");
    }

    let mut spend = [0.0; SPEND_PERIODS.len()];
    for item in activity.data {
        if !item.usage.is_finite() || item.usage < 0.0 {
            bail!("OpenRouter returned invalid activity usage");
        }
        let date = NaiveDate::parse_from_str(&item.date, "%Y-%m-%d")
            .or_else(|_| {
                NaiveDateTime::parse_from_str(&item.date, "%Y-%m-%d %H:%M:%S")
                    .map(|timestamp| timestamp.date())
            })
            .with_context(|| {
                format!("OpenRouter returned invalid activity date {:?}", item.date)
            })?;
        let age = today.signed_duration_since(date).num_days();
        for (index, (days, _)) in SPEND_PERIODS.iter().enumerate() {
            if (1..=*days).contains(&age) {
                spend[index] += item.usage;
            }
        }
    }
    if spend.iter().any(|amount| !amount.is_finite()) {
        bail!("OpenRouter returned an invalid activity total");
    }

    let mut metrics = Vec::with_capacity(1 + SPEND_PERIODS.len());
    metrics.push(Metric::Balance {
        label: "balance".to_owned(),
        amount: credits.total_credits - credits.total_usage,
        currency: "USD".to_owned(),
        used: Some(credits.total_usage),
        limit: None,
    });
    metrics.extend(
        SPEND_PERIODS
            .iter()
            .zip(spend)
            .map(|((_, label), amount)| Metric::Cost {
                label: (*label).to_owned(),
                amount,
                currency: "USD".to_owned(),
            }),
    );
    Ok(metrics)
}

#[derive(Debug, Deserialize)]
struct CreditsResponse {
    data: Credits,
}

#[derive(Debug, Deserialize)]
struct Credits {
    total_credits: f64,
    total_usage: f64,
}

#[derive(Debug, Deserialize)]
struct ActivityResponse {
    data: Vec<Activity>,
}

#[derive(Debug, Deserialize)]
struct Activity {
    date: String,
    usage: f64,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use chrono::NaiveDate;
    use reqwest::Client;
    use serde_json::json;

    use super::{ActivityResponse, CreditsResponse, api_request, management_key, metrics};
    use crate::config::{CredentialSource, CredentialsFile};
    use crate::model::Metric;

    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn computes_the_balance_and_completed_utc_day_spend() {
        let credits: CreditsResponse = serde_json::from_value(json!({
            "data": {"total_credits": 100.0, "total_usage": 25.0}
        }))
        .unwrap();
        let activity: ActivityResponse = serde_json::from_value(json!({
            "data": [
                {"date": "2026-09-07 00:00:00", "usage": 1.0},
                {"date": "2026-09-05", "usage": 2.0},
                {"date": "2026-08-31 00:00:00", "usage": 4.0},
                {"date": "2026-08-09 00:00:00", "usage": 8.0},
                {"date": "2026-08-08", "usage": 16.0},
                {"date": "2026-09-08 00:00:00", "usage": 32.0}
            ]
        }))
        .unwrap();

        assert_eq!(
            metrics(
                credits,
                activity,
                NaiveDate::from_ymd_opt(2026, 9, 8).unwrap()
            )
            .unwrap(),
            vec![
                Metric::Balance {
                    label: "balance".to_owned(),
                    amount: 75.0,
                    currency: "USD".to_owned(),
                    used: Some(25.0),
                    limit: None,
                },
                cost("1d-spend", 1.0),
                cost("7d-spend", 3.0),
                cost("30d-spend", 15.0),
            ]
        );
    }

    #[test]
    fn rejects_invalid_activity_dates() {
        for date in [
            "2026-02-30 00:00:00",
            "2026-09-07 24:00:00",
            "2026-09-07 garbage",
            "2026-09-07 00:00:00 garbage",
        ] {
            let credits: CreditsResponse = serde_json::from_value(json!({
                "data": {"total_credits": 100.0, "total_usage": 25.0}
            }))
            .unwrap();
            let activity: ActivityResponse = serde_json::from_value(json!({
                "data": [{"date": date, "usage": 1.0}]
            }))
            .unwrap();

            let error = metrics(
                credits,
                activity,
                NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
            )
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("OpenRouter returned invalid activity date {date:?}")
            );
        }
    }

    #[test]
    fn builds_authenticated_management_requests() {
        let request = api_request(
            &Client::new(),
            "https://example.com/api/v1/",
            "secret",
            "credits",
        )
        .build()
        .unwrap();

        assert_eq!(request.url().as_str(), "https://example.com/api/v1/credits");
        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert_eq!(request.headers()["accept"], "application/json");
    }

    #[tokio::test]
    async fn reads_a_raw_management_key_file() {
        let path = temporary_path();
        fs::write(&path, "management-key\n").unwrap();

        assert_eq!(
            management_key(&CredentialSource::File(local_file(&path)))
                .await
                .unwrap(),
            "management-key"
        );
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn reads_a_management_key_from_the_environment_source() {
        let credentials = CredentialSource::Env(BTreeMap::from([(
            "OPENROUTER_MANAGEMENT_KEY".to_owned(),
            "management-key".to_owned(),
        )]));

        assert_eq!(
            management_key(&credentials).await.unwrap(),
            "management-key"
        );
    }

    fn cost(label: &str, amount: f64) -> Metric {
        Metric::Cost {
            label: label.to_owned(),
            amount,
            currency: "USD".to_owned(),
        }
    }

    fn local_file(path: &Path) -> CredentialsFile {
        CredentialsFile::from(path.to_str().unwrap().to_owned())
    }

    fn temporary_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-quotas-openrouter-credentials-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
