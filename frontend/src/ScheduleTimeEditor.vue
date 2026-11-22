<script setup lang="ts">
import { CalendarClock } from '@lucide/vue'
import { ZButton } from '@codex-proxy/ui/button'
import { ZInput } from '@codex-proxy/ui/input'
import { ZPopover } from '@codex-proxy/ui/popover'
import { ZSegmented } from '@codex-proxy/ui/segmented'
import { computed, ref, useId, watch } from 'vue'
import { api } from './api'
import { dateTime, jitterRange, message } from './format'
import type { Action } from './types'

const props = defineProps<{ action: Action; timezone: string; busy: boolean; jitterSeconds?: number }>()
const model = defineModel<string>({ required: true })
const id = useId()
const title = computed(() => props.action === 'enable' ? '启用时间' : '停用时间')
const weekdays = ['日', '一', '二', '三', '四', '五', '六'] as const
const weekdayOrder = [1, 2, 3, 4, 5, 6, 0]
const modeOptions = [{ label: '按星期', value: 'weekly' }, { label: 'Cron', value: 'cron' }]
const weekly = /^(\d{1,2}) (\d{1,2}) \* \* (\*|[0-6](?:,[0-6])*)$/.exec(model.value)
const mode = ref(weekly ? 'weekly' : 'cron')
const time = ref(weekly ? `${weekly[2]!.padStart(2, '0')}:${weekly[1]!.padStart(2, '0')}` : props.action === 'enable' ? '08:00' : '22:00')
const days = ref<number[]>(weekly ? weekly[3] === '*' ? [...weekdayOrder] : weekly[3]!.split(',').map(Number) : [1, 2])
const expression = ref(model.value)
const weeklyCron = computed(() => {
  if (!/^\d{2}:\d{2}$/.test(time.value) || !days.value.length) return ''
  const [hour, minute] = time.value.split(':').map(Number)
  const dayList = days.value.length === 7 ? '*' : [...days.value].sort((a, b) => a - b).join(',')
  return `${minute} ${hour} * * ${dayList}`
})
let lastWeeklyCron = weeklyCron.value
const cron = computed(() => mode.value === 'weekly' ? weeklyCron.value : expression.value.trim())
const previewOpen = ref(false)
const upcoming = ref<number[]>([]), calculating = ref(false), previewError = ref('')
let previewRevision = 0
watch(cron, value => { model.value = value })
watch([cron, () => props.timezone], () => { previewOpen.value = false })

function selectMode(next: string) {
  if (next === mode.value) return
  if (next === 'cron' && (weeklyCron.value !== lastWeeklyCron || !expression.value.trim())) expression.value = weeklyCron.value
  lastWeeklyCron = weeklyCron.value
  mode.value = next
}
function toggleDay(day: number) {
  days.value = days.value.includes(day) ? days.value.filter(item => item !== day) : [...days.value, day]
}
function resetPreview() {
  previewRevision++
  upcoming.value = []
  previewError.value = ''
  calculating.value = false
}
function closePreview(event: KeyboardEvent) {
  if (!previewOpen.value) return
  event.stopPropagation()
  event.preventDefault()
  previewOpen.value = false
}
async function calculate() {
  const revision = ++previewRevision
  calculating.value = true
  previewError.value = ''
  try {
    const result = await api.preview(cron.value, props.timezone.trim())
    if (revision === previewRevision) upcoming.value = result.times
  } catch (error) {
    if (revision === previewRevision) previewError.value = message(error)
  } finally {
    if (revision === previewRevision) calculating.value = false
  }
}
</script>

