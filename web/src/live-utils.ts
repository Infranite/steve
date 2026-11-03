export function debounceMax(f: () => void, ms = 300, maxWait = 1000) {
  let timer: ReturnType<typeof setTimeout> | undefined, due: number | undefined
  const cancel = () => { clearTimeout(timer); timer = undefined; due = undefined }
  const schedule = () => { due ??= Date.now() + maxWait; clearTimeout(timer); timer = setTimeout(() => { cancel(); f() }, Math.max(0, Math.min(ms, due - Date.now()))) }
  return { schedule, cancel }
}
export async function bounded<T>(xs: T[], run: (x: T) => Promise<unknown>, limit = 3) {
  let next = 0
  await Promise.all(Array.from({ length: Math.min(limit, xs.length) }, async () => { while (next < xs.length) await run(xs[next++]) }))
}
export function matchesSpace(e: { space_id?: string }, space?: string) { return !e.space_id || e.space_id === space }
