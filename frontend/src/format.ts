import type { Action, Outcome, RunStatus } from './types'

export const outcomeLabels: Record<Outcome, string> = {
  applied: '成功', unchanged: '已是目标状态', verified: '已回读确认',
  failed: '失败', uncertain: '结果待确认', conflict: '规则冲突',
}
export const runStatuses = {
  all_success: { label: '全部成功', type: 'success' },
  partial_success: { label: '部分成功', type: 'warning' },
  all_failed: { label: '全部失败', type: 'danger' },
  conflict: { label: '规则冲突', type: 'warning' },
  needs_review: { label: '存在异常', type: 'warning' },
  pending: { label: '执行中', type: 'info' },
} as const satisfies Record<RunStatus, { label: string; type: string }>
export function outcomeTag(outcome: Outcome | null) {
  if (outcome === 'failed') return 'danger'
  if (outcome === 'uncertain' || outcome === 'conflict') return 'warning'
  return outcome === 'unchanged' || outcome === null ? 'default' : 'success'
}
export const actionLabel = (action: Action | null) => action === 'enable' ? '启用' : action === 'disable' ? '停用' : '跳过'
export const message = (error: unknown) => error instanceof Error ? error.message : '操作失败'
export const localZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
export function dateTime(at: number | null, timezone = localZone()): string {
  if (at === null) return '—'
  try {
    return new Intl.DateTimeFormat('zh-CN', {
      timeZone: timezone, year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', hourCycle: 'h23',
    }).format(at * 1000)
  } catch { return '—' }
}
