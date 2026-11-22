//! 管理页路由、计划配置与日志分页
use std::collections::{BTreeMap, BTreeSet};

use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::management::*,
    client::{TypedCall, TypedReply},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    conflicts,
    host::{self, fault, load, now, save},
    model::{LOG_PAGE_SIZE, MAX_RULES, MAX_TARGETS, Outcome, Rule, RuleInput, State},
    run_logs,
    schedule::Schedule,
};

pub fn registration() -> ManagementRegistration {
    let route = |method: &str, path: &str| ManagementRoute {
        method: method.into(),
        path: path.into(),
        request_content_types: if method == "POST" {
            vec!["application/json".into()]
        } else {
            vec![]
        },
        response_content_types: vec!["application/json".into()],
    };
    ManagementRegistration {
        routes: vec![
            route("GET", "api/accounts"),
            route("GET", "api/dashboard"),
            route("GET", "api/logs"),
            route("GET", "api/runs"),
            route("POST", "api/preview"),
            route("POST", "api/save-rule"),
            route("POST", "api/delete-rule"),
            route("POST", "api/retention"),
        ],
        resources: ["web/index.html", "web/app.js", "web/app.css"]
            .into_iter()
            .map(|path| ManagementResource {
                path: path.into(),
                public: false,
            })
            .collect(),
        pages: vec![ManagementPage {
            id: "account-scheduler".into(),
            title: "账号定时启停".into(),
            description: Some("按计划启停网关账号，查看执行结果".into()),
            entry: "web/index.html".into(),
            icon: None,
        }],
        callbacks: vec![],
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveRule {
    id: Option<String>,
    expected_version: u64,
    rule: RuleInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteRule {
    id: String,
    expected_version: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Retention {
    days: u16,
    expected_version: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    cron: String,
    timezone: String,
}

fn parse<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T, PluginFault> {
    serde_json::from_slice(data).map_err(|_| fault("请求数据无效"))
}

fn check_version(state: &State, expected: u64) -> Result<(), PluginFault> {
    if state.config_version != expected {
        return Err(PluginFault::new(
            ErrorCode::Conflict,
            "计划设置已变化，请重新读取后保存",
        ));
    }
    Ok(())
}

fn dashboard(state: &State, now: i64) -> Value {
    let rules: Vec<_> = state
        .rules
        .iter()
        .map(|rule| {
            let (mut next_at, mut next_action) = (None, None);
            if rule.values.enabled {
                for (action, cron) in rule.values.schedules() {
                    let at = Schedule::parse(cron, &rule.values.timezone)
                        .ok()
                        .and_then(|schedule| schedule.upcoming(now.max(rule.cursor_at), 1).first().copied());
                    let Some(at) = at else { continue };
                    if next_at.is_none_or(|next| at < next) {
                        (next_at, next_action) = (Some(at), Some(action));
                    } else if next_at == Some(at) {
                        next_action = None;
                    }
                }
            }
            json!({ "id": rule.id, "rule": rule.values, "nextAt": next_at, "nextAction": next_action })
        })
        .collect();
    json!({
        "rules": rules,
        "version": state.config_version,
        "retentionDays": state.retention_days,
        "queued": state.jobs.len(),
    })
}

fn logging(
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
    let id = query.get("accountId").map(String::as_str);
    let outcome = query
        .get("outcome")
        .map(|code| {
            serde_json::from_value::<Outcome>(json!(code)).map_err(|_| fault("日志结果无效"))
        })
        .transpose()?;
    let matches: Vec<_> = state
        .logs
        .iter()
        .filter(|log| log.executed_at >= state.cutoff(now))
        .filter(|log| id.is_none_or(|id| log.account_id == id))
        .filter(|log| outcome.is_none_or(|outcome| log.outcome == outcome))
        .collect();
    let total = matches.len();
    let records: Vec<_> = matches
        .into_iter()
        .skip((page - 1) * LOG_PAGE_SIZE)
        .take(LOG_PAGE_SIZE)
        .collect();
    Ok(json!({
        "records": records, "total": total, "pageSize": LOG_PAGE_SIZE,
        "limited": state.dropped_at.is_some_and(|at| at >= state.cutoff(now)),
    }))
}

async fn inner(call: TypedCall<ManagementRequest>) -> Result<Value, PluginFault> {
    let host = &call.host;
    let path = call.request.path.as_str();
    let time = now();
    if call.request.method == "GET" {
        if !call.payload.is_empty() || call.request.query.len() > 1024 {
            return Err(fault("查询参数无效"));
        }
        let query: BTreeMap<String, String> =
            url::form_urlencoded::parse(call.request.query.trim_start_matches('?').as_bytes())
                .into_owned()
                .collect();
        return match path {
            "api/accounts"
                if query.is_empty() || (query.len() == 1 && query.contains_key("cursor")) =>
            {
                host::accounts(host, query.get("cursor").cloned()).await
            }
            "api/dashboard" if query.is_empty() => Ok(dashboard(&load(host).await?.0, time)),
            "api/logs" => logging(&load(host).await?.0, &query, time),
            "api/runs" => run_logs::page(&load(host).await?.0, &query, time),
            _ => Err(fault("查询路径或参数无效")),
        };
    }
    if call.request.method != "POST" || call.payload.len() > 8192 {
        return Err(fault("请求格式无效"));
    }
    if path == "api/preview" {
        let request: Preview = parse(&call.payload)?;
        let schedule = Schedule::parse(&request.cron, request.timezone.trim())?;
        let times = schedule.upcoming(time, 5);
        return if times.is_empty() {
            Err(fault("Cron 暂无可执行时间"))
        } else {
            Ok(json!({ "times": times }))
        };
    }
    let (mut state, version) = load(host).await?;
    match path {
        "api/save-rule" => {
            let mut request: SaveRule = parse(&call.payload)?;
            check_version(&state, request.expected_version)?;
            request.rule.validate(time)?;
            conflicts::check(
                host,
                &request.rule,
                request.id.as_deref(),
                &state.rules,
                time,
            )
            .await?;
            request.rule.account_ids.sort_unstable();
            if let Some(id) = request.id {
                let rule = state
                    .rules
                    .iter_mut()
                    .find(|rule| rule.id == id)
                    .ok_or_else(|| fault("计划已不存在"))?;
                let before = &rule.values;
                let changed = before.account_ids != request.rule.account_ids
                    || before.enable_cron != request.rule.enable_cron
                    || before.enable_jitter_seconds != request.rule.enable_jitter_seconds
                    || before.disable_cron != request.rule.disable_cron
                    || before.timezone != request.rule.timezone
                    || before.enabled != request.rule.enabled;
                rule.values = request.rule;
                if changed {
                    rule.cursor_at = time;
                    state.jobs.retain(|_, job| job.rule_id != id || job.started);
                }
            } else {
                if state.rules.len() >= MAX_RULES {
                    return Err(fault("每个插件实例最多保存 32 条计划"));
                }
                state.rules.push(Rule {
                    id: uuid::Uuid::new_v4().to_string(),
                    values: request.rule,
                    cursor_at: time,
                });
            }
            let unique: BTreeSet<_> = state
                .rules
                .iter()
                .flat_map(|rule| &rule.values.account_ids)
                .collect();
            if unique.len() > MAX_TARGETS {
                return Err(fault("全部计划合计最多选择 160 个不同的账号"));
            }
        }
        "api/delete-rule" => {
            let request: DeleteRule = parse(&call.payload)?;
            check_version(&state, request.expected_version)?;
            let index = state
                .rules
                .iter()
                .position(|rule| rule.id == request.id)
                .ok_or_else(|| fault("计划已不存在"))?;
            state.rules.remove(index);
            state
                .jobs
                .retain(|_, job| job.rule_id != request.id || job.started);
        }
        "api/retention" => {
            let request: Retention = parse(&call.payload)?;
            check_version(&state, request.expected_version)?;
            if !(1..=365).contains(&request.days) {
                return Err(fault("日志保留天数须在 1 至 365 天之间"));
            }
            state.retention_days = request.days;
        }
        _ => return Err(fault("请求路径无效")),
    }
    state.config_version += 1;
    save(host, &mut state, version).await?;
    Ok(dashboard(&state, time))
}

pub async fn handle(
    call: TypedCall<ManagementRequest>,
) -> Result<TypedReply<ManagementResponse>, PluginFault> {
    let (status, data) = match inner(call).await {
        Ok(value) => (200, value),
        Err(error) => (
            if error.code == ErrorCode::Conflict {
                409
            } else {
                400
            },
            json!({ "error": { "message": error.message } }),
        ),
    };
    Ok(TypedReply::new(ManagementResponse {
        status,
        content_type: "application/json".into(),
        headers: vec![],
    })
    .with_payload(serde_json::to_vec(&data).map_err(|_| fault("响应编码失败"))?))
}
