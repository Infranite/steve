/* a relative .ts import so node --test loads this file without a bundler */
import { t } from "./i18n/index.ts"

/** A short page can be a byte-budget boundary. Only an empty page is EOF. */
export async function readEventPages<T extends { seq: number }>(after: number, fetchPage: (after: number) => Promise<T[]>, onPage?: (page: T[]) => void): Promise<T[]> {
  const out: T[] = []
  for (;;) {
    const page = await fetchPage(after)
    if (!page.length) return out
    for (const event of page) {
      if (!Number.isSafeInteger(event.seq) || event.seq <= after) throw new Error(t("shell:misc.cursorStuck"))
      after = event.seq
      out.push(event)
    }
    onPage?.(page)
  }
}
