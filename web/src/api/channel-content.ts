/* Normalised, read-only channel content returned by the Hub. */
export type CardBlock = {
  t: string; text?: string; level?: number; alt?: string; title?: string; expanded?: boolean
  summary?: string; kind?: string; url?: string; tag?: string
  blocks?: CardBlock[]; columns?: CardBlock[][] | string[]; rows?: string[][]
  items?: { text: string; url?: string; style?: string | null; disabled: boolean; callback: boolean }[]
}
export type CardAttachment = {
  type: "card"; format?: string; schema?: number | null; title?: string | null; subtitle?: string | null
  template?: string | null; tags?: string[]; blocks: CardBlock[]; truncated?: boolean
}
export type MediaAttachment = { type: "image" | "file"; name?: string | null; kind?: string; source?: string; url?: string; unavailable?: string }
export type ChannelAttachment = CardAttachment | MediaAttachment
