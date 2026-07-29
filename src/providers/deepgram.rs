use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use reqwest::header::{ACCEPT, AUTHORIZATION};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, credential, environment, response_json};
use crate::config::AccountTarget;
use crate::model::Metric;

const BASE_URL: &str = "https://api.deepgram.com";
const TOKEN_VARIABLE: &str = "DEEPGRAM_API_KEY";

pub(super) struct Deepgram;

impl Provider for Deepgram {
    async fn fetch(&self, client: &Client, account: &AccountTarget) -> Result<Vec<Metric>> {
        fetch_from(client, environment(&account.credentials)?, BASE_URL).await
    }
}

async fn fetch_from(
    client: &Client,
    env: &BTreeMap<String, String>,
    base_url: &str,
) -> Result<Vec<Metric>> {
    let token = credential(env, TOKEN_VARIABLE)?;
    let response = projects_request(client, base_url, token)
        .send()
        .await
        .context("Deepgram project request failed")?;
    let projects: ProjectsResponse = response_json(
        "Deepgram",
        "API key may lack the required project:read scope",
        response,
    )
    .await?;
    let project_id = select_project(&projects)?;

    let response = balances_request(client, base_url, token, &project_id)
        .send()
        .await
        .context("Deepgram balance request failed")?;
    let balances: BalancesResponse = response_json(
        "Deepgram",
        "API key may lack the required billing:read scope",
        response,
    )
    .await?;

    Ok(vec![balance_metric(balances)?])
}

fn projects_request(client: &Client, base_url: &str, token: &str) -> RequestBuilder {
    client
        .get(format!("{}/v1/projects", base_url.trim_end_matches('/')))
        .header(AUTHORIZATION, format!("Token {token}"))
        .header(ACCEPT, "application/json")
}

fn balances_request(
    client: &Client,
    base_url: &str,
    token: &str,
    project_id: &str,
) -> RequestBuilder {
    client
        .get(format!(
            "{}/v1/projects/{project_id}/balances",
            base_url.trim_end_matches('/')
        ))
        .header(AUTHORIZATION, format!("Token {token}"))
        .header(ACCEPT, "application/json")
}

fn select_project(response: &ProjectsResponse) -> Result<String> {
    match response.projects.as_slice() {
        [] => bail!("Deepgram returned no project for this API key"),
        [project] => Ok(project.project_id.clone()),
        _ => bail!("Deepgram returned multiple projects, use a project-scoped API key"),
    }
}

fn balance_metric(response: BalancesResponse) -> Result<Metric> {
    let mut balances = response.balances.into_iter();
    let first = balances
        .next()
        .context("Deepgram returned no prepaid balances")?;
    let currency = first.units.trim().to_ascii_uppercase();
    if currency.is_empty() {
        bail!("Deepgram returned a balance without units");
    }

    let mut amount = first.amount;
    for balance in balances {
        if !balance.units.eq_ignore_ascii_case(&currency) {
            bail!("Deepgram returned balances with mixed currencies");
        }
        amount += balance.amount;
    }
    if !amount.is_finite() {
        bail!("Deepgram returned an invalid balance amount");
    }

    Ok(Metric::Balance {
        label: "balance".to_owned(),
        amount,
        currency,
        used: None,
        limit: None,
    })
}

#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    projects: Vec<Project>,
}

#[derive(Debug, Deserialize)]
struct Project {
    project_id: String,
}

#[derive(Debug, Deserialize)]
struct BalancesResponse {
    balances: Vec<Balance>,
}

#[derive(Debug, Deserialize)]
struct Balance {
    amount: f64,
    units: String,
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;

    use super::{
        BalancesResponse, ProjectsResponse, balance_metric, balances_request, projects_request,
        select_project,
    };
    use crate::model::Metric;

    #[test]
    fn selects_a_single_project() {
        let response: ProjectsResponse = serde_json::from_value(json!({"projects": [{
            "project_id": "project",
            "name": "Main"
        }]}))
        .unwrap();

        assert_eq!(select_project(&response).unwrap(), "project");
    }

    #[test]
    fn sums_balances_with_the_same_currency() {
        let response: BalancesResponse = serde_json::from_value(json!({
            "balances": [
                {"amount": 20.25, "units": "USD"},
                {"amount": 5.75, "units": "usd"}
            ]
        }))
        .unwrap();

        let metric = balance_metric(response).unwrap();

        let Metric::Balance {
            amount, currency, ..
        } = metric
        else {
            panic!("expected balance metric");
        };
        assert!((amount - 26.0).abs() < f64::EPSILON);
        assert_eq!(currency, "USD");
    }

    #[test]
    fn rejects_mixed_balance_currencies() {
        let response: BalancesResponse = serde_json::from_value(json!({
            "balances": [
                {"amount": 20, "units": "USD"},
                {"amount": 5, "units": "EUR"}
            ]
        }))
        .unwrap();

        let error = balance_metric(response).unwrap_err();

        assert!(error.to_string().contains("mixed currencies"));
    }

    #[test]
    fn builds_project_and_balance_requests() {
        let client = Client::new();
        let projects = projects_request(&client, "https://example.com/", "secret")
            .build()
            .unwrap();
        let balances = balances_request(&client, "https://example.com/", "secret", "project")
            .build()
            .unwrap();

        assert_eq!(projects.url().as_str(), "https://example.com/v1/projects");
        assert_eq!(projects.headers()["authorization"], "Token secret");
        assert_eq!(
            balances.url().as_str(),
            "https://example.com/v1/projects/project/balances"
        );
    }
}
