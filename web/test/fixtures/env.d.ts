import type { SendRecoveryInfo } from "../../src/api/channels"
import type { ConversationMessage, RWorkspace } from "../../src/api"

declare global {
  interface Window {
    __calls?: unknown[]
    __say?: unknown[]
    __body?: string
    __release?: () => void
    __decisionError?: boolean
    __decisionId?: string
    __recovery?: SendRecoveryInfo
    __stateError?: boolean
    __stateStale?: boolean
    __reload?: () => void
    __refresh?: () => Promise<void>
    __preview?: (question: string, draft: string) => Promise<void>
    __waitChanges?: unknown[]
    __workflowChanges?: unknown[]
    __graphOpened?: string
    __resyncs?: () => number
    __transcript?: () => string[]
    __load?: () => Promise<void>
    __land?: (p: { workspace?: Partial<RWorkspace>; settings?: Record<string, string> }) => void
    fixtureHub?: { load: () => { thread: ConversationMessage[]; calls: number; fail: boolean }; save: (s: { thread: ConversationMessage[]; calls: number; fail: boolean }) => void }
    renameSession?: (id: string, title: string) => void
    listenJump?: () => Promise<unknown>
  }
}

export {}
