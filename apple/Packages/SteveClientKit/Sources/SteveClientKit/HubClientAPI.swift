import Foundation

public struct ClientDescriptor: Codable, Sendable, Equatable {
    public var kind: String
    public var name: String
    public var platform: String
    public var version: String

    public init(kind: String, name: String, platform: String, version: String) {
        self.kind = kind
        self.name = name
        self.platform = platform
        self.version = version
    }
}

public struct PairSignInResponse: Codable, Sendable, Equatable {
    public struct UserInfo: Codable, Sendable, Equatable {
        public var id: String
        public var name: String

        public init(id: String, name: String) {
            self.id = id
            self.name = name
        }
    }

    public struct HubInfo: Codable, Sendable, Equatable {
        public var version: String
        public var api: Int?

        public init(version: String, api: Int? = nil) {
            self.version = version
            self.api = api
        }
    }

    public var client_id: String
    public var credential: String?
    public var user: UserInfo
    public var hub: HubInfo

    public init(client_id: String, credential: String?, user: UserInfo, hub: HubInfo) {
        self.client_id = client_id
        self.credential = credential
        self.user = user
        self.hub = hub
    }
}

public struct CreatedPairing: Codable, Sendable, Equatable {
    public var id: String
    public var code: String
    public var name: String
    public var expires: Int64
    public var public_url: String?
    public var lan_urls: [String]
    public var loopback_only: Bool
    public var uri: String
}

public struct PairingClaimStatus: Codable, Sendable, Equatable {
    public struct ClaimedClient: Codable, Sendable, Equatable {
        public var id: String
        public var name: String
        public var kind: String
        public var platform: String
    }

    public var id: String
    public var status: String
    public var expires: Int64
    public var client: ClaimedClient?
}

public enum HubClientAPIError: Error, LocalizedError, Sendable, Equatable {
    case invalidResponse
    case missingCredential
    case unauthorized
    case serverError(status: Int, message: String)

    public var errorDescription: String? {
        switch self {
        case .invalidResponse:
            return L10n.tr("Hub 返回了无法识别的响应", "Unrecognized response from Hub")
        case .missingCredential:
            return L10n.tr("Hub 未返回有效的访问凭证", "Hub did not return a client credential")
        case .unauthorized:
            return L10n.tr("尚未登录或访问凭证已失效", "Not signed in or credential expired")
        case .serverError(_, let message):
            return message
        }
    }
}

