//! 宿主唤醒后的到期计算、同账号合并和启停执行
use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Unbounded},
    time::Instant,
};

use gateway_plugin_sdk::{PluginFault, client::HostClient};

use crate::{
    host::{account_enabled, load, now, save, set_enabled},
    model::{Job, Outcome, State},
    schedule::Schedule,
};

fn queue_due(state: &mut State, now: i64) -> Result<bool, PluginFault> {
    let mut due = BTreeMap::<String, Job>::new();
    let mut advanced = false;
    for rule in state.rules.iter_mut().filter(|rule| rule.values.enabled) {
        let previous = rule.cursor_at;
        for (action, cron) in rule.values.schedules() {
            let schedule = Schedule::parse(cron, &rule.values.timezone)?;
            let Some(at) = schedule.latest(now).filter(|at| *at > previous) else {
                continue;
            };
            rule.cursor_at = rule.cursor_at.max(at);
            advanced = true;
            let planned = Job {
                rule_id: rule.id.clone(),
                rule_name: rule.values.name.clone(),
                scheduled_at: at,
                action: Some(action),
                started: false,
                cross_rule_conflict: false,
            };
            for id in &rule.values.account_ids {
                let job = due.entry(id.clone()).or_insert_with(|| planned.clone());
                merge_due(job, &planned);
            }
        }
    }
    for (id, planned) in due {
        let mut superseded = None;
        if let Some(job) = state.jobs.get_mut(&id) {
            if job.started && planned.scheduled_at > job.scheduled_at {
                superseded = Some(job.clone());
            }
            merge_due(job, &planned);
        } else {
            state.jobs.insert(id.clone(), planned);
        }
        if let Some(job) = superseded {
            state.record(id, job, Outcome::Uncertain, now);
        }
    }
    Ok(advanced)
}

fn merge_due(current: &mut Job, candidate: &Job) {
    if candidate.scheduled_at > current.scheduled_at {
        *current = candidate.clone();
    } else if candidate.scheduled_at == current.scheduled_at && current.action != candidate.action {
        current.action = None;
        if current.rule_id != candidate.rule_id || candidate.cross_rule_conflict {
            current.cross_rule_conflict = true;
            current.rule_name = "同一时刻的启停规则".into();
        }
    }
}

fn next_job(state: &State) -> Option<(&String, &Job)> {
    // 从上一次完成的账号之后继续，避免每分钟触发的计划长期占据队首。
    state
        .last_processed_account
        .as_deref()
        .and_then(|id| state.jobs.range::<str, _>((Excluded(id), Unbounded)).next())
        .or_else(|| state.jobs.first_key_value())
}

