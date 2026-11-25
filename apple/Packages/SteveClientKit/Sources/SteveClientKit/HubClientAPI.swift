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

    /// Exchanges a scanned or pasted `stp_...` pairing code for a dedicated client credential at `POST /auth/pair`.
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

    /// Best-effort revocation of a client credential on the Hub (`POST /auth/client/logout`).
    public func signOutClient(endpoint: URL, credential: String) async {
        guard !credential.isEmpty else { return }
        var req = URLRequest(url: endpoint.appendingPathComponent("auth/client/logout"))
        req.httpMethod = "POST"
        req.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        req.timeoutInterval = 4
        _ = try? await session.data(for: req)
    }

    /// Queries `/api/summary` to populate menu bar & runtime summary metrics.
    public func fetchStateSummary(
        endpoint: URL,
        route: HubRouteKind,
        helloVersion: String,
        credential: String?
    ) async throws -> HubRuntimeSummary {
        var req = URLRequest(url: endpoint.appendingPathComponent("api/summary"))
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

        struct RawSummary: Decodable {
            struct UpdateInfo: Decodable { let version: String? }

            let version: String?
            let web_version: String?
            let update: UpdateInfo?
            let workspace_name: String?
            let devices_online: Int?
            let devices_total: Int?
            let sessions_running: Int?
            let decisions_open: Int?
            let thinking: Bool?
        }

        let decoded = try JSONDecoder().decode(RawSummary.self, from: data)
        return HubRuntimeSummary(
            phase: .connected,
            activeEndpoint: endpoint,
            route: route,
            hubVersion: decoded.version ?? helloVersion,
            webVersion: decoded.web_version,
            workspaceName: decoded.workspace_name,
            onlineDevices: decoded.devices_online ?? 0,
            totalDevices: decoded.devices_total ?? 0,
            runningSessions: decoded.sessions_running ?? 0,
            openDecisions: decoded.decisions_open ?? 0,
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
