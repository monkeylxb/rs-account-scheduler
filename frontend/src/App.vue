<script setup lang="ts">
import { CalendarClock, Clock3, Pencil, Plus, RefreshCw, Save, Trash2 } from '@lucide/vue'
import { ZButton } from '@codex-proxy/ui/button'
import { ZCard } from '@codex-proxy/ui/card'
import { ZDialog } from '@codex-proxy/ui/dialog'
import { ZIconButton } from '@codex-proxy/ui/icon-button'
import { ZInput } from '@codex-proxy/ui/input'
import { ZSelect } from '@codex-proxy/ui/select'
import { ZSwitch } from '@codex-proxy/ui/switch'
import { ZTable, type TableColumn } from '@codex-proxy/ui/table'
import { ZTag } from '@codex-proxy/ui/tag'
import { computed, nextTick, onMounted, ref, watch } from 'vue'
import { api } from './api'
import { actionLabel, dateTime, localZone, message, outcomeLabels, outcomeTag, runStatuses } from './format'
import RuleEditor from './RuleEditor.vue'
import type { Account, Dashboard, LogPage, Rule, RuleInput, Run, RunAccount } from './types'

const preview = import.meta.env.DEV
const zone = localZone()
const tabs = [{ id: 'settings', label: '界面设置' }, { id: 'logs', label: '执行日志' }] as const
type Tab = typeof tabs[number]['id']
const section = ref<Tab>('settings')
const tabList = ref<HTMLElement>()
const dashboard = ref<Dashboard>({ rules: [], version: 0, retentionDays: 30, queued: 0 })
const accounts = ref<Account[]>([])
const logPage = ref<LogPage>({ records: [], pageSize: 30, total: 0, limited: false })
const page = ref(1), accountFilter = ref(''), outcomeFilter = ref('')
const retention = ref('30')
const retentionDays = computed(() => Number(retention.value))
const canSaveRetention = computed(() => Number.isInteger(retentionDays.value) && retentionDays.value >= 1
  && retentionDays.value <= 365 && retentionDays.value !== dashboard.value.retentionDays)
const loading = ref(true), accountsLoading = ref(true), logsLoading = ref(false), saving = ref(false)
const error = ref(''), accountError = ref(''), logError = ref(''), editorError = ref(''), notice = ref('')
const editor = ref<{ id: string | null; rule: RuleInput; version: number } | null>(null)
const toDelete = ref<Rule | null>(null)
const selectedRun = ref<Run | null>(null)
const showRun = computed({ get: () => selectedRun.value !== null, set: open => { if (!open) selectedRun.value = null } })
const showDelete = computed({ get: () => !!toDelete.value, set: value => { if (!value && !saving.value) toDelete.value = null } })
const names = computed(() => new Map(accounts.value.map(account => [account.id, account.name])))
const accountName = (id: string) => names.value.get(id) || id
const active = computed(() => dashboard.value.rules.filter(row => row.rule.enabled).length)
const next = computed(() => dashboard.value.rules.filter(row => row.nextAt !== null)
  .sort((left, right) => left.nextAt! - right.nextAt!)[0])
const numPages = computed(() => Math.max(1, Math.ceil(logPage.value.total / logPage.value.pageSize)))
const accountOptions = computed(() => [
  { value: '', label: '全部账号' },
  ...[...new Set([...accounts.value.map(account => account.id), ...logPage.value.records.flatMap(run => run.details.map(row => row.accountId)), accountFilter.value])]
    .filter(Boolean).map(id => ({ value: id, label: accountName(id) })),
])
const outcomeOptions = [{ value: '', label: '全部结果' }, ...Object.entries(runStatuses).map(([value, status]) => ({ value, label: status.label }))]
const planColumns: TableColumn<Rule>[] = [
  { key: 'name', label: '计划名称', kind: 'identity', fixed: false, minWidth: 175 },
  { key: 'accounts', label: '账号', minWidth: 175 },
  { key: 'cron', label: '执行安排', minWidth: 230 },
  { key: 'next', label: '下次执行', kind: 'datetime', width: 170 },
  { key: 'enabled', label: '计划状态', width: 96, align: 'center' },
  { key: 'actions', label: '操作', kind: 'actions', width: 88 },
]
const logColumns: TableColumn<Run>[] = [
  { key: 'ruleName', label: '计划', kind: 'identity', fixed: false, minWidth: 184 },
  { key: 'action', label: '动作', width: 80 },
  { key: 'scheduled', label: '计划时间', kind: 'datetime', width: 160 },
  { key: 'executed', label: '最近执行', kind: 'datetime', width: 160 },
  { key: 'outcome', label: '结果', width: 136 },
  { key: 'details', label: '操作', kind: 'actions', width: 86 },
]
const detailColumns: TableColumn<RunAccount>[] = [
  { key: 'account', label: '账号', kind: 'identity', fixed: false, minWidth: 178 },
  { key: 'executed', label: '实际时间', kind: 'datetime', width: 160 },
  { key: 'outcome', label: '结果', width: 135 },
]
const runKey = (run: Run) => JSON.stringify([run.ruleId, run.ruleId === null ? run.ruleName : null, run.scheduledAt, run.action])
let logRequest = 0

