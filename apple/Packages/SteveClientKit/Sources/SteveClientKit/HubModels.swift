import Foundation

public enum L10n {
    public static var prefersChinese: Bool {
        guard let primary = Locale.preferredLanguages.first?.lowercased() else {
            return true
        }
        return primary.hasPrefix("zh")
    }

    public static func tr(_ zh: String, _ en: String) -> String {
        prefersChinese ? zh : en
    }

    public static var acceptLanguageHeader: String {
        let langs = Locale.preferredLanguages.prefix(3)
        guard !langs.isEmpty else {
            return "zh-CN,en;q=0.8"
        }
        var parts: [String] = []
        for (idx, lang) in langs.enumerated() {
            if idx == 0 {
                parts.append(lang)
            } else {
                let q = max(0.3, 1.0 - Double(idx) * 0.2)
                parts.append(String(format: "%@;q=%.1f", lang, q))
            }
        }
        return parts.joined(separator: ",")
    }
}

public enum HubRouteKind: String, Codable, Sendable, Equatable {
    case loopback
    case lan
    case privateOverlay = "overlay"
    case publicNetwork = "public"
    case proxy
    case unknown

    public var displayTitle: String {
        switch self {
        case .loopback: return L10n.tr("本机", "Local")
        case .lan: return L10n.tr("内网 / 局域网", "LAN")
        case .privateOverlay: return L10n.tr("专用组网", "Private overlay")
        case .publicNetwork: return L10n.tr("公网", "Public")
        case .proxy: return L10n.tr("代理", "Proxy")
        case .unknown: return L10n.tr("未知", "Unknown")
        }
    }

    /// Route label exposed to the web client via Native Bridge v2 (`NativeHub.route`).
    /// Standard codes (`loopback`, `lan`, `proxy`, `public`) are localized by the web UI;
    /// `privateOverlay` supplies vendor-neutral text directly.
    public var bridgeRouteValue: String? {
        switch self {
        case .loopback: return "loopback"
        case .lan: return "lan"
        case .privateOverlay: return L10n.tr("专用组网", "Private overlay")
        case .publicNetwork: return "public"
        case .proxy: return "proxy"
        case .unknown: return nil
        }
    }

    public static func classify(url: URL, explicitPublicURL: URL? = nil) -> HubRouteKind {
        guard let rawHost = url.host?.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "[]")) else {
            return .unknown
        }
        let host = rawHost.replacingOccurrences(of: "\\.+$", with: "", options: .regularExpression)
        guard !host.isEmpty else { return .unknown }
        if host == "localhost" || host.hasSuffix(".localhost") || isLoopbackIPv4(host) || host == "::1" {
            return .loopback
        }
        if isCarrierGradeNATIPv4(host) {
            return .privateOverlay
        }
        if isPrivateOrLinkLocalIPv4(host) {
            return .lan
        }
        if isLocalOnlyHostname(host) {
            return .lan
        }
        if let explicitPublicURL,
           URLNormalizer.canonicalOrigin(explicitPublicURL) == URLNormalizer.canonicalOrigin(url) {
            return .publicNetwork
        }
        return .publicNetwork
    }

    private static let localOnlySuffixes = [
        ".local",
        ".lan",
        ".home",
        ".home.arpa",
        ".internal",
        ".localdomain",
    ]

    private static func isLocalOnlyHostname(_ host: String) -> Bool {
        if localOnlySuffixes.contains(where: { host.hasSuffix($0) }) {
            return true
        }
        return !host.contains(".") && !host.contains(":")
    }

    private static func ipv4Octets(_ host: String) -> (UInt8, UInt8, UInt8, UInt8)? {
        let parts = host.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4,
              let a = UInt8(parts[0]),
              let b = UInt8(parts[1]),
              let c = UInt8(parts[2]),
              let d = UInt8(parts[3]) else {
            return nil
        }
        return (a, b, c, d)
    }

    private static func isLoopbackIPv4(_ host: String) -> Bool {
        guard let (a, _, _, _) = ipv4Octets(host) else { return false }
        return a == 127
    }

    /// RFC 6598 `100.64.0.0/10` address space used by private overlay mesh networks (such as tailcat).
    private static func isCarrierGradeNATIPv4(_ host: String) -> Bool {
        guard let (a, b, _, _) = ipv4Octets(host) else { return false }
        return a == 100 && (64...127).contains(b)
    }

    private static func isPrivateOrLinkLocalIPv4(_ host: String) -> Bool {
        guard let (a, b, _, _) = ipv4Octets(host) else { return false }
        if a == 10 { return true }
        if a == 172 && (16...31).contains(b) { return true }
        if a == 192 && b == 168 { return true }
        if a == 169 && b == 254 { return true }
        return false
    }
}

public enum URLNormalizer {
    /// Normalizes a user-typed or discovered Hub URL into its canonical `http(s)://host[:port]` origin.
    public static func normalize(_ raw: String) -> URL? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        guard !trimmed.isEmpty else { return nil }
        if trimmed.range(of: "^[a-zA-Z][a-zA-Z0-9+.-]*://", options: .regularExpression) != nil,
           trimmed.range(of: "^https?://", options: [.regularExpression, .caseInsensitive]) == nil {
            return nil
        }
        let withScheme: String
        if trimmed.range(of: "^https?://", options: [.regularExpression, .caseInsensitive]) != nil {
            withScheme = trimmed
        } else {
            withScheme = "http://\(trimmed)"
        }
        guard let components = URLComponents(string: withScheme),
              components.user == nil,
              components.password == nil,
              let scheme = components.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = components.host, !host.isEmpty else {
            return nil
        }
        var clean = URLComponents()
        clean.scheme = scheme
        clean.host = host.lowercased()
        if let port = components.port {
            let isDefaultPort = (scheme == "http" && port == 80) || (scheme == "https" && port == 443)
            if !isDefaultPort {
                clean.port = port
            }
        }
        return clean.url
    }

    public static func canonicalOrigin(_ url: URL) -> String {
        normalize(url.absoluteString)?.absoluteString ?? url.absoluteString
    }
}

