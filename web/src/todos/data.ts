/* 待办's data: the routes of steve-digest (`/digest/todos`, each member's own), and one list the sidebar's count and
 * the pages share. It is loaded once per workspace and again on every `digest` stream event, so a todo an agent adds
 * in a conversation shows up without a reload. Every change answers with the id of an undoable change. */
import { useEffect, useState, useSyncExternalStore } from "react"
import { isNotWired, modReq, qs } from "@/api"
import { t } from "@/i18n"
import { st, toast, useHubEvents, type ModuleEvent } from "@/store"
import type { HubEvent } from "@/api"
import type { Goal, Todo, TodoState } from "./model"

export type NewTodo = { text: string; kind?: "todo" | "goal"; parent?: string; due?: number; due_date?: string; target_date?: string; pri?: number; star?: boolean; recur?: string }
/** an absent field stays, `null` clears it */
export type TodoPatch = Partial<{ text: string; parent: string | null; due: number | null; due_date: string | null; target_date: string | null; pri: number; star: boolean; recur: string | null; snooze_until: number | null }>
type Changed = { change?: string | null }

const enc = encodeURIComponent
export const todosApi = {
  list: (q: { state?: TodoState; kind?: "todo" | "goal"; parent?: string; limit?: number } = {}) => modReq<{ todos: Todo[] }>("GET", "/digest/todos" + qs(q)),
  get: (id: string) => modReq<Todo | Goal>("GET", `/digest/todos/${enc(id)}`),
  add: (b: NewTodo) => modReq<{ id: string; change: string }>("POST", "/digest/todos", b),
  patch: (id: string, p: TodoPatch) => modReq<{ ok: true; change: string }>("PATCH", `/digest/todos/${enc(id)}`, p),
  state: (id: string, state: TodoState, item?: string) => modReq<{ ok: true; change: string; next: string | null }>("POST", `/digest/todos/${enc(id)}`, item ? { state, item } : { state }),
  link: (id: string, item: string, linked = true) => modReq<{ changed: boolean; change: string | null }>("POST", `/digest/todos/${enc(id)}/items`, { item, linked }),
  undo: (change: string) => modReq<unknown>("POST", `/digest/changes/${enc(change)}/undo`, {}),
}

/** how many todos the shared list holds: the Hub's most per request */
const LIST_LIMIT = 500
type Shared = { ws: string | null; todos: Todo[] | null; error: string | null; unwired: boolean; ver: number }
const shared: Shared = { ws: null, todos: null, error: null, unwired: false, ver: 0 }
const subs = new Set<() => void>()
const bump = () => { shared.ver++; subs.forEach((f) => f()) }
let inflight: Promise<void> | null = null, again = false

/** (re)loads the list; a call while one is on its way loads once more after it */
export function loadTodos(): Promise<void> {
  if (inflight) { again = true; return inflight }
  const ws = st.workspace?.id ?? null
  if (shared.ws !== ws) { shared.ws = ws; shared.todos = null; shared.error = null; bump() }
  inflight = todosApi.list({ limit: LIST_LIMIT }).then((r) => {
    if (ws !== (st.workspace?.id ?? null)) return
    shared.todos = r.todos; shared.error = null; shared.unwired = false
  }, (e) => {
    shared.unwired = isNotWired(e); shared.error = (e as Error).message || t("core:error.generic")
  }).finally(() => {
    inflight = null; bump()
    if (again) { again = false; void loadTodos() }
  })
  return inflight
}

/** the signed-in person's todos and goals in this workspace; null until loaded */
export function useTodos() {
  useSyncExternalStore((f) => { subs.add(f); return () => { subs.delete(f) } }, () => shared.ver)
  const ws = st.workspace?.id ?? null
  useEffect(() => { if (shared.ws !== ws || (!shared.todos && !inflight)) void loadTodos() }, [ws])
  useHubEvents(["digest"], () => void loadTodos(), 300, { filter: isTodoEvent })
  return shared
}
/** a `digest` event about a todo: every todo write names the todo (and its goal) by its `td_` id. The digest's other
 *  writes (packets, matters, summaries of the owner's conversations) go to the whole workspace too and change no todo */
export const isTodoEvent = (e: HubEvent | ModuleEvent) => e.type !== "digest" || ("id" in e && typeof e.id === "string" && e.id.startsWith("td_"))

/** the time, moved on every minute: which group a todo is in changes with the clock */
export function useMinute() {
  const [now, setNow] = useState(Date.now)
  useEffect(() => { const h = setInterval(() => setNow(Date.now()), 60_000); return () => clearInterval(h) }, [])
  return now
}

const failed = (e: unknown) => toast((e as Error).message || t("core:error.generic"))
/** undoes a change of one's own todos, and says so */
export async function undoChange(change: string) {
  try { await todosApi.undo(change); toast(t("todos:toast.undone")) } catch (e) { failed(e) }
  void loadTodos()
}
/** runs a change; afterwards the list reloads and `done` is said with 撤销 for the change it made. Errors (the Hub's
 *  own words, in the page's language) are said too; the answer is undefined then. */
export async function change<T extends Changed>(p: Promise<T>, done?: string): Promise<T | undefined> {
  try {
    const r = await p
    if (done) toast(done, r.change ? { action: { label: t("todos:action.undo"), onClick: () => void undoChange(r.change!) } } : undefined)
    return r
  } catch (e) { failed(e); return undefined } finally { void loadTodos() }
}
