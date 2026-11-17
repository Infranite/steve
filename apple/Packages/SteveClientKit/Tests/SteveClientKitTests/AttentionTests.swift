import Foundation
import XCTest
@testable import SteveClientKit

private let testScope = AttentionScope(hubProfileID: "hub-test", userID: "user-test", workspaceID: "ws-test")
private let testEndpoint = URL(string: "https://hub.example.test")!
private func entry(_ id: String, kind: AttentionKind = .replies, source: String? = nil,
                   group: String? = nil, notify: Bool? = nil) -> AttentionEntry {
    let destination: AttentionTarget.Destination
    switch kind {
    case .replies: destination = .reply
    case .pending: destination = .pending
    case .feed: destination = .feed
    case .channels: destination = .channel
    }
    return .init(id: id, eventID: "event-" + id, created: 100, order: 100,
                 target: try! .init(destination: destination, entityID: id), sourceID: source, groupKey: group, notify: notify)
}
private func category(_ kind: AttentionKind = .replies, _ entries: [AttentionEntry] = [],
                      count: Int64? = nil, revision: String = "r1", next: String? = nil,
                      snapshot: String? = "s1", available: Bool = true) -> AttentionCategory {
    .init(kind: kind, count: count ?? Int64(entries.count), available: available, revision: revision,
          entries: entries, nextCursor: next, snapshot: snapshot)
}
private func page(_ categories: [AttentionCategory], epoch: String = "epoch1", scope: AttentionScope = testScope) -> AttentionPage {
    .init(epoch: epoch, userID: scope.userID, workspaceID: scope.workspaceID, categories: categories)
}
private func snapshot(_ categories: [AttentionCategory], epoch: String = "epoch1") -> AttentionSnapshot {
    .init(epoch: epoch, categories: Dictionary(uniqueKeysWithValues: categories.map { ($0.kind, $0) }))
}
private func enabled() -> AttentionPreferences { var p = AttentionPreferences(); p.enabled = true; return p }

private actor ScriptAPI: AttentionAPI {
    struct Call: Equatable { let kind: AttentionKind?; let cursor: String?; let scope: AttentionScope }
    private var steps: [Result<AttentionPage, AttentionError>]
    private var calls: [Call] = []
    init(_ steps: [Result<AttentionPage, AttentionError>]) { self.steps = steps }
    func fetch(scope: AttentionScope, endpoint: URL, kind: AttentionKind?, cursor: String?) async throws -> AttentionPage {
        calls.append(.init(kind: kind, cursor: cursor, scope: scope))
        guard !steps.isEmpty else { throw AttentionError.transport(599) }
        return try steps.removeFirst().get()
    }
    func history() -> [Call] { calls }
}
private final class AttentionURLProtocol: URLProtocol {
    static var response: (URLRequest) throws -> (Int, Data) = { _ in (500, Data()) }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            let (status, data) = try Self.response(request)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status,
                httpVersion: "HTTP/1.1", headerFields: nil)!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

/// Intentionally ignores cancellation so scope invalidation must protect the monitor itself.
private actor SuspendedAttentionAPI: AttentionAPI {
    private var continuation: CheckedContinuation<AttentionPage, Error>?
    private var waiting: CheckedContinuation<Void, Never>?
    private(set) var calls = 0
    func fetch(scope: AttentionScope, endpoint: URL, kind: AttentionKind?, cursor: String?) async throws -> AttentionPage {
        calls += 1
        if calls > 1 { return page([category()], scope: scope) }
        return try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            waiting?.resume(); waiting = nil
        }
    }
    func waitUntilStarted() async {
        if continuation != nil { return }
        await withCheckedContinuation { waiting = $0 }
    }
    func finish(_ result: AttentionPage) { continuation?.resume(returning: result); continuation = nil }
}

@MainActor private final class MemoryStore: AttentionCheckpointStore {
    let installationEpoch = "install1"
    var records: [AttentionScope: AttentionCheckpoint] = [:]
    var prefs: [AttentionScope: AttentionPreferences] = [:]
    var fail = false
    func load(scope: AttentionScope) throws -> AttentionCheckpoint? { if fail { throw AttentionError.transport(1) }; return records[scope] }
    func save(_ checkpoint: AttentionCheckpoint, scope: AttentionScope) throws {
        if fail { throw AttentionError.transport(1) }; records[scope] = checkpoint
    }
    func remove(scope: AttentionScope) throws { records[scope] = nil; prefs[scope] = nil }
    func preferences(scope: AttentionScope) throws -> AttentionPreferences { prefs[scope] ?? enabled() }
    func savePreferences(_ preferences: AttentionPreferences, scope: AttentionScope) throws { prefs[scope] = preferences }
}
@MainActor private final class Delivery: AttentionDelivering {
    var notifications: [AttentionNotification] = []
    var states: [AttentionState] = []
    var removed: [AttentionScope] = []
    var onDeliver: (() -> Void)?
    func deliver(_ notification: AttentionNotification) async { notifications.append(notification); onDeliver?() }
    func updateIndicator(_ state: AttentionState) { states.append(state) }
    func remove(scope: AttentionScope) { removed.append(scope) }
}

