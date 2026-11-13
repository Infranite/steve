/* 用量明细里的费用构成（Compact Breakdown）分成几份：最多和颜色一样多份，一份一个颜色；多出来的并成一份灰色的“其他”。
 * 单独成文件，测试能直接引用。 */

export type SplitPart = { id: string; label: string; value: number; color: string }

/** the parts of the range's estimated cost (tokens while nothing is priced): at most `colors.length` parts, each its
 *  own colour; when there are more, the largest `colors.length - 1` and the rest folded into one "others" */
export function splitParts<R extends { key: string; label: string; est: number; total: number }>(rows: R[], colors: readonly string[], other: { color: string; label: (count: number) => string }, unknown: string): { cost: boolean; items: SplitPart[] } {
  const cost = rows.some((r) => r.est > 0)
  const val = (r: R) => (cost ? r.est : r.total)
  const ranked = rows.filter((r) => val(r) > 0).sort((a, b) => val(b) - val(a))
  const keep = ranked.length > colors.length ? ranked.slice(0, colors.length - 1) : ranked, rest = ranked.slice(keep.length)
  const items = [...keep.map((r, i) => ({ id: r.key || "-", label: r.label || unknown, value: val(r), color: colors[i] })),
    ...(rest.length ? [{ id: "other", label: other.label(rest.length), value: rest.reduce((a, r) => a + val(r), 0), color: other.color }] : [])]
  return { cost, items }
}
