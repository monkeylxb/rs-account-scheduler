import { applyResolvedTheme, resolveTheme, DEFAULT_THEME_COLOR, DEFAULT_CUSTOM_THEME_COLOR } from '@codex-proxy/ui/theme'
import { CronDate, CronExpressionParser } from 'cron-parser'
import { dateTime } from './format'
import type { Account, BridgeResponse, Dashboard, Outcome, Rule, RuleInput, Run } from './types'

export function installPreview() {
  if (window.codexProxyPlugin) return false
  const theme = new URLSearchParams(location.search).get('theme') === 'dark' ? 'dark' : 'light'
  applyResolvedTheme(document.documentElement, resolveTheme(theme, DEFAULT_THEME_COLOR, DEFAULT_CUSTOM_THEME_COLOR))
  const timezone = 'Asia/Shanghai'
  const groups = ['生产组', '测试组', '夜间作业组', '备用组', '共享资源组']
  const accounts: Account[] = [
    { id: 'sample-01', name: '工作账号 · 主账号', provider: 'openai', enabled: true, groupNames: ['生产组', '共享资源组'] },
    { id: 'sample-02', name: '工作账号 · 备用账号', provider: 'openai', enabled: true, groupNames: ['生产组', '备用组'] },
    { id: 'sample-03', name: '夜间批处理账号', provider: 'openai', enabled: false, groupNames: ['夜间作业组'] },
    { id: 'sample-04', name: '周末备用账号', provider: 'openai', enabled: false, groupNames: ['备用组'] },
    { id: 'sample-05', name: '共享服务账号', provider: 'xai', enabled: true, groupNames: ['共享资源组'] },
    ...Array.from({ length: 40 }, (_, index): Account => {
      const number = String(index + 1).padStart(2, '0')
      return {
        id: `sample-node-${number}`,
        name: `节点账号 ${number}`,
        provider: index % 4 === 0 ? 'xai' : 'openai',
        enabled: index % 5 !== 4,
        groupNames: index % 11 === 10 ? [] : [groups[index % groups.length]!],
      }
    }),
  ]
  const rules: Rule[] = [
    { id: 'rule-01', rule: { name: '工作账号定时启停', accountIds: ['sample-01', 'sample-02'], enableCron: '0 8 * * 2,3', enableJitterSeconds: 30, disableCron: '0 22 * * 1,2', timezone, enabled: true }, nextAt: null, nextAction: null },
    { id: 'rule-02', rule: { name: '备用账号周末停用', accountIds: ['sample-04'], enableCron: null, disableCron: '0 23 * * 0,6', timezone, enabled: false }, nextAt: null, nextAction: null },
  ]
  const data: Dashboard = { rules, version: 0, retentionDays: 30, queued: 0 }
  const reference = Math.floor(Date.now() / 60000) * 60
  const recordedRun = (ruleName: string, age: number, action: Run['action'], status: Run['status'], outcomes: (Outcome | null)[]): Run => {
    const scheduledAt = reference - age
    const details = outcomes.map((outcome, index) => ({
      accountId: accounts[index]!.id, executedAt: outcome === null ? null : scheduledAt + 12 + index * 2, outcome,
    }))
    const executed = details.flatMap(entry => entry.executedAt === null ? [] : [entry.executedAt])
    const successful = details.filter(entry => entry.outcome === 'applied' || entry.outcome === 'unchanged' || entry.outcome === 'verified').length
    return { ruleId: null, ruleName, scheduledAt, action, executedAt: executed.length ? Math.max(...executed) : null, successful, status, details }
  }
  const runs: Run[] = [
    recordedRun('工作账号定时启停', 3600, 'enable', 'all_success', ['applied', 'unchanged', 'verified']),
    recordedRun('夜间批处理账号停用', 7200, 'disable', 'partial_success', ['applied', 'failed', 'uncertain', 'unchanged', 'applied']),
    recordedRun('共享账号定时启用', 14400, 'enable', 'all_failed', ['failed', 'failed']),
    recordedRun('同一时刻的启停规则', 28800, null, 'conflict', ['conflict', 'conflict']),
    recordedRun('备用账号周末停用', 43200, 'disable', 'needs_review', ['uncertain']),
    recordedRun('工作账号定时启停', 90000, 'disable', 'all_success', ['applied', 'applied', 'applied']),
  ]
  const reply = (value: unknown, status = 200): BridgeResponse => ({
    status, contentType: 'application/json', body: new TextEncoder().encode(JSON.stringify(value)).buffer as ArrayBuffer,
  })
  const error = (message: string, status = 400) => reply({ error: { message } }, status)
  const upcoming = (expression: string, tz: string, count: number) => {
    if (expression.split(/\s+/).length !== 5) throw new Error('Cron 必须为五段格式')
    const next = CronExpressionParser.parse(expression, { tz })
    return Array.from({ length: count }, () => next.next().getTime() / 1000)
  }
  function conflictMessage(rule: RuleInput, editing: string | null) {
    if (!rule.enabled) return null
    const now = Date.now()
    const horizon = now + 366 * 86400 * 1000
    const firstOverlap = (left: string, right: string, rightTimezone: string, deadline: number) => {
      const currentDate = new Date(now)
      const start = CronExpressionParser.parse(left, { tz: rule.timezone, currentDate })
      const end = CronExpressionParser.parse(right, { tz: rightTimezone, currentDate })
      const interval = (fields: typeof start.fields) => fields.minute.values.length > 1 || fields.hour.values.length > 1
      if (rule.timezone === rightTimezone && interval(start.fields) && interval(end.fields)
        && (!start.fields.minute.values.some(value => end.fields.minute.values.includes(value))
          || !start.fields.hour.values.some(value => end.fields.hour.values.includes(value)))) return null
      let a = start.next().getTime(), b = end.next().getTime()
      while (a <= deadline && b <= deadline) {
        if (a === b) return a
        if (a < b) {
          start.reset(new CronDate(b - 1000, rule.timezone))
          a = start.next().getTime()
        } else {
          end.reset(new CronDate(a - 1000, rightTimezone))
          b = end.next().getTime()
        }
      }
      return null
    }
    let first: { at: number; plan: string | null; accountIds: string[] } | null = null
    if (rule.enableCron && rule.disableCron) {
      const deadline = rule.enableCron === rule.disableCron ? Infinity : horizon
      const at = firstOverlap(rule.enableCron, rule.disableCron, rule.timezone, deadline)
      if (at !== null) first = { at, plan: null, accountIds: rule.accountIds }
    }
    for (const other of rules) {
      if (other.id === editing || !other.rule.enabled) continue
      const accountIds = rule.accountIds.filter(id => other.rule.accountIds.includes(id))
      if (!accountIds.length) continue
      for (const [ours, theirs] of [[rule.enableCron, other.rule.disableCron], [rule.disableCron, other.rule.enableCron]]) {
        if (!ours || !theirs) continue
        const at = firstOverlap(ours, theirs, other.rule.timezone, Math.min(horizon, first ? first.at - 1 : horizon))
        if (at !== null) first = { at, plan: other.rule.name, accountIds }
      }
    }
    if (!first) return null
    const account = accounts.find(row => row.id === first.accountIds[0])?.name ?? first.accountIds[0]
    const label = first.accountIds.length === 1 ? `账号「${account}」` : `账号「${account}」等 ${first.accountIds.length} 个账号`
    const subject = first.plan === null ? '本计划的启用与停用时间冲突' : `与计划「${first.plan}」冲突`
    return `${subject}。${label}将在 ${dateTime(first.at / 1000, rule.timezone)}（${rule.timezone}）同时收到启用和停用动作。请调整执行时间、账号或计划状态。`
  }
  const snapshot = (): Dashboard => ({
    ...data,
    rules: data.rules.map(row => {
      const { enabled, enableCron, disableCron, timezone } = row.rule
      const enableAt = enabled && enableCron ? upcoming(enableCron, timezone, 1)[0] : null
      const disableAt = enabled && disableCron ? upcoming(disableCron, timezone, 1)[0] : null
      const nextAt = enableAt === null ? disableAt : disableAt === null ? enableAt : Math.min(enableAt, disableAt)
      const nextAction = enableAt === disableAt ? null : nextAt === enableAt ? 'enable' : 'disable'
      return { ...row, nextAt, nextAction }
    }),
  })
  window.codexProxyPlugin = {
    version: 2, theme,
    async request({ path, query, body }) {
      const params = new URLSearchParams(query)
      const request = body ? JSON.parse(body) : {}
      try {
        if (path === 'api/accounts') {
          const page = Number(params.get('cursor') ?? '1')
          const end = page * 20
          return reply({ accounts: accounts.slice(end - 20, end), nextCursor: end < accounts.length ? String(page + 1) : null })
        }
        if (path === 'api/dashboard') return reply(snapshot())
        if (path === 'api/runs') {
          const records = runs.filter(row =>
            (row.executedAt ?? row.scheduledAt) >= Date.now() / 1000 - data.retentionDays * 86400
            && (!params.has('accountId') || row.details.some(detail => detail.accountId === params.get('accountId')))
            && (!params.has('outcome') || row.status === params.get('outcome')),
          )
          const start = (Number(params.get('page') || '1') - 1) * 30
          return reply({ records: records.slice(start, start + 30), total: records.length, pageSize: 30, limited: false })
        }
        if (path === 'api/preview') return reply({ times: upcoming(request.cron, request.timezone, 5) })
        if (request.expectedVersion !== data.version) return error('计划设置已变化，请重新读取后保存', 409)
        if (path === 'api/save-rule') {
          const rule = request.rule
          if (!rule.name.trim() || !rule.accountIds.length) return error('请输入计划名称并选择账号')
          if (!rule.enableCron && !rule.disableCron) return error('请至少设置启用或停用时间')
          const enableCron = rule.enableCron?.trim().replace(/\s+/g, ' ') || null
          const disableCron = rule.disableCron?.trim().replace(/\s+/g, ' ') || null
          const enableJitterSeconds = enableCron ? rule.enableJitterSeconds ?? 0 : 0
          if (!Number.isSafeInteger(enableJitterSeconds) || enableJitterSeconds < 0)
            return error('启用抖动须为非负整数秒')
          if (enableCron) upcoming(enableCron, rule.timezone, 1)
          if (disableCron) upcoming(disableCron, rule.timezone, 1)
          const normalized = { ...rule, enableCron, disableCron, enableJitterSeconds }
          const conflict = conflictMessage(normalized, request.id)
          if (conflict) return error(conflict, 409)
          if (request.id) {
            const index = rules.findIndex(row => row.id === request.id)
            if (index < 0) return error('计划已不存在')
            rules[index] = { id: request.id, rule: normalized, nextAt: null, nextAction: null }
          } else rules.push({ id: crypto.randomUUID(), rule: normalized, nextAt: null, nextAction: null })
        } else if (path === 'api/delete-rule') {
          const index = rules.findIndex(row => row.id === request.id)
          if (index < 0) return error('计划已不存在')
          rules.splice(index, 1)
        } else if (path === 'api/retention') {
          if (!Number.isInteger(request.days) || request.days < 1 || request.days > 365) return error('日志保留天数须在 1 至 365 天之间')
          data.retentionDays = request.days
        } else return error('路由不存在', 404)
        data.version++
        return reply(snapshot())
      } catch {
        return error('请检查 Cron 表达式与 IANA 时区')
      }
    },
  }
  return true
}
