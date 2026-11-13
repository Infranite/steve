import { lang, t } from "@/i18n"

/** a tool permission question as the Hub words it (“允许执行：<command>”), led in the UI language. Its own module so
 *  what ui.tsx itself shows (the running-status pill) can read it without importing ui.tsx back. */
export const permissionQuestion = (q: string) => (lang() === "zh-CN" ? q : q.replace(/^允许执行(?:[：:]\s*(?=\S))?/, (m) => t("core:wait.allowRun") + (m.length > 4 ? ": " : "")))
/** the command a tool permission question asks about, without the Hub's prefix */
export const permissionCommand = (q: string) => q.replace(/^允许执行[：:]\s*/, "")
