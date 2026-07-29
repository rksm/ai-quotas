use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Utc};
use reqwest::header::ACCEPT;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::Deserialize;

use super::{Provider, USER_AGENT, credential, environment, response_json};
use crate::config::CredentialSource;
use crate::model::Metric;

const BASE_URL: &str = "https://api.openai.com";
const TOKEN_VARIABLE: &str = "OPENAI_ADMIN_KEY";

pub(super) struct OpenAiApi;

impl Provider for OpenAiApi {
    async fn fetch(&self, client: &Client, credentials: &CredentialSource) -> Result<Vec<Metric>> {
        fetch_from(client, environment(credentials)?, BASE_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    env: &BTreeMap<String, String>,
    base_url: &str,
) -> Result<Vec<Metric>> {
    let token = credential(env, TOKEN_VARIABLE)?;
    let start_time = Utc::now()
        .date_naive()
        .with_day(1)
        .expect("every month has a first day")
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp();

    let response = costs_request(client, base_url, token, start_time)
        .send()
        .await
        .context("OpenAI costs request failed")?;
    let costs: CostsResponse = response_json(
        "OpenAI",
        "key must be an organization Admin API key",
        response,
    )
    .await?;
    let (spent, currency) = cost_total(costs)?;
    let mut metrics = vec![Metric::Cost {
        label: "month-spend".to_owned(),
        amount: spent,
        currency: currency.clone(),
    }];

    let response = spend_limit_request(client, base_url, token)
        .send()
        .await
        .context("OpenAI spend-limit request failed")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(metrics);
    }

    let limit: SpendLimit = response_json(
        "OpenAI",
        "key must be an organization Admin API key",
        response,
    )
    .await?;
    metrics.push(spend_limit_metric(limit, spent, &currency)?);
    Ok(metrics)
}

fn costs_request(client: &Client, base_url: &str, token: &str, start_time: i64) -> RequestBuilder {
    client
        .get(format!(
            "{}/v1/organization/costs",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT)
        .query(&[("start_time", start_time), ("limit", 31)])
}

fn spend_limit_request(client: &Client, base_url: &str, token: &str) -> RequestBuilder {
    client
        .get(format!(
            "{}/v1/organization/spend_limit",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT)
}

fn cost_total(response: CostsResponse) -> Result<(f64, String)> {
    let mut total = 0.0;
    let mut currency: Option<String> = None;

    for result in response.data.into_iter().flat_map(|bucket| bucket.results) {
        let value = result.amount.value.parse("cost")?;
        total += value;

        let result_currency = normalized_currency(&result.amount.currency)?;
        match &currency {
            Some(currency) if currency != &result_currency => {
                bail!("OpenAI returned costs with mixed currencies");
            }
            Some(_) => {}
            None => currency = Some(result_currency),
        }
    }

    if !total.is_finite() {
        bail!("OpenAI returned an invalid total cost");
    }
    Ok((total, currency.unwrap_or_else(|| "USD".to_owned())))
}

fn spend_limit_metric(limit: SpendLimit, spent: f64, cost_currency: &str) -> Result<Metric> {
    let amount = limit.threshold_amount.parse("spend limit")?;
    if amount < 0.0 {
        bail!("OpenAI returned a negative spend limit");
    }

    let currency = normalized_currency(&limit.currency)?;
    if !currency.eq_ignore_ascii_case(cost_currency) {
        bail!("OpenAI returned costs and a spend limit with different currencies");
    }

    Ok(Metric::Balance {
        label: "spend-limit".to_owned(),
        amount: (amount - spent).max(0.0),
        currency,
        used: Some(spent),
        limit: Some(amount),
    })
}

fn normalized_currency(currency: &str) -> Result<String> {
    let currency = currency.trim().to_ascii_uppercase();
    if currency.is_empty() {
        bail!("OpenAI returned an empty currency");
    }
    Ok(currency)
}

#[derive(Debug, Deserialize)]
struct CostsResponse {
    data: Vec<CostBucket>,
}

#[derive(Debug, Deserialize)]
struct CostBucket {
    results: Vec<CostResult>,
}

#[derive(Debug, Deserialize)]
struct CostResult {
    amount: Amount,
}

#[derive(Debug, Deserialize)]
struct Amount {
    value: Decimal,
    currency: String,
}

#[derive(Debug, Deserialize)]
struct SpendLimit {
    threshold_amount: Decimal,
    currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Decimal {
    Number(f64),
    String(String),
}

impl Decimal {
    fn parse(self, label: &str) -> Result<f64> {
        let value = match self {
            Self::Number(value) => value,
            Self::String(value) => value
                .parse()
                .with_context(|| format!("OpenAI returned an invalid {label}"))?,
        };
        if !value.is_finite() {
            bail!("OpenAI returned an invalid {label}");
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;

    use super::{
        CostsResponse, SpendLimit, cost_total, costs_request, spend_limit_metric,
        spend_limit_request,
    };
    use crate::model::Metric;

    #[test]
    fn sums_string_and_numeric_costs() {
        let response: CostsResponse = serde_json::from_value(json!({
            "data": [
                {"results": [
                    {"amount": {"value": "0.70487225", "currency": "usd"}},
                    {"amount": {"value": 7.223_865, "currency": "USD"}}
                ]},
                {"results": [
                    {"amount": {"value": "0.6334794", "currency": "usd"}}
                ]}
            ]
        }))
        .unwrap();

        let (amount, currency) = cost_total(response).unwrap();

        assert!((amount - 8.562_216_65).abs() < f64::EPSILON);
        assert_eq!(currency, "USD");
    }

    #[test]
    fn treats_an_empty_month_as_zero_usd() {
        let response: CostsResponse = serde_json::from_value(json!({
            "data": [{"results": []}]
        }))
        .unwrap();

        assert_eq!(cost_total(response).unwrap(), (0.0, "USD".to_owned()));
    }

    #[test]
    fn rejects_mixed_cost_currencies() {
        let response: CostsResponse = serde_json::from_value(json!({
            "data": [{"results": [
                {"amount": {"value": 1, "currency": "USD"}},
                {"amount": {"value": 2, "currency": "EUR"}}
            ]}]
        }))
        .unwrap();

        let error = cost_total(response).unwrap_err();

        assert!(error.to_string().contains("mixed currencies"));
    }

    #[test]
    fn computes_remaining_spend_limit_headroom() {
        let limit: SpendLimit = serde_json::from_value(json!({
            "threshold_amount": 100,
            "currency": "USD"
        }))
        .unwrap();

        assert_eq!(
            spend_limit_metric(limit, 23.5, "usd").unwrap(),
            Metric::Balance {
                label: "spend-limit".to_owned(),
                amount: 76.5,
                currency: "USD".to_owned(),
                used: Some(23.5),
                limit: Some(100.0),
            }
        );
    }

    #[test]
    fn builds_authenticated_admin_requests() {
        let client = Client::new();
        let costs = costs_request(&client, "https://example.com/", "secret", 1_785_283_200)
            .build()
            .unwrap();
        let limit = spend_limit_request(&client, "https://example.com/", "secret")
            .build()
            .unwrap();

        assert_eq!(
            costs.url().as_str(),
            "https://example.com/v1/organization/costs?start_time=1785283200&limit=31"
        );
        assert_eq!(costs.headers()["authorization"], "Bearer secret");
        assert_eq!(
            limit.url().as_str(),
            "https://example.com/v1/organization/spend_limit"
        );
        assert_eq!(limit.headers()["authorization"], "Bearer secret");
    }
}
