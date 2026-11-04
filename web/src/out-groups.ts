/* an item's outputs sorted for the 产出 tab: the latest delivery's results are the outputs; records of the work
 * (self-check screenshots, logs) and what earlier deliveries handed over go to the work record, kept by delivery.
 * An output delivered again (same kind and location, or title) is a new version of it. Pure, so it is tested alone. */

/** kind, title, location, artifact id, delivered at, role: the `Out` of store.ts */
export type OutRow = [string, string, string | null, string, number?, ("record" | "result")?]
export type OutV<O extends OutRow = OutRow> = { o: O; v: number; vs: number; delivery: number; record: boolean; old: boolean }
export type OutGroups<O extends OutRow = OutRow> = { final: OutV<O>[]; records: { delivery: number; at: number; outs: OutV<O>[] }[] }

/** outputs written by one `steve_deliver` land within moments of each other */
const SAME_DELIVERY_MS = 3000
/** what outputs the bot gave no role (and those from before roles) are recognised as records by */
const RECORD_WORDS = /自查|自测|调试(记录|日志|截图|输出)|\bdebug\b|(运行|调试|测试|构建)日志|日志$|\blogs?$|中间(结果|产物|文件|版本)|临时|草稿|过程记录|执行记录/i

export const outKey = (o: OutRow) => `${o[0]}\u0001${o[2] || o[1]}`
export const isRecord = (o: OutRow) => o[5] ? o[5] === "record" : RECORD_WORDS.test(o[0]) || RECORD_WORDS.test(o[1])

export function groupOuts<O extends OutRow>(outs: O[]): OutGroups<O> {
  const rows = outs.map((o, n) => ({ o, n })).sort((a, b) => (a.o[4] ?? 0) - (b.o[4] ?? 0) || a.n - b.n)
  /* deliveries: runs of outputs close in time, numbered from 1 */
  let d = 0, last = -Infinity
  const seen = new Map<string, number>(), total = new Map<string, number>()
  for (const { o } of rows) total.set(outKey(o), (total.get(outKey(o)) ?? 0) + 1)
  const vs: OutV<O>[] = rows.map(({ o }) => {
    const at = o[4] ?? 0
    if (d === 0 || at - last > SAME_DELIVERY_MS) d++
    last = at
    const k = outKey(o), v = (seen.get(k) ?? 0) + 1
    seen.set(k, v)
    return { o, v, vs: total.get(k)!, delivery: d, record: isRecord(o), old: false }
  })
  /* the outputs: the results of the newest delivery that has any */
  const cur = Math.max(0, ...vs.filter((x) => !x.record).map((x) => x.delivery))
  const final = vs.filter((x) => x.delivery === cur && !x.record)
  const rest = vs.filter((x) => !final.includes(x)).map((x) => ({ ...x, old: !x.record }))
  const records = [...new Set(rest.map((x) => x.delivery))].sort((a, b) => b - a).map((delivery) => {
    const outs = rest.filter((x) => x.delivery === delivery)
    return { delivery, at: Math.min(...outs.map((x) => x.o[4] ?? 0)), outs }
  })
  return { final, records }
}

const IMAGE = /\.(png|jpe?g|gif|webp|avif|bmp|svg)$/i
/** an output that is one picture, by its location */
export const isImageOut = (o: OutRow) => !!o[2] && IMAGE.test(o[2].trim())
