use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;

use crate::model::{Service, Thresholds};

pub struct Config {
    pub services: BTreeMap<Service, ServiceConfig>,
}

pub struct ServiceConfig {
    pub thresholds: Thresholds,
    pub accounts: Vec<AccountConfig>,
}

pub struct AccountTarget {
    pub service: Service,
    pub name: String,
    pub credentials: CredentialSource,
    pub balance: Option<BalanceAnchor>,
    pub thresholds: Thresholds,
}

pub enum CredentialSource {
    Env(BTreeMap<String, String>),
    File(PathBuf),
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BalanceAnchor {
    pub as_of: DateTime<FixedOffset>,
    pub amount: f64,
    pub currency: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub env: Option<BTreeMap<String, String>>,
    pub credentials_file: Option<PathBuf>,
    pub balance: Option<BalanceAnchor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    thresholds: ThresholdOverrides,
    services: BTreeMap<Service, RawServiceConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServiceConfig {
    #[serde(default)]
    thresholds: ThresholdOverrides,
    accounts: Vec<AccountConfig>,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThresholdOverrides {
    quota_warn: Option<f64>,
    quota_critical: Option<f64>,
    balance_critical: Option<f64>,
}

impl Config {
    /// Parse and validate a configuration document.
    ///
    /// # Errors
    ///
    /// Returns an error when the YAML shape or any configured value is invalid.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let raw: RawConfig = serde_yaml::from_str(yaml).context("could not parse YAML")?;
        let global_thresholds = raw.thresholds.apply(Thresholds::default());
        validate_thresholds(global_thresholds, "thresholds")?;

        if raw.services.is_empty() {
            bail!("services must contain at least one service");
        }

        let services = raw
            .services
            .into_iter()
            .map(|(service, raw_service)| {
                validate_accounts(service, &raw_service.accounts)?;
                let thresholds = raw_service.thresholds.apply(global_thresholds);
                validate_thresholds(thresholds, &format!("services.{service}.thresholds"))?;

                Ok((
                    service,
                    ServiceConfig {
                        thresholds,
                        accounts: raw_service.accounts,
                    },
                ))
            })
            .collect::<Result<_>>()?;

        Ok(Self { services })
    }

    /// Read, parse, and validate a configuration file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or its contents are invalid.
    pub fn load(path: &Path) -> Result<Self> {
        let yaml = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        Self::from_yaml(&yaml)
            .with_context(|| format!("invalid configuration in {}", path.display()))
    }

    /// Select configured accounts using intersecting service and account filters.
    ///
    /// # Errors
    ///
    /// Returns an error when the filters match no configured accounts.
    pub fn select(
        self,
        service_filters: &[Service],
        account_filters: &[String],
    ) -> Result<Vec<AccountTarget>> {
        let mut targets = Vec::new();
        for (service, config) in self.services {
            if !service_filters.is_empty() && !service_filters.contains(&service) {
                continue;
            }
            for account in config.accounts {
                if !account_filters.is_empty() && !account_filters.contains(&account.name) {
                    continue;
                }
                let credentials = match (account.env, account.credentials_file) {
                    (Some(env), None) => CredentialSource::Env(env),
                    (None, Some(path)) => CredentialSource::File(path),
                    _ => bail!(
                        "account {:?} must configure exactly one of env or credentials_file",
                        account.name
                    ),
                };
                targets.push(AccountTarget {
                    service,
                    name: account.name,
                    credentials,
                    balance: account.balance,
                    thresholds: config.thresholds,
                });
            }
        }

        if targets.is_empty() {
            bail!("filters matched no configured accounts");
        }

        Ok(targets)
    }
}

impl ThresholdOverrides {
    fn apply(self, base: Thresholds) -> Thresholds {
        Thresholds {
            quota_warn: self.quota_warn.unwrap_or(base.quota_warn),
            quota_critical: self.quota_critical.unwrap_or(base.quota_critical),
            balance_critical: self.balance_critical.unwrap_or(base.balance_critical),
        }
    }
}

/// Return the default configuration path.
///
/// # Errors
///
/// Returns an error when the home directory cannot be determined.
pub fn default_path() -> Result<PathBuf> {
    let home = env::var_os("HOME").filter(|value| !value.is_empty());
    let home = home.context("HOME is not set, pass --config explicitly")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("ai-quotas")
        .join("config.yaml"))
}

fn validate_accounts(service: Service, accounts: &[AccountConfig]) -> Result<()> {
    if accounts.is_empty() {
        bail!("services.{service}.accounts must contain at least one account");
    }

    let mut names = HashSet::new();
    for account in accounts {
        if account.name.trim().is_empty() {
            bail!("services.{service}.accounts contains an empty account name");
        }
        if !names.insert(account.name.as_str()) {
            bail!(
                "services.{service}.accounts contains duplicate name {:?}",
                account.name
            );
        }
        match (&account.env, &account.credentials_file) {
            (Some(_), None) => {}
            (None, Some(path)) if service == Service::ClaudeCode => {
                validate_credentials_path(service, &account.name, path)?;
            }
            (None, Some(_)) => {
                bail!(
                    "services.{service}.accounts entry {:?} uses credentials_file, which is only supported by claude-code",
                    account.name
                );
            }
            _ => {
                bail!(
                    "services.{service}.accounts entry {:?} must configure exactly one of env or credentials_file",
                    account.name
                );
            }
        }
        validate_balance(service, &account.name, account.balance.as_ref())?;
    }

    Ok(())
}

fn validate_balance(
    service: Service,
    account: &str,
    balance: Option<&BalanceAnchor>,
) -> Result<()> {
    let Some(balance) = balance else {
        return Ok(());
    };
    if service != Service::OpenaiApi {
        bail!(
            "services.{service}.accounts entry {account:?} uses balance, which is only supported by openai-api"
        );
    }
    if !balance.amount.is_finite() {
        bail!("services.{service}.accounts entry {account:?} has an invalid balance amount");
    }
    if balance.currency.trim().is_empty() {
        bail!("services.{service}.accounts entry {account:?} has an empty balance currency");
    }
    Ok(())
}

fn validate_credentials_path(service: Service, account: &str, path: &Path) -> Result<()> {
    let path_text = path
        .to_str()
        .context("credentials_file path is not valid UTF-8")?;
    if path_text.is_empty() {
        bail!("services.{service}.accounts entry {account:?} has an empty credentials_file path");
    }
    if !path.is_absolute() && !path_text.starts_with("~/") {
        bail!(
            "services.{service}.accounts entry {account:?} credentials_file must be absolute or start with ~/"
        );
    }
    Ok(())
}

fn validate_thresholds(thresholds: Thresholds, location: &str) -> Result<()> {
    if !thresholds.quota_warn.is_finite() || !(0.0..=100.0).contains(&thresholds.quota_warn) {
        bail!("{location}.quota_warn must be between 0 and 100");
    }
    if !thresholds.quota_critical.is_finite() || !(0.0..=100.0).contains(&thresholds.quota_critical)
    {
        bail!("{location}.quota_critical must be between 0 and 100");
    }
    if thresholds.quota_warn >= thresholds.quota_critical {
        bail!("{location}.quota_warn must be less than quota_critical");
    }
    if !thresholds.balance_critical.is_finite() || thresholds.balance_critical < 0.0 {
        bail!("{location}.balance_critical must be zero or greater");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Config;
    use crate::model::Service;

    const CONFIG: &str = r"
thresholds:
  quota_warn: 75
services:
  claude-code:
    accounts:
      - name: personal
        env:
          CLAUDE_CODE_OAUTH_TOKEN: token
      - name: work
        credentials_file: /home/example/.claude-work/.credentials.json
  deepgram:
    thresholds:
      balance_critical: 25
    accounts:
      - name: main
        env:
          DEEPGRAM_API_KEY: key
  runpod:
    accounts:
      - name: main
        env:
          RUNPOD_API_KEY: key
";

    #[test]
    fn resolves_global_defaults_and_service_overrides() {
        let config = Config::from_yaml(CONFIG).unwrap();
        let claude = &config.services[&Service::ClaudeCode];
        let deepgram = &config.services[&Service::Deepgram];
        let runpod = &config.services[&Service::Runpod];

        assert_close(claude.thresholds.quota_warn, 75.0);
        assert_close(claude.thresholds.quota_critical, 90.0);
        assert_close(claude.thresholds.balance_critical, 10.0);
        assert_close(deepgram.thresholds.balance_critical, 25.0);
        assert_close(runpod.thresholds.balance_critical, 10.0);
        assert_eq!(claude.accounts[0].name, "personal");
        assert!(matches!(
            claude.accounts[1].credentials_file.as_deref(),
            Some(path)
                if path == std::path::Path::new(
                    "/home/example/.claude-work/.credentials.json"
                )
        ));
    }

    #[test]
    fn rejects_unknown_services() {
        let error = Config::from_yaml(
            r"
services:
  unknown:
    accounts:
      - name: main
        env: {}
",
        )
        .err()
        .expect("unknown service should fail");

        assert!(format!("{error:#}").contains("unknown variant `unknown`"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = Config::from_yaml(
            r"
extra: true
services: {}
",
        )
        .err()
        .expect("unknown field should fail");

        assert!(format!("{error:#}").contains("unknown field"));
    }

    #[test]
    fn rejects_duplicate_account_names_within_a_service() {
        let error = Config::from_yaml(
            r"
services:
  codex:
    accounts:
      - name: main
        env: {}
      - name: main
        env: {}
",
        )
        .err()
        .expect("duplicate account should fail");

        assert!(error.to_string().contains("duplicate name"));
    }

    #[test]
    fn rejects_invalid_threshold_ranges() {
        let error = Config::from_yaml(
            r"
thresholds:
  quota_warn: 95
  quota_critical: 90
services:
  codex:
    accounts:
      - name: main
        env: {}
",
        )
        .err()
        .expect("invalid thresholds should fail");

        assert!(error.to_string().contains("must be less"));
    }

    #[test]
    fn rejects_credentials_file_for_other_services() {
        let error = Config::from_yaml(
            r"
services:
  codex:
    accounts:
      - name: main
        credentials_file: /home/example/.codex/auth.json
",
        )
        .err()
        .expect("unsupported credentials file should fail");

        assert!(error.to_string().contains("only supported by claude-code"));
    }

    #[test]
    fn rejects_ambiguous_credential_sources() {
        let error = Config::from_yaml(
            r"
services:
  claude-code:
    accounts:
      - name: work
        credentials_file: /home/example/.claude-work/.credentials.json
        env:
          CLAUDE_CODE_OAUTH_TOKEN: token
",
        )
        .err()
        .expect("ambiguous credentials should fail");

        assert!(
            error
                .to_string()
                .contains("exactly one of env or credentials_file")
        );
    }

    #[test]
    fn rejects_accounts_without_a_credential_source() {
        let error = Config::from_yaml(
            r"
services:
  claude-code:
    accounts:
      - name: work
",
        )
        .err()
        .expect("missing credentials should fail");

        assert!(
            error
                .to_string()
                .contains("exactly one of env or credentials_file")
        );
    }

    #[test]
    fn rejects_relative_credentials_paths() {
        let error = Config::from_yaml(
            r"
services:
  claude-code:
    accounts:
      - name: work
        credentials_file: .claude-work/.credentials.json
",
        )
        .err()
        .expect("relative credentials path should fail");

        assert!(
            error
                .to_string()
                .contains("must be absolute or start with ~/")
        );
    }

    #[test]
    fn parses_an_openai_balance_anchor() {
        let targets = Config::from_yaml(
            r#"
services:
  openai-api:
    accounts:
      - name: main
        env:
          OPENAI_ADMIN_KEY: secret
        balance:
          as_of: "2026-07-29T18:00:00+02:00"
          amount: 20.5
          currency: USD
"#,
        )
        .unwrap()
        .select(&[Service::OpenaiApi], &[])
        .unwrap();

        let balance = targets[0].balance.as_ref().unwrap();
        assert_eq!(
            balance.as_of,
            chrono::DateTime::parse_from_rfc3339("2026-07-29T18:00:00+02:00").unwrap()
        );
        assert_close(balance.amount, 20.5);
        assert_eq!(balance.currency, "USD");
    }

    #[test]
    fn rejects_balance_anchors_for_other_services() {
        let error = Config::from_yaml(
            r#"
services:
  deepgram:
    accounts:
      - name: main
        env:
          DEEPGRAM_API_KEY: secret
        balance:
          as_of: "2026-07-29T18:00:00+02:00"
          amount: 20.5
          currency: USD
"#,
        )
        .err()
        .expect("unsupported balance should fail");

        assert!(error.to_string().contains("only supported by openai-api"));
    }

    #[test]
    fn selects_the_intersection_of_service_and_account_filters() {
        let targets = Config::from_yaml(CONFIG)
            .unwrap()
            .select(&[Service::Deepgram], &["main".to_owned()])
            .unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].service, Service::Deepgram);
        assert_eq!(targets[0].name, "main");
    }

    #[test]
    fn rejects_filters_that_match_nothing() {
        let error = Config::from_yaml(CONFIG)
            .unwrap()
            .select(&[Service::Codex], &[])
            .err()
            .expect("unmatched filters should fail");

        assert!(error.to_string().contains("matched no configured accounts"));
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < f64::EPSILON);
    }
}