final class AttentionTests: XCTestCase {
    private func evaluate(_ data: AttentionSnapshot, old: AttentionCheckpoint? = nil,
                          policy: AttentionPolicy = .init(), preferences: AttentionPreferences = enabled(),
                          foreground: AttentionForeground? = nil, installation: String = "install1") -> AttentionPolicyResult {
        policy.evaluate(snapshot: data, scope: testScope, installationEpoch: installation,
                        checkpoint: old, preferences: preferences, foreground: foreground)
    }
    func testInitialBaselineAllFourCategoriesUnitsAndEpochResetAreSilent() {
        let categories = AttentionKind.allCases.map { category($0, [entry($0.rawValue, kind: $0, notify: true)]) }
        let first = evaluate(snapshot(categories))
        XCTAssertTrue(first.notifications.isEmpty)
        XCTAssertEqual(first.checkpoint.baselineKinds, Set(AttentionKind.allCases))
        XCTAssertEqual(categories.map(\.unit), [.messages, .cards, .entries, .conversations])
        let next = evaluate(snapshot([category(.replies, [entry("new")])], epoch: "epoch2"), old: first.checkpoint)
        XCTAssertTrue(next.notifications.isEmpty)
        XCTAssertEqual(next.checkpoint.epoch, "epoch2")
        let reinstall = evaluate(snapshot([category(.replies, [entry("new")])]), old: first.checkpoint, installation: "install2")
        XCTAssertTrue(reinstall.notifications.isEmpty)
    }
    func testReconnectReadDeleteReturnNeverReplayButDistinctEventForSameSourceNotifies() {
        let baseline = evaluate(snapshot([category()]))
        let firstData = snapshot([category(.replies, [entry("one", source: "source1")])])
        let first = evaluate(firstData, old: baseline.checkpoint)
        XCTAssertEqual(first.notifications.count, 1)
        let reconnect = evaluate(firstData, old: first.checkpoint)
        XCTAssertTrue(reconnect.notifications.isEmpty)
        let read = evaluate(snapshot([category()]), old: reconnect.checkpoint)
        XCTAssertTrue(evaluate(firstData, old: read.checkpoint).notifications.isEmpty)
        let distinct = evaluate(snapshot([category(.replies, [entry("different-event", source: "source1")])]), old: read.checkpoint)
        XCTAssertEqual(distinct.notifications.count, 1)
    }
    func testFeedReportsForSameFlowAreDistinctEventsEvenInOnePoll() {
        let base = evaluate(snapshot([category(.feed)])).checkpoint
        let firstData = snapshot([category(.feed, [entry("report-run1", kind: .feed, source: "same-flow")])])
        let first = evaluate(firstData, old: base)
        XCTAssertEqual(first.notifications.count, 1)
        let nextData = snapshot([category(.feed, [entry("report-run1", kind: .feed, source: "same-flow"),
                                                entry("report-run2", kind: .feed, source: "same-flow")])])
        let next = evaluate(nextData, old: first.checkpoint)
        XCTAssertEqual(next.notifications.count, 1)
        XCTAssertEqual(next.notifications.first?.target.entityID, "report-run2")
        XCTAssertTrue(evaluate(nextData, old: next.checkpoint).notifications.isEmpty)
        let together = evaluate(nextData, old: base)
        XCTAssertEqual(together.notifications.count, 2)
        XCTAssertEqual(together.notifications.map(\.eventCount), [1, 1])
    }
    func testExactForegroundSuppressionDoesNotSuppressOtherPageOrChangeCounts() {
        let base = evaluate(snapshot([category()])).checkpoint
        let e = entry("one")
        let data = snapshot([category(.replies, [e], count: 12)])
        let focused = AttentionForeground(scope: testScope, target: e.target, activelyViewing: true)
        XCTAssertTrue(evaluate(data, old: base, foreground: focused).notifications.isEmpty)
        let other = AttentionForeground(scope: testScope, target: entry("other").target, activelyViewing: true)
        XCTAssertEqual(evaluate(data, old: base, foreground: other).notifications.count, 1)
        let unfocused = AttentionForeground(scope: testScope, target: e.target, activelyViewing: false)
        XCTAssertEqual(evaluate(data, old: base, foreground: unfocused).notifications.count, 1)
        let otherScope = AttentionScope(hubProfileID: "other", userID: testScope.userID, workspaceID: testScope.workspaceID)
        XCTAssertEqual(evaluate(data, old: base, foreground: .init(scope: otherScope, target: e.target, activelyViewing: true)).notifications.count, 1)
        XCTAssertEqual(data.categories[.replies]?.count, 12)
    }
    func testMultipleVisibleEntitiesSuppressOnlyThoseExactTargets() {
        let base = evaluate(snapshot([category()])).checkpoint
        let first = entry("first"), second = entry("second"), offscreen = entry("offscreen")
        let visible = AttentionForeground(scope: testScope, target: first.target, additionalTargets: [second.target], activelyViewing: true)
        let data = snapshot([category(.replies, [first, second, offscreen])])
        let result = evaluate(data, old: base, foreground: visible)
        XCTAssertEqual(result.notifications.map(\.target), [offscreen.target])
        XCTAssertTrue(evaluate(data, old: result.checkpoint).notifications.isEmpty, "Suppressed entities are remembered, not replayed on blur")
    }