public struct HubProfile: Identifiable, Codable, Sendable, Equatable {
    public static let localDefaultID = "local-default"

    public var id: String
    public var name: String
    public var publicURL: URL?
    public var lanURLs: [URL]
    public var isLocalDefault: Bool
    public var clientID: String?
    public var userName: String?
    public var lastKnownVersion: String?
    public var lastConnectedAt: Date?

    public init(
        id: String = UUID().uuidString,
        name: String,
        publicURL: URL? = nil,
        lanURLs: [URL] = [],
        isLocalDefault: Bool = false,
        clientID: String? = nil,
        userName: String? = nil,
        lastKnownVersion: String? = nil,
        lastConnectedAt: Date? = nil
    ) {
        self.id = id
        self.name = name
        self.publicURL = publicURL
        self.lanURLs = lanURLs
        self.isLocalDefault = isLocalDefault
        self.clientID = clientID
        self.userName = userName
        self.lastKnownVersion = lastKnownVersion
        self.lastConnectedAt = lastConnectedAt
    }

    /// Ordered candidate endpoints:
    /// - When `publicURL` is configured, connect via `publicURL` first by default, then fall back to `lanURLs`.
    /// - When `publicURL` is absent, connect via `lanURLs`.
    public var orderedEndpoints: [URL] {
        var result: [URL] = []
        var seen = Set<String>()
        if let pub = publicURL {
            let key = URLNormalizer.canonicalOrigin(pub)
            seen.insert(key)
            result.append(pub)
        }
        for lan in lanURLs {
            let key = URLNormalizer.canonicalOrigin(lan)
            if seen.insert(key).inserted {
                result.append(lan)
            }
        }
        return result
    }

    public var primaryDisplayAddress: String {
        if let pub = publicURL {
            return URLNormalizer.canonicalOrigin(pub)
        }
        if let firstLan = lanURLs.first {
            return URLNormalizer.canonicalOrigin(firstLan)
        }
        return "http://127.0.0.1:8440"
    }

    public static func makeLocalDefault(port: Int = 8440) -> HubProfile {
        let loopback = URL(string: "http://127.0.0.1:\(port)")!
        return HubProfile(
            id: localDefaultID,
            name: L10n.tr("本机 Hub", "Local Hub"),
            publicURL: nil,
            lanURLs: [loopback],
            isLocalDefault: true
        )
    }
}

/// A Hub entry returned by Native Bridge v2 `hubs.list`, matching `NativeHub` in `web/src/hub.ts`.
public struct NativeHubBridgeItem: Codable, Sendable, Equatable {
    public var id: String
    public var name: String
    public var address: String
    public var current: Bool
    public var signedIn: Bool
    public var route: String?
    public var version: String?
    public var removable: Bool

    public init(
        id: String,
        name: String,
        address: String,
        current: Bool,
        signedIn: Bool,
        route: String? = nil,
        version: String? = nil,
        removable: Bool
    ) {
        self.id = id
        self.name = name
        self.address = address
        self.current = current
        self.signedIn = signedIn
        self.route = route
        self.version = version
        self.removable = removable
    }

    public func asBridgeDictionary() -> [String: Any] {
        var dict: [String: Any] = [
            "id": id,
            "name": name,
            "address": address,
            "current": current,
            "signedIn": signedIn,
            "removable": removable,
        ]
        if let route, !route.isEmpty {
            dict["route"] = route
        }
        if let version, !version.isEmpty {
            dict["version"] = version
        }
        return dict
    }
}

public enum HubConnectionPhase: String, Sendable, Equatable {
    case connecting
    case connected
    case authRequired
    case offline
}

public struct HubRuntimeSummary: Sendable, Equatable {
    public var phase: HubConnectionPhase
    public var activeEndpoint: URL?
    public var route: HubRouteKind
    public var hubVersion: String?
    public var webVersion: String?
    public var workspaceName: String?
    public var onlineDevices: Int
    public var totalDevices: Int
    public var runningSessions: Int
    public var openDecisions: Int
    public var steveThinking: Bool
    public var updateAvailable: String?
    public var errorMessage: String?
    public var updatedAt: Date

    public init(
        phase: HubConnectionPhase = .connecting,
        activeEndpoint: URL? = nil,
        route: HubRouteKind = .unknown,
        hubVersion: String? = nil,
        webVersion: String? = nil,
        workspaceName: String? = nil,
        onlineDevices: Int = 0,
        totalDevices: Int = 0,
        runningSessions: Int = 0,
        openDecisions: Int = 0,
        steveThinking: Bool = false,
        updateAvailable: String? = nil,
        errorMessage: String? = nil,
        updatedAt: Date = Date()
    ) {
        self.phase = phase
        self.activeEndpoint = activeEndpoint
        self.route = route
        self.hubVersion = hubVersion
        self.webVersion = webVersion
        self.workspaceName = workspaceName
        self.onlineDevices = onlineDevices
        self.totalDevices = totalDevices
        self.runningSessions = runningSessions
        self.openDecisions = openDecisions
        self.steveThinking = steveThinking
        self.updateAvailable = updateAvailable
        self.errorMessage = errorMessage
        self.updatedAt = updatedAt
    }
}
