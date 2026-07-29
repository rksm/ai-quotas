use anyhow::{Result, bail};
use reqwest::Client;

use super::{Provider, credential, environment};
use crate::config::CredentialSource;
use crate::model::Metric;

const TOKEN_VARIABLE: &str = "OPENAI_API_KEY";

pub(super) struct OpenAiApi;

impl Provider for OpenAiApi {
    async fn fetch(&self, _client: &Client, credentials: &CredentialSource) -> Result<Vec<Metric>> {
        credential(environment(credentials)?, TOKEN_VARIABLE)?;
        bail!("OpenAI does not expose prepaid balance to API keys")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use reqwest::Client;

    use super::{OpenAiApi, Provider};
    use crate::config::CredentialSource;

    #[tokio::test]
    async fn reports_the_missing_public_balance_api() {
        let credentials = CredentialSource::Env(BTreeMap::from([(
            "OPENAI_API_KEY".to_owned(),
            "secret".to_owned(),
        )]));

        let error = OpenAiApi
            .fetch(&Client::new(), &credentials)
            .await
            .expect_err("OpenAI balance fetch should be unsupported");

        assert_eq!(
            error.to_string(),
            "OpenAI does not expose prepaid balance to API keys"
        );
    }
}
