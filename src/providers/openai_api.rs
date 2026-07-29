use std::collections::BTreeMap;

use anyhow::{Result, bail};
use reqwest::Client;

use super::{Provider, credential};
use crate::model::Metric;

const TOKEN_VARIABLE: &str = "OPENAI_API_KEY";

pub(super) struct OpenAiApi;

impl Provider for OpenAiApi {
    async fn fetch(&self, _client: &Client, env: &BTreeMap<String, String>) -> Result<Vec<Metric>> {
        credential(env, TOKEN_VARIABLE)?;
        bail!("OpenAI does not expose prepaid balance to API keys")
    }
}
