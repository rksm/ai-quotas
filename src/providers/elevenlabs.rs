use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use reqwest::header::ACCEPT;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use super::{Provider, credential, response_json};
use crate::model::Metric;

const BASE_URL: &str = "https://api.elevenlabs.io";
const TOKEN_VARIABLE: &str = "ELEVENLABS_API_KEY";

pub(super) struct ElevenLabs;

impl Provider for ElevenLabs {
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
    let response = subscription_request(client, base_url, token)
        .send()
        .await
        .context("ElevenLabs subscription request failed")?;
    let subscription: Subscription = response_json(
        "ElevenLabs",
        "API key may lack the required user_read scope",
        response,
    )
    .await?;

    Ok(vec![subscription_metric(&subscription)?])
}

fn subscription_request(client: &Client, base_url: &str, token: &str) -> RequestBuilder {
    client
        .get(format!(
            "{}/v1/user/subscription",
            base_url.trim_end_matches('/')
        ))
        .header("xi-api-key", token)
        .header(ACCEPT, "application/json")
}

fn subscription_metric(subscription: &Subscription) -> Result<Metric> {
    if !subscription.character_count.is_finite()
        || subscription.character_count < 0.0
        || !subscription.character_limit.is_finite()
        || subscription.character_limit < 0.0
    {
        bail!("ElevenLabs returned invalid credit counts");
    }

    Ok(Metric::Balance {
        label: "credits".to_owned(),
        amount: (subscription.character_limit - subscription.character_count).max(0.0),
        currency: "credits".to_owned(),
        used: Some(subscription.character_count),
        limit: Some(subscription.character_limit),
    })
}

#[derive(Debug, Deserialize)]
struct Subscription {
    character_count: f64,
    character_limit: f64,
}

#[cfg(test)]
mod tests {
    use reqwest::Client;

    use super::{Subscription, subscription_metric, subscription_request};
    use crate::model::Metric;

    #[test]
    fn computes_remaining_subscription_credits() {
        let metric = subscription_metric(&Subscription {
            character_count: 18_797.0,
            character_limit: 100_000.0,
        })
        .unwrap();

        let Metric::Balance {
            amount,
            used,
            limit,
            currency,
            ..
        } = metric
        else {
            panic!("expected balance metric");
        };
        assert!((amount - 81_203.0).abs() < f64::EPSILON);
        assert_eq!(used, Some(18_797.0));
        assert_eq!(limit, Some(100_000.0));
        assert_eq!(currency, "credits");
    }

    #[test]
    fn clamps_overused_credits_to_zero_remaining() {
        let metric = subscription_metric(&Subscription {
            character_count: 110.0,
            character_limit: 100.0,
        })
        .unwrap();

        let Metric::Balance { amount, .. } = metric else {
            panic!("expected balance metric");
        };
        assert!(amount.abs() < f64::EPSILON);
    }

    #[test]
    fn builds_the_subscription_request() {
        let request = subscription_request(&Client::new(), "https://example.com/", "secret")
            .build()
            .unwrap();

        assert_eq!(
            request.url().as_str(),
            "https://example.com/v1/user/subscription"
        );
        assert_eq!(request.headers()["xi-api-key"], "secret");
    }
}
