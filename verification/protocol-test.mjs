import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const executable = process.platform === 'win32' ? 'account-scheduler.exe' : 'account-scheduler'
const binary = process.argv[2] || fileURLToPath(new URL(`../backend/target/debug/${executable}`, import.meta.url))
const protocolVersion = 7
const contributes = {
  management: { id: 'personal.account-scheduler.management', version: 2, stages: ['management'], inputFormats: [], outputFormats: [] },
  maintenance: { id: 'personal.account-scheduler.maintenance', version: 1, stages: ['maintenance'], inputFormats: [], outputFormats: [] },
}
const records = new Map(), writes = [], reads = [], handles = new Map()
const accounts = new Map([
  ['acct_1', { provider: 'openai', name: '工作账号', enabled: true }],
  ['acct_2', { provider: 'xai', name: '备用账号', enabled: false }],
])
const runtimeAccount = (id, account) => ({
  account_id: id, provider_id: account.provider, credential_revision: 1, name: account.name,
  email: null, upstream_user_id: null, upstream_account_id: null, plan_type: null, authentication_kind: 'api_key',
  enabled: account.enabled, credential_state: 'ready', has_refresh_token: false,
  access_token_expires_at_ms: null, next_refresh_at_ms: null,
})
let child, buffered, pending, ready, sequence = 99, errors = '', incarnation, handleSequence = 0
let failFinalSave = false, ambiguousUpdate = false

