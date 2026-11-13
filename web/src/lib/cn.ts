/* `cn` for the whole client (vite and tsconfig alias the bare "cn" import here): the cn package's merge, plus the
 * type scale. The type-* utilities set font size and leading, but the merge does not know that, so a component's
 * own text-sm / leading-normal (or an earlier type-*) would win by CSS order. A type-* class drops the size and
 * leading classes before it, whatever their breakpoint. */
import { createCn } from "cn/engine"
import tables from "cn/tables"

const merge = createCn(tables)
const TYPE = /^(?:[a-z0-9-]+:)*type-(?:display|large-title|title|headline|body|chat|callout|footnote|label|code)!?$/
const SIZE = /^(?:[a-z0-9-]+:)*(?:text-(?:xs|sm|base|lg|xl|2xl|\[\d[^\]]*\])|leading-\S+)!?$/

export const cn: typeof merge = (...args) => {
  const xs = merge(...args).split(" ")
  const last = xs.findLastIndex((x) => TYPE.test(x))
  if (last < 0) return xs.join(" ")
  return xs.filter((x, n) => n >= last || !(SIZE.test(x) || TYPE.test(x))).join(" ")
}
export { clsx, twJoin } from "cn/engine"
