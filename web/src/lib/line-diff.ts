/* A line diff for the before/after of a drafted text (a Bot's working rules or system prompt): the longest common
 * subsequence of the lines, then runs of unchanged lines far from a change folded into one "n lines unchanged" mark.
 * Plain data, no React, so node --test can load it. */

export type DiffLine = { k: "same" | "add" | "del"; text: string } | { k: "fold"; n: number }

/** the lines of `a` and `b` in order, each kept, removed or added */
export function lineDiff(a: string, b: string): { k: "same" | "add" | "del"; text: string }[] {
  const x = a === "" ? [] : a.split("\n"), y = b === "" ? [] : b.split("\n")
  const n = x.length, m = y.length
  /* lcs[i][j]: the common lines of x[i..] and y[j..] */
  const lcs: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0))
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) lcs[i][j] = x[i] === y[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1])
  const out: { k: "same" | "add" | "del"; text: string }[] = []
  let i = 0, j = 0
  while (i < n && j < m) {
    if (x[i] === y[j]) { out.push({ k: "same", text: x[i] }); i++; j++ }
    else if (lcs[i + 1][j] >= lcs[i][j + 1]) out.push({ k: "del", text: x[i++] })
    else out.push({ k: "add", text: y[j++] })
  }
  while (i < n) out.push({ k: "del", text: x[i++] })
  while (j < m) out.push({ k: "add", text: y[j++] })
  return out
}

/** the diff with unchanged runs more than `context` lines away from a change folded */
export function foldedDiff(a: string, b: string, context = 2): DiffLine[] {
  const d = lineDiff(a, b)
  const near = d.map(() => false)
  d.forEach((l, n) => { if (l.k !== "same") for (let x = Math.max(0, n - context); x <= Math.min(d.length - 1, n + context); x++) near[x] = true })
  const out: DiffLine[] = []
  for (let n = 0; n < d.length; ) {
    if (near[n]) { out.push(d[n++]); continue }
    let e = n; while (e < d.length && !near[e]) e++
    /* a single far line is cheaper to show than its fold mark */
    if (e - n === 1) out.push(d[n]); else out.push({ k: "fold", n: e - n })
    n = e
  }
  return out
}

/** how many lines were added and removed */
export const diffCounts = (a: string, b: string) => lineDiff(a, b).reduce((c, l) => (l.k === "add" ? { ...c, add: c.add + 1 } : l.k === "del" ? { ...c, del: c.del + 1 } : c), { add: 0, del: 0 })