    func testChannelOptInOptOutAndQuietIsNotMute() {
        let base = evaluate(snapshot([category(.channels)])).checkpoint
        let data = snapshot([category(.channels, [entry("group", kind: .channels, notify: false)])])
        XCTAssertTrue(evaluate(data, old: base).notifications.isEmpty)
        var p = enabled(); p.channelOverrides["group"] = true
        XCTAssertEqual(evaluate(data, old: base, preferences: p).notifications.count, 1)
        p.channelOverrides["group"] = false
        let serverAllowed = snapshot([category(.channels, [entry("group", kind: .channels, notify: true)])])
        XCTAssertTrue(evaluate(serverAllowed, old: base, preferences: p).notifications.isEmpty)
        // No "quiet" heuristic: only explicit notify / local override can decide eligibility.
        XCTAssertEqual(evaluate(serverAllowed, old: base).notifications.count, 1)
        let missingEligibility = snapshot([category(.channels, [entry("group", kind: .channels)])])
        XCTAssertTrue(evaluate(missingEligibility, old: base).notifications.isEmpty)
    }
    func testExistingChannelNewEventNotifiesAndSameEventNeverReplays() {
        let target = try! AttentionTarget(destination: .channel, entityID: "same-conversation")
        func incoming(_ event: String) -> AttentionEntry {
            .init(id: "same-conversation", eventID: event, created: 100, target: target,
                  sourceID: "same-conversation", groupKey: "conversation-lifetime", notify: true)
        }
        let base = evaluate(snapshot([category(.channels, [incoming("baseline-message")])]))
        XCTAssertTrue(base.notifications.isEmpty)
        // Conversation ID, source, group and count all stay unchanged; only event_id advances.
        let newEvent = snapshot([category(.channels, [incoming("new-message")])])
        let next = evaluate(newEvent, old: base.checkpoint)
        XCTAssertEqual(next.notifications.count, 1)
        XCTAssertEqual(next.notifications.first?.target, target)
        XCTAssertEqual(next.checkpoint.seen, ["event:channels:baseline-message", "event:channels:new-message"])
        let repeatEvent = evaluate(newEvent, old: next.checkpoint)
        XCTAssertTrue(repeatEvent.notifications.isEmpty)
        let read = evaluate(snapshot([category(.channels)]), old: repeatEvent.checkpoint)
        XCTAssertTrue(evaluate(newEvent, old: read.checkpoint).notifications.isEmpty)
        let another = evaluate(snapshot([category(.channels, [incoming("another-message")])]), old: read.checkpoint)
        XCTAssertEqual(another.notifications.count, 1)
    }
    func testExistingChannelDisabledOrForegroundEventDoesNotMuteLaterMessages() {
        let target = try! AttentionTarget(destination: .channel, entityID: "same-conversation")
        func incoming(_ event: String) -> AttentionEntry {
            .init(id: "same-conversation", eventID: event, created: 100, target: target, notify: true)
        }
        let base = evaluate(snapshot([category(.channels)])).checkpoint
        let disabledData = snapshot([category(.channels, [incoming("disabled-message")])])
        let disabled = evaluate(disabledData, old: base, preferences: AttentionPreferences())
        XCTAssertTrue(disabled.notifications.isEmpty)
        XCTAssertTrue(evaluate(disabledData, old: disabled.checkpoint).notifications.isEmpty)
        let viewingData = snapshot([category(.channels, [incoming("viewing-message")])])
        let viewing = evaluate(viewingData, old: disabled.checkpoint,
                               foreground: .init(scope: testScope, target: target, activelyViewing: true))
        XCTAssertTrue(viewing.notifications.isEmpty)
        let later = evaluate(snapshot([category(.channels, [incoming("later-message")])]), old: viewing.checkpoint)
        XCTAssertEqual(later.notifications.count, 1)
    }
    func testDisabledCategoriesAreRememberedWithoutReplayWhenEnabled() {
        let base = evaluate(snapshot([category()])).checkpoint
        var p = enabled(); p.enabledCategories.remove(.replies)
        let data = snapshot([category(.replies, [entry("one")])])
        let disabled = evaluate(data, old: base, preferences: p)
        XCTAssertTrue(disabled.notifications.isEmpty)
        XCTAssertTrue(evaluate(data, old: disabled.checkpoint).notifications.isEmpty)
        XCTAssertFalse(AttentionPreferences().enabled)
        XCTAssertFalse(AttentionPreferences().privacyPreviews)
        XCTAssertFalse(AttentionPreferences().sound)
    }
    func testSharedGroupReferencesDedupAcrossCategoriesAndBacklogCollapsesClearly() {
        let base = evaluate(snapshot(AttentionKind.allCases.map { category($0) })).checkpoint
        let data = snapshot([
            category(.replies, [entry("reply", group: "shared")]),
            category(.feed, [entry("delivery", kind: .feed, group: "shared")]),
            category(.pending, [entry("card", kind: .pending, group: "shared")])
        ])
        let grouped = evaluate(data, old: base)
        XCTAssertEqual(grouped.notifications.count, 1)
        XCTAssertEqual(grouped.notifications[0].kinds, [.replies, .feed, .pending])
        XCTAssertEqual(grouped.notifications[0].eventCount, 1)
        let backlog = evaluate(snapshot([category(.replies, (0..<45).map { entry("e\($0)") })]), old: base)
        XCTAssertEqual(backlog.notifications.count, 1)
        XCTAssertTrue(backlog.notifications[0].isSummary)
        XCTAssertEqual(backlog.notifications[0].eventCount, 45)
        XCTAssertEqual(backlog.checkpoint.seen.count, 45)
    }
    func testCapacityIsExplicitFailClosedWithoutEvictionAndMissingCategoryGetsSilentBaseline() {
        let policy = AttentionPolicy(maximumRememberedIDs: 1)
        let base = evaluate(snapshot([category(.replies, [entry("one")])]), policy: policy)
        XCTAssertEqual(base.status, .ready)
        let full = evaluate(snapshot([category(.replies, [entry("two")])]), old: base.checkpoint, policy: policy)
        XCTAssertEqual(full.status, .capacityReached)
        XCTAssertTrue(full.notifications.isEmpty)
        XCTAssertEqual(full.checkpoint.seen, base.checkpoint.seen)
        let later = evaluate(snapshot([category()]), old: full.checkpoint, policy: policy)
        XCTAssertEqual(later.status, .capacityReached)
        let supportedLater = evaluate(snapshot([category(.feed, [entry("old", kind: .feed)])]), old: base.checkpoint)
        XCTAssertTrue(supportedLater.notifications.isEmpty)
    }
    func testAllowlistedTargetNormalizationRejectsURLsAndBadRoutes() throws {
        XCTAssertEqual(try AttentionTarget.normalize(kind: .channels, route: "", entityID: "chan:space1"),
                       try AttentionTarget(destination: .channel, entityID: "space1"))
        for route in ["#/feed", "javascript:alert(1)", "https://example.test", ""] {
            XCTAssertThrowsError(try AttentionTarget.normalize(kind: .feed, route: route, entityID: "entry1"))
        }
        XCTAssertThrowsError(try AttentionTarget.normalize(kind: .channels, route: "", entityID: "chan:"))
        XCTAssertThrowsError(try AttentionTarget(destination: .reply, entityID: "x#other"))
    }
    func testDecoderStrictScopeVersionUnitsAndTolerantOptionalExtras() throws {
        let json = """
        {"v":1,"epoch":"epoch1","user_id":"user-test","workspace_id":"ws-test","history_policy":"since_attention_v1",
         "new_optional":"ignored","categories":[{"kind":"replies","count":1,"unit":"messages","available":true,
         "revision":"r1","snapshot":"s1","entries":[{"id":"m1","event_id":"ev1","created":22,"target":{"route":"","entity_id":"m1"}}]}]}
        """
        let decoded = try HubAttentionAPI.decode(Data(json.utf8), scope: testScope)
        XCTAssertEqual(decoded.categories[0].entries[0].target.entityID, "m1")
        let future = json.replacingOccurrences(of: "\"categories\":[", with: "\"categories\":[{\"kind\":\"future_kind\",\"different_schema\":true},")
        XCTAssertEqual(try HubAttentionAPI.decode(Data(future.utf8), scope: testScope).categories.count, 1)
        for malformed in [json.replacingOccurrences(of: "\"v\":1", with: "\"v\":2"),
                          json.replacingOccurrences(of: "user-test", with: "another-user"),
                          json.replacingOccurrences(of: "ws-test", with: "another-ws"),
                          json.replacingOccurrences(of: "messages", with: "cards"),
                          json.replacingOccurrences(of: "since_attention_v1", with: "all_history")] {
            XCTAssertThrowsError(try HubAttentionAPI.decode(Data(malformed.utf8), scope: testScope))
        }
    }
    func testPaginationCompletesSnapshotAndUsesCategoryCursor() async throws {
        let api = ScriptAPI([
            .success(page([category(.replies, [entry("a")], count: 3, next: "opaque1"), category(.pending)])),
            .success(page([category(.replies, [entry("b")], count: 3, next: "opaque2")])),
            .success(page([category(.replies, [entry("c")], count: 3)]))
        ])
        let result = try await AttentionSnapshotLoader.load(api: api, scope: testScope, endpoint: testEndpoint)
        XCTAssertEqual(result.categories[.replies]?.entries.map(\.id), ["a", "b", "c"])
        let calls = await api.history()
        XCTAssertEqual(calls.map(\.kind), [nil, .replies, .replies])
        XCTAssertEqual(calls.map(\.cursor), [nil, "opaque1", "opaque2"])
    }
    func testRevisionRaceRestartsWholeSnapshotWithoutMixingPartialPages() async throws {
        let api = ScriptAPI([
            .success(page([category(.replies, [entry("discard")], count: 2, next: "old")])),
            .success(page([category(.replies, [entry("race")], count: 2, revision: "r2", snapshot: "s2")])),
            .success(page([category(.replies, [entry("new")], revision: "r3", snapshot: "s3")]))
        ])
        let result = try await AttentionSnapshotLoader.load(api: api, scope: testScope, endpoint: testEndpoint)
        XCTAssertEqual(result.categories[.replies]?.entries.map(\.id), ["new"])
        let calls = await api.history(); XCTAssertEqual(calls.map(\.cursor), [nil, "old", nil])
    }
    func test409RestartsAndPersistentRaceFailsWithoutPublication() async throws {
        let api = ScriptAPI([
            .success(page([category(.replies, [entry("discard")], next: "p")])), .failure(.snapshotChanged),
            .success(page([category(.replies, [entry("discard2")], next: "p2")])), .failure(.snapshotChanged)
        ])
        do { _ = try await AttentionSnapshotLoader.load(api: api, scope: testScope, endpoint: testEndpoint); XCTFail("expected race") }
        catch { XCTAssertEqual(error as? AttentionError, .snapshotChanged) }
    }
    @MainActor func testMonitorPreservesLastSuccessOn401404TransientErrorsAndNoFakeZero() async throws {
        let api = ScriptAPI([.success(page([category(.replies, (0..<9).map { entry("old-\($0)") }, count: 9), category(.feed, available: false)])),
                             .failure(.transport(503)), .failure(.authRequired), .failure(.unsupported)])
        let delivery = Delivery(); let store = MemoryStore()
        let monitor = AttentionMonitor(api: api, store: store, delivery: delivery,
                                       schedule: .init(now: { Date(timeIntervalSince1970: 123) }))
        monitor.configure(scope: testScope, endpoint: testEndpoint, automaticallyPoll: false)
        XCTAssertNil(monitor.state.categories[.replies]?.lastSuccess)
        XCTAssertEqual(monitor.state.categories[.replies]?.phase, .loading)
        await monitor.refreshAndWait()
        XCTAssertEqual(monitor.state.categories[.replies]?.phase, .known)
        XCTAssertEqual(monitor.state.categories[.pending]?.phase, .unsupported)
        XCTAssertNil(monitor.state.categories[.pending]?.lastSuccess)
        XCTAssertEqual(monitor.state.categories[.feed]?.phase, .unsupported)
        XCTAssertEqual(monitor.state.updatedAt, Date(timeIntervalSince1970: 123))
        for phase: AttentionPhase in [.stale, .authRequired, .unsupported] {
            await monitor.refreshAndWait()
            XCTAssertEqual(monitor.state.categories[.replies]?.phase, phase)
            XCTAssertEqual(monitor.state.categories[.replies]?.lastSuccess?.count, 9)
            XCTAssertTrue(monitor.state.hasAttention)
        }
        XCTAssertTrue(delivery.notifications.isEmpty)
        try monitor.stop(clearPersisted: true)
        XCTAssertNil(store.records[testScope]); XCTAssertFalse(monitor.state.hasAttention)
        XCTAssertEqual(delivery.removed, [testScope])
    }
    @MainActor func testMonitorWritesCheckpointBeforeDeliveryAndStorageFailureNeverBanners() async {
        let api = ScriptAPI([.success(page([category()])), .success(page([category(.replies, [entry("new")])])),
                             .success(page([category(.replies, [entry("another")])]))])
        let delivery = Delivery(); let store = MemoryStore()
        let monitor = AttentionMonitor(api: api, store: store, delivery: delivery)
        monitor.configure(scope: testScope, endpoint: testEndpoint, automaticallyPoll: false)
        await monitor.refreshAndWait()
        delivery.onDeliver = { XCTAssertTrue(store.records[testScope]?.seen.contains("event:replies:event-new") == true) }
        await monitor.refreshAndWait()
        XCTAssertEqual(delivery.notifications.count, 1)
        store.fail = true
        await monitor.refreshAndWait()
        XCTAssertEqual(delivery.notifications.count, 1)
        XCTAssertEqual(monitor.state.dedupStatus, .storageUnavailable)
        XCTAssertEqual(monitor.state.categories[.replies]?.lastSuccess?.entries.first?.id, "another")
    }
    @MainActor func testDefaultsRelaunchPersistenceScopedPreferencesAndNoBodiesOrSecrets() async throws {
        let suite = "attention-test-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = try DefaultsAttentionStore(defaults: defaults)
        let install = store.installationEpoch
        let api = ScriptAPI([.success(page([category()])), .success(page([category(.replies, [entry("new")])]))])
        let delivery = Delivery()
        var prefs = enabled(); prefs.channelOverrides["space"] = true
        try store.savePreferences(prefs, scope: testScope)
        let monitor = AttentionMonitor(api: api, store: store, delivery: delivery)
        monitor.configure(scope: testScope, endpoint: testEndpoint, automaticallyPoll: false)
        await monitor.refreshAndWait(); await monitor.refreshAndWait()
        XCTAssertEqual(delivery.notifications.count, 1)
        try monitor.stop()
        let relaunchedStore = try DefaultsAttentionStore(defaults: defaults)
        XCTAssertEqual(relaunchedStore.installationEpoch, install)
        XCTAssertEqual(try relaunchedStore.preferences(scope: testScope), prefs)
        let newDelivery = Delivery()
        let newMonitor = AttentionMonitor(api: ScriptAPI([.success(page([category(.replies, [entry("new")])]))]),
                                          store: relaunchedStore, delivery: newDelivery)
        newMonitor.configure(scope: testScope, endpoint: testEndpoint, automaticallyPoll: false)
        await newMonitor.refreshAndWait()
        XCTAssertTrue(newDelivery.notifications.isEmpty)
        let raw = String(data: defaults.data(forKey: "steve.attention.v1")!, encoding: .utf8)!
        XCTAssertFalse(raw.contains("https:")); XCTAssertFalse(raw.contains("credential")); XCTAssertFalse(raw.contains("target"))
        let other = AttentionScope(hubProfileID: "other", userID: testScope.userID, workspaceID: testScope.workspaceID)
        XCTAssertNil(try relaunchedStore.load(scope: other))
        try newMonitor.stop(clearPersisted: true)
        XCTAssertNil(try relaunchedStore.load(scope: testScope))
    }
    func testLastPageCompletionTokenMayDifferAndIntermediateTokenMayBeAbsent() async throws {
        let api = ScriptAPI([
            .success(page([category(.replies, [entry("a")], count: 2, next: "opaque", snapshot: nil)])),
            .success(page([category(.replies, [entry("b")], count: 2, snapshot: "complete-token")]))
        ])
        let result = try await AttentionSnapshotLoader.load(api: api, scope: testScope, endpoint: testEndpoint)
        XCTAssertEqual(result.categories[.replies]?.snapshot, "complete-token")
        XCTAssertEqual(result.categories[.replies]?.entries.count, 2)
        let differentTokens = ScriptAPI([
            .success(page([category(.replies, [entry("a")], count: 2, next: "opaque", snapshot: "intermediate")])),
            .success(page([category(.replies, [entry("b")], count: 2, snapshot: "final")]))
        ])
        let complete = try await AttentionSnapshotLoader.load(api: differentTokens, scope: testScope, endpoint: testEndpoint)
        XCTAssertEqual(complete.categories[.replies]?.snapshot, "final")
    }
    func testCompletedSnapshotsRejectTruncatedAndOverfullEntriesEvenWithCompletionToken() async {
        let scripts: [[Result<AttentionPage, AttentionError>]] = [
            [.success(page([category(.replies, [entry("a")], count: 2)]))],
            [.success(page([category(.replies, [entry("a"), entry("b")], count: 1)]))],
            [.success(page([category(.replies, [entry("a")], count: 3, next: "next")])),
             .success(page([category(.replies, [entry("b")], count: 3)]))]
        ]
        for script in scripts {
            do {
                _ = try await AttentionSnapshotLoader.load(api: ScriptAPI(script), scope: testScope, endpoint: testEndpoint)
                XCTFail("count mismatch must not silently establish a partial baseline")
            } catch { XCTAssertEqual(error as? AttentionError, .invalidResponse) }
        }
    }

