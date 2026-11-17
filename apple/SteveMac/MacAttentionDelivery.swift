import AppKit
import SteveClientKit
import SwiftUI
import UserNotifications

@MainActor protocol MacAttentionNotificationCenter: AnyObject {
    func install(delegate: UNUserNotificationCenterDelegate)
    func authorizationStatus() async -> UNAuthorizationStatus
    func requestAuthorization(sound: Bool) async throws -> Bool
    func add(_ request: UNNotificationRequest) async throws
    func delivered() async -> [UNNotification]
    func removeDelivered(_ identifiers: [String])
    func removePending(_ identifiers: [String])
}

@MainActor final class SystemAttentionNotificationCenter: MacAttentionNotificationCenter {
    private let center: UNUserNotificationCenter
    init(center: UNUserNotificationCenter = .current()) { self.center = center }
    func install(delegate: UNUserNotificationCenterDelegate) { center.delegate = delegate }
    func authorizationStatus() async -> UNAuthorizationStatus { await center.notificationSettings().authorizationStatus }
    func requestAuthorization(sound: Bool) async throws -> Bool {
        try await center.requestAuthorization(options: sound ? [.alert, .badge, .sound] : [.alert, .badge])
    }
    func add(_ request: UNNotificationRequest) async throws { try await center.add(request) }
    func delivered() async -> [UNNotification] { await center.deliveredNotifications() }
    func removeDelivered(_ identifiers: [String]) { center.removeDeliveredNotifications(withIdentifiers: identifiers) }
    func removePending(_ identifiers: [String]) { center.removePendingNotificationRequests(withIdentifiers: identifiers) }
}

/// App-level adapter, intentionally not installed by initialization. The host must retain this object,
/// call installDelegate early at app launch, and wire onOpen to its scoped navigation/restore path.
/// System Focus/DND is respected; there are no critical/time-sensitive overrides or action categories.
@MainActor final class MacAttentionDelivery: NSObject, ObservableObject, AttentionDelivering, UNUserNotificationCenterDelegate {
    @Published private(set) var permission: UNAuthorizationStatus = .notDetermined
    @Published private(set) var deliveryFailed = false
    var onOpen: ((AttentionScope, AttentionTarget) -> Void)?
    /// Rechecked at actual foreground delivery time, in addition to monitor-side suppression.
    var presentationAllowed: ((AttentionScope, AttentionTarget) -> Bool)?
    private let center: any MacAttentionNotificationCenter
    private let setDockBadge: @MainActor (String?) -> Void
    private var generations: [AttentionScope: UInt64] = [:]
    private var submitted: [AttentionScope: Set<String>] = [:]
    private let prefix = "steve.attention.v1."

