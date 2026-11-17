import Foundation

public enum AttentionKind: String, Codable, CaseIterable, Sendable {
    case replies, pending, feed, channels
    public var unit: AttentionUnit {
        switch self {
        case .replies: return .messages
        case .pending: return .cards
        case .feed: return .entries
        case .channels: return .conversations
        }
    }
}

public enum AttentionUnit: String, Codable, Sendable { case messages, cards, entries, conversations }

/// Native identity only. Endpoint and credentials deliberately do not belong in persisted scope metadata.
public struct AttentionScope: Codable, Hashable, Sendable {
    public let hubProfileID: String
    public let userID: String
    public let workspaceID: String
    public init(hubProfileID: String, userID: String, workspaceID: String) {
        self.hubProfileID = hubProfileID; self.userID = userID; self.workspaceID = workspaceID
    }
}

/// An allowlisted navigation intent, never a URL or executable Web fragment.
public struct AttentionTarget: Codable, Hashable, Sendable {
    public enum Destination: String, Codable, Sendable { case reply, pending, feed, channel }
    public let destination: Destination
    public let entityID: String
    public init(destination: Destination, entityID: String) throws {
        guard Self.validID(entityID) else { throw AttentionError.invalidResponse }
        self.destination = destination; self.entityID = entityID
    }
    public static func normalize(kind: AttentionKind, route: String, entityID: String?) throws -> Self {
        guard let id = entityID else { throw AttentionError.invalidResponse }
        switch (kind, route) {
        case (.replies, ""): return try .init(destination: .reply, entityID: id)
        case (.pending, "inbox"): return try .init(destination: .pending, entityID: id)
        case (.feed, "feed"): return try .init(destination: .feed, entityID: id)
        case (.channels, ""):
            guard id.hasPrefix("chan:") else { throw AttentionError.invalidResponse }
            return try .init(destination: .channel, entityID: String(id.dropFirst(5)))
        default: throw AttentionError.invalidResponse
        }
    }
    public static func validID(_ id: String) -> Bool {
        !id.isEmpty && id.utf8.count <= 512 && !id.unicodeScalars.contains {
            CharacterSet.controlCharacters.contains($0) || "/\\?#".unicodeScalars.contains($0)
        }
    }
}

public struct AttentionEntry: Sendable, Equatable {
    public let id: String
    public let eventID: String
    public let created: Int64
    public let order: Int64?
    public let target: AttentionTarget
    public let sourceID: String?
    public let groupKey: String?
    public let notify: Bool?
    public init(id: String, eventID: String, created: Int64, order: Int64? = nil,
                target: AttentionTarget, sourceID: String? = nil, groupKey: String? = nil, notify: Bool? = nil) {
        self.id = id; self.eventID = eventID; self.created = created; self.order = order
        self.target = target; self.sourceID = sourceID; self.groupKey = groupKey; self.notify = notify
    }
}

public struct AttentionCategory: Sendable, Equatable {
    public let kind: AttentionKind
    public let count: Int64
    public let unit: AttentionUnit
    public let available: Bool
    public let revision: String
    public let entries: [AttentionEntry]
    public let nextCursor: String?
    /// Completion token. Intermediate pages may omit it; a complete available category must have one.
    public let snapshot: String?
    public init(kind: AttentionKind, count: Int64, available: Bool = true, revision: String,
                entries: [AttentionEntry], nextCursor: String? = nil, snapshot: String? = nil) {
        self.kind = kind; self.count = count; self.unit = kind.unit; self.available = available
        self.revision = revision; self.entries = entries; self.nextCursor = nextCursor; self.snapshot = snapshot
    }
}

public struct AttentionPage: Sendable {
    public let epoch: String
    public let userID: String
    public let workspaceID: String
    public let categories: [AttentionCategory]
    public init(epoch: String, userID: String, workspaceID: String, categories: [AttentionCategory]) {
        self.epoch = epoch; self.userID = userID; self.workspaceID = workspaceID; self.categories = categories
    }
}

public enum AttentionError: Error, Equatable, Sendable {
    case authRequired, unsupported, snapshotChanged, invalidResponse, transport(Int)
}

public enum AttentionPhase: String, Sendable { case loading, known, stale, unsupported, authRequired }
public enum AttentionDedupStatus: String, Sendable { case ready, capacityReached, storageUnavailable }
public struct AttentionCategoryState: Sendable, Equatable {
    public var phase: AttentionPhase
    /// Nil means unknown, NOT zero. Stale/auth/unsupported may retain the last successful value.
    public var lastSuccess: AttentionCategory?
    public init(phase: AttentionPhase, lastSuccess: AttentionCategory? = nil) {
        self.phase = phase; self.lastSuccess = lastSuccess
    }
}
public struct AttentionState: Sendable {
    public var scope: AttentionScope?
    public var categories: [AttentionKind: AttentionCategoryState] = [:]
    public var updatedAt: Date?
    public var dedupStatus: AttentionDedupStatus = .ready
    public var hasAttention: Bool {
        categories.values.contains { ($0.lastSuccess?.count ?? 0) > 0 }
    }
    public init(scope: AttentionScope? = nil) { self.scope = scope }
}

public struct AttentionPreferences: Codable, Sendable, Equatable {
    /// Default is off; permission requests are exclusively explicit UI actions.
    public var enabled = false
    public var enabledCategories = Set(AttentionKind.allCases)
    public var privacyPreviews = false
    public var sound = false
    /// Keys are channel target entity IDs, scoped by the caller's preference store.
    public var channelOverrides: [String: Bool] = [:]
    public init() {}
}

public struct AttentionForeground: Sendable, Equatable {
    public let scope: AttentionScope
    public let target: AttentionTarget
    public let targets: Set<AttentionTarget>
    public let activelyViewing: Bool
    public init(scope: AttentionScope, target: AttentionTarget, activelyViewing: Bool) {
        self.scope = scope; self.target = target; self.targets = [target]; self.activelyViewing = activelyViewing
    }
    /// A viewport can contain several final replies/cards. Preserve the single-target initializer for callers.
    public init(scope: AttentionScope, target: AttentionTarget, additionalTargets: Set<AttentionTarget>, activelyViewing: Bool) {
        self.scope = scope; self.target = target; self.targets = additionalTargets.union([target]); self.activelyViewing = activelyViewing
    }
}

/// Contains metadata only. No fetched message bodies or credentials ever cross the delivery boundary.
public struct AttentionNotification: Sendable, Equatable {
    public let id: String
    public let scope: AttentionScope
    public let target: AttentionTarget
    public let kinds: Set<AttentionKind>
    public let eventCount: Int
    public let isSummary: Bool
    public let privacyPreviews: Bool
    public let sound: Bool
    public init(id: String, scope: AttentionScope, target: AttentionTarget, kinds: Set<AttentionKind>,
                eventCount: Int, isSummary: Bool, privacyPreviews: Bool, sound: Bool) {
        self.id = id; self.scope = scope; self.target = target; self.kinds = kinds
        self.eventCount = eventCount; self.isSummary = isSummary
        self.privacyPreviews = privacyPreviews; self.sound = sound
    }
}

@MainActor public protocol AttentionDelivering: AnyObject {
    func deliver(_ notification: AttentionNotification) async
    func updateIndicator(_ state: AttentionState)
    func remove(scope: AttentionScope)
}
