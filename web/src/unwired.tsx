/* Modules that are designed but not wired to the Hub yet. They keep their place in the navigation and say plainly
 * what they will do and when — no sample data, nothing that looks like it works. */
import { BookOpenIcon, ContactRoundIcon, KeyRoundIcon, MessagesSquareIcon, PlugIcon, RadioIcon, UserRoundIcon, WorkflowIcon, LockIcon, MailIcon, ChartNoAxesColumnIcon, type LucideIcon } from "lucide-react"
import { Page, SectionLabel, Group, Row } from "@/ui"
import { useT } from "@/i18n"

type Mod = { icon: LucideIcon; phase: "p1" | "p3" | "p4" | "rustHub"; now?: true }
/* what each module will do; the words are in shell:unwired.<id>, read at render time */
export const UNWIRED = {
  contacts: { icon: ContactRoundIcon, phase: "p1" },
  signals: { icon: RadioIcon, phase: "p3", now: true },
  auto: { icon: WorkflowIcon, phase: "p4" },
  accounts: { icon: KeyRoundIcon, phase: "p3" },
  vault: { icon: LockIcon, phase: "p4" },
  personas: { icon: UserRoundIcon, phase: "p3" },
  mail: { icon: MailIcon, phase: "rustHub", now: true },
  github: { icon: PlugIcon, phase: "rustHub", now: true },
  knowledge: { icon: BookOpenIcon, phase: "p3", now: true },
  stats: { icon: ChartNoAxesColumnIcon, phase: "rustHub", now: true },
  integrations: { icon: PlugIcon, phase: "rustHub", now: true },
  channels: { icon: MessagesSquareIcon, phase: "rustHub", now: true },
} satisfies Record<string, Mod>
type Id = keyof typeof UNWIRED

export function Unwired({ id }: { id: keyof typeof UNWIRED | string }) {
  const t = useT()
  const key: Id = Object.hasOwn(UNWIRED, id) ? (id as Id) : "contacts", m: Mod = UNWIRED[key], I = m.icon
  /* only some modules have their own "now" line; the key is built from the id, so it is checked by hand */
  const now = m.now ? t(`shell:unwired.${key}.now` as "shell:unwired.signals.now") : t("shell:unwired.notBuilt")
  const will = t(`shell:unwired.${key}.will`, { returnObjects: true }) as unknown as [string, string][]
  return (
    <Page title={t(`shell:unwired.${key}.title`)} sub={t(`shell:unwired.${key}.sub`)} max={640}>
      <div className="flex flex-col gap-8" data-unwired={id}>
        <div className="flex items-start gap-3 rounded-xl border px-4 py-3.5">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground"><I className="size-[18px]" strokeWidth={1.5} /></span>
          <div className="min-w-0"><div className="type-headline">{t("shell:unwired.notWired")}</div>
            <div className="mt-0.5 type-callout text-muted-foreground">{now} {t("shell:unwired.planned", { phase: t(`shell:unwired.phase.${m.phase}`) })}</div></div>
        </div>
        <section><SectionLabel>{t("shell:unwired.willHead")}</SectionLabel>
          <Group>{will.map(([w, d]) => <Row key={w} title={w} sub={d} />)}</Group></section>
      </div>
    </Page>
  )
}
