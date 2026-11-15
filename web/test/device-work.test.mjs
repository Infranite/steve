import assert from 'node:assert/strict'
import { test } from 'node:test'
import { groupDeviceWork } from '../src/device-work.ts'
import { sampleState, vitalsFacts, chartPaths, gib } from '../src/device-vitals-model.ts'
const s = (id, status, extra = {}) => ({ id, status, device_id: 'd1', readonly: false, busy: false, updated: 100, queued: [], ...extra })
const d = (id, sid) => ({ id, session_id: sid, status: 'open', item_id: 'item-' + sid })
test('raw running/idle requests are exclusive; duplicates and readonly do not inflate load', () => {
  const rows = [s('run', 'running'), s('idle', 'idle'), s('start', 'starting'), s('run', 'running'), s('busy', 'idle', { busy: true }), s('fail', 'error'), s('ro', 'running', { readonly: true }), s('end', 'ended'), s('old', 'idle')]
  const g = groupDeviceWork('d1', rows, [d('dr', 'run'), d('di', 'idle'), d('dr', 'run'), d('dr2', 'run')])
  assert.deepEqual(g.attention.map(r => r.session.id), ['idle', 'run'])
  assert.equal(g.attention[1].session.status, 'running')
  assert.equal(g.attention[0].session.status, 'idle')
  assert.deepEqual(g.current.map(r => r.session.id), ['fail', 'start', 'busy'])
  assert.deepEqual(g.recent.map(r => r.session.id), ['end', 'old', 'ro'])
  assert.equal(g.running, 0); assert.equal(g.starting, 1)
  assert.equal(g.attention[1].decisions.length, 2)
})
test('queue counts messages and sessions with stable IDs; unowned/foreign/readonly are excluded', () => {
  const rows = [s('a', 'idle', { queued: [{ id: 'q1' }, { id: 'q1' }, { id: 'q2', session_id: 'a' }, { id: 'wrong', session_id: 'b' }] }), s('b', 'running', { queued: [{ id: 'q3' }] }), s('unknown', 'idle', { device_id: null, queued: [{ id: 'q4' }] }), s('foreign', 'idle', { device_id: 'd2', queued: [{ id: 'q5' }] }), s('ro', 'idle', { readonly: true, queued: [{ id: 'q6' }] })]
  const g = groupDeviceWork('d1', rows, [d('request', 'b')])
  assert.equal(g.queueMessages, 3); assert.equal(g.queueSessions, 2)
  assert.deepEqual(g.current.map(r => r.session.id), ['a']); assert.equal(g.attention[0].queue[0].id, 'q3')
})
test('ended requests remain actionable and errors persist until explicit closure', () => {
  const rows = [s('closed', 'error', { item_id: 'closed' }), s('unresolved', 'error'), s('ended', 'ended')]
  const g = groupDeviceWork('d1', rows, [d('de', 'ended')], new Set(['closed']))
  assert.equal(g.attention[0].session.status, 'ended')
  assert.equal(g.current[0].session.id, 'unresolved'); assert.equal(g.recent[0].session.id, 'closed')
  assert.equal(groupDeviceWork('x', rows, []).current.length, 0)
})
test('GiB, load per CPU and distinct roots retain evidence; no invented unknown zero', () => {
  const v = { latest: { at: 1000, cpus: 4, load: [8, 4, 2], mem_total: 8 * 2**30, mem_available: 2 * 2**30, roots: [{root:'a', free:12*2**30, total:1024*2**30},{root:'b',free:12*2**30,total:1024*2**30}] }, series: [] }
  const f = vitalsFacts(v)
  assert.equal(f.loadRatio, 2); assert.equal(f.memUsed, .75); assert.equal(f.disks.length, 2); assert.equal(gib(f.disks[0].free), 12)
  assert.equal(vitalsFacts({latest:{at:1},series:[]}).memUsed, null)
  assert.equal(sampleState(undefined, false, 0), 'loading'); assert.equal(sampleState(null, false, 0), 'none')
  assert.equal(sampleState(v, true, 0), 'failed'); assert.equal(sampleState(v, false, 0), 'unknown')
  v.sampling = { interval_ms:60000, stale_after_ms:135000 }
  assert.equal(sampleState(v, false, 136000), 'fresh'); assert.equal(sampleState(v, false, 136001), 'stale')
})
test('trend breaks on null and missing intervals and preserves actual scale beyond 100%', () => {
  const v = {latest:{at:300},series:[{at:100,load:2},{at:150,load:null},{at:200,load:1},{at:300,load:3}],gaps:[{from:200,to:300}]}
  const c = chartPaths(v, 'load'); assert.equal(c.max, 3); assert.equal(c.paths.length, 3)
  assert.ok(c.paths.every(p => !p.includes('NaN')))
})
