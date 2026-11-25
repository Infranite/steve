/* Typed wire fixtures for channel-reply.browser.mjs and channel-reply-refresh.browser.mjs. */
import type { DecisionAnswer } from "../../src/api.ts"
import type { ROutContext } from "../../src/api/channels.ts"
import type { ById, DecisionView, MessageView, PageOf, Snapshot } from "../../src/gen/index.ts"
import { decisionView, pageResponse, snapshotViews, viewsResponse } from "./wire-views.ts"

export function channelReplyContext(id: string): ROutContext {
  return {
    space: { id: id === "cm_next" ? "sp_next" : "sp_product", name: id === "cm_next" ? "另一个聊天" : "产品讨论群", kind: "group", external: true },
    as_you: id === "cm_next",
    persona: id === "cm_next" ? null : "值班助手",
    text: "正式草稿",
    reply_to: { id: "cm_in", sender: "陈同学", text: "你是谁啊？\n我想了解你的职责。", at: 1 },
    before: [{ sender: "林同学", text: "欢迎新同学。", at: 0 }],
  }
}

export function channelReplyRefreshContext(draft: string): ROutContext {
  return {
    space: { id: "sp_product", name: "产品讨论群", kind: "group", external: true },
    as_you: false,
    persona: "值班助手",
    text: draft,
    reply_to: { id: "cm_in", sender: "陈同学", text: "你是谁啊？", at: 1 },
    before: [],
  }
}

export const channelReplyOpenDecision: DecisionView = decisionView({
  id: "dc_reply",
  ws: "ws_fixture",
  kind: "approve",
  subject_type: "chan_message",
  subject_id: "cm_reply",
  question: { kind: "raw", text: "以值班助手回复「产品讨论群」的消息" },
  draft: "我是值班助手。",
  options: ["yes", "no"],
  created: 1,
})

export function channelReplyRefreshDecision(draft: string): DecisionView {
  return decisionView({
    id: "dc_reply",
    ws: "ws_fixture",
    kind: "approve",
    subject_type: "chan_message",
    subject_id: "cm_reply",
    question: { kind: "raw", text: "审批渠道回复" },
    draft,
    options: ["yes", "no", "to_item"],
    created: 1,
  })
}

export function settleChannelReplyDecision(
  decision: DecisionView,
  body: { choice: string; text?: string },
  resolved: number,
): DecisionView {
  return {
    ...decision,
    draft: body.text ?? decision.draft,
    status: "resolved",
    result: { choice: body.choice, send_at: 2000 },
    resolved,
  }
}

export function reopenChannelReplyDecision(decision: DecisionView): DecisionView {
  return { ...decision, status: "open", result: null, resolved: null }
}

export function channelReplyDecisionAnswer(choice: string): DecisionAnswer {
  return {
    id: "dc_reply",
    revision: 1,
    status: "resolved",
    kind: "approve",
    subject_type: "chan_message",
    result: { revision: 1, choice, ...(choice === "yes" ? { send_at: 2000 } : {}) },
  }
}

export function decisionSnapshot(rows: DecisionView[], rev: number): Snapshot {
  return snapshotViews({ decision: rows }, { rev, cursor: String(rev) })
}

export function decisionViews(views: DecisionView[], rev: number, missing: string[] = []): ById {
  return viewsResponse(views, rev, missing)
}

export function emptyMessagePage(rev: number): PageOf<MessageView> {
  return pageResponse([], rev, null)
}
