import type { Account, Dashboard, LogPage, RuleInput } from './types'

async function request<T>(path: string, input?: unknown, query?: URLSearchParams): Promise<T> {
  const host = window.codexProxyPlugin
  if (host?.version !== 2 || typeof host.request !== 'function') throw new Error('网关宿主桥不可用')
  const response = await host.request({
    method: input === undefined ? 'GET' : 'POST',
    path,
    ...(query?.size ? { query: query.toString() } : {}),
    ...(input === undefined ? {} : { contentType: 'application/json', body: JSON.stringify(input) }),
  })
  if (response.contentType.split(';', 1)[0]?.trim().toLowerCase() !== 'application/json')
    throw new Error('插件返回了不支持的内容类型')
  let value: unknown
  try { value = JSON.parse(new TextDecoder().decode(response.body)) }
  catch { throw new Error('插件返回了无效数据') }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('插件返回的数据格式无效')
  if (response.status < 200 || response.status >= 300) {
    const error = 'error' in value ? value.error : null
    throw new Error(error && typeof error === 'object' && 'message' in error && typeof error.message === 'string'
      ? error.message : `插件请求失败（HTTP ${response.status}）`)
  }
  return value as T
}

export const api = {
  accounts: (cursor: string | null) => request<{ accounts: Account[]; nextCursor: string | null }>(
    'api/accounts', undefined, cursor ? new URLSearchParams({ cursor }) : undefined,
  ),
  dashboard: () => request<Dashboard>('api/dashboard'),
  logs: (page: number, accountId: string, outcome: string) => {
    const query = new URLSearchParams({ page: String(page) })
    if (accountId) query.set('accountId', accountId)
    if (outcome) query.set('outcome', outcome)
    return request<LogPage>('api/runs', undefined, query)
  },
  preview: (cron: string, timezone: string) => request<{ times: number[] }>('api/preview', { cron, timezone }),
  saveRule: (id: string | null, rule: RuleInput, expectedVersion: number) =>
    request<Dashboard>('api/save-rule', { id, rule, expectedVersion }),
  deleteRule: (id: string, expectedVersion: number) => request<Dashboard>('api/delete-rule', { id, expectedVersion }),
  retention: (days: number, expectedVersion: number) => request<Dashboard>('api/retention', { days, expectedVersion }),
}
