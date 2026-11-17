import Foundation

/// Exact entities actually rendered by the trusted current document. No user or Hub identity is accepted from Web.
public struct NativeAttentionVisibility: Equatable, Sendable {
    public let hubID: String
    public let generation: UInt64
    public let documentID: String
    public let workspaceID: String
    public let targets: Set<AttentionTarget>
    public let focused: Bool
    public let visible: Bool

    public func activelyViews(_ target: AttentionTarget, scope: AttentionScope,
                             context: NativeShellContext?, nativeFocused: Bool) -> Bool {
        guard nativeFocused, focused, visible, let context, context.ready, context.authenticated,
              context.focused, context.visible, context.hubID == scope.hubProfileID,
              context.workspaceID == scope.workspaceID, hubID == scope.hubProfileID,
              workspaceID == scope.workspaceID, documentID == context.documentID,
              generation == context.generation else { return false }
        return targets.contains(target)
    }
}

/// Unready startup/reload is unknown, not sign-out. Only ready authenticated metadata confirms a workspace.
public enum NativeAttentionContextUpdate: Equatable, Sendable {
    case transient
    case signedOut
    case workspace(id: String, name: String?)

    public static func classify(_ context: NativeShellContext?, hubID: String) -> Self {
        guard let context, context.hubID == hubID, context.ready else { return .transient }
        guard context.authenticated else { return .signedOut }
        guard let id = context.workspaceID else { return .transient }
        return .workspace(id: id, name: context.workspaceName)
    }
}

extension AttentionTarget {
    /// Matches encodeURIComponent, and never accepts a notification-supplied URL or route.
    public var nativeRoute: String {
        let safe = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()")
        func encode(_ value: String) -> String { value.addingPercentEncoding(withAllowedCharacters: safe)! }
        switch destination {
        case .reply: return "#/"
        case .pending: return "#/inbox"
        case .feed: return "#/feed?entry=" + encode(entityID)
        case .channel: return "#/c/" + encode("chan:" + entityID)
        }
    }
}

@MainActor extension SteveClientCoordinator {
    func acceptAttentionVisibility(_ args: [String: Any], hubID: String, documentID: String) throws {
        guard let context = shellContext, context.authenticated, context.ready,
              context.hubID == hubID, context.documentID == documentID,
              context.generation == shellState.generation,
              let workspace = args["workspaceID"] as? String, workspace == context.workspaceID,
              let focused = Self.shellBoolean(args["focused"]), let visible = Self.shellBoolean(args["visible"]),
              let raw = args["targets"] as? [[String: Any]], raw.count <= 256 else {
            throw BridgeCallError.invalidArgument("Stale attention visibility scope")
        }
        let targets = try Set(raw.map { item -> AttentionTarget in
            guard let name = item["destination"] as? String, let destination = AttentionTarget.Destination(rawValue: name),
                  let id = item["entityID"] as? String else { throw BridgeCallError.invalidArgument("Invalid visible attention target") }
            return try AttentionTarget(destination: destination, entityID: id)
        })
        shellAttentionVisibility = NativeAttentionVisibility(hubID: hubID, generation: shellState.generation,
            documentID: documentID, workspaceID: workspace, targets: targets, focused: focused, visible: visible)
    }

    func hasLandedDeepTarget(_ intent: NativeNavigationIntent, context: NativeShellContext) -> Bool {
        guard let id = intent.targetID, let report = shellAttentionVisibility,
              report.hubID == context.hubID, report.documentID == context.documentID,
              report.generation == context.generation, report.workspaceID == context.workspaceID,
              report.visible else { return false }
        return report.targets.contains { $0.entityID == id && $0.nativeRoute == intent.route && context.route == $0.nativeRoute }
    }
}
