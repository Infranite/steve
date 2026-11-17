import Foundation

public struct AttentionSchedule: Sendable {
    public var cadence: TimeInterval
    public var maximumBackoff: TimeInterval
    public var now: @Sendable () -> Date
    public var sleep: @Sendable (TimeInterval) async throws -> Void
    public init(cadence: TimeInterval = 30, maximumBackoff: TimeInterval = 900,
                now: @escaping @Sendable () -> Date = { Date() },
                sleep: @escaping @Sendable (TimeInterval) async throws -> Void = {
                    try await Task.sleep(for: .seconds($0))
                }) {
        self.cadence = max(1, cadence); self.maximumBackoff = max(self.cadence, maximumBackoff)
        self.now = now; self.sleep = sleep
    }
    public func delay(failures: Int) -> TimeInterval {
        min(maximumBackoff, cadence * pow(2, Double(min(max(0, failures), 20))))
    }
}

/// Sole native notification producer; no dependency on WebView and no read/approval API.
/// All callbacks and scope changes are MainActor-isolated. Requests are serialized even if an
/// injected transport ignores cancellation. Generation checks reject every late result.
@MainActor public final class AttentionMonitor {
    public private(set) var state = AttentionState()
    public var onStateChange: ((AttentionState) -> Void)?
    public var foreground: AttentionForeground?
    public private(set) var preferences = AttentionPreferences()
    private let api: any AttentionAPI
    private let store: any AttentionCheckpointStore
    private let delivery: any AttentionDelivering
    private let policy: AttentionPolicy
    private let schedule: AttentionSchedule
    private var endpoint: URL?
    private var generation: UInt64 = 0
    private var runner: Task<Void, Never>?
    private var request: Task<Void, Never>?
    private var sleeper: Task<Void, Error>?
    private var failures = 0
    private var refreshRequested = false

    public init(api: any AttentionAPI, store: any AttentionCheckpointStore, delivery: any AttentionDelivering,
                policy: AttentionPolicy = .init(), schedule: AttentionSchedule = .init()) {
        self.api = api; self.store = store; self.delivery = delivery; self.policy = policy; self.schedule = schedule
    }
    deinit { runner?.cancel(); request?.cancel(); sleeper?.cancel() }

    /// Call only with native-verified identity/workspace. Endpoint changes also invalidate in-flight requests.
    /// Set automaticallyPoll=false for a deterministic host/test that calls refreshAndWait itself.
    public func configure(scope: AttentionScope, endpoint: URL, automaticallyPoll: Bool = true) {
        let oldScope = state.scope
        invalidate()
        if let oldScope { delivery.remove(scope: oldScope) }
        self.endpoint = endpoint; foreground = nil; failures = 0
        state = AttentionState(scope: scope)
        for kind in AttentionKind.allCases { state.categories[kind] = .init(phase: .loading) }
        do { preferences = try store.preferences(scope: scope) }
        catch { preferences = .init(); state.dedupStatus = .storageUnavailable }
        publish()
        guard automaticallyPoll else { return }
        let token = generation
        runner = Task { [weak self] in
            while !Task.isCancelled {
                guard let sleeper = await self?.runCycle(token: token) else { return }
                _ = await sleeper.result
                self?.finishedSleep(token: token)
            }
        }
    }
    private func runCycle(token: UInt64) async -> Task<Void, Error>? {
        guard generation == token, !Task.isCancelled else { return nil }
        await refreshAndWait()
        guard generation == token, !Task.isCancelled else { return nil }
        let sleep = schedule.sleep
        let delay = refreshRequested ? 0 : schedule.delay(failures: failures)
        refreshRequested = false
        let sleeper = Task { if delay > 0 { try await sleep(delay) } }
        self.sleeper = sleeper
        return sleeper
    }
    private func finishedSleep(token: UInt64) {
        guard generation == token else { return }
        sleeper = nil; refreshRequested = false
    }
    /// Wake/activation/reconnect should call this; rapid refreshes coalesce, never overlap requests.
    public func refresh() {
        refreshRequested = true
        sleeper?.cancel()
    }
    public func refreshAndWait() async {
        guard let scope = state.scope, let endpoint else { return }
        let token = generation
        // Same-generation callers join one request instead of issuing redundant polls.
        if let request, requestGeneration == token { await request.value; return }
        let predecessor = request
        requestSequence &+= 1
        let sequence = requestSequence
        requestGeneration = token
        let task = Task { [weak self] in
            await predecessor?.value
            guard let self, !Task.isCancelled, self.generation == token else { return }
            await self.poll(scope: scope, endpoint: endpoint, token: token)
        }
        request = task
        await task.value
        if generation == token, requestSequence == sequence { request = nil; requestGeneration = nil }
    }
    private var requestGeneration: UInt64?
    private var requestSequence: UInt64 = 0