pub async fn reconcile(host: &HostClient, budget_ms: u64) -> Result<(), PluginFault> {
    let started = Instant::now();
    let (mut state, mut version) = load(host).await?;
    let now_at = now();
    let pruned = state.prune(now_at);
    if queue_due(&mut state, now_at)? || pruned {
        version = Some(save(host, &mut state, version).await?);
    }
    for _ in 0..12 {
        // 为本轮的宿主写入与结果持久化预留调用时间
        if started.elapsed().as_millis() + 6500 >= u128::from(budget_ms) {
            break;
        }
        let Some((id, job)) = next_job(&state).map(|(id, job)| (id.clone(), job.clone())) else {
            break;
        };
        // 本轮检查后发生的更新由下一轮重新聚合，避免旧停用覆盖新的启用
        let minute_changed = now().div_euclid(60) != now_at.div_euclid(60);
        if minute_changed {
            break;
        }
        let outcome = if let Some(action) = job.action {
            match account_enabled(host, &id).await {
                Ok(enabled) if enabled == action.enabled() => {
                    if job.started {
                        Outcome::Verified
                    } else {
                        Outcome::Unchanged
                    }
                }
                _ if job.started => Outcome::Uncertain,
                Err(_) => Outcome::Failed,
                Ok(_) => {
                    if let Some(entry) = state.jobs.get_mut(&id) {
                        entry.started = true;
                    }
                    version = Some(save(host, &mut state, version).await?);
                    match set_enabled(host, &id, action.enabled()).await {
                        Ok(()) => Outcome::Applied,
                        Err(error)
                            if error
                                .http_status
                                .is_some_and(|code| (400..500).contains(&code)) =>
                        {
                            Outcome::Failed
                        }
                        Err(_) => match account_enabled(host, &id).await {
                            Ok(enabled) if enabled == action.enabled() => Outcome::Verified,
                            _ => Outcome::Uncertain,
                        },
                    }
                }
            }
        } else {
            Outcome::Conflict
        };
        state.jobs.remove(&id);
        state.last_processed_account = Some(id.clone());
        state.record(id, job, outcome, now());
        version = Some(save(host, &mut state, version).await?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Rule, RuleInput};
    use chrono::DateTime;

    fn at(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    fn rule(id: &str, cron: &str, action: Action, cursor_at: i64) -> Rule {
        Rule {
            id: id.to_owned(),
            cursor_at,
            values: RuleInput {
                name: id.to_owned(),
                account_ids: vec!["sample-1".into()],
                enable_cron: (action == Action::Enable).then(|| cron.into()),
                disable_cron: (action == Action::Disable).then(|| cron.into()),
                timezone: "UTC".into(),
                enabled: true,
            },
        }
    }

    #[test]
    fn restart_only_queues_latest_action_per_account_once() {
        let start = at("2026-11-20T16:00:00Z");
        let after = at("2026-11-24T09:00:00Z");
        let mut state = State {
            rules: vec![
                rule("关闭", "0 22 * * 1,2", Action::Disable, start),
                rule("开启", "0 8 * * 2,3", Action::Enable, start),
            ],
            ..State::default()
        };
        assert!(queue_due(&mut state, after).unwrap());
        assert_eq!(state.jobs.len(), 1);
        let job = state.jobs.remove("sample-1").unwrap();
        assert_eq!(job.action, Some(Action::Enable));
        assert_eq!(job.scheduled_at, at("2026-11-24T08:00:00Z"));
        assert!(!queue_due(&mut state, after + 30).unwrap());
        assert!(state.jobs.is_empty());
    }

    #[test]
    fn one_plan_can_disable_and_enable_the_same_accounts() {
        let start = at("2026-11-23T21:59:00Z");
        let mut pair = rule("夜间切换", "0 22 * * 1,2", Action::Disable, start);
        pair.values.enable_cron = Some("0 8 * * 2,3".into());
        let mut state = State {
            rules: vec![pair],
            ..State::default()
        };
        assert!(queue_due(&mut state, start + 60).unwrap());
        assert_eq!(
            state.jobs.remove("sample-1").unwrap().action,
            Some(Action::Disable)
        );

        let morning = at("2026-11-24T08:00:00Z");
        assert!(queue_due(&mut state, morning).unwrap());
        assert_eq!(
            state.jobs.remove("sample-1").unwrap().action,
            Some(Action::Enable)
        );
        assert!(!queue_due(&mut state, morning + 30).unwrap());

        let start_after_downtime = at("2026-11-25T09:00:00Z");
        assert!(queue_due(&mut state, start_after_downtime).unwrap());
        assert_eq!(
            state.jobs["sample-1"].scheduled_at,
            at("2026-11-25T08:00:00Z")
        );
        assert_eq!(state.jobs["sample-1"].action, Some(Action::Enable));
    }

    #[test]
    fn overlapping_crons_inside_one_plan_conflict() {
        let start = at("2026-11-23T21:59:00Z");
        let mut pair = rule("夜间切换", "0 22 * * 1,2", Action::Disable, start);
        pair.values.enable_cron = Some("0 22 * * 1".into());
        let mut state = State {
            rules: vec![pair],
            ..State::default()
        };
        assert!(queue_due(&mut state, start + 60).unwrap());
        let job = &state.jobs["sample-1"];
        assert_eq!(job.action, None);
        assert_eq!(job.rule_name, "夜间切换");
    }

    #[test]
    fn conflicted_actions_are_logged_without_a_write_target() {
        let start = at("2026-11-23T21:59:00Z");
        let mut state = State {
            rules: vec![
                rule("关闭", "0 22 * * 1", Action::Disable, start),
                rule("开启", "0 22 * * 1", Action::Enable, start),
                rule("再次开启", "0 22 * * 1", Action::Enable, start),
            ],
            ..State::default()
        };
        assert!(queue_due(&mut state, start + 60).unwrap());
        assert_eq!(state.jobs["sample-1"].action, None);
    }

    #[test]
    fn paused_rules_and_triggers_predating_creation_are_ignored() {
        let at = at("2026-11-23T22:00:00Z");
        let mut state = State::default();
        let mut paused = rule("关闭", "0 22 * * 1", Action::Disable, at - 10);
        paused.values.enabled = false;
        state.rules = vec![paused, rule("打开", "0 22 * * 1", Action::Enable, at)];
        assert!(!queue_due(&mut state, at + 10).unwrap());
        assert!(state.jobs.is_empty());
    }

    #[test]
    fn recurring_lower_ids_do_not_starve_pending_accounts() {
        let start = at("2026-11-23T21:59:00Z");
        let mut recurring = rule("关闭", "* * * * *", Action::Disable, start);
        recurring.values.account_ids.push("sample-2".into());
        let mut state = State {
            rules: vec![recurring],
            ..State::default()
        };
        queue_due(&mut state, start + 60).unwrap();
        assert_eq!(next_job(&state).unwrap().0, "sample-1");
        state.jobs.remove("sample-1");
        state.last_processed_account = Some("sample-1".into());
        queue_due(&mut state, start + 120).unwrap();
        assert_eq!(next_job(&state).unwrap().0, "sample-2");
        state.jobs.remove("sample-2");
        state.last_processed_account = Some("sample-2".into());
        assert_eq!(next_job(&state).unwrap().0, "sample-1");
    }

    #[test]
    fn expired_logs_are_removed_by_execution_time() {
        let first = at("2026-11-21T22:00:00Z");
        let mut state = State::default();
        let job = Job {
            rule_id: "test".into(),
            rule_name: "测试".into(),
            scheduled_at: first,
            action: Some(Action::Enable),
            started: false,
            cross_rule_conflict: false,
        };
        state.retention_days = 2;
        state.record("sample-1".into(), job.clone(), Outcome::Applied, first);
        state.record("sample-2".into(), job, Outcome::Unchanged, first + 86400);
        assert!(state.prune(first + 172801));
        assert_eq!(state.logs.len(), 1);
        assert_eq!(state.logs[0].account_id, "sample-2");
    }
}
