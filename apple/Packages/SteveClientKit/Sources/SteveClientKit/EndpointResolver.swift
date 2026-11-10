import Foundation

public struct HubHelloResponse: Codable, Sendable, Equatable {
    public struct LoginModes: Codable, Sendable, Equatable {
        public var token: Bool
        public var oidc: Bool
    }

    public var name: String
    public var version: String
    public var api: Int
    public var login: LoginModes
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

public enum EndpointResolutionError: Error, LocalizedError, Sendable {
    case noCandidates
    case unreachable(tried: [URL])

    public var errorDescription: String? {
        switch self {
        case .noCandidates:
            return "未配置可用的 Hub 地址"
        case .unreachable(let tried):
            let joined = tried.map(URLNormalizer.canonicalOrigin).joined(separator: ", ")
            return "无法连接到 Hub（已尝试：\(joined)）"
        }
    }
}

public struct EndpointResolver: Sendable {
    public typealias Probe = @Sendable (URL) async throws -> HubHelloResponse

    private let probe: Probe

    public init(probe: @escaping Probe = EndpointResolver.defaultProbe) {
        self.probe = probe
    }

    /// Resolves a `HubProfile` following the routing contract:
    /// - When `profile.publicURL` is configured, connect via `publicURL` first by default; fall back to `lanURLs` if unreachable.
    /// - When `profile.publicURL` is absent, connect via `lanURLs`.
    public func resolve(profile: HubProfile) async throws -> ResolvedHubEndpoint {
        try await resolve(publicURL: profile.publicURL, lanURLs: profile.lanURLs)
    }

    /// Resolves a `PairingPayload` during mobile QR pairing using the same public-first / LAN-fallback rule.
    public func resolve(pairing: PairingPayload) async throws -> ResolvedHubEndpoint {
        try await resolve(publicURL: pairing.publicURL, lanURLs: pairing.lanURLs)
    }

    public func resolve(publicURL: URL?, lanURLs: [URL]) async throws -> ResolvedHubEndpoint {
        var candidates: [URL] = []
        var seen = Set<String>()
        if let pub = publicURL {
            seen.insert(URLNormalizer.canonicalOrigin(pub))
            candidates.append(pub)
        }
        for lan in lanURLs {
            if seen.insert(URLNormalizer.canonicalOrigin(lan)).inserted {
                candidates.append(lan)
            }
        }
        guard !candidates.isEmpty else {
            throw EndpointResolutionError.noCandidates
        }

        let publicOrigin = publicURL.map(URLNormalizer.canonicalOrigin)
        for candidate in candidates {
            do {
                let hello = try await probe(candidate)
                guard hello.name == "steve" else { continue }
                let route = HubRouteKind.classify(url: candidate, explicitPublicURL: publicURL)
                let usedFallback = (publicOrigin != nil) && (URLNormalizer.canonicalOrigin(candidate) != publicOrigin)
                return ResolvedHubEndpoint(
                    url: candidate,
                    route: route,
                    hello: hello,
                    usedFallbackFromPublic: usedFallback
                )
            } catch {
                continue
            }
        }
        throw EndpointResolutionError.unreachable(tried: candidates)
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