    public func setPreferences(_ preferences: AttentionPreferences) throws {
        guard let scope = state.scope else { return }
        try store.savePreferences(preferences, scope: scope)
        self.preferences = preferences
        // Changing preferences never replays previously observed sources.
    }
    /// Use clearPersisted=true on logout/removal. Ordinary Hub switching retains scoped dedup memory.
    /// Stop first, then remove; storage failures are reported to the caller, not silently ignored.
    public func stop(clearPersisted: Bool = false) throws {
        let oldScope = state.scope
        invalidate(); endpoint = nil; foreground = nil; preferences = .init()
        state = AttentionState(); publish()
        if let oldScope {
            delivery.remove(scope: oldScope)
            if clearPersisted { try store.remove(scope: oldScope) }
        }
    }
    /// Removes a non-active Hub/user/workspace after the host enumerates its known native scopes.
    public func remove(scope: AttentionScope) throws {
        if state.scope == scope { try stop(clearPersisted: true) }
        else { delivery.remove(scope: scope); try store.remove(scope: scope) }
    }
    private func invalidate() {
        generation &+= 1
        runner?.cancel(); runner = nil; sleeper?.cancel(); sleeper = nil
        request?.cancel(); refreshRequested = false
    }
    private func poll(scope: AttentionScope, endpoint: URL, token: UInt64) async {
        do {
            let snapshot = try await AttentionSnapshotLoader.load(api: api, scope: scope, endpoint: endpoint)
            guard generation == token, !Task.isCancelled else { return }
            var notifications: [AttentionNotification] = []
            do {
                let checkpoint = try store.load(scope: scope)
                let result = policy.evaluate(snapshot: snapshot, scope: scope, installationEpoch: store.installationEpoch,
                                             checkpoint: checkpoint, preferences: preferences, foreground: foreground)
                // Persist BEFORE delivery: a crash may lose a banner but must never replay one on relaunch.
                try store.save(result.checkpoint, scope: scope)
                state.dedupStatus = result.status; notifications = result.notifications
            } catch { state.dedupStatus = .storageUnavailable }
            for kind in AttentionKind.allCases {
                if let category = snapshot.categories[kind] {
                    state.categories[kind] = .init(phase: category.available ? .known : .unsupported,
                                                  lastSuccess: category.available ? category : state.categories[kind]?.lastSuccess)
                } else {
                    state.categories[kind] = .init(phase: .unsupported, lastSuccess: state.categories[kind]?.lastSuccess)
                }
            }
            state.updatedAt = schedule.now(); failures = 0; publish()
            for notification in notifications {
                guard generation == token, !Task.isCancelled else { return }
                await delivery.deliver(notification)
            }
        } catch {
            guard generation == token, !Task.isCancelled else { return }
            failures = min(failures + 1, 20)
            let phase: AttentionPhase
            switch error {
            case AttentionError.authRequired: phase = .authRequired
            case AttentionError.unsupported: phase = .unsupported
            default: phase = .stale
            }
            for kind in AttentionKind.allCases {
                state.categories[kind] = .init(phase: phase, lastSuccess: state.categories[kind]?.lastSuccess)
            }
            publish()
        }
    }
    private func publish() { delivery.updateIndicator(state); onStateChange?(state) }
}
