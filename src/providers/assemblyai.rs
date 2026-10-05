use anyhow::{Context, Result};
use reqwest::header::{ACCEPT, COOKIE};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, USER_AGENT, raw_credential, response_json};
use crate::config::AccountTarget;
use crate::model::Metric;

// AssemblyAI's public API has no balance endpoint. This undocumented
// endpoint backs the dashboard Billing page and needs the Cookie header of a
// browser login session, not an API key.
const BASE_URL: &str = "https://www.assemblyai.com/dashboard/api";
const COOKIE_VARIABLE: &str = "ASSEMBLYAI_DASHBOARD_COOKIE";

pub(super) struct AssemblyAi;

impl Provider for AssemblyAi {
    async fn fetch(&self, client: &Client, account: &AccountTarget) -> Result<Vec<Metric>> {
        let cookie = raw_credential("AssemblyAI", COOKIE_VARIABLE, &account.credentials).await?;
        let response = balance_request(client, BASE_URL, &cookie)
            .send()
            .await
            .context("AssemblyAI balance request failed")?;
        let response: BalanceResponse = response_json(
            "AssemblyAI",
            "the dashboard login may lack billing access",
            response,
        )
        .await?;

        // The balance is in USD and goes negative when usage outruns funds.
        Ok(vec![Metric::Balance {
            label: "balance".to_owned(),
            amount: response.balance,
            currency: "USD".to_owned(),
            used: None,
            limit: None,
        }])
    }
}

fn balance_request(client: &Client, base_url: &str, cookie: &str) -> RequestBuilder {
    client
        .get(format!(
            "{}/accounts/balance",
            base_url.trim_end_matches('/')
        ))
        .header(COOKIE, cookie)
        .header(ACCEPT, "application/json")
        .header("user-agent", USER_AGENT)
}

#[derive(Debug, Deserialize)]
struct BalanceResponse {
    balance: f64,
}

#[cfg(test)]
mod tests {
    use reqwest::Client;

    use super::balance_request;

    #[test]
    fn builds_a_cookie_authenticated_balance_request() {
        let request = balance_request(
            &Client::new(),
            "https://example.com/dashboard/api/",
            "session=secret",
        )
        .build()
        .unwrap();

        assert_eq!(
            request.url().as_str(),
            "https://example.com/dashboard/api/accounts/balance"
        );
        assert_eq!(request.headers()["cookie"], "session=secret");
    }
}