public struct HubClientAPI: Sendable {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    /// Exchanges a scanned `stp_...` QR pairing code for a dedicated client credential at `POST /auth/pair`.
    public func claimPairing(
        endpoint: URL,
        code: String,
        client: ClientDescriptor
    ) async throws -> PairSignInResponse {
        var req = URLRequest(url: endpoint.appendingPathComponent("auth/pair"))
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(L10n.acceptLanguageHeader, forHTTPHeaderField: "Accept-Language")
        req.timeoutInterval = 10

        struct Body: Encodable {
            let code: String
            let cookie: Bool
            let client: ClientDescriptor
        }
        req.httpBody = try JSONEncoder().encode(Body(code: code, cookie: false, client: client))

        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw HubClientAPIError.invalidResponse
        }
        if !(200...299).contains(http.statusCode) {
            let msg = Self.extractErrorMessage(from: data) ?? "HTTP \(http.statusCode)"
            throw HubClientAPIError.serverError(status: http.statusCode, message: msg)
        }
        let decoded = try JSONDecoder().decode(PairSignInResponse.self, from: data)
        guard let cred = decoded.credential, !cred.isEmpty else {
            throw HubClientAPIError.missingCredential
        }
        return decoded
    }

    /// Signs in with a user name and access token at `POST /auth/token` (`cookie: false`) to obtain a Bearer credential.
    public func signInWithToken(
        endpoint: URL,
        user: String,
        token: String,
        client: ClientDescriptor
    ) async throws -> PairSignInResponse {
        var req = URLRequest(url: endpoint.appendingPathComponent("auth/token"))
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(L10n.acceptLanguageHeader, forHTTPHeaderField: "Accept-Language")
        req.timeoutInterval = 10

        struct Body: Encodable {
            let user: String
            let token: String
            let cookie: Bool
            let client: ClientDescriptor
        }
        req.httpBody = try JSONEncoder().encode(Body(user: user, token: token, cookie: false, client: client))

        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw HubClientAPIError.invalidResponse
        }
        if !(200...299).contains(http.statusCode) {
            let msg = Self.extractErrorMessage(from: data) ?? "HTTP \(http.statusCode)"
            throw HubClientAPIError.serverError(status: http.statusCode, message: msg)
        }
        let decoded = try JSONDecoder().decode(PairSignInResponse.self, from: data)
        guard let cred = decoded.credential, !cred.isEmpty else {
            throw HubClientAPIError.missingCredential
        }
        return decoded
    }

    /// Best-effort revocation of a client credential on the Hub (`POST /auth/client/logout`).
    public func signOutClient(endpoint: URL, credential: String) async {
        guard !credential.isEmpty else { return }
        var req = URLRequest(url: endpoint.appendingPathComponent("auth/client/logout"))
        req.httpMethod = "POST"
        req.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        req.timeoutInterval = 4
        _ = try? await session.data(for: req)
    }

    /// Creates a QR pairing code on the active Hub (`POST /api/access/pairings`).
    public func createPairing(endpoint: URL, credential: String?) async throws -> CreatedPairing {
        var req = URLRequest(url: endpoint.appendingPathComponent("api/access/pairings"))
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.setValue(L10n.acceptLanguageHeader, forHTTPHeaderField: "Accept-Language")
        if let credential, !credential.isEmpty {
            req.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        }
        req.httpBody = Data("{}".utf8)
        req.timeoutInterval = 8

        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw HubClientAPIError.invalidResponse
        }
        if http.statusCode == 401 {
            throw HubClientAPIError.unauthorized
        }
        if !(200...299).contains(http.statusCode) {
            let msg = Self.extractErrorMessage(from: data) ?? "HTTP \(http.statusCode)"
            throw HubClientAPIError.serverError(status: http.statusCode, message: msg)
        }
        return try JSONDecoder().decode(CreatedPairing.self, from: data)
    }

    /// Polls QR pairing claim status (`GET /api/access/pairings/{id}`).
    public func pairingStatus(endpoint: URL, id: String, credential: String?) async throws -> PairingClaimStatus {
        let safeID = id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? id
        var req = URLRequest(url: endpoint.appendingPathComponent("api/access/pairings/\(safeID)"))
        req.httpMethod = "GET"
        req.setValue(L10n.acceptLanguageHeader, forHTTPHeaderField: "Accept-Language")
        if let credential, !credential.isEmpty {
            req.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        }
        req.timeoutInterval = 8

        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw HubClientAPIError.invalidResponse
        }
        if http.statusCode == 401 {
            throw HubClientAPIError.unauthorized
        }
        if !(200...299).contains(http.statusCode) {
            let msg = Self.extractErrorMessage(from: data) ?? "HTTP \(http.statusCode)"
            throw HubClientAPIError.serverError(status: http.statusCode, message: msg)
        }
        return try JSONDecoder().decode(PairingClaimStatus.self, from: data)
    }

    /// Queries `/api/state` to populate menu bar & runtime summary metrics.
    public func fetchStateSummary(
        endpoint: URL,
        route: HubRouteKind,
        helloVersion: String,
        credential: String?
    ) async throws -> HubRuntimeSummary {
        var req = URLRequest(url: endpoint.appendingPathComponent("api/state"))
        req.httpMethod = "GET"
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.setValue(L10n.acceptLanguageHeader, forHTTPHeaderField: "Accept-Language")
        req.timeoutInterval = 8
        if let credential, !credential.isEmpty {
            req.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        }

        let (data, response) = try await session.data(for: req)
        guard let http = response as? HTTPURLResponse else {
            throw HubClientAPIError.invalidResponse
        }
        if http.statusCode == 401 {
            return HubRuntimeSummary(
                phase: .authRequired,
                activeEndpoint: endpoint,
                route: route,
                hubVersion: helloVersion,
                updatedAt: Date()
            )
        }
        guard (200...299).contains(http.statusCode) else {
            let msg = Self.extractErrorMessage(from: data) ?? "HTTP \(http.statusCode)"
            throw HubClientAPIError.serverError(status: http.statusCode, message: msg)
        }

        struct RawState: Decodable {
            struct Workspace: Decodable { let name: String? }
            struct Device: Decodable { let status: String? }
            struct Session: Decodable { let status: String?; let busy: Bool? }
            struct Decision: Decodable { let status: String? }
            struct UpdateInfo: Decodable { let version: String? }

            let version: String?
            let web_version: String?
            let update: UpdateInfo?
            let workspace: Workspace?
            let devices: [Device]?
            let sessions: [Session]?
            let decisions: [Decision]?
            let thinking: Bool?
        }

        let decoded = try JSONDecoder().decode(RawState.self, from: data)
        let devices = decoded.devices ?? []
        let onlineDevices = devices.filter {
            let s = ($0.status ?? "").lowercased()
            return s != "离线" && s != "offline"
        }.count
        let runningSessions = (decoded.sessions ?? []).filter {
            $0.busy == true || $0.status == "running" || $0.status == "starting"
        }.count
        let openDecisions = (decoded.decisions ?? []).filter {
            ($0.status ?? "open") == "open"
        }.count

        return HubRuntimeSummary(
            phase: .connected,
            activeEndpoint: endpoint,
            route: route,
            hubVersion: decoded.version ?? helloVersion,
            webVersion: decoded.web_version,
            workspaceName: decoded.workspace?.name,
            onlineDevices: onlineDevices,
            totalDevices: devices.count,
            runningSessions: runningSessions,
            openDecisions: openDecisions,
            steveThinking: decoded.thinking ?? false,
            updateAvailable: decoded.update?.version,
            errorMessage: nil,
            updatedAt: Date()
        )
    }

    private static func extractErrorMessage(from data: Data) -> String? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let err = obj["error"] as? String,
              !err.isEmpty else {
            return nil
        }
        return err
    }
}
