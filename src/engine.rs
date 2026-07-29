use std::time::Duration;

use anyhow::Result;
use futures::future::join_all;
use reqwest::Client;

use crate::config::AccountTarget;
use crate::model::{EvaluatedMetric, Metric, Service};
use crate::providers;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct AccountRefresh {
    pub service: Service,
    pub account: String,
    pub result: Result<Vec<EvaluatedMetric>, String>,
}

/// Fetch all selected accounts concurrently while preserving configuration order.
pub async fn query(client: &Client, targets: &[AccountTarget]) -> Vec<AccountRefresh> {
    query_with(&RemoteFetcher { client }, targets, FETCH_TIMEOUT).await
}

trait Fetcher {
    async fn fetch(&self, target: &AccountTarget) -> Result<Vec<Metric>>;
}

struct RemoteFetcher<'a> {
    client: &'a Client,
}

impl Fetcher for RemoteFetcher<'_> {
    async fn fetch(&self, target: &AccountTarget) -> Result<Vec<Metric>> {
        providers::fetch(target.service, self.client, &target.env).await
    }
}

async fn query_with<F>(
    fetcher: &F,
    targets: &[AccountTarget],
    timeout: Duration,
) -> Vec<AccountRefresh>
where
    F: Fetcher + Sync,
{
    join_all(targets.iter().map(|target| async move {
        let result = match tokio::time::timeout(timeout, fetcher.fetch(target)).await {
            Ok(Ok(metrics)) => Ok(metrics
                .into_iter()
                .map(|metric| target.thresholds.evaluate(metric))
                .collect()),
            Ok(Err(error)) => Err(format!("{error:#}")),
            Err(_) => Err(format!(
                "fetch timed out after {} seconds",
                timeout.as_secs()
            )),
        };

        AccountRefresh {
            service: target.service,
            account: target.name.clone(),
            result,
        }
    }))
    .await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future;
    use std::time::Duration;

    use anyhow::{Result, bail};
    use chrono::{TimeZone, Utc};

    use super::{AccountRefresh, Fetcher, query_with};
    use crate::config::AccountTarget;
    use crate::model::{Level, Metric, Service, Thresholds};

    struct FakeFetcher;

    impl Fetcher for FakeFetcher {
        async fn fetch(&self, target: &AccountTarget) -> Result<Vec<Metric>> {
            match target.name.as_str() {
                "slow" => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    Ok(vec![window(80.0)])
                }
                "error" => bail!("auth token expired"),
                "pending" => future::pending().await,
                _ => Ok(vec![window(20.0)]),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn preserves_target_order_and_evaluates_thresholds() {
        let targets = vec![target("slow"), target("fast")];

        let refreshes = query_with(&FakeFetcher, &targets, Duration::from_secs(10)).await;

        assert_eq!(account_names(&refreshes), ["slow", "fast"]);
        assert_eq!(refreshes[0].result.as_ref().unwrap()[0].level, Level::Warn);
        assert_eq!(refreshes[1].result.as_ref().unwrap()[0].level, Level::Ok);
    }

    #[tokio::test(start_paused = true)]
    async fn isolates_errors_and_timeouts_per_account() {
        let targets = vec![target("error"), target("pending"), target("fast")];

        let refreshes = query_with(&FakeFetcher, &targets, Duration::from_secs(5)).await;

        assert_eq!(
            refreshes[0].result.as_ref().unwrap_err(),
            "auth token expired"
        );
        assert_eq!(
            refreshes[1].result.as_ref().unwrap_err(),
            "fetch timed out after 5 seconds"
        );
        assert!(refreshes[2].result.is_ok());
    }

    fn target(name: &str) -> AccountTarget {
        AccountTarget {
            service: Service::Codex,
            name: name.to_owned(),
            env: BTreeMap::new(),
            thresholds: Thresholds::default(),
        }
    }

    fn window(used_percent: f64) -> Metric {
        Metric::Window {
            label: "7d".to_owned(),
            used_percent,
            used: None,
            limit: None,
            resets_at: Utc.timestamp_opt(0, 0).unwrap().fixed_offset(),
        }
    }

    fn account_names(refreshes: &[AccountRefresh]) -> Vec<&str> {
        refreshes
            .iter()
            .map(|refresh| refresh.account.as_str())
            .collect()
    }
}
