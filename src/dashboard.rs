use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use serde::Serialize;

use crate::engine::AccountRefresh;
use crate::model::{EvaluatedMetric, Service};

#[derive(Clone, Debug)]
pub struct Dashboard {
    pub generated_at: DateTime<FixedOffset>,
    pub entries: Vec<AccountSnapshot>,
}

#[derive(Clone, Debug)]
pub struct AccountSnapshot {
    pub service: Service,
    pub account: String,
    pub status: AccountStatus,
    pub error: Option<String>,
    pub metrics: Vec<EvaluatedMetric>,
    pub updated_at: Option<DateTime<FixedOffset>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Ok,
    Error,
    Stale,
}

impl Dashboard {
    #[must_use]
    pub fn new(generated_at: DateTime<FixedOffset>, refreshes: Vec<AccountRefresh>) -> Self {
        let entries = refreshes
            .into_iter()
            .map(|refresh| AccountSnapshot::new(generated_at, refresh))
            .collect();
        Self {
            generated_at,
            entries,
        }
    }

    pub fn update(&mut self, generated_at: DateTime<FixedOffset>, refreshes: Vec<AccountRefresh>) {
        let previous = std::mem::take(&mut self.entries)
            .into_iter()
            .map(|entry| ((entry.service, entry.account.clone()), entry))
            .collect::<BTreeMap<_, _>>();
        let mut previous = previous;

        self.entries = refreshes
            .into_iter()
            .map(|refresh| {
                let key = (refresh.service, refresh.account.clone());
                match previous.remove(&key) {
                    Some(entry) => entry.update(generated_at, refresh),
                    None => AccountSnapshot::new(generated_at, refresh),
                }
            })
            .collect();
        self.generated_at = generated_at;
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.status != AccountStatus::Ok)
    }
}

impl AccountSnapshot {
    fn new(generated_at: DateTime<FixedOffset>, refresh: AccountRefresh) -> Self {
        let AccountRefresh {
            service,
            account,
            result,
        } = refresh;
        match result {
            Ok(metrics) => Self {
                service,
                account,
                status: AccountStatus::Ok,
                error: None,
                metrics,
                updated_at: Some(generated_at),
            },
            Err(error) => Self {
                service,
                account,
                status: AccountStatus::Error,
                error: Some(error),
                metrics: Vec::new(),
                updated_at: None,
            },
        }
    }

    fn update(mut self, generated_at: DateTime<FixedOffset>, refresh: AccountRefresh) -> Self {
        match refresh.result {
            Ok(metrics) => {
                self.status = AccountStatus::Ok;
                self.error = None;
                self.metrics = metrics;
                self.updated_at = Some(generated_at);
            }
            Err(error) => {
                self.error = Some(error);
                if self.updated_at.is_some() {
                    self.status = AccountStatus::Stale;
                } else {
                    self.status = AccountStatus::Error;
                    self.metrics.clear();
                }
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, FixedOffset};

    use super::{AccountStatus, Dashboard};
    use crate::engine::AccountRefresh;
    use crate::model::{EvaluatedMetric, Level, Metric, Service};

    #[test]
    fn retains_the_last_success_until_a_later_success_replaces_it() {
        let first_at = timestamp("2026-07-29T12:00:00+00:00");
        let mut dashboard = Dashboard::new(first_at, vec![success(20.0)]);

        let first_failure_at = first_at + Duration::minutes(2);
        dashboard.update(first_failure_at, vec![failure("auth error")]);
        let stale = &dashboard.entries[0];
        assert_eq!(stale.status, AccountStatus::Stale);
        assert_eq!(stale.error.as_deref(), Some("auth error"));
        assert_eq!(stale.updated_at, Some(first_at));
        assert_close(used_percent(stale), 20.0);

        dashboard.update(
            first_at + Duration::minutes(5),
            vec![failure("still unavailable")],
        );
        let stale = &dashboard.entries[0];
        assert_eq!(stale.updated_at, Some(first_at));
        assert_close(used_percent(stale), 20.0);

        let recovered_at = first_at + Duration::minutes(6);
        dashboard.update(recovered_at, vec![success(40.0)]);
        let recovered = &dashboard.entries[0];
        assert_eq!(recovered.status, AccountStatus::Ok);
        assert_eq!(recovered.error, None);
        assert_eq!(recovered.updated_at, Some(recovered_at));
        assert_close(used_percent(recovered), 40.0);
    }

    #[test]
    fn keeps_an_initial_failure_empty_and_current() {
        let now = timestamp("2026-07-29T12:00:00+00:00");
        let dashboard = Dashboard::new(now, vec![failure("auth error")]);
        let failed = &dashboard.entries[0];

        assert_eq!(failed.status, AccountStatus::Error);
        assert!(failed.metrics.is_empty());
        assert_eq!(failed.updated_at, None);
        assert!(dashboard.has_errors());
    }

    fn success(used_percent: f64) -> AccountRefresh {
        AccountRefresh {
            service: Service::ClaudeCode,
            account: "work".to_owned(),
            result: Ok(vec![EvaluatedMetric {
                metric: Metric::Window {
                    label: "5h".to_owned(),
                    used_percent,
                    used: None,
                    limit: None,
                    resets_at: timestamp("2026-07-29T14:00:00+00:00"),
                },
                level: Level::Ok,
            }]),
        }
    }

    fn failure(error: &str) -> AccountRefresh {
        AccountRefresh {
            service: Service::ClaudeCode,
            account: "work".to_owned(),
            result: Err(error.to_owned()),
        }
    }

    fn timestamp(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).unwrap()
    }

    fn used_percent(snapshot: &super::AccountSnapshot) -> f64 {
        let Metric::Window { used_percent, .. } = &snapshot.metrics[0].metric else {
            panic!("expected window metric");
        };
        *used_percent
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < f64::EPSILON);
    }
}