<template>
  <section class="schedule-time" :class="`schedule-time-${action}`" :aria-labelledby="`${id}-title`">
    <div class="time-heading">
      <h4 :id="`${id}-title`">{{ title }}</h4>
      <ZSegmented :model-value="mode" :options="modeOptions" :aria-label="`${title}设置方式`" size="small" :disabled="busy"
        @update:model-value="selectMode" />
    </div>
    <div v-if="mode === 'weekly'" class="weekly-fields">
      <div class="time-field">
        <span :id="`${id}-weekdays`">星期</span>
        <div class="weekdays" role="group" :aria-labelledby="`${id}-weekdays`">
          <button v-for="day in weekdayOrder" :key="day" type="button" class="weekday" :disabled="busy"
            :aria-label="'星期' + weekdays[day]" :aria-pressed="days.includes(day)" @click="toggleDay(day)">{{ weekdays[day] }}</button>
        </div>
      </div>
      <div class="time-field">
        <label :for="`${id}-time`">时间</label>
        <input :id="`${id}-time`" v-model="time" type="time" class="time-input" :disabled="busy" />
      </div>
    </div>
    <div v-else class="time-field">
      <label :for="`${id}-cron`">五段 Cron</label>
      <ZInput :id="`${id}-cron`" v-model="expression" placeholder="0 22 * * 1,2" :disabled="busy" />
    </div>
    <slot />
    <div class="preview-line">
      <code v-if="mode === 'weekly' && weeklyCron" title="生成的 Cron 表达式">{{ weeklyCron }}</code>
      <ZPopover v-model="previewOpen" placement="bottom-end" :disabled="!cron || !timezone.trim() || busy"
        @open="calculate" @close="resetPreview">
        <template #reference="{ id: panelId, open }">
          <ZButton size="small" :aria-label="`预览${title}`" :aria-controls="open ? panelId : undefined" :aria-expanded="open"
            :disabled="!cron || !timezone.trim() || busy" @keydown.esc="closePreview">
            <template #icon><CalendarClock :size="15" /></template>预览时间
          </ZButton>
        </template>
        <section class="time-preview" :aria-labelledby="`${id}-upcoming`">
          <div class="time-preview-heading">
            <h5 :id="`${id}-upcoming`">{{ title }}预览</h5>
            <small>{{ timezone.trim() }}</small>
            <small v-if="action === 'enable' && jitterSeconds">每次启用{{ jitterRange(jitterSeconds) }}</small>
          </div>
          <div aria-live="polite">
            <p v-if="calculating" class="preview-message">正在计算时间…</p>
            <p v-else-if="previewError" role="alert" class="preview-message error">{{ previewError }}</p>
            <ol v-else-if="upcoming.length" :aria-label="`接下来 ${upcoming.length} 次${action === 'enable' ? '启用' : '停用'}`">
              <li v-for="at in upcoming" :key="at">{{ dateTime(at, timezone.trim()) }}</li>
            </ol>
          </div>
        </section>
      </ZPopover>
    </div>
  </section>
</template>

<style scoped>
.schedule-time {
  --action-color: var(--cp-color-green-on-container);
  --action-fill: var(--cp-color-green-container-strong);
  display: grid;
  gap: 9px;
  min-width: 0;
  padding: 10px;
  border: 1px solid var(--cp-color-border);
  border-radius: 9px;
}
.schedule-time-disable { --action-color: var(--cp-color-warning-text); --action-fill: var(--cp-color-warning-container); }
.time-heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 7px; }
.time-heading h4 { margin: 0; padding: 4px 8px; border-radius: 5px; background: var(--action-fill); color: var(--action-color); font-size: 12px; font-weight: 700; }
.weekly-fields { display: grid; grid-template-columns: minmax(0, 1fr) minmax(92px, 1fr); gap: 8px; }
@media (min-width: 421px) { .weekly-fields { grid-template-columns: minmax(0, 3fr) minmax(100px, 1fr); } }
@media (max-width: 375px) { .weekly-fields { grid-template-columns: 1fr; } }
.time-field { display: grid; gap: 4px; min-width: 0; }
.time-field > label, .time-field > span { font-size: 11px; font-weight: 650; }
.weekdays { display: grid; grid-template-columns: repeat(7, minmax(0, 1fr)); gap: 4px; }
.weekday {
  min-height: 32px;
  padding: 0;
  border: 1px solid var(--cp-color-border);
  border-radius: 6px;
  background: transparent;
  color: var(--cp-color-text);
  cursor: pointer;
  font: inherit;
}
.weekday:hover { background: var(--cp-color-fill-quaternary); }
.weekday[aria-pressed="true"] { border-color: transparent; background: var(--action-fill); color: var(--action-color); font-weight: 700; }
.weekday:disabled, .time-input:disabled { cursor: not-allowed; opacity: .65; }
.weekday:focus-visible, .time-input:focus-visible { outline: 2px solid var(--cp-color-primary); outline-offset: 1px; }
.time-input {
  width: 100%;
  min-width: 0;
  min-height: 32px;
  padding: 0 6px;
  border: 0;
  border-radius: 6px;
  background: var(--cp-input-bg);
  color: inherit;
  font: inherit;
}
.preview-line { display: flex; align-items: center; justify-content: flex-end; flex-wrap: wrap; gap: 8px; }
.preview-line code {
  margin-right: auto;
  padding: 4px 7px;
  border-radius: 5px;
  background: var(--cp-color-fill-quaternary);
  color: var(--cp-color-text-secondary);
  font: 12px var(--cp-font-family-code);
  overflow-wrap: anywhere;
}
.time-preview { width: min(14.5rem, calc(100vw - 24px)); padding: 12px; }
.time-preview-heading { display: grid; gap: 2px; margin-bottom: 7px; }
.time-preview h5 { margin: 0; font-size: 13px; font-weight: 700; }
.time-preview small { overflow-wrap: anywhere; color: var(--cp-color-text-secondary); font-size: 11px; }
.time-preview ol { margin: 0; padding: 0; list-style: none; }
.time-preview li { padding: 5px 0; border-top: 1px solid var(--cp-color-border); font-size: 12px; font-variant-numeric: tabular-nums; }
.preview-message { margin: 7px 0 0; overflow-wrap: anywhere; font-size: 12px; }
</style>
