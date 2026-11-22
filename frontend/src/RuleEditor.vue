<script setup lang="ts">
import { CalendarClock, Check, Clock3, Pause, Play, Search, UsersRound } from '@lucide/vue'
import { ZButton } from '@codex-proxy/ui/button'
import { ZCheckbox } from '@codex-proxy/ui/checkbox'
import { ZDialog } from '@codex-proxy/ui/dialog'
import { ZInput } from '@codex-proxy/ui/input'
import { ZPagination } from '@codex-proxy/ui/pagination'
import { ZSegmented } from '@codex-proxy/ui/segmented'
import { ZSwitch } from '@codex-proxy/ui/switch'
import { computed, reactive, ref, watch } from 'vue'
import ScheduleTimeEditor from './ScheduleTimeEditor.vue'
import type { Account, RuleInput } from './types'

const props = defineProps<{ accounts: Account[]; initial: RuleInput; editing: boolean; busy: boolean; error: string; accountsLoading: boolean; accountsFailed: boolean }>()
const emit = defineEmits<{ close: []; save: [rule: RuleInput] }>()
const form = reactive({ ...props.initial, accountIds: [...props.initial.accountIds] })
const actionOptions = [
  { label: '仅启用', value: 'enable', icon: Play },
  { label: '仅停用', value: 'disable', icon: Pause },
  { label: '启用和停用', value: 'both' },
]
const actions = ref(form.enableCron !== null && form.disableCron !== null ? 'both' : form.enableCron !== null ? 'enable' : 'disable')
const enableCron = ref(form.enableCron ?? '0 8 * * 1,2')
const disableCron = ref(form.disableCron ?? '0 22 * * 1,2')
const hasEnable = computed(() => actions.value !== 'disable')
const hasDisable = computed(() => actions.value !== 'enable')
const jitterInput = ref(String(props.initial.enableJitterSeconds ?? 0))
const jitterSeconds = computed(() => Number(jitterInput.value))
const validJitter = computed(() => String(jitterInput.value).trim() !== ''
  && Number.isSafeInteger(jitterSeconds.value) && jitterSeconds.value >= 0)
const query = ref('')
const pickerList = ref<HTMLElement>()
const accountPage = ref(1), accountPageSize = ref(5)
const availableAccounts = computed(() => {
  const known = new Set(props.accounts.map(account => account.id))
  const missing: Account[] = form.accountIds.filter(id => !known.has(id))
    .map(id => ({ id, name: id, provider: '未读取到该账号', enabled: false, groupNames: [] }))
  return [...props.accounts, ...missing]
})
const filteredAccounts = computed(() => {
  const term = query.value.toLowerCase().trim()
  if (!term) return availableAccounts.value
  return availableAccounts.value.filter(account => [account.name, account.id, account.provider, ...account.groupNames]
    .some(value => value.toLowerCase().includes(term)))
})
const pagedAccounts = computed(() => {
  const start = (accountPage.value - 1) * accountPageSize.value
  return filteredAccounts.value.slice(start, start + accountPageSize.value)
})
watch([query, accountPageSize], () => { accountPage.value = 1 })
watch([query, accountPageSize, accountPage], () => pickerList.value?.scrollTo({ top: 0 }))
watch(filteredAccounts, accounts => {
  accountPage.value = Math.min(accountPage.value, Math.max(1, Math.ceil(accounts.length / accountPageSize.value)))
})
const canSubmit = computed(() => !!form.name.trim() && !!form.timezone.trim() && form.accountIds.length > 0
  && (!hasEnable.value || (!!enableCron.value && validJitter.value))
  && (!hasDisable.value || !!disableCron.value))
function accountDetails(account: Account) {
  const provider = account.provider === 'openai' ? 'OpenAI' : account.provider === 'xai' ? 'xAI' : account.provider
  return [provider, ...account.groupNames].join(' · ')
}
function selectAccount(id: string, checked: boolean) {
  if (checked && form.accountIds.length < 20 && !form.accountIds.includes(id)) form.accountIds.push(id)
  if (!checked) form.accountIds = form.accountIds.filter(item => item !== id)
}
function submit() {
  if (!canSubmit.value || props.busy) return
  emit('save', {
    name: form.name,
    accountIds: [...form.accountIds],
    enableCron: hasEnable.value ? enableCron.value : null,
    disableCron: hasDisable.value ? disableCron.value : null,
    timezone: form.timezone.trim(),
    enabled: form.enabled,
    ...(hasEnable.value && jitterSeconds.value > 0 ? { enableJitterSeconds: jitterSeconds.value } : {}),
  })
}
</script>