    func testMissingLastTokenRepeatedCursorAndDuplicateIDsFailClosed() async {
        let scripts: [[Result<AttentionPage, AttentionError>]] = [
            [.success(page([category(snapshot: nil)]))],
            [.success(page([category(next: "same")])), .success(page([category(next: "same")]))],
            [.success(page([category(.replies, [entry("a")], count: 2, next: "next")])),
             .success(page([category(.replies, [entry("a")], count: 2)]))],
            [.success(page([category(next: "next")])), .success(page([category(snapshot: nil)]))]
        ]
        for script in scripts {
            do {
                _ = try await AttentionSnapshotLoader.load(api: ScriptAPI(script), scope: testScope, endpoint: testEndpoint)
                XCTFail("invalid complete snapshot accepted")
            } catch { XCTAssertEqual(error as? AttentionError, .invalidResponse) }
        }
    }
    func testDecoderAllowsIntermediateWithoutSnapshotButRejectsMalformedFinalAndUnsolicitedUnchanged() throws {
        let json = """
        {"v":1,"epoch":"epoch1","user_id":"user-test","workspace_id":"ws-test","history_policy":"since_attention_v1",
         "categories":[{"kind":"channels","count":1,"unit":"conversations","available":true,
         "revision":"r1","next_cursor":"opaque&cursor","entries":[{"id":"m1","event_id":"ev1","created":22,
         "notify":true,"target":{"route":"","entity_id":"chan:space1"}}]}]}
        """
        let decoded = try HubAttentionAPI.decode(Data(json.utf8), scope: testScope)
        XCTAssertNil(decoded.categories.first?.snapshot)
        XCTAssertEqual(decoded.categories.first?.entries.first?.target.entityID, "space1")
        for bad in [json.replacingOccurrences(of: "\"next_cursor\":\"opaque&cursor\",", with: ""),
                    json.replacingOccurrences(of: "\"revision\":\"r1\"", with: "\"revision\":\"r1\",\"unchanged\":true"),
                    json.replacingOccurrences(of: "\"event_id\":\"ev1\"", with: "\"event_id\":\"\(String(repeating: "a", count: 513))\""),
                    json.replacingOccurrences(of: "chan:space1", with: "chan:bad/path")] {
            XCTAssertThrowsError(try HubAttentionAPI.decode(Data(bad.utf8), scope: testScope))
        }
    }
    func testHTTPUsesOnlyScopedGETWithEncodedCursorAndClassifiesFailures() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AttentionURLProtocol.self]
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }
        let api = HubAttentionAPI(session: session) { scope in
            XCTAssertEqual(scope, testScope); return "fixture-token"
        }
        for (status, expected) in [(401, AttentionError.authRequired), (403, .authRequired), (404, .unsupported), (501, .unsupported),
                                   (409, .snapshotChanged), (503, .transport(503))] {
            AttentionURLProtocol.response = { request in
                XCTAssertEqual(request.httpMethod, "GET")
                XCTAssertNil(request.httpBody)
                XCTAssertFalse(request.httpShouldHandleCookies)
                XCTAssertEqual(request.url?.path, "/api/me/attention")
                XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer fixture-token")
                XCTAssertEqual(request.value(forHTTPHeaderField: "x-steve-ws"), testScope.workspaceID)
                let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!
                XCTAssertEqual(Dictionary(uniqueKeysWithValues: query.map { ($0.name, $0.value!) }),
                               ["limit": "100", "kind": "channels", "cursor": "opaque&?=+雪"])
                return (status, Data())
            }
            do {
                _ = try await api.fetch(scope: testScope, endpoint: testEndpoint, kind: .channels, cursor: "opaque&?=+雪")
                XCTFail("HTTP failure accepted")
            } catch { XCTAssertEqual(error as? AttentionError, expected) }
        }
        let noCredential = HubAttentionAPI(session: session) { _ in nil }
        AttentionURLProtocol.response = { _ in XCTFail("missing credentials must not send a request"); return (200, Data()) }
        do { _ = try await noCredential.fetch(scope: testScope, endpoint: testEndpoint); XCTFail("expected auth") }
        catch { XCTAssertEqual(error as? AttentionError, .authRequired) }
    }
    func testDMNotifyTrueAndExplicitGroupOptInWithoutQuietHeuristics() {
        let base = evaluate(snapshot([category(.channels)])).checkpoint
        let data = snapshot([category(.channels, [entry("dm", kind: .channels, notify: true),
            entry("group", kind: .channels, notify: false)])])
        let result = evaluate(data, old: base)
        XCTAssertEqual(result.notifications.map(\.target.entityID), ["dm"])
        var p = enabled(); p.channelOverrides["group"] = true
        XCTAssertEqual(evaluate(data, old: result.checkpoint, preferences: p).notifications.count, 0)
    }
    @MainActor func testLateOldScopeResultCannotPublishOrPersistAndSwitchRemovesOldNotifications() async {
        let api = SuspendedAttentionAPI(); let store = MemoryStore(); let delivery = Delivery()
        let monitor = AttentionMonitor(api: api, store: store, delivery: delivery)
        monitor.configure(scope: testScope, endpoint: testEndpoint, automaticallyPoll: false)
        let oldRequest = Task { await monitor.refreshAndWait() }
        await api.waitUntilStarted()
        let next = AttentionScope(hubProfileID: "other-hub", userID: "other-user", workspaceID: "other-ws")
        monitor.configure(scope: next, endpoint: testEndpoint, automaticallyPoll: false)
        let newRequest = Task { await monitor.refreshAndWait() }
        await api.finish(page([category(.replies, [entry("old-private-id")])]))
        await oldRequest.value; await newRequest.value
        XCTAssertEqual(monitor.state.scope, next)
        XCTAssertEqual(monitor.state.categories[.replies]?.lastSuccess?.count, 0)
        XCTAssertNil(store.records[testScope])
        XCTAssertNotNil(store.records[next])
        XCTAssertEqual(delivery.removed, [testScope])
        XCTAssertTrue(delivery.notifications.isEmpty)
        try? monitor.stop()
    }
    @MainActor func testConcurrentRefreshesJoinOneRequestAndStopDiscardsLateResult() async throws {
        let api = SuspendedAttentionAPI(); let store = MemoryStore(); let delivery = Delivery()
        let monitor = AttentionMonitor(api: api, store: store, delivery: delivery)
        monitor.configure(scope: testScope, endpoint: testEndpoint, automaticallyPoll: false)
        let first = Task { await monitor.refreshAndWait() }
        await api.waitUntilStarted()
        let joined = Task { await monitor.refreshAndWait() }
        await Task.yield()
        try monitor.stop(clearPersisted: true)
        await api.finish(page([category(.replies, [entry("late")])]))
        await first.value; await joined.value
        let calls = await api.calls
        XCTAssertEqual(calls, 1)
        XCTAssertNil(monitor.state.scope)
        XCTAssertTrue(store.records.isEmpty)
        XCTAssertTrue(delivery.notifications.isEmpty)
    }
    @MainActor func testStoreCapacityRefusesEvictionAndCorruptArchiveIsNotSilentlyReset() throws {
        let suite = "attention-test-" + UUID().uuidString
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = try DefaultsAttentionStore(defaults: defaults, maxScopes: 1)
        let checkpoint = AttentionCheckpoint(epoch: "e", installationEpoch: store.installationEpoch)
        try store.save(checkpoint, scope: testScope)
        let other = AttentionScope(hubProfileID: "other", userID: "u", workspaceID: "w")
        XCTAssertThrowsError(try store.save(checkpoint, scope: other))
        XCTAssertNotNil(try store.load(scope: testScope))
        defaults.set(Data("not-json".utf8), forKey: "steve.attention.v1")
        XCTAssertThrowsError(try DefaultsAttentionStore(defaults: defaults))
    }
    @MainActor func testAutomaticBackgroundPollingAndWakeRefreshNeedNoWebView() async throws {
        let api = ScriptAPI([.success(page([category()])), .success(page([category(.replies, [entry("wake-new")])]))])
        let store = MemoryStore(); let delivery = Delivery()
        let baseline = expectation(description: "automatic silent baseline")
        let banner = expectation(description: "wake refresh delivery")
        let monitor = AttentionMonitor(api: api, store: store, delivery: delivery,
                                       schedule: .init(cadence: 3600, maximumBackoff: 3600))
        var gotBaseline = false
        monitor.onStateChange = { state in
            if !gotBaseline, state.categories[.replies]?.phase == .known {
                gotBaseline = true; baseline.fulfill()
            }
        }
        delivery.onDeliver = { banner.fulfill() }
        monitor.configure(scope: testScope, endpoint: testEndpoint)
        await fulfillment(of: [baseline], timeout: 2)
        XCTAssertTrue(delivery.notifications.isEmpty)
        monitor.refresh()
        await fulfillment(of: [banner], timeout: 2)
        XCTAssertEqual(monitor.state.categories[.replies]?.lastSuccess?.count, 1)
        XCTAssertEqual(delivery.notifications.count, 1)
        let calls = await api.history()
        XCTAssertEqual(calls.count, 2)
        try monitor.stop()
    }
    func testGloballyDisabledSourcesRemainRememberedWhenEnabledLater() {
        let base = evaluate(snapshot([category()])).checkpoint
        let data = snapshot([category(.replies, [entry("disabled-new")])])
        let disabled = evaluate(data, old: base, preferences: AttentionPreferences())
        XCTAssertTrue(disabled.notifications.isEmpty)
        XCTAssertTrue(evaluate(data, old: disabled.checkpoint, preferences: enabled()).notifications.isEmpty)
    }
    func testIdentityBootstrapUsesNativeBearerAndRejectsMissingOrForeignShape() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [AttentionURLProtocol.self]
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel() }
        AttentionURLProtocol.response = { request in
            XCTAssertEqual(request.url?.path, "/api/me")
            XCTAssertEqual(request.httpMethod, "GET")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer native-fixture-token")
            XCTAssertFalse(request.httpShouldHandleCookies)
            return (200, Data("{\"user\":{\"id\":\"native-user\"}}".utf8))
        }
        let user = try await HubAttentionAPI.authenticatedUserID(endpoint: testEndpoint, credential: "native-fixture-token", session: session)
        XCTAssertEqual(user, "native-user")
        for (status, body, expected) in [(401, "{}", AttentionError.authRequired), (403, "{}", .authRequired), (200, "{\"user\":{\"name\":\"not-an-id\"}}", .invalidResponse)] {
            AttentionURLProtocol.response = { _ in (status, Data(body.utf8)) }
            do {
                _ = try await HubAttentionAPI.authenticatedUserID(endpoint: testEndpoint, credential: "native-fixture-token", session: session)
                XCTFail("invalid native identity accepted")
            } catch { XCTAssertEqual(error as? AttentionError, expected) }
        }
    }
    func testBackoffIsBoundedAndCadenceConfigurable() {
        let schedule = AttentionSchedule(cadence: 5, maximumBackoff: 30)
        XCTAssertEqual((0...5).map { schedule.delay(failures: $0) }, [5, 10, 20, 30, 30, 30])
    }
}
