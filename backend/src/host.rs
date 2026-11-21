//! 原生宿主查询、受管管理请求与私有状态的版本校验

use crate::model::{MAX_STATE_BYTES, State};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        host::{
            AUTH_GET_RUNTIME, AuthGetRequest, AuthRuntimeAccount, STATE_GET, STATE_PUT,
            StateGetRequest, StateGetResult, StatePutRequest, StatePutResult,
        },
        middleware::{MiddlewareHeader, http::Version},
    },
    client::{HostClient, HttpBody, HttpFrame, HttpRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const STATE_NAMESPACE: &str = "account-scheduler";
const STATE_KEY: &str = "schedule";

pub fn fault(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::Rejected, message)
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub async fn load(host: &HostClient) -> Result<(State, Option<u64>), PluginFault> {
    let request = StateGetRequest {
        namespace: STATE_NAMESPACE.into(),
        key: STATE_KEY.into(),
    };
    let reply = host
        .call(
            STATE_GET,
            serde_json::to_value(request).map_err(|_| fault("插件状态查询参数无效"))?,
            vec![],
        )
        .await
        .map_err(|e| e.into_plugin_fault())?;
    let result: StateGetResult =
        serde_json::from_value(reply.result).map_err(|_| fault("插件状态无法读取"))?;
    match result.record {
        Some(record) => Ok((
            serde_json::from_value(record.value).map_err(|_| fault("插件状态格式不匹配"))?,
            Some(record.version),
        )),
        None => Ok((State::default(), None)),
    }
}

pub async fn save(
    host: &HostClient,
    state: &mut State,
    version: Option<u64>,
) -> Result<u64, PluginFault> {
    state.prune(now());
    let mut bytes = serde_json::to_vec(state).map_err(|_| fault("插件状态编码失败"))?;
    while bytes.len() > MAX_STATE_BYTES && state.forget_oldest_log() {
        bytes = serde_json::to_vec(state).map_err(|_| fault("插件状态编码失败"))?;
    }
    if bytes.len() > MAX_STATE_BYTES {
        return Err(fault("插件状态已达到容量上限"));
    }
    let request = StatePutRequest {
        namespace: STATE_NAMESPACE.into(),
        key: STATE_KEY.into(),
        value: serde_json::from_slice(&bytes).map_err(|_| fault("插件状态编码失败"))?,
        expected_version: version,
    };
    let reply = host
        .call(
            STATE_PUT,
            serde_json::to_value(request).map_err(|_| fault("插件状态保存参数无效"))?,
            vec![],
        )
        .await
        .map_err(|e| e.into_plugin_fault())?;
    let result: StatePutResult =
        serde_json::from_value(reply.result).map_err(|_| fault("插件状态保存结果无效"))?;
    Ok(result.version)
}

async fn admin(
    host: &HostClient,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, PluginFault> {
    let (headers, body) = match body {
        Some(value) => (
            vec![MiddlewareHeader {
                name: "content-type".into(),
                value: b"application/json".to_vec(),
            }],
            HttpBody::from_bytes(serde_json::to_vec(&value).map_err(|_| fault("请求编码失败"))?),
        ),
        None => (vec![], HttpBody::empty()),
    };
    let mut reply = host
        .dispatch_http(HttpRequest {
            settings: Value::Null,
            method: method.into(),
            uri: path.into(),
            version: Version::Http11,
            headers,
            timeout_ms: Some(4000),
            body,
        })
        .await?;
    let mut bytes = Vec::new();
    while let Some(frame) = reply.body.read().await? {
        if let HttpFrame::Data(chunk) = frame {
            if bytes.len() + chunk.len() > 2097152 {
                reply.body.close().await?;
                return Err(fault("宿主响应过大"));
            }
            bytes.extend(chunk);
        }
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| fault("宿主响应无效"))?;
    if !(200..300).contains(&reply.status) || value.get("code").and_then(Value::as_u64) != Some(200)
    {
        let mut error = fault("宿主未确认管理请求");
        error.http_status = Some(reply.status);
        return Err(error);
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| fault("宿主响应缺少数据"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    id: String,
    name: String,
    provider: String,
    enabled: bool,
    group_names: Vec<String>,
}

pub async fn accounts(host: &HostClient, cursor: Option<String>) -> Result<Value, PluginFault> {
    #[derive(Deserialize)]
    struct AccountPage {
        items: Vec<AccountView>,
        page: Page,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Page {
        total_pages: u32,
    }
    #[derive(Deserialize)]
    struct AccountView {
        id: String,
        name: String,
        provider: String,
        enabled: bool,
        groups: Vec<Group>,
    }
    #[derive(Deserialize)]
    struct Group {
        name: String,
    }
    let page = cursor
        .as_deref()
        .unwrap_or("1")
        .parse::<u32>()
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| fault("账号列表页码无效"))?;
    let path = format!("/api/admin/accounts?page={page}&pageSize=50");
    let response: AccountPage = serde_json::from_value(admin(host, "GET", &path, None).await?)
        .map_err(|_| fault("无法读取宿主账号列表"))?;
    let accounts: Vec<_> = response
        .items
        .into_iter()
        .map(|account| Account {
            id: account.id,
            name: account.name,
            provider: account.provider,
            enabled: account.enabled,
            group_names: account.groups.into_iter().map(|group| group.name).collect(),
        })
        .collect();
    let next = (page < response.page.total_pages).then(|| (page + 1).to_string());
    Ok(json!({ "accounts": accounts, "nextCursor": next }))
}

async fn runtime_account(host: &HostClient, id: &str) -> Result<AuthRuntimeAccount, PluginFault> {
    let request = AuthGetRequest {
        account_id: id.into(),
    };
    let reply = host
        .call(
            AUTH_GET_RUNTIME,
            json!({}),
            serde_json::to_vec(&request).map_err(|_| fault("账号参数无效"))?,
        )
        .await
        .map_err(|e| e.into_plugin_fault())?;
    let account: AuthRuntimeAccount =
        serde_json::from_slice(&reply.payload).map_err(|_| fault("宿主返回了无效账号状态"))?;
    if account.account_id != id {
        return Err(fault("宿主返回的账号与请求不一致"));
    }
    Ok(account)
}

pub async fn account_enabled(host: &HostClient, id: &str) -> Result<bool, PluginFault> {
    Ok(runtime_account(host, id).await?.enabled)
}

pub async fn account_name(host: &HostClient, id: &str) -> Result<String, PluginFault> {
    Ok(runtime_account(host, id).await?.name)
}

pub async fn set_enabled(host: &HostClient, id: &str, enabled: bool) -> Result<(), PluginFault> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct UpdatedAccounts {
        account_ids: Vec<String>,
    }
    let data = admin(
        host,
        "POST",
        "/api/admin/accounts/batch-update",
        Some(json!({ "accountIds": [id], "enabled": enabled })),
    )
    .await?;
    let result: UpdatedAccounts =
        serde_json::from_value(data).map_err(|_| fault("无法确认宿主账号更新结果"))?;
    if result.account_ids != [id] {
        return Err(fault("宿主未确认所选账号的更新"));
    }
    Ok(())
}
