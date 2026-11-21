//! 按计划、触发时刻和动作汇总账号执行结果。
use std::collections::BTreeMap;

use gateway_plugin_sdk::PluginFault;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    host::fault,
    model::{Action, LOG_PAGE_SIZE, Outcome, State},
};

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    AllSuccess,
    PartialSuccess,
    AllFailed,
    Pending,
    Conflict,
    NeedsReview,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountResult<'a> {
    account_id: &'a str,
    executed_at: Option<i64>,
    outcome: Option<Outcome>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run<'a> {
    rule_id: Option<&'a str>,
    rule_name: &'a str,
    scheduled_at: i64,
    action: Option<Action>,
    executed_at: Option<i64>,
    successful: usize,
    status: Status,
    details: Vec<AccountResult<'a>>,
}

// 旧日志没有计划 ID，以当时的名称归并。时间优先保证最近一批排在前面。
type RunKey<'a> = (i64, Option<Action>, Option<&'a str>, &'a str);

fn add<'a>(
    runs: &mut BTreeMap<RunKey<'a>, Run<'a>>,
    rule_id: Option<&'a str>,
    rule_name: &'a str,
    scheduled_at: i64,
    action: Option<Action>,
    detail: AccountResult<'a>,
) {
    let key = (
        scheduled_at,
        action,
        rule_id,
        if rule_id.is_some() { "" } else { rule_name },
    );
    let group = runs.entry(key).or_insert_with(|| Run {
        rule_id,
        rule_name,
        scheduled_at,
        action,
        executed_at: None,
        successful: 0,
        status: Status::Pending,
        details: Vec::new(),
    });
    group.executed_at = group.executed_at.max(detail.executed_at);
    group.details.push(detail);
}

impl Run<'_> {
    fn summarize(&mut self) {
        self.successful = self
            .details
            .iter()
            .filter(|entry| {
                matches!(
                    entry.outcome,
                    Some(Outcome::Applied | Outcome::Verified | Outcome::Unchanged)
                )
            })
            .count();
        self.status = if self.details.iter().any(|entry| entry.outcome.is_none()) {
            Status::Pending
        } else if self.successful == self.details.len() {
            Status::AllSuccess
        } else if self.successful > 0 {
            Status::PartialSuccess
        } else if self
            .details
            .iter()
            .all(|entry| entry.outcome == Some(Outcome::Failed))
        {
            Status::AllFailed
        } else if self
            .details
            .iter()
            .all(|entry| entry.outcome == Some(Outcome::Conflict))
        {
            Status::Conflict
        } else {
            Status::NeedsReview
        };
    }
}

pub fn page(
    state: &State,
    query: &BTreeMap<String, String>,
    now: i64,
) -> Result<Value, PluginFault> {
    if query
        .keys()
        .any(|key| !matches!(key.as_str(), "page" | "accountId" | "outcome"))
    {
        return Err(fault("日志筛选参数无效"));
    }
    let page = query
        .get("page")
        .map(String::as_str)
        .unwrap_or("1")
        .parse::<usize>()
        .ok()
        .filter(|value| (1..=1000).contains(value))
        .ok_or_else(|| fault("日志页码无效"))?;
    let account = query.get("accountId");
    let status = query
        .get("outcome")
        .map(|code| {
            serde_json::from_value::<Status>(json!(code)).map_err(|_| fault("日志结果无效"))
        })
        .transpose()?;
    let cutoff = state.cutoff(now);
    let mut runs = BTreeMap::new();
    for log in state.logs.iter().filter(|log| log.executed_at >= cutoff) {
        add(
            &mut runs,
            log.rule_id.as_deref(),
            &log.rule_name,
            log.scheduled_at,
            log.action,
            AccountResult {
                account_id: &log.account_id,
                executed_at: Some(log.executed_at),
                outcome: Some(log.outcome),
            },
        );
    }
    for (account_id, job) in &state.jobs {
        add(
            &mut runs,
            job.log_rule_id(),
            &job.rule_name,
            job.scheduled_at,
            job.action,
            AccountResult {
                account_id,
                executed_at: None,
                outcome: None,
            },
        );
    }
    let records: Vec<_> = runs
        .into_values()
        .rev()
        .filter_map(|mut run| {
            if !account.is_none_or(|id| run.details.iter().any(|entry| entry.account_id == id)) {
                return None;
            }
            run.summarize();
            status
                .is_none_or(|status| run.status == status)
                .then_some(run)
        })
        .collect();
    let total = records.len();
    let records: Vec<_> = records
        .into_iter()
        .skip((page - 1) * LOG_PAGE_SIZE)
        .take(LOG_PAGE_SIZE)
        .collect();
    Ok(
        json!({ "records": records, "total": total, "pageSize": LOG_PAGE_SIZE,
        "limited": state.dropped_at.is_some_and(|at| at >= cutoff) }),
    )
}
