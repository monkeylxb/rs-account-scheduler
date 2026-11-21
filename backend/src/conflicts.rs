//! 保存前检查同一账号是否会同时触发相反动作。
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use gateway_plugin_sdk::{ErrorCode, PluginFault, client::HostClient};

use crate::{
    host::account_name,
    model::{Rule, RuleInput},
    schedule::Schedule,
};

const CHECK_DAYS: i64 = 366;

struct Conflict<'a> {
    time: i64,
    other_plan: Option<&'a str>,
    accounts: Vec<&'a str>,
}

pub async fn check(
    host: &HostClient,
    draft: &RuleInput,
    editing: Option<&str>,
    rules: &[Rule],
    now: i64,
) -> Result<(), PluginFault> {
    let Some(conflict) = find(draft, editing, rules, now)? else {
        return Ok(());
    };
    let first = conflict.accounts[0];
    let account = account_name(host, first)
        .await
        .unwrap_or_else(|_| first.to_owned());
    let accounts = if conflict.accounts.len() == 1 {
        format!("账号「{account}」")
    } else {
        format!("账号「{account}」等 {} 个账号", conflict.accounts.len())
    };
    let time = DateTime::<Utc>::from_timestamp(conflict.time, 0)
        .zip(draft.timezone.parse::<Tz>().ok())
        .map(|(time, zone)| {
            time.with_timezone(&zone)
                .format("%Y/%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| conflict.time.to_string());
    let subject = conflict.other_plan.map_or_else(
        || "本计划的启用与停用时间冲突".to_owned(),
        |name| format!("与计划「{name}」冲突"),
    );
    Err(PluginFault::new(
        ErrorCode::Conflict,
        format!(
            "{subject}。{accounts}将在 {time}（{}）同时收到启用和停用动作。请调整执行时间、账号或计划状态。",
            draft.timezone
        ),
    ))
}

fn find<'a>(
    draft: &'a RuleInput,
    editing: Option<&str>,
    rules: &'a [Rule],
    now: i64,
) -> Result<Option<Conflict<'a>>, PluginFault> {
    if !draft.enabled {
        return Ok(None);
    }
    let through = now.saturating_add(CHECK_DAYS * 86_400);
    let parse = |expression: &str| Schedule::parse(expression, &draft.timezone);
    let enable = draft.enable_cron.as_deref().map(parse).transpose()?;
    let disable = draft.disable_cron.as_deref().map(parse).transpose()?;
    let mut found = None;
    let own_deadline = if draft.enable_cron == draft.disable_cron {
        i64::MAX
    } else {
        through
    };
    if let (Some(enable), Some(disable)) = (&enable, &disable)
        && let Some(time) = enable.first_overlap(disable, now, own_deadline)
    {
        found = Some(Conflict {
            time,
            other_plan: None,
            accounts: draft.account_ids.iter().map(String::as_str).collect(),
        });
    }
    for rule in rules
        .iter()
        .filter(|rule| rule.values.enabled && editing != Some(rule.id.as_str()))
    {
        let accounts: Vec<_> = draft
            .account_ids
            .iter()
            .filter(|id| rule.values.account_ids.contains(id))
            .map(String::as_str)
            .collect();
        if accounts.is_empty() {
            continue;
        }
        for (ours, theirs) in [
            (&enable, &rule.values.disable_cron),
            (&disable, &rule.values.enable_cron),
        ] {
            let (Some(ours), Some(theirs)) = (ours, theirs) else {
                continue;
            };
            let deadline = found
                .as_ref()
                .map_or(through, |conflict| through.min(conflict.time - 1));
            let theirs = Schedule::parse(theirs, &rule.values.timezone)?;
            if let Some(time) = ours.first_overlap(&theirs, now, deadline) {
                found = Some(Conflict {
                    time,
                    other_plan: Some(&rule.values.name),
                    accounts: accounts.clone(),
                });
            }
        }
    }
    Ok(found)
}
