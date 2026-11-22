export type Action = 'enable' | 'disable'
export type Outcome = 'applied' | 'unchanged' | 'verified' | 'failed' | 'uncertain' | 'conflict'

export interface Account { id: string; name: string; provider: string; enabled: boolean; groupNames: string[] }
export interface RuleInput {
  name: string
  accountIds: string[]
  enableCron: string | null
  enableJitterSeconds?: number
  disableCron: string | null
  timezone: string
  enabled: boolean
}
export interface Rule { id: string; rule: RuleInput; nextAt: number | null; nextAction: Action | null }
export interface Dashboard { rules: Rule[]; version: number; retentionDays: number; queued: number }
export interface RunAccount {
  accountId: string
  executedAt: number | null
  outcome: Outcome | null
}
export type RunStatus = 'all_success' | 'partial_success' | 'all_failed' | 'pending' | 'conflict' | 'needs_review'
export interface Run {
  ruleId: string | null
  ruleName: string
  scheduledAt: number
  executedAt: number | null
  action: Action | null
  successful: number
  status: RunStatus
  details: RunAccount[]
}
export interface LogPage { records: Run[]; pageSize: number; total: number; limited: boolean }

export interface BridgeResponse { status: number; contentType: string; body: ArrayBuffer }
export interface BridgeRequest { path: string; method: 'GET' | 'POST'; query?: string; contentType?: string; body?: string }
export interface Bridge {
  version: 2
  theme: 'light' | 'dark'
  request(input: BridgeRequest): Promise<BridgeResponse>
}
declare global { interface Window { codexProxyPlugin?: Bridge } }
