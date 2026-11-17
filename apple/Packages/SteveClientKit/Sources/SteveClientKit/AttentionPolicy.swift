import Foundation

public struct AttentionCheckpoint: Codable, Sendable {
    public var epoch: String
    public var installationEpoch: String
    public var baselineKinds: Set<AttentionKind> = []
    public var seen: Set<String> = []
    public var capacityReached = false
    public init(epoch: String, installationEpoch: String) {
        self.epoch = epoch; self.installationEpoch = installationEpoch
    }
}

@MainActor public protocol AttentionCheckpointStore: AnyObject {
    var installationEpoch: String { get }
    func load(scope: AttentionScope) throws -> AttentionCheckpoint?
    func save(_ checkpoint: AttentionCheckpoint, scope: AttentionScope) throws
    func remove(scope: AttentionScope) throws
    func preferences(scope: AttentionScope) throws -> AttentionPreferences
    func savePreferences(_ preferences: AttentionPreferences, scope: AttentionScope) throws
}

/// Dedicated defaults domain supplied by the host (tests use a throwaway suite). Only identifiers,
/// dedup checkpoints and local preferences persist; there are no bodies, URLs or credentials.
@MainActor public final class DefaultsAttentionStore: AttentionCheckpointStore {
    private struct Record: Codable {
        var scope: AttentionScope
        var checkpoint: AttentionCheckpoint?
        var preferences = AttentionPreferences()
    }
    private struct Archive: Codable { var installationEpoch: String; var records: [Record] = [] }
    public enum StorageError: Error { case capacityReached, corrupt }
    private let defaults: UserDefaults
    private let key: String
    private let maxScopes: Int
    private var archive: Archive
    public var installationEpoch: String { archive.installationEpoch }
    /// Native host uses the metadata scopes to remove an account/profile without retaining any credentials.
    public var scopes: [AttentionScope] { archive.records.map(\.scope) }
    public init(defaults: UserDefaults, key: String = "steve.attention.v1", maxScopes: Int = 32) throws {
        self.defaults = defaults; self.key = key; self.maxScopes = max(1, maxScopes)
        if let data = defaults.data(forKey: key) {
            do { archive = try JSONDecoder().decode(Archive.self, from: data) }
            catch { throw StorageError.corrupt }
        } else {
            archive = Archive(installationEpoch: UUID().uuidString)
            defaults.set(try JSONEncoder().encode(archive), forKey: key)
        }
    }
    public func load(scope: AttentionScope) throws -> AttentionCheckpoint? {
        archive.records.first { $0.scope == scope }?.checkpoint
    }
    public func preferences(scope: AttentionScope) throws -> AttentionPreferences {
        archive.records.first { $0.scope == scope }?.preferences ?? AttentionPreferences()
    }
    public func save(_ checkpoint: AttentionCheckpoint, scope: AttentionScope) throws {
        try update(scope: scope) { $0.checkpoint = checkpoint }
    }
    public func savePreferences(_ preferences: AttentionPreferences, scope: AttentionScope) throws {
        try update(scope: scope) { $0.preferences = preferences }
    }
    public func remove(scope: AttentionScope) throws {
        var next = archive; next.records.removeAll { $0.scope == scope }
        try persist(next)
    }
    private func update(scope: AttentionScope, change: (inout Record) -> Void) throws {
        var next = archive
        if let index = next.records.firstIndex(where: { $0.scope == scope }) { change(&next.records[index]) }
        else {
            // Never silently evict another identity's dedup memory.
            guard next.records.count < maxScopes else { throw StorageError.capacityReached }
            var record = Record(scope: scope); change(&record); next.records.append(record)
        }
        try persist(next)
    }
    private func persist(_ next: Archive) throws {
        let data = try JSONEncoder().encode(next)
        defaults.set(data, forKey: key); archive = next
    }
}

public struct AttentionPolicyResult: Sendable {
    public let checkpoint: AttentionCheckpoint
    public let notifications: [AttentionNotification]
    public let status: AttentionDedupStatus
}

