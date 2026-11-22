//! 宿主唤醒后的到期计算、同账号合并和启停执行
use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Unbounded},
    time::{Duration, Instant, SystemTime},
};

use gateway_plugin_sdk::{PluginFault, client::HostClient};
use tokio::sync::Mutex;

use crate::{
    host::{account_enabled, load, now, save, set_enabled},
    model::{Action, Job, Outcome, State},
    schedule::Schedule,
};

const WAKE_INTERVAL: Duration = Duration::from_secs(30);
const RESULT_RESERVE: Duration = Duration::from_millis(6500);

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
                execute_after: None,
                action: Some(action),
                started: false,
                cross_rule_conflict: false,
            };
            let jitter = rule
                .values
                .enable_jitter_seconds
                .filter(|_| action == Action::Enable);
            for id in &rule.values.account_ids {
                let mut planned = planned.clone();
                if let Some(seconds) = jitter {
                    // 从本轮发现到期计划时开始分散，停机补执行也沿用同一范围
                    let offset = uuid::Uuid::new_v4().as_u128() % (u128::from(seconds) + 1);
                    planned.execute_after = Some(now.saturating_add(offset as i64));
                }
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
        current.execute_after = None;
        if current.rule_id != candidate.rule_id || candidate.cross_rule_conflict {
            current.cross_rule_conflict = true;
            current.rule_name = "同一时刻的启停规则".into();
        }
    }
}

fn next_job(state: &State, now: i64) -> Option<(&String, &Job)> {
    // 从上一次完成的账号之后继续，避免每分钟触发的计划长期占据队首。
    let ready = |(_, job): &(&String, &Job)| job.ready_at() <= now;
    state
        .last_processed_account
        .as_deref()
        .and_then(|id| {
            state
                .jobs
                .range::<str, _>((Excluded(id), Unbounded))
                .find(ready)
        })
        .or_else(|| state.jobs.iter().find(ready))
}

async fn apply_job(
    host: &HostClient,
    state: &mut State,
    version: &mut Option<u64>,
    id: String,
    job: Job,
) -> Result<(), PluginFault> {
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
                *version = Some(save(host, state, *version).await?);
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
    *version = Some(save(host, state, *version).await?);
    Ok(())
}

pub async fn reconcile(
    host: &HostClient,
    budget_ms: u64,
    lock: &Mutex<()>,
) -> Result<(), PluginFault> {
    let started = Instant::now();
    let budget = Duration::from_millis(budget_ms);
    let mut remaining = 12;
    loop {
        let Ok(guard) = lock.try_lock() else {
            return Ok(());
        };
        let (mut state, mut version) = load(host).await?;
        let observed_at = now();
        let minute = observed_at.div_euclid(60);
        let pruned = state.prune(observed_at);
        if queue_due(&mut state, observed_at)? || pruned {
            version = Some(save(host, &mut state, version).await?);
        }
        while remaining > 0 && started.elapsed() + RESULT_RESERVE < budget {
            let current = now();
            // 跨分钟后先重读计划，较新的停用可以覆盖仍在等待的启用
            if current.div_euclid(60) != minute {
                break;
            }
            let Some((id, job)) =
                next_job(&state, current).map(|(id, job)| (id.clone(), job.clone()))
            else {
                break;
            };
            apply_job(host, &mut state, &mut version, id, job).await?;
            remaining -= 1;
        }
        if remaining == 0 || started.elapsed() + RESULT_RESERVE >= budget {
            break;
        }
        let Some(earliest) = state.jobs.values().map(Job::ready_at).min() else {
            break;
        };
        drop(guard);
        let elapsed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        if elapsed.as_secs() as i64 / 60 != minute {
            continue;
        }
        let to_earliest = Duration::from_secs(earliest.max(0) as u64).saturating_sub(elapsed);
        if to_earliest > WAKE_INTERVAL || started.elapsed() + to_earliest + RESULT_RESERVE >= budget
        {
            break;
        }
        let to_minute = Duration::from_secs(((minute + 1) * 60) as u64).saturating_sub(elapsed);
        // 较远任务等宿主下一次唤醒，近处任务用定时器等候并让出管理锁
        tokio::time::sleep(to_earliest.min(to_minute)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Rule, RuleInput};
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
                enable_jitter_seconds: None,
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
    fn jitter_deadlines_are_saved_per_account_from_detection_time() {
        let start = at("2026-11-23T21:59:00Z");
        let observed_at = start + 77;
        let mut enable = rule("开启", "0 22 * * 1", Action::Enable, start);
        enable.values.account_ids.push("sample-2".into());
        enable.values.enable_jitter_seconds = Some(45);
        let mut state = State {
            rules: vec![enable],
            ..State::default()
        };

        assert!(queue_due(&mut state, observed_at).unwrap());
        let encoded = serde_json::to_value(&state).unwrap();
        let mut restored: State = serde_json::from_value(encoded.clone()).unwrap();
        assert!(!queue_due(&mut restored, observed_at + 1).unwrap());
        assert_eq!(restored.jobs.len(), 2);
        for id in ["sample-1", "sample-2"] {
            let job = &restored.jobs[id];
            assert_eq!(job.scheduled_at, start + 60);
            assert!((observed_at..=observed_at + 45).contains(&job.ready_at()));
            assert_eq!(encoded["jobs"][id]["executeAfter"], job.ready_at());
        }
    }

    #[test]
    fn ready_jobs_bypass_waiting_enables_and_newer_disables_replace_them() {
        let start = at("2026-11-23T21:59:00Z");
        let mut pair = rule("启停", "1 22 * * 1", Action::Disable, start);
        pair.values.enable_cron = Some("0 22 * * 1".into());
        pair.values.enable_jitter_seconds = Some(180);
        let mut other = rule("其他账号停用", "0 22 * * 1", Action::Disable, start);
        other.values.account_ids = vec!["sample-2".into()];
        let mut state = State {
            rules: vec![pair, other],
            ..State::default()
        };

        assert!(queue_due(&mut state, start + 60).unwrap());
        state.jobs.get_mut("sample-1").unwrap().execute_after = Some(start + 180);
        let (id, ready) = next_job(&state, start + 60).unwrap();
        assert_eq!(id, "sample-2");
        assert_eq!(ready.execute_after, None);
        state.jobs.remove("sample-2");
        assert!(next_job(&state, start + 119).is_none());

        assert!(queue_due(&mut state, start + 120).unwrap());
        let (_, ready) = next_job(&state, start + 120).unwrap();
        assert_eq!(ready.action, Some(Action::Disable));
        assert_eq!(ready.execute_after, None);
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
        assert_eq!(next_job(&state, start + 60).unwrap().0, "sample-1");
        state.jobs.remove("sample-1");
        state.last_processed_account = Some("sample-1".into());
        queue_due(&mut state, start + 120).unwrap();
        assert_eq!(next_job(&state, start + 120).unwrap().0, "sample-2");
        state.jobs.remove("sample-2");
        state.last_processed_account = Some("sample-2".into());
        assert_eq!(next_job(&state, start + 120).unwrap().0, "sample-1");
    }

    #[test]
    fn expired_logs_are_removed_by_execution_time() {
        let first = at("2026-11-21T22:00:00Z");
        let mut state = State::default();
        let job = Job {
            rule_id: "test".into(),
            rule_name: "测试".into(),
            scheduled_at: first,
            execute_after: None,
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
