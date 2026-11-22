//! 定时计划、待处理动作与保留期限内的执行记录
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gateway_plugin_sdk::PluginFault;
use serde::{Deserialize, Serialize};

use crate::{host::fault, schedule::Schedule};

pub const MAX_RULES: usize = 32;
pub const MAX_ACCOUNTS_PER_RULE: usize = 20;
pub const MAX_TARGETS: usize = 160;
pub const MAX_LOGS: usize = 1000;
pub const MAX_STATE_BYTES: usize = 240_000;
pub const LOG_PAGE_SIZE: usize = 30;
const MAX_JITTER_SECONDS: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Enable,
    Disable,
}

impl Action {
    pub fn enabled(self) -> bool {
        self == Self::Enable
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RuleInputWire")]
pub struct RuleInput {
    pub name: String,
    pub account_ids: Vec<String>,
    pub enable_cron: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_jitter_seconds: Option<u64>,
    pub disable_cron: Option<String>,
    pub timezone: String,
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuleInputWire {
    name: String,
    account_ids: Vec<String>,
    enable_cron: Option<String>,
    enable_jitter_seconds: Option<u64>,
    disable_cron: Option<String>,
    action: Option<Action>,
    cron: Option<String>,
    timezone: String,
    enabled: bool,
}

impl TryFrom<RuleInputWire> for RuleInput {
    type Error = &'static str;

    fn try_from(input: RuleInputWire) -> Result<Self, Self::Error> {
        let (enable_cron, disable_cron) = match (
            input.action,
            input.cron,
            input.enable_cron,
            input.disable_cron,
        ) {
            (None, None, enable, disable) => (enable, disable),
            (Some(Action::Enable), Some(cron), None, None) => (Some(cron), None),
            (Some(Action::Disable), Some(cron), None, None) => (None, Some(cron)),
            _ => return Err("计划的启停时间格式无效"),
        };
        if input
            .enable_jitter_seconds
            .is_some_and(|seconds| seconds > MAX_JITTER_SECONDS)
        {
            return Err("启用抖动须为可精确表示的非负整数秒");
        }
        let enable_jitter_seconds = input
            .enable_jitter_seconds
            .filter(|&seconds| seconds > 0 && enable_cron.is_some());
        Ok(Self {
            name: input.name,
            account_ids: input.account_ids,
            enable_cron,
            enable_jitter_seconds,
            disable_cron,
            timezone: input.timezone,
            enabled: input.enabled,
        })
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"-_.:".contains(&value))
}

impl RuleInput {
    pub fn schedules(&self) -> impl Iterator<Item = (Action, &str)> {
        [
            (Action::Enable, self.enable_cron.as_deref()),
            (Action::Disable, self.disable_cron.as_deref()),
        ]
        .into_iter()
        .filter_map(|(action, cron)| cron.map(|cron| (action, cron)))
    }

    pub fn validate(&mut self, now: i64) -> Result<(), PluginFault> {
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() || self.name.len() > 96 || self.name.chars().any(char::is_control) {
            return Err(fault("计划名称不能为空，最多 96 字节"));
        }
        if self.account_ids.is_empty()
            || self.account_ids.len() > MAX_ACCOUNTS_PER_RULE
            || self.account_ids.iter().any(|id| !valid_id(id))
            || self.account_ids.iter().collect::<BTreeSet<_>>().len() != self.account_ids.len()
        {
            return Err(fault("请选择 1 至 20 个不同的账号"));
        }
        self.timezone = self.timezone.trim().to_owned();
        if self.enable_cron.is_none() && self.disable_cron.is_none() {
            return Err(fault("请至少设置启用或停用时间"));
        }
        for cron in [&mut self.enable_cron, &mut self.disable_cron]
            .into_iter()
            .flatten()
        {
            *cron = cron.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        for (_, cron) in self.schedules() {
            let schedule = Schedule::parse(cron, &self.timezone)?;
            if schedule.upcoming(now, 1).is_empty() {
                return Err(fault("Cron 暂无可执行时间"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    pub values: RuleInput,
    pub cursor_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub rule_id: String,
    pub rule_name: String,
    pub scheduled_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execute_after: Option<i64>,
    pub action: Option<Action>,
    pub started: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cross_rule_conflict: bool,
}

impl Job {
    pub fn ready_at(&self) -> i64 {
        self.execute_after.unwrap_or(self.scheduled_at)
    }

    pub fn log_rule_id(&self) -> Option<&str> {
        (!self.cross_rule_conflict).then_some(self.rule_id.as_str())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    Unchanged,
    Verified,
    Failed,
    Uncertain,
    Conflict,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Log {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    pub rule_name: String,
    pub scheduled_at: i64,
    pub executed_at: i64,
    pub action: Option<Action>,
    pub outcome: Outcome,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub config_version: u64,
    pub retention_days: u16,
    pub rules: Vec<Rule>,
    pub(crate) jobs: BTreeMap<String, Job>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_processed_account: Option<String>,
    pub(crate) logs: VecDeque<Log>,
    pub(crate) dropped_at: Option<i64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            config_version: 0,
            retention_days: 30,
            rules: Vec::new(),
            jobs: BTreeMap::new(),
            last_processed_account: None,
            logs: VecDeque::new(),
            dropped_at: None,
        }
    }
}

impl State {
    pub fn cutoff(&self, now: i64) -> i64 {
        now.saturating_sub(i64::from(self.retention_days) * 86400)
    }

    pub fn prune(&mut self, now: i64) -> bool {
        let cutoff = self.cutoff(now);
        let before = self.logs.len();
        self.logs.retain(|log| log.executed_at >= cutoff);
        let expired_notice = self.dropped_at.is_some_and(|at| at < cutoff);
        if expired_notice {
            self.dropped_at = None;
        }
        before != self.logs.len() || expired_notice
    }

    pub fn forget_oldest_log(&mut self) -> bool {
        let Some(log) = self.logs.pop_back() else {
            return false;
        };
        self.dropped_at = Some(self.dropped_at.unwrap_or(i64::MIN).max(log.executed_at));
        true
    }

    pub fn record(&mut self, id: String, job: Job, outcome: Outcome, now: i64) {
        self.logs.push_front(Log {
            account_id: id,
            rule_id: job.log_rule_id().map(str::to_owned),
            rule_name: job.rule_name,
            scheduled_at: job.scheduled_at,
            executed_at: now,
            action: job.action,
            outcome,
        });
        while self.logs.len() > MAX_LOGS {
            self.forget_oldest_log();
        }
    }
}
