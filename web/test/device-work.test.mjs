import assert from 'node:assert/strict'
import { test } from 'node:test'
import { groupDeviceWork } from '../src/device-work.ts'
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
test('authoritative room waits stay current without inflating execution or losing Node reasons', () => {
  const room = { running: 32, max: 32, waiting: 2, reason: 'capacity', detail: null }
  const refused = { running: 1, max: 32, waiting: 1, reason: 'node', detail: 'workspace storage unavailable' }
  const rows = [s('running', 'running'), s('capacity', 'running', { room }), s('node', 'idle', { room: refused }), s('needs', 'running', { room })]
  const g = groupDeviceWork('d1', rows, [d('request', 'needs')])
  assert.equal(g.running, 1)
  assert.equal(g.waiting, 2)
  assert.deepEqual(g.attention.map(r => r.session.id), ['needs'])
  assert.equal(g.current.find(r => r.session.id === 'node').session.room.detail, 'workspace storage unavailable')
  assert.equal(g.recentTotal, 0)
})