function apply(result: Dashboard, resetRetention = false) {
  if (resetRetention || retention.value === String(dashboard.value.retentionDays)) retention.value = String(result.retentionDays)
  dashboard.value = result
}
function switchTab(tab: Tab) {
  if (tab === section.value) return
  section.value = tab
  notice.value = ''
  if (tab === 'logs') void loadLogs()
}
async function onTabKey(event: KeyboardEvent) {
  const destinations: Partial<Record<string, Tab>> = {
    ArrowLeft: section.value === 'settings' ? 'logs' : 'settings',
    ArrowRight: section.value === 'settings' ? 'logs' : 'settings',
    Home: 'settings',
    End: 'logs',
  }
  const destination = destinations[event.key]
  if (!destination) return
  event.preventDefault()
  switchTab(destination)
  await nextTick()
  tabList.value?.querySelector<HTMLElement>('[aria-selected="true"]')?.focus()
}
async function loadAccounts() {
  accountsLoading.value = true
  accountError.value = ''
  try {
    const rows = new Map<string, Account>(), cursors = new Set<string>()
    let cursor: string | null = null
    do {
      const result = await api.accounts(cursor)
      for (const account of result.accounts) rows.set(account.id, account)
      cursor = result.nextCursor
      if (cursor && cursors.has(cursor)) throw new Error('网关账号分页游标重复')
      if (cursor) cursors.add(cursor)
    } while (cursor)
    accounts.value = [...rows.values()].sort((left, right) => left.name.localeCompare(right.name, 'zh-CN'))
  } catch (cause) { accountError.value = message(cause) }
  finally { accountsLoading.value = false }
}
async function loadLogs() {
  const request = ++logRequest
  logsLoading.value = true
  logError.value = ''
  try {
    const result = await api.logs(page.value, accountFilter.value, outcomeFilter.value)
    if (request !== logRequest) return
    logPage.value = result
    if (page.value > numPages.value) page.value = numPages.value
  } catch (cause) { if (request === logRequest) logError.value = message(cause) }
  finally { if (request === logRequest) logsLoading.value = false }
}
async function refresh() {
  loading.value = true
  error.value = ''
  try { apply(await api.dashboard()) }
  catch (cause) { error.value = message(cause) }
  finally { loading.value = false }
}
async function refreshAll() {
  await Promise.all([refresh(), loadAccounts()])
}
function openEditor(row?: Rule) {
  editorError.value = ''
  editor.value = {
    id: row?.id ?? null,
    version: dashboard.value.version,
    rule: row?.rule ?? { name: '', accountIds: [], enableCron: '0 8 * * 1,2', disableCron: '0 22 * * 1,2', timezone: zone, enabled: true },
  }
}
async function saveRule(rule: RuleInput) {
  const current = editor.value
  if (!current || saving.value) return
  saving.value = true
  editorError.value = ''
  try {
    apply(await api.saveRule(current.id, rule, current.version))
    editor.value = null
    notice.value = '计划已保存'
  } catch (cause) { editorError.value = message(cause) }
  finally { saving.value = false }
}
async function mutate(action: () => Promise<Dashboard>, success: string, resetRetention = false) {
  if (saving.value) return
  saving.value = true
  error.value = ''
  notice.value = ''
  try {
    apply(await action(), resetRetention)
    notice.value = success
  } catch (cause) { error.value = message(cause) }
  finally { saving.value = false }
}
async function deleteRule() {
  const selected = toDelete.value
  if (!selected) return
  toDelete.value = null
  await mutate(() => api.deleteRule(selected.id, dashboard.value.version), '计划已删除')
}
async function saveRetention() {
  if (!canSaveRetention.value || saving.value) return
  await mutate(() => api.retention(retentionDays.value, dashboard.value.version), '日志保留天数已保存', true)
  await loadLogs()
}

watch([accountFilter, outcomeFilter], () => { if (page.value !== 1) page.value = 1; else void loadLogs() })
watch(page, () => void loadLogs())
onMounted(() => void refreshAll())
</script>

