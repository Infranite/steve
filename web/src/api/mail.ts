/* Mail (email.md §9): the Rust Hub's /api/mail/…. The TS Hub doesn't have it; `NotWired` (from modReq) tells a page to
 * fall back to its Unwired placeholder. Shapes follow steve-mail's api.rs / drafts.rs. Sending only ever happens through
 * a `mail_draft` decision: `POST /mail/drafts/:id/send` opens it and approves it as you, or the approval card's `send`. */
import { modReq, qs, type RFile } from "@/api"
const e = encodeURIComponent
const get = <T>(p: string) => modReq<T>("GET", p)
const post = <T = any>(p: string, b: unknown = {}) => modReq<T>("POST", p, b)

export type MailSettings = { signature: string; duty: string; send_delay_s: number; archive_on_done: boolean; sync_days: number; append_sent: "auto" | "yes" | "no"; revision: number; updated: number }
export type MailAccount = { connection_id: string; address: string; adapter: string; status: string; last_sync: number | null; unread: number; settings: MailSettings }
export type MailView = "needs_reply" | "inbox" | "waiting" | "sent" | "drafts" | "all"
export type ThreadState = "open" | "needs_reply" | "waiting" | "done" | "muted"
export type DraftStatus = "draft" | "pending" | "approved" | "sending" | "sent" | "failed" | "unknown" | "discarded"
export type ThreadBrief = {
  id: string; connection: { id: string; address: string }; subject: string; last_at: number; count: number; unread: number
  state: ThreadState | string; from: { name: string; party_id: string | null; org: string | null; addr?: string } | null; snippet: string
  has_attachments: boolean; signal: { event_id: string; outcome_label: string } | null; item_id: string | null; draft: { id: string; status: DraftStatus } | null
}
export type Addr = { addr: string; name: string; party_id?: string | null }
export type MailAttachment = { id: string; name: string; mime: string; size: number; file_id: string | null }
export type Message = {
  id: string; direction: "in" | "out"; from: Addr; to: Addr[]; cc: Addr[]; subject: string; date: number
  text: string | null; text_new: string | null; has_html: boolean; purged: boolean; attachments: MailAttachment[]
}
export type Rcpt = { addr: string; name: string; party_id: string | null; external: boolean }
export type Draft = {
  id: string; connection_id: string; thread_id: string | null; reply_to: string | null; mode: "reply" | "reply_all" | "forward" | "new"
  to: Rcpt[]; cc: Rcpt[]; bcc: Rcpt[]; subject: string; body: string; files: RFile[]; author: string; status: DraftStatus
  decision_id: string | null; basis: { verdict: string; entries?: unknown[]; spans?: { text: string; why?: string }[] } | null
  send_at: number | null; error: string | null; item_id: string | null; session_id: string | null; sent_message: string | null; created: number; updated: number
}
export type Thread = ThreadBrief & { messages: Message[]; drafts: Draft[]; items: { id: string; title: string; status: string }[] }
export type NewDraft = { connection_id: string; mode: Draft["mode"]; thread_id?: string; reply_to?: string; to: (string | { addr: string; name?: string })[]; cc?: string[]; bcc?: string[]; subject: string; body: string; files?: RFile[] }

