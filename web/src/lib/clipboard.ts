export type CopyResult = { ok: true } | { ok: false; error: unknown }

type ClipboardLike = Pick<Clipboard, "writeText">

/** Copy `text` through `navigator.clipboard` (or an injected clipboard) without hiding failures. */
export async function copyText(
  text: string,
  clipboard: ClipboardLike | null | undefined = globalThis.navigator?.clipboard,
): Promise<CopyResult> {
  if (!clipboard || typeof clipboard.writeText !== "function") {
    return { ok: false, error: new Error("Clipboard API is unavailable") }
  }
  try {
    await clipboard.writeText(text)
    return { ok: true }
  } catch (error) {
    return { ok: false, error }
  }
}