<template>
  <main class="workspace" :class="{ 'local-preview': preview }">
    <div v-if="preview" class="preview-badge"><span></span>本地模拟数据</div>
    <div ref="tabList" role="tablist" aria-label="账号定时启停" class="section-tabs" @keydown="onTabKey">
      <button v-for="tab in tabs" :id="`${tab.id}-tab`" :key="tab.id" type="button" role="tab"
        :aria-selected="section === tab.id" :aria-controls="`${tab.id}-panel`" :tabindex="section === tab.id ? 0 : -1"
        @click="switchTab(tab.id)">{{ tab.label }}</button>
    </div>

    <div v-if="error" role="alert" class="feedback error">{{ error }}</div>
    <div v-if="notice" role="status" class="feedback success">{{ notice }}</div>

    <section v-show="section === 'settings'" id="settings-panel" role="tabpanel" aria-labelledby="settings-tab" class="tab-panel">
      <div class="overview">
        <ZCard padding="none" class="summary-card">
          <div><span class="summary-label">启用计划</span><strong>{{ active }} <small>/ {{ dashboard.rules.length }}</small></strong></div>
          <span class="summary-icon blue"><CalendarClock :size="20" /></span>
        </ZCard>
        <ZCard padding="none" class="summary-card">
          <div><span class="summary-label" title="已启用计划中最早的下一次触发时间">下次计划执行</span><strong class="summary-time">{{ dateTime(next?.nextAt ?? null, next?.rule.timezone ?? zone) }}</strong>
            <small v-if="next">{{ next.rule.name }} · {{ next.nextAction ? actionLabel(next.nextAction) : '启停冲突' }} · {{ next.rule.timezone }}</small></div>
          <span class="summary-icon green"><Clock3 :size="20" /></span>
        </ZCard>
      </div>

      <ZCard padding="none" class="panel">
        <div class="panel-heading">
          <div><h2>定时计划</h2><p v-if="dashboard.queued">待执行 {{ dashboard.queued }} 个账号动作</p></div>
          <div class="toolbar">
            <ZIconButton title="刷新计划和账号" aria-label="刷新计划和账号" :disabled="loading || saving" @click="refreshAll"><RefreshCw :size="17" /></ZIconButton>
            <ZButton type="primary" :disabled="loading || saving" @click="openEditor()"><template #icon><Plus :size="17" /></template>新建计划</ZButton>
          </div>
        </div>
        <p v-if="accountError" class="error" role="alert">读取账号失败：{{ accountError }}</p>
        <ZTable :columns="planColumns" :data="dashboard.rules" :row-key="(row: Rule) => row.id" :loading="loading && !dashboard.rules.length" empty-text="暂无定时计划">
          <template #name="{ row }"><span class="plan-name" :title="row.rule.name">{{ row.rule.name }}</span></template>
          <template #accounts="{ row }">
            <div class="target-list" :title="row.rule.accountIds.map(accountName).join('、')">
              <span>{{ accountName(row.rule.accountIds[0]) }}</span><small v-if="row.rule.accountIds.length > 1">另 {{ row.rule.accountIds.length - 1 }} 个</small>
            </div>
          </template>
          <template #cron="{ row }">
            <div class="cron-cell">
              <div v-if="row.rule.enableCron" class="cron-action"><ZTag size="small" type="success">启用</ZTag><code>{{ row.rule.enableCron }}</code></div>
              <div v-if="row.rule.disableCron" class="cron-action"><ZTag size="small" type="warning">停用</ZTag><code>{{ row.rule.disableCron }}</code></div>
              <small>{{ row.rule.timezone }}</small>
            </div>
          </template>
          <template #next="{ row }">
            <div class="cron-cell"><span class="tabular">{{ dateTime(row.nextAt, row.rule.timezone) }}</span>
              <small v-if="row.nextAt !== null">{{ row.nextAction ? actionLabel(row.nextAction) : '启停冲突' }}</small>
            </div>
          </template>
          <template #enabled="{ row }">
            <ZSwitch :key="row.id + saving + row.rule.enabled" :model-value="row.rule.enabled" :disabled="saving || loading" :aria-label="`启用计划 ${row.rule.name}`"
              @update:model-value="mutate(() => api.saveRule(row.id, { ...row.rule, enabled: $event }, dashboard.version), $event ? '计划已启用' : '计划已暂停')" />
          </template>
          <template #actions="{ row }">
            <div class="row-actions">
              <ZIconButton size="small" :disabled="saving" :aria-label="`编辑 ${row.rule.name}`" title="编辑计划" @click="openEditor(row)"><Pencil :size="15" /></ZIconButton>
              <ZIconButton size="small" :disabled="saving" :aria-label="`删除 ${row.rule.name}`" title="删除计划" @click="toDelete = row"><Trash2 :size="15" /></ZIconButton>
            </div>
          </template>
        </ZTable>
      </ZCard>
    </section>

    <section v-show="section === 'logs'" id="logs-panel" role="tabpanel" aria-labelledby="logs-tab" class="tab-panel">
      <ZCard padding="none" class="panel">
        <div class="panel-heading logs-heading">
          <div><h2>执行日志</h2><p>时间按 {{ zone }} 显示</p></div>
          <form class="retention" @submit.prevent="saveRetention">
            <label for="log-retention">保留天数</label>
            <ZInput v-model="retention" id="log-retention" type="number" :min="1" :max="365" size="small" aria-label="日志保留天数" />
            <ZIconButton title="保存日志保留天数" aria-label="保存日志保留天数" :disabled="loading || saving || !canSaveRetention" @click="saveRetention"><Save :size="16" /></ZIconButton>
          </form>
        </div>
        <div class="log-toolbar">
          <ZSelect v-model="accountFilter" :options="accountOptions" aria-label="筛选日志账号" />
          <ZSelect v-model="outcomeFilter" :options="outcomeOptions" aria-label="筛选日志结果" />
          <ZIconButton title="刷新日志" aria-label="刷新日志" :disabled="logsLoading" @click="loadLogs"><RefreshCw :size="16" /></ZIconButton>
        </div>
        <p v-if="logError" class="error" role="alert">{{ logError }}</p>
        <p v-if="logPage.limited" class="muted">日志达到存储上限，较早记录已清理</p>
        <ZTable class="log-table" scrollbar-always-visible :columns="logColumns" :data="logPage.records" :loading="logsLoading && !logPage.records.length"
          :row-key="runKey" empty-text="暂无执行记录">
          <template #ruleName="{ row }"><span class="plan-name" :title="row.ruleName">{{ row.ruleName }}</span></template>
          <template #action="{ row }">{{ actionLabel(row.action) }}</template>
          <template #scheduled="{ row }"><span class="tabular">{{ dateTime(row.scheduledAt, zone) }}</span></template>
          <template #executed="{ row }"><span class="tabular">{{ dateTime(row.executedAt, zone) }}</span></template>
          <template #outcome="{ row }">
            <div class="cron-cell">
              <ZTag size="small" :type="runStatuses[row.status].type">{{ runStatuses[row.status].label }}</ZTag>
              <small>{{ row.successful }} / {{ row.details.length }} 成功</small>
            </div>
          </template>
          <template #details="{ row }"><ZButton size="small" :aria-label="`详情 ${row.ruleName} ${dateTime(row.scheduledAt, zone)}`" @click="selectedRun = row">详情</ZButton></template>
        </ZTable>
        <div v-if="logPage.total" class="pager">
          <span>共 {{ logPage.total }} 条 · 第 {{ page }} / {{ numPages }} 页</span>
          <ZButton size="small" :disabled="page <= 1 || logsLoading" @click="page--">上一页</ZButton>
          <ZButton size="small" :disabled="page >= numPages || logsLoading" @click="page++">下一页</ZButton>
        </div>
      </ZCard>
    </section>

    <ZDialog v-if="selectedRun" v-model="showRun" title="执行详情" width="42rem"
      :description="`${selectedRun.ruleName} · ${actionLabel(selectedRun.action)} · ${dateTime(selectedRun.scheduledAt, zone)}`">
      <div class="execution-summary">
        <ZTag :type="runStatuses[selectedRun.status].type">{{ runStatuses[selectedRun.status].label }}</ZTag>
        <span>已记录 {{ selectedRun.details.length }} 个账号，{{ selectedRun.successful }} 个成功</span>
      </div>
      <ZTable class="log-table" scrollbar-always-visible size="small" :columns="detailColumns" :data="selectedRun.details"
        :row-key="(row: RunAccount, index: number) => `${row.accountId}/${index}`">
        <template #account="{ row }"><span :title="row.accountId">{{ accountName(row.accountId) }}</span></template>
        <template #executed="{ row }"><span class="tabular">{{ dateTime(row.executedAt, zone) }}</span></template>
        <template #outcome="{ row }"><ZTag size="small" :type="outcomeTag(row.outcome)">{{ row.outcome ? outcomeLabels[row.outcome] : '待执行' }}</ZTag></template>
      </ZTable>
      <template #footer><ZButton @click="showRun = false">关闭</ZButton></template>
    </ZDialog>
    <RuleEditor v-if="editor" :initial="editor.rule" :editing="!!editor.id" :accounts="accounts" :busy="saving" :error="editorError"
      :accounts-loading="accountsLoading" :accounts-failed="!!accountError" @save="saveRule" @close="editor = null" />
    <ZDialog v-model="showDelete" title="删除计划" type="warning" width="27rem">
      <p>删除「{{ toDelete?.rule.name }}」后，该计划将停止触发，已有执行记录按保留天数清理。</p>
      <template #footer><ZButton @click="showDelete = false">取消</ZButton><ZButton type="danger" @click="deleteRule">删除计划</ZButton></template>
    </ZDialog>
  </main>
</template>
