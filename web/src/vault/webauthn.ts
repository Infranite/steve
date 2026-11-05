/* WebAuthn glue between webauthn-rs's JSON (base64url byte strings) and navigator.credentials (ArrayBuffers).
 * Registration: `create(await registerStart(name))` → registerFinish. Reveal: `get(await revealStart(...))` → revealFinish. */
import { t } from "@/i18n"

const toBuf = (s: string): ArrayBuffer => {
  const b = atob(s.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(s.length / 4) * 4, "="))
  const u = new Uint8Array(b.length); for (let i = 0; i < b.length; i++) u[i] = b.charCodeAt(i)
  return u.buffer
}
const toB64u = (b: ArrayBuffer | null | undefined): string | undefined => {
  if (!b) return undefined
  let s = ""; for (const x of new Uint8Array(b)) s += String.fromCharCode(x)
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "")
}
const creds = (xs?: any[]) => xs?.map((c) => ({ ...c, id: toBuf(c.id) }))

export const webauthnAvailable = () => typeof window !== "undefined" && !!window.PublicKeyCredential && !!navigator.credentials && window.isSecureContext

/** `navigator.credentials.create` from a webauthn-rs `CreationChallengeResponse`; returns a `RegisterPublicKeyCredential` */
export async function createPasskey(ccr: any): Promise<unknown> {
  const pk = ccr.publicKey
  const c = (await navigator.credentials.create({ publicKey: { ...pk, challenge: toBuf(pk.challenge), user: { ...pk.user, id: toBuf(pk.user.id) }, excludeCredentials: creds(pk.excludeCredentials) } })) as PublicKeyCredential | null
  if (!c) throw new Error(t("vault:webauthn.notRegistered"))
  const r = c.response as AuthenticatorAttestationResponse
  return { id: c.id, rawId: toB64u(c.rawId), type: c.type, extensions: c.getClientExtensionResults?.() ?? {},
    response: { attestationObject: toB64u(r.attestationObject), clientDataJSON: toB64u(r.clientDataJSON), transports: r.getTransports?.() } }
}

/** `navigator.credentials.get` from a webauthn-rs `RequestChallengeResponse`; returns a `PublicKeyCredential` assertion */
export async function getAssertion(rcr: any): Promise<unknown> {
  const pk = rcr.publicKey
  const c = (await navigator.credentials.get({ publicKey: { ...pk, challenge: toBuf(pk.challenge), allowCredentials: creds(pk.allowCredentials) } })) as PublicKeyCredential | null
  if (!c) throw new Error(t("vault:webauthn.notVerified"))
  const r = c.response as AuthenticatorAssertionResponse
  return { id: c.id, rawId: toB64u(c.rawId), type: c.type, extensions: c.getClientExtensionResults?.() ?? {},
    response: { authenticatorData: toB64u(r.authenticatorData), clientDataJSON: toB64u(r.clientDataJSON), signature: toB64u(r.signature), userHandle: toB64u(r.userHandle) } }
}

/** a user who cancels the system sheet gets a calm message, not a DOMException name */
export function webauthnError(e: unknown): string {
  const n = (e as any)?.name
  if (n === "NotAllowedError" || n === "AbortError") return t("vault:webauthn.cancelled")
  if (n === "InvalidStateError") return t("vault:webauthn.alreadyRegistered")
  if (n === "SecurityError") return t("vault:webauthn.insecure")
  return (e as Error)?.message || t("vault:webauthn.failed")
}