function send(meta, data, payload = Buffer.alloc(0)) {
  if (!(payload instanceof Buffer)) payload = Buffer.from(JSON.stringify(payload))
  if (data !== undefined) {
    const dataBytes = Buffer.from(JSON.stringify(data))
    meta = { ...meta, data_bytes: dataBytes.length }
    payload = Buffer.concat([dataBytes, payload])
  }
  const json = Buffer.from(JSON.stringify(meta)), header = Buffer.alloc(12)
  header.writeUInt32BE(json.length); header.writeBigUInt64BE(BigInt(payload.length), 4)
  child.stdin.write(Buffer.concat([header, json, payload]))
}
function result(id, value = {}, payload = Buffer.alloc(0)) { send({ type: 'result', id }, value, payload) }
function error(id, code = 'rejected') { send({ type: 'error', id, error: { code, message: 'mock rejection', send_state: 'not_sent' } }) }
function httpResult(id, data) {
  const handle = `body_${++handleSequence}`
  handles.set(handle, Buffer.from(JSON.stringify({ code: 200, data })))
  return result(id, { status: 200, version: 'HTTP/1.1', headers: [], body: { kind: 'handle', handle }, response: null, session: false })
}
async function callback(meta, params, payload) {
  reads.push(meta.method)
  const data = payload.length ? JSON.parse(payload.toString()) : params
  if (meta.method === 'host.auth.get_runtime') {
    const account = accounts.get(data.account_id)
    return account ? result(meta.id, {}, runtimeAccount(data.account_id, account)) : error(meta.id)
  }
  if (meta.method === 'host.state.get') {
    assert.equal(data.namespace, 'account-scheduler')
    return result(meta.id, { record: records.get(data.key) ?? null })
  }
  if (meta.method === 'host.state.put') {
    assert.equal(data.namespace, 'account-scheduler')
    const existing = records.get(data.key)
    if ((existing?.version ?? null) !== data.expected_version) return error(meta.id, 'conflict')
    if (failFinalSave && Object.keys(data.value.jobs).length === 0 && Object.values(existing.value.jobs).some(job => job.started)) {
      failFinalSave = false
      return error(meta.id)
    }
    const version = (existing?.version ?? 0) + 1
    records.set(data.key, { value: structuredClone(data.value), version, schema_version: 1 })
    return result(meta.id, { version })
  }
  if (meta.method === 'host.http.dispatch') {
    if (params.method === 'GET') {
      const url = new URL(params.uri, 'http://host.invalid')
      assert.equal(url.pathname, '/api/admin/accounts')
      assert.equal(url.searchParams.get('pageSize'), '50')
      const page = url.searchParams.get('page')
      assert.ok(page === '1' || page === '2')
      const id = `acct_${page}`, account = accounts.get(id)
      const groups = page === '1' ? [{ id: 'group-prod', name: '生产组' }, { id: 'group-svc', name: '共享资源组' }] : []
      const item = { id, ...account, notes: '只留在宿主', groups }
      return httpResult(meta.id, { items: [item], page: { totalPages: 2 } })
    }
    assert.equal(params.uri, '/api/admin/accounts/batch-update')
    assert.equal(params.method, 'POST')
    assert.deepEqual(Object.keys(data).sort(), ['accountIds', 'enabled'])
    const [id] = data.accountIds
    assert.equal(data.accountIds.length, 1)
    assert.equal(typeof data.enabled, 'boolean')
    assert.equal(records.get('schedule').value.jobs[id].started, true)
    accounts.get(id).enabled = data.enabled
    writes.push([id, data.enabled])
    if (ambiguousUpdate) { ambiguousUpdate = false; return error(meta.id, 'uncertain') }
    return httpResult(meta.id, { accountIds: [id] })
  }
  if (meta.method === 'host.http.body_read') {
    const value = handles.get(params.handle)
    if (value) { handles.delete(params.handle); return result(meta.id, { eof: false, trailers: null }, value) }
    return result(meta.id, { eof: true, trailers: null })
  }
  if (meta.method === 'host.http.body_close') return result(meta.id, {})
  throw new Error(`Unexpected host capability ${meta.method}`)
}
function parse() {
  while (buffered.length >= 12) {
    const m = buffered.readUInt32BE(), p = Number(buffered.readBigUInt64BE(4)), total = 12 + m + p
    if (buffered.length < total) return
    const meta = JSON.parse(buffered.subarray(12, 12 + m).toString())
    const dataStart = 12 + m + (meta.request_context_bytes ?? 0)
    const dataEnd = dataStart + (meta.data_bytes ?? 0)
    assert.ok(dataEnd <= total)
    const data = meta.data_bytes ? JSON.parse(buffered.subarray(dataStart, dataEnd).toString()) : undefined
    const payload = buffered.subarray(dataEnd, total)
    buffered = buffered.subarray(total)
    if (meta.type === 'ready') {
      assert.equal(meta.protocol_version, protocolVersion)
      assert.equal(meta.incarnation, incarnation)
      ready.resolve()
    }
    else if (meta.type === 'callback') {
      assert.ok(pending.has(meta.parent_id))
      callback(meta, data, payload).catch(e => { ready.reject(e); for (const x of pending.values()) x.reject(e) })
    }
    else if (meta.type === 'suspend') {
      const waiter = pending.get(meta.id)
      assert.ok(waiter && !waiter.suspended)
      assert.equal(meta.epoch, ++waiter.epoch)
      waiter.suspended = true
    }
    else if (meta.type === 'resume') {
      const waiter = pending.get(meta.id)
      assert.ok(waiter?.suspended)
      assert.equal(meta.epoch, waiter.epoch)
      waiter.suspended = false
      send({ type: 'resumed', id: meta.id, epoch: meta.epoch })
    }
    else if (meta.type === 'result' || meta.type === 'error') {
      const waiter = pending.get(meta.id)
      if (waiter) { pending.delete(meta.id); meta.type === 'error' ? waiter.reject(new Error(JSON.stringify(meta.error))) : waiter.resolve({ result: data, payload }) }
    }
  }
}
async function start() {
  buffered = Buffer.alloc(0); pending = new Map(); ready = Promise.withResolvers(); errors = ''; incarnation = `test_${Date.now()}`
  child = spawn(binary, [], { stdio: ['pipe', 'pipe', 'pipe'] })
  child.stdout.on('data', data => { buffered = Buffer.concat([buffered, data]); parse() })
  child.stderr.on('data', data => { errors += data }); child.on('error', e => ready.reject(e))
  child.on('exit', code => { if (code) { const e = new Error(`Plugin exited ${code}: ${errors}`); ready.reject(e); for (const w of pending.values()) w.reject(e) } })
  send({ type: 'hello', handshake: { protocol_version: protocolVersion, artifact_sha256: 'a'.repeat(64), plugin_id: 'personal.account-scheduler', instance_id: 'test_instance', generation: 1, incarnation, contributes } }, {})
  await Promise.race([ready.promise, new Promise((_, reject) => setTimeout(() => reject(new Error(`Handshake timeout: ${errors}`)), 10000).unref())])
}
async function stop() { const exited = new Promise(resolve => child.once('exit', resolve)); send({ type: 'shutdown' }); child.stdin.end(); await exited }
async function call(method, params = {}, payload = Buffer.alloc(0), stage = 'management') {
  const id = sequence += 2, waiter = Promise.withResolvers(); pending.set(id, waiter)
  waiter.epoch = 0
  waiter.suspended = false
  const timeoutMs = stage === 'registration' ? 5000 : 60000
  send({ type: 'call', id, method, context: { call_id: id, instance_id: 'test_instance', generation: 1, incarnation, stage, timeout_ms: timeoutMs, resource_stream: false, resource_scope_id: `scope_${id}` } }, params, payload)
  return Promise.race([waiter.promise, new Promise((_, reject) => setTimeout(() => reject(new Error(`Call timeout ${method}: ${errors}`)), 10000).unref())])
}
async function request(path, body, query = '') {
  const reply = await call('management.handle', { method: body === undefined ? 'GET' : 'POST', path, query, content_type: body === undefined ? null : 'application/json', headers: [] }, body === undefined ? Buffer.alloc(0) : body)
  return { status: reply.result.status, value: JSON.parse(reply.payload.toString()) }
}