    init(center: (any MacAttentionNotificationCenter)? = nil,
         setDockBadge: @escaping @MainActor (String?) -> Void = { NSApp.dockTile.badgeLabel = $0 }) {
        self.center = center ?? SystemAttentionNotificationCenter(); self.setDockBadge = setDockBadge
        super.init()
    }
    func installDelegate() { center.install(delegate: self) }
    func refreshPermission() async { permission = await center.authorizationStatus() }
    /// Call only from the explicit Enable button, never startup, polling or setPreferences.
    func requestPermissionFromUserAction(sound: Bool) async {
        do { _ = try await center.requestAuthorization(sound: sound); deliveryFailed = false }
        catch { deliveryFailed = true }
        await refreshPermission()
    }
    func openSystemNotificationSettings() {
        // Fixed OS settings destination, unrelated to notification payload/navigation.
        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }
    func updateIndicator(_ state: AttentionState) { setDockBadge(state.hasAttention ? "•" : nil) }
    func deliver(_ notification: AttentionNotification) async {
        let generation = generations[notification.scope, default: 0]
        let status = await center.authorizationStatus()
        guard generations[notification.scope, default: 0] == generation else { return }
        permission = status
        guard status == .authorized || status == .provisional else { return }
        if presentationAllowed?(notification.scope, notification.target) == false { return }
        let content = UNMutableNotificationContent()
        content.title = L10n.tr("Steve 有新提醒", "New attention in Steve")
        if notification.isSummary {
            content.body = L10n.tr("有 \(notification.eventCount) 个新提醒，请打开 Steve 查看。",
                                   "\(notification.eventCount) new notifications. Open Steve to review.")
        } else if notification.privacyPreviews {
            // Category-only preview. No body/title fetching or business text storage is necessary.
            let labels = AttentionKind.allCases.filter { notification.kinds.contains($0) }.map { kind in
                switch kind {
                case .replies: return L10n.tr("Steve 回复", "Steve reply")
                case .pending: return L10n.tr("待处理", "Needs attention")
                case .feed: return L10n.tr("完成与交付", "Completion and delivery")
                case .channels: return L10n.tr("渠道消息", "Channel conversation")
                }
            }
            content.body = labels.joined(separator: " · ")
        } else { content.body = L10n.tr("打开 Steve 查看新提醒。", "Open Steve to review your notification.") }
        if notification.sound { content.sound = .default }
        content.userInfo = Self.payload(scope: notification.scope, target: notification.target)
        let identifier = prefix + notification.id
        submitted[notification.scope, default: []].insert(identifier)
        do {
            try await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
            if generations[notification.scope, default: 0] != generation {
                center.removeDelivered([identifier]); center.removePending([identifier])
            } else { deliveryFailed = false }
        } catch { deliveryFailed = true }
    }
    func remove(scope: AttentionScope) {
        generations[scope, default: 0] &+= 1
        let known = Array(submitted.removeValue(forKey: scope) ?? [])
        center.removePending(known); center.removeDelivered(known)
        Task { [weak self] in
            guard let self else { return }
            let notifications = await self.center.delivered()
            let identifiers = notifications.compactMap { item -> String? in
                let request = item.request
                guard request.identifier.hasPrefix(self.prefix),
                      Self.decodePayload(request.content.userInfo)?.0 == scope,
                      self.submitted[scope]?.contains(request.identifier) != true else { return nil }
                return request.identifier
            }
            self.center.removeDelivered(identifiers)
        }
    }
    private static func payload(scope: AttentionScope, target: AttentionTarget) -> [String: String] {
        ["v": "1", "hub": scope.hubProfileID, "user": scope.userID, "workspace": scope.workspaceID,
         "destination": target.destination.rawValue, "entity": target.entityID]
    }
    private static func decodePayload(_ payload: [AnyHashable: Any]) -> (AttentionScope, AttentionTarget)? {
        let allowed: Set<String> = ["v", "hub", "user", "workspace", "destination", "entity"]
        guard Set(payload.keys.compactMap { $0 as? String }) == allowed,
              payload.count == allowed.count, payload["v"] as? String == "1",
              let hub = payload["hub"] as? String, AttentionTarget.validID(hub),
              let user = payload["user"] as? String, AttentionTarget.validID(user),
              let workspace = payload["workspace"] as? String, AttentionTarget.validID(workspace),
              let destination = payload["destination"] as? String,
              let kind = AttentionTarget.Destination(rawValue: destination),
              let entity = payload["entity"] as? String,
              let target = try? AttentionTarget(destination: kind, entityID: entity) else { return nil }
        return (.init(hubProfileID: hub, userID: user, workspaceID: workspace), target)
    }
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        // Dismissal is not navigation or read. No approval/send/read actions are registered.
        let payload = response.notification.request.content.userInfo
        let isOpen = response.actionIdentifier == UNNotificationDefaultActionIdentifier
        Task { @MainActor [weak self] in
            defer { completionHandler() }
            guard isOpen, let (scope, target) = Self.decodePayload(payload) else { return }
            self?.onOpen?(scope, target)
        }
    }
    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
        willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        let payload = notification.request.content.userInfo
        let hasSound = notification.request.content.sound != nil
        Task { @MainActor [weak self] in
            guard let self, let (scope, target) = Self.decodePayload(payload),
                  self.presentationAllowed?(scope, target) != false else { completionHandler([]); return }
            completionHandler(hasSound ? [.banner, .list, .sound] : [.banner, .list])
        }
    }
}
