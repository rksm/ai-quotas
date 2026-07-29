use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, credential, environment, response_json};
use crate::config::CredentialSource;
use crate::model::Metric;

const API_URL: &str = "https://api.runpod.io/graphql";
const BALANCE_QUERY: &str = "query { myself { clientBalance } }";
const TOKEN_VARIABLE: &str = "RUNPOD_API_KEY";

pub(super) struct Runpod;

impl Provider for Runpod {
    async fn fetch(&self, client: &Client, credentials: &CredentialSource) -> Result<Vec<Metric>> {
        fetch_from(client, environment(credentials)?, API_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    env: &BTreeMap<String, String>,
    api_url: &str,
) -> Result<Vec<Metric>> {
    let token = credential(env, TOKEN_VARIABLE)?;
    let response = balance_request(client, api_url, token)
        .send()
        .await
        .context("Runpod balance request failed")?;
    let response: BalanceResponse = response_json(
        "Runpod",
        "API key may lack read access to account data",
        response,
    )
    .await?;

    Ok(vec![balance_metric(response)?])
}

fn balance_request(client: &Client, api_url: &str, token: &str) -> RequestBuilder {
    client
        .post(api_url.trim_end_matches('/'))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(ACCEPT, "application/json")
        .json(&serde_json::json!({ "query": BALANCE_QUERY }))
}

fn balance_metric(response: BalanceResponse) -> Result<Metric> {
    if !response.errors.is_empty() {
        bail!("Runpod GraphQL request failed");
    }

    let account = response
        .data
        .and_then(|data| data.myself)
        .context("Runpod returned no account data")?;
    if !account.client_balance.is_finite() {
        bail!("Runpod returned an invalid balance amount");
    }

    Ok(Metric::Balance {
        label: "balance".to_owned(),
        amount: account.client_balance,
        currency: "USD".to_owned(),
        used: None,
        limit: None,
    })
}

#[derive(Debug, Deserialize)]
struct BalanceResponse {
    data: Option<BalanceData>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct BalanceData {
    myself: Option<Account>,
}

#[derive(Debug, Deserialize)]
struct Account {
    #[serde(rename = "clientBalance")]
    client_balance: f64,
}

#[cfg(test)]
mod tests {
    use reqwest::{Client, Method};
    use serde_json::json;

    use super::{
        Account, BALANCE_QUERY, BalanceData, BalanceResponse, balance_metric, balance_request,
    };
    use crate::model::Metric;

    #[test]
    fn maps_the_account_balance() {
        let response: BalanceResponse = serde_json::from_value(json!({
            "data": {
                "myself": {
                    "clientBalance": 23.4567
                }
            }
        }))
        .unwrap();

        let metric = balance_metric(response).unwrap();

        assert_eq!(
            metric,
            Metric::Balance {
                label: "balance".to_owned(),
                amount: 23.4567,
                currency: "USD".to_owned(),
                used: None,
                limit: None,
            }
        );
    }

    #[test]
    fn rejects_graphql_errors_even_with_partial_data() {
        let response: BalanceResponse = serde_json::from_value(json!({
            "data": {
                "myself": {
                    "clientBalance": 23.4567
                }
            },
            "errors": [{"message": "not authorized"}]
        }))
        .unwrap();

        let error = balance_metric(response).unwrap_err();

        assert!(error.to_string().contains("GraphQL request failed"));
    }

    #[test]
    fn rejects_missing_account_data() {
        let response: BalanceResponse =
            serde_json::from_value(json!({"data": {"myself": null}})).unwrap();

        let error = balance_metric(response).unwrap_err();

        assert!(error.to_string().contains("no account data"));
    }

    #[test]
    fn rejects_non_finite_balances() {
        let response = BalanceResponse {
            data: Some(BalanceData {
                myself: Some(Account {
                    client_balance: f64::NAN,
                }),
            }),
            errors: Vec::new(),
        };

        let error = balance_metric(response).unwrap_err();

        assert!(error.to_string().contains("invalid balance amount"));
    }

    #[test]
    fn builds_an_authenticated_graphql_request() {
        let request = balance_request(&Client::new(), "https://example.com/graphql/", "secret")
            .build()
            .unwrap();

        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.url().as_str(), "https://example.com/graphql");
        assert_eq!(request.headers()["authorization"], "Bearer secret");
        assert!(!request.url().as_str().contains("secret"));

        let body = request.body().unwrap().as_bytes().unwrap();
        let payload: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(payload, json!({"query": BALANCE_QUERY}));
    }
}
