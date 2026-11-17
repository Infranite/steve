import assert from 'node:assert/strict'
import { test } from 'node:test'
import { hubDirectory, rememberHub, savedHubs, normalizeAlias, setSharedHubName } from '../src/hub.ts'
import { hubDraftErrors, linkedHubDevice, runtimeSetupHrefFor, normalizeHubDraft, parseHubInfo, parseHubSnapshot, UnsupportedHubMetadata } from '../src/hub-management-model.ts'

test('Hub device link requires a unique explicit inventory binding, never a name or role', () => {
  const named = { id: 'inventory-hub', name: 'Same name', roles: ['Hub'], node_id: null }
  const local = { id: 'inventory-local', name: 'A renamed device', node_id: 'local' }
  assert.equal(linkedHubDevice([named]), undefined)
  assert.equal(linkedHubDevice([{ id: 'legacy', name: 'Hub' }]), undefined)
  assert.equal(linkedHubDevice([named, local]), local)
  assert.equal(linkedHubDevice([local, { id: 'other', name: 'Other', node_id: 'local' }]), undefined)
})

test('Unicode scalar limits, newline normalization and plain text', () => {
  assert.deepEqual(hubDraftErrors({display_name:'😀'.repeat(64),purpose_note:'😀'.repeat(500)}),{name:false,note:false})
  assert.equal(hubDraftErrors({display_name:'😀'.repeat(65),purpose_note:''}).name,true)
  assert.equal(hubDraftErrors({display_name:' \n ',purpose_note:''}).name,true)
  assert.equal(hubDraftErrors({display_name:'a\nb',purpose_note:''}).name,true)
  assert.equal(hubDraftErrors({display_name:'a',purpose_note:'😀'.repeat(501)}).note,true)
  assert.equal(hubDraftErrors({display_name:'a',purpose_note:'\0'}).note,true)
  assert.deepEqual(normalizeHubDraft({display_name:'  a  ',purpose_note:'<b>x</b>\r\nx\ry'}),{display_name:'a',purpose_note:'<b>x</b>\nx\ny'})
  assert.equal(parseHubSnapshot({metadata:{hub_id:"fake"},seen:100}),null)
  assert.throws(()=>parseHubInfo({}),UnsupportedHubMetadata)
  assert.throws(()=>parseHubInfo({contract:1,capabilities:{shared_metadata:1},metadata:{}}))
})

test('browser rename and clear persist without selecting or losing version; failures reject', async () => {
  const map = new Map()
  let fail = false
  globalThis.localStorage = {getItem:k=>map.get(k)??null,setItem:(k,v)=>{if(fail) throw Error('full');map.set(k,v)},removeItem:k=>map.delete(k)}
  globalThis.location = { origin:'https://hub.example.com' }
  try {
    rememberHub(location.origin,{name:'Old',version:'1.2.3',now:100})
    map.set('steve.hub','https://hub.example.com')
    const dir = hubDirectory(), entry = (await dir.list())[0]
    await dir.rename(entry,'  私人😀  ')
    assert.equal(savedHubs()[0].name,'私人😀')
    assert.equal(savedHubs()[0].version,'1.2.3')
    assert.equal(savedHubs()[0].lastUsed,100)
    assert.equal(map.get('steve.hub'),location.origin)
    setSharedHubName('Team')
    assert.equal((await dir.list())[0].name,'私人😀 · Team')
    await dir.rename(entry,'')
    assert.equal(savedHubs()[0].name,undefined)
    assert.equal((await dir.list())[0].name,'Team')
    setSharedHubName('Newest', {hub_id:'hub_stable',revision:5})
    setSharedHubName('Late old response', {hub_id:'hub_stable',revision:4})
    assert.equal((await dir.list())[0].name,'Newest')
    fail = true
    await assert.rejects(dir.rename(entry,'Unsaved'))
    assert.equal(savedHubs()[0].name,undefined)
    assert.throws(()=>normalizeAlias('😀'.repeat(65)))
    assert.equal(normalizeAlias('😀'.repeat(64)),'😀'.repeat(64))
  } finally { delete globalThis.localStorage; delete globalThis.location }
})

test('native rename uses dedicated capability and preserves legacy bridge behavior', async () => {
  const calls = []
  globalThis.window = {__STEVE_NATIVE__:{v:2,platform:'ios',can:['hubs.list'],call:async(op,args)=>{calls.push([op,args]);return {hubs:[]}}}}
  try {
    assert.equal(hubDirectory().rename,undefined)
    window.__STEVE_NATIVE__.can.push('hubs.rename')
    await hubDirectory().rename({id:'stable-profile',address:'https://hub.example.com'},'')
    assert.deepEqual(calls,[['hubs.rename',{id:'stable-profile',alias:''}]])
    window.__STEVE_NATIVE__.call=async()=>{throw Error('disk full')}
    await assert.rejects(hubDirectory().rename({id:'stable-profile'},'New'))
  } finally { delete globalThis.window }
})

test('harness setup links open the Hub\'s own Node page, or the Node list when the binding is unknown', () => {
  assert.equal(runtimeSetupHrefFor([{ id: 'd1', name: 'Studio Mac', node_id: 'local' }]), '#/devices/nodes/local')
  assert.equal(runtimeSetupHrefFor([]), '#/devices/nodes')
  assert.equal(runtimeSetupHrefFor([{ id: 'd1', name: 'A', node_id: 'local' }, { id: 'd2', name: 'B', node_id: 'local' }]), '#/devices/nodes')
})
