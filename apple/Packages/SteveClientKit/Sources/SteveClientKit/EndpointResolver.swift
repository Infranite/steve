import Foundation

public struct HubHelloResponse: Codable, Sendable, Equatable {
    public struct LoginModes: Codable, Sendable, Equatable {
        public var token: Bool
        public var oidc: Bool

        public init(token: Bool, oidc: Bool) {
            self.token = token
            self.oidc = oidc
        }
    }

    public var name: String
    public var version: String
    public var api: Int
    public var login: LoginModes

    public init(name: String, version: String, api: Int, login: LoginModes) {
        self.name = name
        self.version = version
        self.api = api
        self.login = login
    }
}

public struct ResolvedHubEndpoint: Sendable, Equatable {
    public var url: URL
    public var route: HubRouteKind
    public var hello: HubHelloResponse
    public var usedFallbackFromPublic: Bool

    public init(url: URL, route: HubRouteKind, hello: HubHelloResponse, usedFallbackFromPublic: Bool) {
        self.url = url
        self.route = route
        self.hello = hello
        self.usedFallbackFromPublic = usedFallbackFromPublic
    }
}

public enum EndpointResolutionError: Error, LocalizedError, Sendable, Equatable {
    case noCandidates
    case unreachable(tried: [URL])
    case incompatibleAPI(version: Int)

    public var errorDescription: String? {
        switch self {
        case .noCandidates:
            return L10n.tr("未配置可用的 Hub 地址", "No Hub address configured")
        case .unreachable(let tried):
            let joined = tried.map(URLNormalizer.canonicalOrigin).joined(separator: ", ")
            return L10n.tr(
                "无法连接到 Hub（已尝试：\(joined)）",
                "Could not reach Hub (tried: \(joined))"
            )
        case .incompatibleAPI(let version):
            return L10n.tr(
                "Hub 接口版本（v\(version)）与当前客户端不兼容，请升级客户端或 Hub",
                "Hub API version (v\(version)) is incompatible with this client"
            )
        }
    }
}

public struct EndpointResolver: Sendable {
    public static let supportedAPIVersion = 1
    public typealias Probe = @Sendable (URL) async throws -> HubHelloResponse

    private let probe: Probe

    public init(probe: @escaping Probe = EndpointResolver.defaultProbe) {
        self.probe = probe
    }

    /// Resolves a `HubProfile` following the routing contract:
    /// - When `profile.publicURL` is configured, connect via `publicURL` first by default; fall back to `lanURLs` if unreachable.
    /// - When `profile.publicURL` is absent, connect via `lanURLs` (probing multiple LAN candidates concurrently, first responder wins).
    public func resolve(profile: HubProfile) async throws -> ResolvedHubEndpoint {
        try await resolve(publicURL: profile.publicURL, lanURLs: profile.lanURLs)
    }

    /// Resolves a `PairingPayload` during mobile QR pairing using the same public-first / LAN-fallback rule.
    public func resolve(pairing: PairingPayload) async throws -> ResolvedHubEndpoint {
        try await resolve(publicURL: pairing.publicURL, lanURLs: pairing.lanURLs)
    }

    public func resolve(publicURL: URL?, lanURLs: [URL]) async throws -> ResolvedHubEndpoint {
        var lanCandidates: [URL] = []
        var seen = Set<String>()
        if let pub = publicURL {
            seen.insert(URLNormalizer.canonicalOrigin(pub))
        }
        for lan in lanURLs {
            if seen.insert(URLNormalizer.canonicalOrigin(lan)).inserted {
                lanCandidates.append(lan)
            }
        }

        let allCandidates = (publicURL.map { [$0] } ?? []) + lanCandidates
        guard !allCandidates.isEmpty else {
            throw EndpointResolutionError.noCandidates
        }

        var incompatibleVersion: Int?

        // 1. Public URL takes strict priority when configured.
        if let pub = publicURL {
            if let hello = try? await probe(pub), hello.name == "steve" {
                if hello.api == Self.supportedAPIVersion {
                    return ResolvedHubEndpoint(
                        url: pub,
                        route: HubRouteKind.classify(url: pub, explicitPublicURL: pub),
                        hello: hello,
                        usedFallbackFromPublic: false
                    )
                } else {
                    incompatibleVersion = hello.api
                }
            }
        }

        // 2. Probe LAN candidates concurrently; first compatible responder wins.
        if !lanCandidates.isEmpty {
            let probeFn = self.probe
            let (winner, badAPI) = await Self.raceCandidates(lanCandidates, probe: probeFn)
            if let (url, hello) = winner {
                return ResolvedHubEndpoint(
                    url: url,
                    route: HubRouteKind.classify(url: url, explicitPublicURL: publicURL),
                    hello: hello,
                    usedFallbackFromPublic: publicURL != nil
                )
            }
            if let badAPI {
                incompatibleVersion = badAPI
            }
        }

        if let incompatibleVersion {
            throw EndpointResolutionError.incompatibleAPI(version: incompatibleVersion)
        }
        throw EndpointResolutionError.unreachable(tried: allCandidates)
    }

    private enum ProbeOutcome: Sendable {
        case ok(URL, HubHelloResponse)
        case incompatible(Int)
        case failed
    }

    private static func raceCandidates(
        _ candidates: [URL],
        probe: @escaping Probe
    ) async -> ((URL, HubHelloResponse)?, Int?) {
        if candidates.count == 1 {
            let only = candidates[0]
            guard let hello = try? await probe(only), hello.name == "steve" else {
                return (nil, nil)
            }
            if hello.api == supportedAPIVersion {
                return ((only, hello), nil)
            }
            return (nil, hello.api)
        }

        return await withTaskGroup(of: ProbeOutcome.self) { group in
            for candidate in candidates {
                group.addTask {
                    guard let hello = try? await probe(candidate), hello.name == "steve" else {
                        return .failed
                    }
                    if hello.api == supportedAPIVersion {
                        return .ok(candidate, hello)
                    }
                    return .incompatible(hello.api)
                }
            }
            var seenIncompatible: Int?
            while let next = await group.next() {
                switch next {
                case .ok(let url, let hello):
                    group.cancelAll()
                    return ((url, hello), nil)
                case .incompatible(let ver):
                    seenIncompatible = ver
                case .failed:
                    continue
                }
            }
            return (nil, seenIncompatible)
        }
    }

    public static let defaultProbe: Probe = { baseURL in
        let target = baseURL.appendingPathComponent("auth/hello")
        var request = URLRequest(url: target)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 4.5
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, (200...299).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        let decoded = try JSONDecoder().decode(HubHelloResponse.self, from: data)
        guard decoded.name == "steve" else {
            throw URLError(.cannotParseResponse)
        }
        return decoded
    }
}