const scan = () => call('plugin.reconcile', {}, Buffer.alloc(0), 'maintenance')
const state = () => records.get('schedule').value
const edit = (rule, expectedVersion, id = null) => request('api/save-rule', { id, rule, expectedVersion })
const sample = { accountIds: ['acct_1'], name: '测试账号晚间停用', action: 'disable', cron: '* * * * *', timezone: 'Asia/Shanghai', enabled: true }
const planDue = () => { for (const rule of state().rules) rule.cursorAt = Math.floor(Date.now() / 1000) - 120 }
const check = label => console.log(`PASS ${label}`)

try {
  await start()
  const installed = await call('plugin.register', {}, Buffer.alloc(0), 'registration')
  assert.deepEqual(installed.result.contributes, contributes)
  const registration = JSON.parse((await call('management.register', {}, Buffer.alloc(0), 'registration')).payload.toString())
  assert.equal(registration.pages[0].entry, 'web/index.html')
  assert.equal(registration.routes.length, 8)
  const first = await request('api/accounts')
  assert.deepEqual(first.value.accounts, [{ id: 'acct_1', name: '工作账号', provider: 'openai', enabled: true, groupNames: ['生产组', '共享资源组'] }])
  assert.equal(first.value.nextCursor, '2')
  const second = await request('api/accounts', undefined, 'cursor=2')
  assert.deepEqual(second.value, { accounts: [{ id: 'acct_2', name: '备用账号', provider: 'xai', enabled: false, groupNames: [] }], nextCursor: null })
  assert.equal((await request('api/accounts', undefined, 'cursor=0')).status, 400)
  check('RPC v7 and paginated account and group projection from the native account directory')

  const upcoming = await request('api/preview', { cron: '0 22 * * 1,2', timezone: 'Asia/Shanghai' })
  assert.equal(upcoming.status, 200)
  assert.equal(upcoming.value.times.length, 5)
  assert.ok(upcoming.value.times.every(at => new Date(at * 1000).getUTCHours() === 14))
  assert.equal((await request('api/preview', { cron: '0 0 22 * * 1', timezone: 'UTC' })).status, 400)
  assert.equal((await request('api/preview', { cron: '* * * * *', timezone: 'Invalid/Zone' })).status, 400)
  check('Cron preview validates five fields and timezone')

  const saved = await edit(sample, 0)
  assert.equal(saved.status, 200)
  assert.equal(saved.value.version, 1)
  const id = saved.value.rules[0].id
  state().rules[0].values = structuredClone(sample)
  const existing = (await request('api/dashboard')).value.rules[0]
  assert.equal(existing.id, id)
  assert.equal(existing.rule.enableCron, null)
  assert.equal(existing.rule.disableCron, sample.cron)
  assert.equal(existing.nextAction, 'disable')
  check('Previously stored single-action rules load without migration')
  assert.equal((await edit(sample, 0)).status, 409)
  const before = records.get('schedule').version
  await scan()
  assert.equal(records.get('schedule').version, before)
  assert.deepEqual(writes, [])
  check('Saving sets a prospective cursor and rejects stale edits')

  planDue()
  await scan()
  assert.deepEqual(writes, [['acct_1', false]])
  assert.equal(state().logs[0].outcome, 'applied')
  const cursorVersion = records.get('schedule').version
  await scan()
  assert.equal(writes.length, 1)
  assert.equal(records.get('schedule').version, cursorVersion)
  check('A due plan only changes the selected account flag once')

  const changed = await edit({ ...sample, action: 'enable' }, 1, id)
  assert.equal(changed.status, 200)
  planDue()
  ambiguousUpdate = true
  await scan()
  assert.deepEqual(writes.at(-1), ['acct_1', true])
  assert.equal(state().logs[0].outcome, 'verified')
  check('An uncertain response is confirmed by reading the account flag')

  assert.equal((await edit(sample, 2, id)).status, 200)
  planDue()
  failFinalSave = true
  await assert.rejects(scan(), /mock rejection/)
  assert.equal(state().jobs.acct_1.started, true)
  assert.equal(accounts.get('acct_1').enabled, false)
  const priorWrites = writes.length
  await stop()
  await start()
  await scan()
  assert.equal(writes.length, priorWrites)
  assert.equal(state().logs[0].outcome, 'verified')
  assert.deepEqual(state().jobs, {})
  check('Restart finalizes an applied action without replaying the account update')

  const opposite = { ...sample, name: '冲突规则', action: 'enable' }
  const rejected = await edit(opposite, 3)
  assert.equal(rejected.status, 409)
  assert.match(rejected.value.error.message, /测试账号晚间停用.*工作账号.*Asia\/Shanghai/)
  const paused = await edit({ ...opposite, enabled: false }, 3)
  assert.equal(paused.status, 200)
  const oppositeId = paused.value.rules.find(row => row.rule.name === opposite.name).id
  assert.equal((await edit(opposite, 4, oppositeId)).status, 409)
  check('Save and reactivation reject opposing plans while allowing paused drafts')
  // 模拟新校验上线之前已保存的冲突计划，保留运行时兜底覆盖。
  state().rules.find(row => row.id === oppositeId).values.enabled = true
  planDue()
  await scan()
  assert.equal(writes.length, priorWrites)
  assert.equal(state().logs[0].outcome, 'conflict')
  const filtered = await request('api/logs', undefined, 'page=1&accountId=acct_1&outcome=conflict')
  assert.equal(filtered.value.total, 1)
  assert.equal(filtered.value.records[0].action, null)
  check('Conflicting plans leave the target unchanged and appear in log filters')

  state().logs.at(-1).executedAt -= 86401 * 2
  const savedRetention = await request('api/retention', { days: 1, expectedVersion: 4 })
  assert.equal(savedRetention.value.retentionDays, 1)
  const allLogs = await request('api/logs')
  assert.equal(allLogs.value.total, 3)
  assert.equal((await request('api/retention', { days: 0, expectedVersion: 5 })).status, 400)
  const removed = await request('api/delete-rule', { id, expectedVersion: 5 })
  assert.equal(removed.value.rules.length, 1)
  assert.equal((await request('api/logs')).value.total, 3)
  assert.ok(!reads.includes('host.auth.get') && !reads.includes('host.auth.save'))
  check('Retention removes expired records and deleting a rule preserves its history')

  const pair = { name: '上班启停计划', accountIds: ['acct_2'], enableCron: '0 8 * * 2,3', disableCron: '0 22 * * 1,2', timezone: 'Asia/Shanghai', enabled: true }
  const selfConflict = await edit({ ...pair, enableCron: '0 22 * * 1', disableCron: '0 22 * * 1,2' }, 6)
  assert.equal(selfConflict.status, 409)
  assert.match(selfConflict.value.error.message, /本计划的启用与停用时间冲突.*备用账号/)
  assert.equal((await edit({ ...pair, enableCron: null, disableCron: null }, 6)).status, 400)
  const savedPair = await edit(pair, 6)
  assert.equal(savedPair.status, 200)
  const existingPair = savedPair.value.rules.find(row => row.rule.name === pair.name)
  assert.deepEqual(existingPair.rule, pair)
  const enableAt = (await request('api/preview', { cron: pair.enableCron, timezone: pair.timezone })).value.times[0]
  const disableAt = (await request('api/preview', { cron: pair.disableCron, timezone: pair.timezone })).value.times[0]
  assert.equal(existingPair.nextAt, Math.min(enableAt, disableAt))
  assert.equal(existingPair.nextAction, enableAt < disableAt ? 'enable' : 'disable')
  check('A plan saves both actions and reports its earliest upcoming action')

  const current = Math.floor(Date.now() / 1000), scheduledAt = current - 600
  const partial = { ruleId: 'sample_group', ruleName: '同名计划', scheduledAt, action: 'enable' }
  state().logs.push(
    { ...partial, accountId: 'acct_1', executedAt: current - 30, outcome: 'unchanged' },
    { ...partial, accountId: 'acct_2', executedAt: current - 20, outcome: 'failed' },
    { ...partial, ruleId: 'different_group', accountId: 'acct_2', executedAt: current - 10, outcome: 'failed' },
    { ...partial, ruleId: 'unclear_group', accountId: 'acct_2', executedAt: current - 10, outcome: 'uncertain' },
  )
  const mixed = await request('api/runs', undefined, 'accountId=acct_1&outcome=partial_success')
  assert.equal(mixed.value.total, 1)
  assert.equal(mixed.value.records[0].details.length, 2)
  assert.equal(mixed.value.records[0].successful, 1)
  assert.equal(mixed.value.records[0].executedAt, current - 20)
  assert.equal((await request('api/runs', undefined, 'outcome=all_failed')).value.records[0].ruleId, 'different_group')
  assert.equal((await request('api/runs', undefined, 'outcome=needs_review')).value.records[0].ruleId, 'unclear_group')
  state().jobs.acct_3 = { ...partial, started: false }
  const pendingRun = (await request('api/runs', undefined, 'outcome=pending')).value.records[0]
  assert.equal(pendingRun.details.length, 3)
  assert.equal(pendingRun.details.find(detail => detail.accountId === 'acct_3').outcome, null)
  check('Plan runs group by identity and time, preserve all details after filtering, and distinguish pending and uncertain results')
  await stop()
} catch (cause) {
  child?.kill()
  console.error(cause)
  process.exitCode = 1
}
