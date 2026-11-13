/* Where a memory came from and what it is about, as links: the knowledge page's memory rows, a subject's page
 * and the steward's report all say it the same way. */
import type { ReactNode } from "react"
import { cn } from "cn"
import { tm } from "@/store"
import { monthDayShort } from "@/i18n/format"
import { t } from "@/i18n"
import type { Mem, ObjRef } from "@/api/knowledge"

/** a subject's page on the knowledge page: `#/knowledge/about/<type>:<id>` */
export const subjectHref = (a: { type: string; id: string }) => `#/knowledge/about/${encodeURIComponent(a.type)}:${encodeURIComponent(a.id)}`
export const parseSubject = (s: string): { type: string; id: string } | null => {
  const at = s.indexOf(":"); if (at <= 0) return null
  return { type: decodeURIComponent(s.slice(0, at)), id: decodeURIComponent(s.slice(at + 1)) }
}
/** what kind of thing a subject is: 联系人, 项目, Bot … */
export const subjectKind = (type: string) => (["party", "project", "bot", "item", "space", "persona", "me"].includes(type) ? t(`knowledge:subject.kind.${type as "party"}`) : type)
export const aboutName = (a: ObjRef) => a.name || (a.type === "party" ? t("knowledge:mems.aboutParty", { id: a.id }) : a.type === "item" ? t("knowledge:mems.aboutItem", { id: a.id }) : a.id)

/** where it came from, in words: “你写的”, “11-12 的对话”, “ST-12 交付时” … */
export const originLabel = (m: Mem) => (m.origin?.kind === "you" || (!m.origin && !m.source_label) ? t("knowledge:mems.youWrote") : m.origin?.label || m.source_label || t("knowledge:mems.youWrote"))
/** the hover text of a source: who wrote it down and the words it was based on */
export const originTitle = (m: Mem) => {
  const o = m.origin; if (!o) return originLabel(m)
  return [originLabel(m), o.kind !== "you" && o.writer_name ? t("knowledge:origin.by", { name: o.writer_name }) : "", o.evidence ? t("knowledge:cand.evidence", { evidence: o.evidence }) : ""].filter(Boolean).join("\n")
}

const LINK = "rounded-sm underline-offset-2 hover:text-foreground hover:underline focus-visible:outline-2 focus-visible:outline-ring"
/** a link inside a muted meta line; plain text when there is nowhere to go */
export function MetaLink({ href, children, title, className, ...rest }: { href?: string; children: ReactNode; title?: string; className?: string; [k: `data-${string}`]: unknown }) {
  if (!href) return <span title={title} className={className} {...rest}>{children}</span>
  return <a href={href} title={title} className={cn(LINK, className)} onClick={(e) => e.stopPropagation()} {...rest}>{children}</a>
}

/** “关于 李总 · 来源：11-12 的对话”, each part a link */
export function OriginLine({ m, about = true }: { m: Mem; about?: boolean }) {
  const abouts = about ? m.about : []
  return <>
    {abouts.length > 0 && <>{t("knowledge:origin.about")}{abouts.map((a, n) => <span key={a.type + a.id}>{n > 0 && t("knowledge:listSep")}<MetaLink href={subjectHref(a)} data-about-link={a.id}>{aboutName(a)}</MetaLink></span>)}<span aria-hidden> · </span></>}
    {t("knowledge:origin.from")}<MetaLink href={m.origin?.href} title={originTitle(m)} data-origin-link={m.origin?.kind ?? "you"}>{originLabel(m)}</MetaLink>
  </>
}
/** the plain text of the same line (the panel's title and search) */
export const originText = (m: Mem) => [m.about.length ? t("knowledge:mems.aboutLabel", { names: m.about.map(aboutName).join(t("knowledge:listSep")) }) : "", t("knowledge:panel.source", { source: originLabel(m) })].filter(Boolean).join(" · ")

/** “记于 11-11 · 用过 18 次” */
export const savedLine = (m: Mem) => [m.created ? t("knowledge:panel.saved", { date: monthDayShort(m.created) }) : "", m.used?.count ? t("knowledge:origin.used", { count: m.used.count }) : ""].filter(Boolean).join(" · ")
/** “最近用于 11-13 10:24” */
export const lastUsed = (m: Mem) => (m.used?.last ? t("knowledge:origin.lastUsed", { time: tm(m.used.last) }) : "")
