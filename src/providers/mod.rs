mod assemblyai;
mod claude_code;
mod codex;
mod deepgram;
mod elevenlabs;
mod grok;
mod openai_api;
mod openrouter;
mod runpod;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;

use crate::config::{AccountTarget, CredentialSource};
use crate::model::{Metric, Service};

const USER_AGENT: &str = concat!("ai-quotas/", env!("CARGO_PKG_VERSION"));

trait Provider {
    async fn fetch(&self, client: &Client, account: &AccountTarget) -> Result<Vec<Metric>>;
}

/// Fetch the fixed metric set for one configured service account.
///
/// # Errors
///
/// Returns an error when credentials are missing, a request fails, or the
/// provider returns an unusable response.
pub async fn fetch(client: &Client, account: &AccountTarget) -> Result<Vec<Metric>> {
    match account.service {
        Service::ClaudeCode => claude_code::ClaudeCode.fetch(client, account).await,
        Service::Codex => codex::Codex.fetch(client, account).await,
        Service::Grok => grok::Grok.fetch(client, account).await,
        Service::OpenaiApi => openai_api::OpenAiApi.fetch(client, account).await,
        Service::Openrouter => openrouter::OpenRouter.fetch(client, account).await,
        Service::Assemblyai => assemblyai::AssemblyAi.fetch(client, account).await,
        Service::Deepgram => deepgram::Deepgram.fetch(client, account).await,
        Service::Elevenlabs => elevenlabs::ElevenLabs.fetch(client, account).await,
        Service::Runpod => runpod::Runpod.fetch(client, account).await,
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

/// Read a secret from an env variable or from a file that contains only the
/// secret. Surrounding whitespace in the file is ignored.
async fn raw_credential(
    service: &str,
    variable: &str,
    credentials: &CredentialSource,
) -> Result<String> {
    match credentials {
        CredentialSource::Env(env) => credential(env, variable).map(str::to_owned),
        CredentialSource::File(file) => {
            let contents = file.read(service).await?;
            let value = contents.trim();
            if value.is_empty() {
                bail!("{service} credentials {file} are empty");
            }
            Ok(value.to_owned())
        }
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use super::raw_credential;
    use crate::config::{CredentialSource, CredentialsFile};

    #[tokio::test]
    async fn reads_a_raw_credential_file() {
        let path =
            std::env::temp_dir().join(format!("ai-quotas-raw-credential-{}", std::process::id()));
        fs::write(&path, "secret\n").unwrap();
        let file = CredentialsFile::from(path.to_str().unwrap().to_owned());

        assert_eq!(
            raw_credential("Example", "EXAMPLE_KEY", &CredentialSource::File(file))
                .await
                .unwrap(),
            "secret"
        );
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn reads_a_raw_credential_from_the_environment_source() {
        let credentials = CredentialSource::Env(BTreeMap::from([(
            "EXAMPLE_KEY".to_owned(),
            "secret".to_owned(),
        )]));

        assert_eq!(
            raw_credential("Example", "EXAMPLE_KEY", &credentials)
                .await
                .unwrap(),
            "secret"
        );
    }
}
