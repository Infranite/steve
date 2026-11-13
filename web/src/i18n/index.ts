/* ───────── i18n: zh-CN (the source language and fallback) and en ─────────
 *
 * Conventions
 * - Strings live in locales/<lang>/<ns>.ts as plain nested objects, one file per namespace (NAMESPACES below).
 *   zh-CN is the source: its keys are the type every `t()` call is checked against, so a missing or misspelled
 *   key fails `npm -w web run check`. en has the same keys (web/test/i18n.test.mjs compares them).
 * - Keys read '<ns>:<area>.<what>', e.g. t("mail:list.empty"), t("common:action.save"). Areas and names are
 *   lowerCamelCase English that says what the text is for, not what it says.
 * - Keep the zh-CN text byte-identical to the text the UI shows today: full-width punctuation, the spaces around
 *   numbers ("3 分钟前"), the ellipsis character "…". Moving a string into a file must not change what a zh-CN
 *   user sees.
 * - Values go in through interpolation, {{name}}: t("mail:sent", { to }). React escapes, so escapeValue is off;
 *   never build HTML from a translation. For markup inside a sentence use <Trans> from react-i18next.
 * - Plurals use `count`: write key_other in zh-CN (Chinese has one form), key_one and key_other in en, and call
 *   t("ns:key", { count }).
 * - Never concatenate translated fragments ("已" + t(...) or `${a}${b}`): word order differs between languages.
 *   Give the whole sentence one key with placeholders.
 * - Dates, times, numbers and durations go through ./format.ts, not hand-written templates.
 * - Inside components use `const t = useT()` from here (every namespace, so the same "<ns>:<area>.<what>" keys
 *   type-check and the component re-renders on a language change); outside React (store, helpers) import `t`.
 *   Module-level constant tables of labels must become functions or be translated where they are rendered,
 *   since the language can change while the page is open.
 * - Shared words (保存, 取消, 加载中…) are in the "common" namespace; reuse them instead of adding copies.
 *
 * The resources are bundled and init is synchronous, so importing this module (also from node --test) is enough.
 * Imports here use explicit .ts extensions so node can load the files without a bundler.
 */
import i18n from "i18next"
import { initReactI18next, useTranslation } from "react-i18next"
import zhCN from "./locales/zh-CN/index.ts"
import en from "./locales/en/index.ts"

export const NAMESPACES = ["accounts", "api", "views", "manage", "core", "pages", "shell", "auto", "channels", "github", "integrations", "knowledge", "caps", "mail", "people", "stats", "thread", "vault", "yolo", "misc", "common"] as const
export type Namespace = (typeof NAMESPACES)[number]
export const LANGS = ["zh-CN", "en"] as const
export type Lang = (typeof LANGS)[number]
export const FALLBACK: Lang = "zh-CN"
const KEY = "steve-lang"

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "common"
    resources: typeof zhCN
  }
}

const isLang = (v: unknown): v is Lang => typeof v === "string" && (LANGS as readonly string[]).includes(v)
/** a BCP 47 tag from the browser to one of ours: any Chinese is zh-CN, any English is en */
export function matchLang(tag: string | null | undefined): Lang | undefined {
  const t = (tag ?? "").toLowerCase()
  if (t === "zh" || t.startsWith("zh-")) return "zh-CN"
  if (t === "en" || t.startsWith("en-")) return "en"
  return undefined
}
/** the saved choice, else the browser's languages in order, else zh-CN. Outside a browser (node --test) always zh-CN. */
export function detectLang(): Lang {
  if (typeof document === "undefined") return FALLBACK
  try { const saved = localStorage.getItem(KEY); if (isLang(saved)) return saved } catch { /* storage blocked */ }
  const nav = typeof navigator === "undefined" ? [] : navigator.languages?.length ? navigator.languages : [navigator.language]
  for (const l of nav) { const m = matchLang(l); if (m) return m }
  return FALLBACK
}

const setHtmlLang = (l: string) => { if (typeof document !== "undefined") document.documentElement.lang = l }
i18n.on("languageChanged", setHtmlLang)

void i18n.use(initReactI18next).init({
  resources: { "zh-CN": zhCN, en },
  lng: detectLang(),
  fallbackLng: FALLBACK,
  supportedLngs: [...LANGS],
  load: "currentOnly",
  ns: [...NAMESPACES],
  defaultNS: "common",
  initAsync: false,
  returnNull: false,
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
})
setHtmlLang(i18n.language)

/** the language the UI is in now */
export const lang = (): Lang => (isLang(i18n.language) ? i18n.language : FALLBACK)
/** switch the UI language and remember it on this device */
export function setLang(l: Lang) {
  try { localStorage.setItem(KEY, l) } catch { /* storage blocked: the choice lasts until reload */ }
  void i18n.changeLanguage(l)
}

export { t } from "i18next"
/** `t` for a component: takes any "<ns>:<key>" and re-renders the component when the language changes */
export const useT = () => useTranslation(NAMESPACES).t
export default i18n