<template>
  <ZDialog :model-value="true" :title="editing ? '编辑计划' : '新建计划'" type="info" width="55rem"
    :close-on-click-modal="!busy" :close-on-press-escape="!busy" :show-close="!busy" @update:model-value="!$event && emit('close')">
    <template #icon><CalendarClock class="editor-dialog-icon" :size="21" /></template>
    <form id="schedule-editor" class="editor" @submit.prevent="submit">
      <div class="editor-basics">
        <div class="editor-field">
          <label for="schedule-name">计划名称</label>
          <ZInput id="schedule-name" v-model="form.name" maxlength="64" placeholder="例如 工作日晚间停用" :disabled="busy" />
        </div>
        <div class="editor-field">
          <span id="schedule-action-label">执行动作</span>
          <ZSegmented v-model="actions" :options="actionOptions" aria-labelledby="schedule-action-label" :disabled="busy" />
        </div>
      </div>

      <div class="editor-columns">
        <section class="editor-panel" aria-labelledby="schedule-accounts-heading">
          <div class="editor-panel-heading">
            <h3 id="schedule-accounts-heading"><UsersRound :size="17" />选择账号</h3>
            <span class="picker-count" aria-live="polite">已选 {{ form.accountIds.length }} / 20</span>
          </div>
          <ZInput v-model="query" clearable placeholder="搜索账号名称、ID 或组名" aria-label="搜索账号">
            <template #prefix><Search :size="15" /></template>
          </ZInput>
          <p class="picker-total" aria-live="polite">
            <template v-if="accountsLoading">正在读取账号</template>
            <template v-else-if="query.trim()">匹配 {{ filteredAccounts.length }} / {{ availableAccounts.length }} 个账号</template>
            <template v-else>共 {{ availableAccounts.length }} 个账号</template>
          </p>
          <div ref="pickerList" class="account-picker">
            <p v-if="accountsFailed && !accounts.length" class="empty-hint">账号列表读取失败，关闭弹窗后可重新刷新</p>
            <p v-else-if="!accountsLoading && !filteredAccounts.length" class="empty-hint">{{ query.trim() ? '没有匹配账号' : '暂无可选账号' }}</p>
            <ZCheckbox v-for="account in pagedAccounts" :key="account.id" class="picker-row"
              :class="{ 'picker-row-selected': form.accountIds.includes(account.id) }"
              :model-value="form.accountIds.includes(account.id)" :aria-label="account.name"
              :disabled="busy || (form.accountIds.length >= 20 && !form.accountIds.includes(account.id))"
              @update:model-value="selectAccount(account.id, Boolean($event))">
              <template #label>
                <span class="picker-details">
                  <span class="picker-identity">
                    <span class="picker-name" :title="account.id">{{ account.name }}</span>
                    <small :title="accountDetails(account)">{{ accountDetails(account) }}</small>
                  </span>
                  <span class="picker-state" :class="{ 'picker-state-active': account.enabled }">{{ account.enabled ? '启用' : '停用' }}</span>
                </span>
              </template>
            </ZCheckbox>
          </div>
          <ZPagination v-if="availableAccounts.length" v-model:current-page="accountPage" v-model:page-size="accountPageSize"
            :page-sizes="[5, 10, 20, 50]" :total="filteredAccounts.length" :disabled="accountsLoading || busy" class="account-pagination" />
        </section>

        <section class="editor-panel" aria-labelledby="schedule-time-heading">
          <div class="editor-panel-heading editor-time-heading">
            <h3 id="schedule-time-heading"><Clock3 :size="17" />执行时间</h3>
            <div class="editor-timezone">
              <label for="schedule-time-zone">时区</label>
              <ZInput id="schedule-time-zone" v-model="form.timezone" size="small" placeholder="Asia/Shanghai" :disabled="busy" />
            </div>
          </div>
          <ScheduleTimeEditor v-show="hasEnable" v-model="enableCron" action="enable" :timezone="form.timezone" :busy="busy"
            :jitter-seconds="validJitter ? jitterSeconds : undefined">
            <div class="enable-jitter">
              <label for="schedule-enable-jitter">启用抖动</label>
              <ZInput id="schedule-enable-jitter" v-model="jitterInput" class="jitter-input" type="number" :min="0"
                :step="1" size="small" aria-describedby="schedule-jitter-help" :aria-invalid="!validJitter" :disabled="busy">
                <template #suffix>秒</template>
              </ZInput>
              <p id="schedule-jitter-help" :class="{ error: !validJitter }">
                {{ validJitter ? '每个账号在设定范围内错开启用，0 秒关闭抖动' : '请输入非负整数秒' }}
              </p>
            </div>
          </ScheduleTimeEditor>
          <ScheduleTimeEditor v-show="hasDisable" v-model="disableCron" action="disable" :timezone="form.timezone" :busy="busy" />
        </section>
      </div>
      <p v-if="error" role="alert" class="feedback error">{{ error }}</p>
    </form>
    <template #footer>
      <div class="editor-footer">
        <div class="enable-plan">
          <span id="schedule-enabled-label">启用计划</span>
          <ZSwitch v-model="form.enabled" aria-labelledby="schedule-enabled-label" :disabled="busy" />
        </div>
        <div class="editor-buttons">
          <ZButton :disabled="busy" @click="emit('close')">取消</ZButton>
          <ZButton type="primary" :disabled="!canSubmit || busy" :loading="busy" @click="submit">
            <template #icon><Check :size="16" /></template>保存计划
          </ZButton>
        </div>
      </div>
    </template>
  </ZDialog>
