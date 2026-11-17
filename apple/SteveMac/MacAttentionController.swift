import AppKit
import Combine
import SteveClientKit
import SwiftUI

/// App-lifetime owner: WebView supplies confirmed workspace/visible entities, never credentials or identity.
/// A transient document reload/closed window clears foreground only, not the monitor's confirmed workspace.
@MainActor final class MacAttentionController: ObservableObject {
    @Published private(set) var state = AttentionState()
    @Published private(set) var preferences = AttentionPreferences()
    @Published private(set) var error: String?
    @Published private(set) var workspaceName: String?
    let delivery: MacAttentionDelivery
    let monitor: AttentionMonitor
    private let coordinator: SteveClientCoordinator
    private let store: DefaultsAttentionStore?
    private var subscriptions = Set<AnyCancellable>()
    private var observers: [NSObjectProtocol] = []
    private var wakeObserver: NSObjectProtocol?
    private var identityTask: Task<Void, Never>?
    private var identityGeneration: UInt64 = 0
    private var identity: Identity?
    private var confirmedWorkspace: String?
    private var credentials: [String: String] = [:]
    private var activeHub: String
    private var blockedCredential: String?
    private var pendingOpen: (AttentionScope, AttentionTarget)?
    private var suspending = false
    private struct Identity { let hub: String; let user: String; let credential: String; let endpoint: URL }