export const mailApi = {
  accounts: () => get<MailAccount[]>("/mail/accounts"),
  patchAccount: (id: string, b: Partial<Pick<MailSettings, "signature" | "duty" | "send_delay_s" | "archive_on_done" | "sync_days">>) => modReq<MailAccount>("PATCH", "/mail/accounts/" + e(id), b),
  sync: (id: string) => post<{ ok: true }>(`/mail/accounts/${e(id)}/sync`),
  threads: (p: { account?: string; view?: MailView; q?: string; before?: number; limit?: number }) => get<ThreadBrief[]>("/mail/threads" + qs(p)),
  thread: (id: string) => get<Thread>("/mail/threads/" + e(id)),
  patchThread: (id: string, state: "open" | "done" | "muted") => modReq<ThreadBrief>("PATCH", "/mail/threads/" + e(id), { state }),
  archive: (id: string) => post(`/mail/threads/${e(id)}/archive`),
  read: (id: string) => post(`/mail/threads/${e(id)}/read`),
  toItem: (id: string, b: { bot?: string; mode?: "start" | "propose" } = {}) => post<{ item_id: string }>(`/mail/threads/${e(id)}/to-item`, b),
  toKnowledge: (id: string) => post<{ doc_id: string }>(`/mail/threads/${e(id)}/to-knowledge`),
  /** the mail bot drafts a reply (≤90 s); the result is a pending draft with its approval card */
  quickDraft: (id: string, b: { instruction?: string; bot?: string } = {}) => post<Draft>(`/mail/threads/${e(id)}/draft`, b),
  draft: (id: string) => get<Draft>("/mail/drafts/" + e(id)),
  newDraft: (b: NewDraft) => post<Draft>("/mail/drafts", b),
  putDraft: (id: string, b: Partial<NewDraft>) => modReq<Draft>("PUT", "/mail/drafts/" + e(id), b),
  deleteDraft: (id: string) => modReq<{ ok: true }>("DELETE", "/mail/drafts/" + e(id)),
  /** your own “发送”: opens the approval and resolves it as you; it goes out after the mailbox's send delay */
  sendDraft: (id: string) => post<Draft>(`/mail/drafts/${e(id)}/send`),
  /** before `send_at`: back to pending; afterwards 409 邮件已经发出，不能撤回 */
  cancelDraft: (id: string) => post<Draft>(`/mail/drafts/${e(id)}/cancel`),
  landAttachment: (id: string) => post<RFile>(`/mail/attachments/${e(id)}/file`),
  /** the sandboxed original HTML (CSP: no scripts; remote images only with images=1) */
  htmlUrl: (id: string, images = false) => `/api/mail/messages/${e(id)}/html${images ? "?images=1" : ""}`,
  rawUrl: (id: string) => `/api/mail/messages/${e(id)}/raw`,
}

/** the mail adapters (accounts page: AccountDetail's mailbox section) */
export const MAIL_ADAPTERS = ["imap", "gmail", "outlook"]

/* IMAP presets of the new-mailbox dialog (email.md §1, same as steve-mail's adapters::imap::PRESETS) */
export type ImapPreset = { id: string; name: string; imap_host: string; imap_port: number; smtp_host: string; smtp_port: number; smtp_security: "tls" | "starttls"; hint: string; help?: string }
export const IMAP_PRESETS: ImapPreset[] = [
  { id: "icloud", name: "iCloud", imap_host: "imap.mail.me.com", imap_port: 993, smtp_host: "smtp.mail.me.com", smtp_port: 587, smtp_security: "starttls", hint: "在 Apple ID 账户设置里生成 App 专用密码", help: "https://support.apple.com/zh-cn/102654" },
  { id: "qq", name: "QQ 邮箱", imap_host: "imap.qq.com", imap_port: 993, smtp_host: "smtp.qq.com", smtp_port: 465, smtp_security: "tls", hint: "在 QQ 邮箱设置里开启 IMAP 服务并生成授权码" },
  { id: "163", name: "网易 163", imap_host: "imap.163.com", imap_port: 993, smtp_host: "smtp.163.com", smtp_port: 465, smtp_security: "tls", hint: "在 163 邮箱设置里开启 IMAP 服务并生成授权码" },
  { id: "fastmail", name: "Fastmail", imap_host: "imap.fastmail.com", imap_port: 993, smtp_host: "smtp.fastmail.com", smtp_port: 465, smtp_security: "tls", hint: "在 Fastmail 设置的 Privacy & Security 里生成应用专用密码" },
  { id: "gmail", name: "Gmail", imap_host: "imap.gmail.com", imap_port: 993, smtp_host: "smtp.gmail.com", smtp_port: 465, smtp_security: "tls", hint: "需要先开启两步验证，再生成应用专用密码", help: "https://support.google.com/accounts/answer/185833" },
  { id: "custom", name: "其他", imap_host: "", imap_port: 993, smtp_host: "", smtp_port: 465, smtp_security: "tls", hint: "向邮箱服务商要 IMAP、SMTP 的地址和应用专用密码。只支持加密连接（TLS 或 STARTTLS）。" },
]

export const VIEWS: [MailView, string][] = [["needs_reply", "需要处理"], ["inbox", "收件箱"], ["waiting", "等对方"], ["sent", "已发送"], ["drafts", "草稿"]]
export const DRAFT_STATUS: Record<DraftStatus, [string, string]> = {
  draft: ["草稿", "idle"], pending: ["等你批准", "wait"], approved: ["即将发出", "run"], sending: ["正在发送", "run"], sent: ["已发出", "done"], failed: ["没发出去", "fail"], unknown: ["发送结果不明，待核实", "wait"], discarded: ["已放弃", "drop"],
}