</template>

<style scoped>
.editor-dialog-icon { color: var(--cp-color-primary-text); }
.editor { display: grid; gap: 18px; }
.editor-basics, .editor-columns {
  display: grid;
  grid-template-columns: minmax(0, 11fr) minmax(0, 9fr);
  gap: 14px;
}
.editor-columns { grid-template-columns: minmax(0, 9fr) minmax(0, 11fr); }
.editor-field { display: grid; align-content: start; gap: 6px; min-width: 0; }
.editor-field > label, .editor-field > span { font-size: 12px; font-weight: 650; }
.editor-panel {
  display: grid;
  align-content: start;
  gap: 12px;
  min-width: 0;
  padding: 14px;
  border: 1px solid var(--cp-color-border);
  border-radius: 12px;
}
.editor-panel-heading { display: flex; align-items: center; justify-content: space-between; gap: 6px; min-height: 24px; }
.editor-panel-heading h3 { display: flex; align-items: center; gap: 7px; margin: 0; font-size: 14px; font-weight: 700; }
.editor-panel-heading svg { color: var(--cp-color-primary-text); }
.editor-time-heading { flex-wrap: wrap; }
.editor-timezone { display: flex; align-items: center; gap: 7px; min-width: 0; }
.editor-timezone label { font-size: 11px; font-weight: 650; white-space: nowrap; }
.editor-timezone > :last-child { width: clamp(112px, 21vw, 170px); }
.enable-jitter { display: flex; align-items: center; flex-wrap: wrap; gap: 4px 8px; }
.enable-jitter label { font-size: 11px; font-weight: 650; }
.jitter-input { width: 90px; }
.enable-jitter p { flex-basis: 100%; margin: 0; color: var(--cp-color-text-secondary); font-size: 11px; }
.enable-jitter p.error { color: var(--cp-color-error-text); }
.picker-count, .picker-state {
  padding: 4px 7px;
  border-radius: 6px;
  background: var(--cp-color-fill-quaternary);
  color: var(--cp-color-text-secondary);
  font-size: 11px;
  white-space: nowrap;
}
.picker-count { font-variant-numeric: tabular-nums; font-weight: 650; }
.picker-total { margin: 0; color: var(--cp-color-text-secondary); font-size: 11px; }
.account-pagination { margin-top: 0; }
.account-picker {
  display: flex;
  flex-direction: column;
  gap: 3px;
  height: 246px;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-gutter: stable;
}
.account-picker .picker-row {
  display: grid;
  flex: none;
  grid-template-columns: auto minmax(0, 1fr);
  min-height: 46px;
  gap: 10px;
  padding: 8px;
  border-radius: 7px;
}
.picker-row:hover { background: var(--cp-color-fill-quaternary); }
.account-picker .picker-row-selected { background: var(--cp-table-row-selected-bg); }
.picker-details { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.picker-identity { display: grid; min-width: 0; gap: 4px; line-height: 1.15; }
.picker-name, .picker-identity small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.picker-identity small, .empty-hint { color: var(--cp-color-text-secondary); font-size: 11px; }
.picker-state-active { background: var(--cp-color-green-container-strong); color: var(--cp-color-green-on-container); }
.empty-hint { margin: 0; padding: 12px 8px; }
.editor-footer, .editor-buttons, .enable-plan { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
.editor-footer { width: 100%; justify-content: space-between; padding-top: 12px; border-top: 1px solid var(--cp-color-border); }
.editor-buttons { margin-left: auto; }
.enable-plan { font-size: 13px; font-weight: 650; }
@media (max-width: 700px) {
  .editor-columns { grid-template-columns: minmax(0, 1fr); }
  .account-picker { height: 174px; }
}
@media (max-width: 480px) {
  .editor-basics { grid-template-columns: minmax(0, 1fr); }
  .editor, .editor-columns, .editor-basics { gap: 11px; }
  .editor-panel { gap: 9px; padding: 11px; }
}
</style>
