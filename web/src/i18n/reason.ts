import type { HttpErrorBody } from "../gen/HttpErrorBody.ts"
import { REASONS } from "../gen/reasons.ts"
import i18n from "./index.ts"
import { date, msDur } from "./format.ts"

type ReasonBody = Pick<HttpErrorBody, "error"> & Partial<Pick<HttpErrorBody, "reason" | "params">>

/** Display parameters never drive logic; unknown reasons retain the Hub's safe detail. */
export function reasonText(body: ReasonBody): string {
  const reason = body.reason
  const key = `reason:${reason}`
  if (!reason || !Object.hasOwn(REASONS, reason)) return body.error
  const params = Object.fromEntries(Object.entries(body.params ?? {}).map(([name, value]) => [
    name,
    typeof value === "number" && name.endsWith("_at") ? date(value, { dateStyle: "medium", timeStyle: "short" })
      : typeof value === "number" && name.endsWith("_ms") ? msDur(value) : value,
  ]))
  // The generated registry is runtime data; static UI keys remain checked by the typed t() API.
  const options = { defaultValue: body.error, replace: params, ...(typeof params.count === "number" ? { count: params.count } : {}) }
  return i18n.exists(key, options) ? i18n.t(key, options) : body.error
}