    init(coordinator: SteveClientCoordinator, defaults: UserDefaults) {
        self.coordinator = coordinator
        activeHub = coordinator.hubStore.activeHubID
        let delivery = MacAttentionDelivery()
        self.delivery = delivery
        let storage = try? DefaultsAttentionStore(defaults: defaults)
        store = storage
        let hubStore = coordinator.hubStore
        let api = HubAttentionAPI { scope in
            await MainActor.run { hubStore.credential(for: scope.hubProfileID) }
        }
        monitor = AttentionMonitor(api: api, store: storage ?? UnavailableAttentionStore(), delivery: delivery)
        if storage == nil { error = L10n.tr("本机提醒存储不可用；系统通知已暂停。", "Local attention storage is unavailable; notifications are paused.") }
        monitor.onStateChange = { [weak self] value in self?.received(value) }
        delivery.onOpen = { [weak self] scope, target in self?.open(scope: scope, target: target) }
        delivery.presentationAllowed = { [weak self] scope, target in
            guard let self, self.monitor.state.scope == scope,
                  self.identity?.user == scope.userID,
                  self.coordinator.hubStore.activeHubID == scope.hubProfileID,
                  self.coordinator.hubStore.credential(for: scope.hubProfileID) == self.identity?.credential else { return false }
            return !self.isActivelyViewing(scope: scope, target: target)
        }
        delivery.installDelegate() // Registration never requests OS permission.
        coordinator.hubStore.$activeHubID.sink { [weak self] id in
            guard let self, id != self.activeHub else { return }
            self.activeHub = id
            if self.pendingOpen?.0.hubProfileID != id { self.pendingOpen = nil }
            self.invalidateIdentity(clearWorkspace: true)
            self.scheduleSync()
        }.store(in: &subscriptions)
        coordinator.hubStore.$profiles.sink { [weak self] profiles in
            self?.inspectProfiles(profiles)
            self?.scheduleSync()
        }.store(in: &subscriptions)
        coordinator.$shellContext.sink { [weak self] context in
            guard let self else { return }
            switch NativeAttentionContextUpdate.classify(context, hubID: self.activeHub) {
            case .signedOut:
                self.invalidateIdentity(clearWorkspace: true)
                self.cleanup(hub: self.activeHub)
            case .workspace(let workspace, let name):
                if self.confirmedWorkspace != workspace {
                    self.confirmedWorkspace = workspace
                    self.stopMonitor()
                }
                self.workspaceName = name
            case .transient:
                if let context, context.hubID == self.activeHub, context.authenticated,
                   let workspace = context.workspaceID, let previous = self.confirmedWorkspace, previous != workspace {
                    self.confirmedWorkspace = nil
                    self.stopMonitor()
                }
            }
            self.scheduleSync()
        }.store(in: &subscriptions)
        coordinator.$shellHubSwitchTarget.sink { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self, self.coordinator.shellHubSwitchTarget == nil,
                      let pending = self.pendingOpen,
                      pending.0.hubProfileID != self.coordinator.hubStore.activeHubID else { return }
                self.pendingOpen = nil // Declined/expired switch must not open later on an unrelated switch.
            }
        }.store(in: &subscriptions)
        coordinator.$shellAttentionVisibility.sink { [weak self] report in
            if report == nil { self?.monitor.foreground = nil }
            self?.scheduleForeground()
        }.store(in: &subscriptions)
        coordinator.$summary.sink { [weak self] _ in self?.scheduleSync() }.store(in: &subscriptions)
        for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification,
                     NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification,
                     NSWindow.willCloseNotification, NSWindow.didMiniaturizeNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.updateForeground(); self?.monitor.refresh() }
            })
        }
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.retry() }
        }
        scheduleSync()
    }

    private func scheduleSync() { Task { @MainActor [weak self] in self?.synchronize() } }
    private func scheduleForeground() { Task { @MainActor [weak self] in self?.updateForeground() } }
    private func inspectProfiles(_ profiles: [HubProfile]) {
        let ids = Set(profiles.map(\.id))
        for hub in Set((store?.scopes.map(\.hubProfileID) ?? []) + Array(credentials.keys)) {
            let token = coordinator.hubStore.credential(for: hub)
            if !ids.contains(hub) || token?.isEmpty != false { cleanup(hub: hub) }
        }
        for profile in profiles {
            let token = coordinator.hubStore.credential(for: profile.id)
            if let previous = credentials[profile.id], previous != token, profile.id == activeHub {
                invalidateIdentity(clearWorkspace: true)
            }
            credentials[profile.id] = token
        }
        credentials = credentials.filter { ids.contains($0.key) }
    }
    private func invalidateIdentity(clearWorkspace: Bool) {
        identityGeneration &+= 1; identityTask?.cancel(); identityTask = nil
        identity = nil; blockedCredential = nil
        if clearWorkspace { confirmedWorkspace = nil; workspaceName = nil }
        stopMonitor()
    }
    private func stopMonitor() {
        do { try monitor.stop() } catch { showStorageError() }
        preferences = .init()
    }
    private func cleanup(hub: String, excludingUser: String? = nil) {
        for scope in store?.scopes ?? [] where scope.hubProfileID == hub && scope.userID != excludingUser {
            do { try monitor.remove(scope: scope) } catch { showStorageError() }
        }
    }
    private func showStorageError() { error = L10n.tr("无法保存本机提醒设置或清理记录。", "Could not save local attention settings or clean up records.") }

    private func synchronize() {
        let hub = coordinator.hubStore.activeHubID
        guard hub == activeHub, let credential = coordinator.hubStore.credential(for: hub), !credential.isEmpty else {
            if monitor.state.scope != nil || identity != nil || identityTask != nil { invalidateIdentity(clearWorkspace: true) }
            return
        }
        // A sign-in page is authoritative for loss of authentication even if Keychain still holds an expired token.
        if let context = coordinator.shellContext, context.hubID == hub, context.ready, !context.authenticated { return }
        guard let profile = coordinator.hubStore.profiles.first(where: { $0.id == hub }) else { return }
        // Do not use the previous Hub's transient summary endpoint during a switch.
        let candidates = profile.orderedEndpoints
        let endpoint = coordinator.summary.activeEndpoint.flatMap { candidates.contains($0) ? $0 : nil } ?? candidates.first
        guard let endpoint, blockedCredential != credential else { return }
        if let identity, identity.hub == hub, identity.credential == credential, identity.endpoint == endpoint {
            configureIfReady(identity)
            return
        }
        guard identityTask == nil else { return }
        stopMonitor()
        identityGeneration &+= 1
        let generation = identityGeneration
        identityTask = Task { [weak self] in
            do {
                let user = try await HubAttentionAPI.authenticatedUserID(endpoint: endpoint, credential: credential)
                guard let self, !Task.isCancelled, self.identityGeneration == generation,
                      self.coordinator.hubStore.activeHubID == hub,
                      self.coordinator.hubStore.credential(for: hub) == credential else { return }
                self.identityTask = nil
                let identity = Identity(hub: hub, user: user, credential: credential, endpoint: endpoint)
                self.identity = identity
                if self.store != nil { self.error = nil }
                self.cleanup(hub: hub, excludingUser: user)
                self.configureIfReady(identity)
            } catch {
                guard let self, !Task.isCancelled, self.identityGeneration == generation else { return }
                self.identityTask = nil
                self.error = L10n.tr("无法验证提醒账号；请检查连接或重新登录。", "Could not verify the attention account; check the connection or sign in again.")
                if error as? AttentionError == .authRequired {
                    self.blockedCredential = credential
                    self.cleanup(hub: hub)
                    self.confirmedWorkspace = nil
                }
            }
        }
    }
    private func configureIfReady(_ identity: Identity) {
        if let pending = pendingOpen, pending.0.hubProfileID == identity.hub,
           let context = coordinator.shellContext, context.ready, context.authenticated, context.hubID == identity.hub {
            pendingOpen = nil
            if pending.0.userID == identity.user {
                _ = coordinator.navigate(route: pending.1.nativeRoute, workspaceID: pending.0.workspaceID, targetID: pending.1.entityID)
            } else { error = L10n.tr("该通知属于另一个账号，无法打开。", "This notification belongs to a different account and cannot be opened.") }
        }
        guard let workspace = confirmedWorkspace else { return }
        let scope = AttentionScope(hubProfileID: identity.hub, userID: identity.user, workspaceID: workspace)
        if monitor.state.scope != scope || monitorEndpoint != identity.endpoint {
            monitorEndpoint = identity.endpoint
            monitor.configure(scope: scope, endpoint: identity.endpoint)
            preferences = monitor.preferences
        }
        updateForeground()
    }
    private var monitorEndpoint: URL?
    private func received(_ value: AttentionState) {
        guard !suspending else { return }
        state = value
        if value.categories.values.contains(where: { $0.phase == .authRequired }), let scope = value.scope {
            suspending = true
            blockedCredential = coordinator.hubStore.credential(for: scope.hubProfileID)
            identityGeneration &+= 1; identityTask?.cancel(); identityTask = nil; identity = nil
            stopMonitor(); cleanup(hub: scope.hubProfileID)
            state = value // Preserve honest auth-required/stale data, not a fabricated zero.
            suspending = false
            error = L10n.tr("提醒需要重新登录；监测已暂停。", "Attention requires sign-in; monitoring is suspended.")
        }
    }
    func retry() { blockedCredential = nil; synchronize(); monitor.refresh(); Task { await delivery.refreshPermission() } }
    func setPreferences(_ value: AttentionPreferences) {
        do { try monitor.setPreferences(value); preferences = monitor.preferences; error = nil }
        catch { showStorageError() }
    }
    func isActivelyViewing(scope: AttentionScope, target: AttentionTarget) -> Bool {
        guard let report = coordinator.shellAttentionVisibility else { return false }
        let nativeFocused = NSApp.isActive && MacMainWindow.windows.contains {
            $0.isVisible && $0.isKeyWindow && !$0.isMiniaturized
        }
        return report.activelyViews(target, scope: scope, context: coordinator.shellContext, nativeFocused: nativeFocused)
    }
    private func updateForeground() {
        guard let scope = monitor.state.scope, let report = coordinator.shellAttentionVisibility,
              let target = report.targets.first, isActivelyViewing(scope: scope, target: target) else {
            monitor.foreground = nil; return
        }
        // Adapter also rechecks every exact target immediately before OS delivery.
        monitor.foreground = AttentionForeground(scope: scope, target: target, additionalTargets: report.targets, activelyViewing: true)
    }
    func open(scope: AttentionScope, target: AttentionTarget) {
        guard coordinator.hubStore.profiles.contains(where: { $0.id == scope.hubProfileID }),
              coordinator.hubStore.credential(for: scope.hubProfileID)?.isEmpty == false else {
            error = L10n.tr("通知对应的 Hub 已移除或退出登录。", "The notification's Hub was removed or signed out.")
            MacMainWindow.bringUpMainWindow(); return
        }
        pendingOpen = (scope, target)
        coordinator.switchToHub(id: scope.hubProfileID)
        MacMainWindow.bringUpMainWindow()
        scheduleSync()
    }
    func stop() {
        identityGeneration &+= 1; identityTask?.cancel(); identityTask = nil
        stopMonitor()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers.removeAll()
        if let wakeObserver { NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver) }
        wakeObserver = nil
    }
}

/// Fail closed when the persistent archive cannot be decoded; never silently reset a dedup epoch.
@MainActor private final class UnavailableAttentionStore: AttentionCheckpointStore {
    var installationEpoch: String { "unavailable" }
    func load(scope: AttentionScope) throws -> AttentionCheckpoint? { throw AttentionError.invalidResponse }
    func save(_ checkpoint: AttentionCheckpoint, scope: AttentionScope) throws { throw AttentionError.invalidResponse }
    func remove(scope: AttentionScope) throws { throw AttentionError.invalidResponse }
    func preferences(scope: AttentionScope) throws -> AttentionPreferences { throw AttentionError.invalidResponse }
    func savePreferences(_ preferences: AttentionPreferences, scope: AttentionScope) throws { throw AttentionError.invalidResponse }
}
