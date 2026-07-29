mod claude_code;
mod codex;
mod deepgram;
mod elevenlabs;
mod openai_api;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;

use crate::config::CredentialSource;
use crate::model::{Metric, Service};

const USER_AGENT: &str = concat!("ai-quotas/", env!("CARGO_PKG_VERSION"));

trait Provider {
    async fn fetch(&self, client: &Client, credentials: &CredentialSource) -> Result<Vec<Metric>>;
}

/// Fetch the fixed metric set for one configured service account.
///
/// # Errors
///
/// Returns an error when credentials are missing, a request fails, or the
/// provider returns an unusable response.
pub async fn fetch(
    service: Service,
    client: &Client,
    credentials: &CredentialSource,
) -> Result<Vec<Metric>> {
    match service {
        Service::ClaudeCode => claude_code::ClaudeCode.fetch(client, credentials).await,
        Service::Codex => codex::Codex.fetch(client, credentials).await,
        Service::OpenaiApi => openai_api::OpenAiApi.fetch(client, credentials).await,
        Service::Deepgram => deepgram::Deepgram.fetch(client, credentials).await,
        Service::Elevenlabs => elevenlabs::ElevenLabs.fetch(client, credentials).await,
    }
}

fn environment(credentials: &CredentialSource) -> Result<&BTreeMap<String, String>> {
    match credentials {
        CredentialSource::Env(env) => Ok(env),
        CredentialSource::File(_) => bail!("credentials_file is not supported by this provider"),
    }
}

fn credential<'a>(env: &'a BTreeMap<String, String>, variable: &str) -> Result<&'a str> {
    let value = env
        .get(variable)
        .with_context(|| format!("missing {variable}"))?;
    if value.trim().is_empty() {
        bail!("{variable} is empty");
    }
    Ok(value)
}

async fn response_json<T>(service: &str, forbidden_hint: &str, response: Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        if status == StatusCode::UNAUTHORIZED {
            bail!("{service} credential expired or is invalid (HTTP {status})");
        }
        if status == StatusCode::FORBIDDEN {
            bail!("{service} access forbidden, {forbidden_hint} (HTTP {status})");
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            bail!("{service} request was rate limited (HTTP {status})");
        }
        bail!("{service} request failed (HTTP {status})");
    }

    response
        .json()
        .await
        .with_context(|| format!("{service} returned malformed JSON"))
}