/// Pure policy. Every observed source is remembered, including suppressed/disabled/read-later sources.
/// There is no timestamp cutoff, count-difference heuristic, fuzzy grouping or historical cursor.
public struct AttentionPolicy: Sendable {
    public let maximumRememberedIDs: Int
    public let summaryThreshold: Int
    public init(maximumRememberedIDs: Int = 30_000, summaryThreshold: Int = 4) {
        self.maximumRememberedIDs = max(1, maximumRememberedIDs)
        self.summaryThreshold = max(2, summaryThreshold)
    }
    public func evaluate(snapshot: AttentionSnapshot, scope: AttentionScope, installationEpoch: String,
                         checkpoint old: AttentionCheckpoint?, preferences: AttentionPreferences,
                         foreground: AttentionForeground?) -> AttentionPolicyResult {
        var checkpoint = old.flatMap { $0.epoch == snapshot.epoch && $0.installationEpoch == installationEpoch ? $0 : nil }
            ?? AttentionCheckpoint(epoch: snapshot.epoch, installationEpoch: installationEpoch)
        let previouslySeen = checkpoint.seen
        var observed = Set<String>()
        var candidates: [(AttentionKind, AttentionEntry)] = []
        for kind in AttentionKind.allCases {
            guard let category = snapshot.categories[kind], category.available else { continue }
            let baseline = !checkpoint.baselineKinds.contains(kind)
            for entry in category.entries {
                let keys = identities(kind: kind, entry: entry)
                observed.formUnion(keys)
                guard !baseline, previouslySeen.isDisjoint(with: keys), preferences.enabled,
                      preferences.enabledCategories.contains(kind) else { continue }
                if kind == .channels {
                    // notify:false remains visible but not banner-eligible unless explicitly opted in.
                    // quiet is not a mute flag and is intentionally absent from this policy.
                    let allowed = preferences.channelOverrides[entry.target.entityID] ?? (entry.notify == true)
                    guard allowed else { continue }
                } else if entry.notify == false { continue }
                if let foreground, foreground.activelyViewing, foreground.scope == scope,
                   foreground.targets.contains(entry.target) { continue }
                candidates.append((kind, entry))
            }
            checkpoint.baselineKinds.insert(kind)
        }
        let union = previouslySeen.union(observed)
        // Bounded, fail-closed policy: never evict IDs (that would replay read/unread sources).
        // At capacity latch banners off for this epoch and expose capacityReached in native state.
        // Counts still update; the UI must show this status. Epoch change starts a silent baseline.
        if union.count > maximumRememberedIDs || checkpoint.capacityReached {
            checkpoint.capacityReached = true
            return .init(checkpoint: checkpoint, notifications: [], status: .capacityReached)
        }
        checkpoint.seen = union
        var groups: [(kinds: Set<AttentionKind>, entry: AttentionEntry, keys: Set<String>)] = []
        for (kind, entry) in candidates {
            var merged = (kinds: Set([kind]), entry: entry, keys: identities(kind: kind, entry: entry))
            // Merge connected references, including a source appearing through multiple group references.
            // Reverse removal preserves unrelated group ordering and avoids count-based deduplication.
            let overlaps = groups.indices.filter { !groups[$0].keys.isDisjoint(with: merged.keys) }
            if let first = overlaps.first { merged.entry = groups[first].entry }
            for index in overlaps.reversed() {
                merged.kinds.formUnion(groups[index].kinds); merged.keys.formUnion(groups[index].keys)
                groups.remove(at: index)
            }
            if let first = overlaps.first { groups.insert(merged, at: first) }
            else { groups.append(merged) }
        }
        let notifications: [AttentionNotification]
        if groups.count >= summaryThreshold, let first = groups.first {
            notifications = [AttentionNotification(id: UUID().uuidString, scope: scope, target: first.entry.target,
                kinds: groups.reduce(into: Set<AttentionKind>()) { $0.formUnion($1.kinds) }, eventCount: groups.count,
                isSummary: true, privacyPreviews: preferences.privacyPreviews, sound: preferences.sound)]
        } else {
            notifications = groups.map { group in
                AttentionNotification(id: UUID().uuidString, scope: scope, target: group.entry.target,
                    kinds: group.kinds, eventCount: 1, isSummary: false,
                    privacyPreviews: preferences.privacyPreviews, sound: preferences.sound)
            }
        }
        return .init(checkpoint: checkpoint, notifications: notifications, status: .ready)
    }
    private func identities(kind: AttentionKind, entry: AttentionEntry) -> Set<String> {
        let event = "event:\(kind.rawValue):\(entry.eventID)"
        // A channels row is a conversation aggregate: its id/source/group can remain unchanged
        // when a NEW incoming message replaces the newest event. Only event_id identifies that
        // observation; persisting the conversation identity would mute its entire future lifetime.
        if kind == .channels { return [event] }
        // source_id is NOT an event identity: feed reports reuse a flow ID across distinct runs.
        // Only the explicit shared-event group is safe to alias (notification source_key[1] is
        // personal_notifications.event_id in the backend), never a source/item/conversation ID.
        var keys: Set<String> = [event]
        if let group = entry.groupKey { keys.insert("group:\(group)") }
        return keys
    }
}
