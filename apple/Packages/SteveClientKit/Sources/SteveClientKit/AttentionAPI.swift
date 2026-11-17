import Foundation

public protocol AttentionAPI: Sendable {
    func fetch(scope: AttentionScope, endpoint: URL, kind: AttentionKind?, cursor: String?) async throws -> AttentionPage
}

/// Credentials are obtained for each request from native storage, never from Web content.
public struct HubAttentionAPI: AttentionAPI {
    public typealias CredentialSource = @Sendable (AttentionScope) async throws -> String?
    private let credential: CredentialSource
    private let session: URLSession
    private static let safeSession: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpShouldSetCookies = false
        configuration.urlCache = nil
        return URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)
    }()
    private final class NoRedirects: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
        func urlSession(_ session: URLSession, task: URLSessionTask,
                        willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                        completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
    }
    /// A custom session must also refuse redirects, so an endpoint cannot redirect native credentials elsewhere.
    public init(session: URLSession? = nil, credential: @escaping CredentialSource) {
        self.session = session ?? Self.safeSession; self.credential = credential
    }
    /// Bootstrap identity from the authenticated Hub, never from a Web context report or a profile display name.
    /// Injected test sessions must refuse redirects just like the default session.
    public static func authenticatedUserID(endpoint: URL, credential: String, session: URLSession? = nil) async throws -> String {
        guard ["https", "http"].contains(endpoint.scheme?.lowercased() ?? ""), endpoint.host != nil,
              endpoint.user == nil, endpoint.password == nil, endpoint.query == nil, endpoint.fragment == nil,
              !credential.isEmpty else { throw AttentionError.authRequired }
        var request = URLRequest(url: endpoint.appendingPathComponent("api/me"), cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 20)
        request.httpShouldHandleCookies = false
        request.setValue("Bearer \(credential)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await (session ?? safeSession).data(for: request)
        try Task.checkCancellation()
        guard let http = response as? HTTPURLResponse else { throw AttentionError.invalidResponse }
        if http.statusCode == 401 || http.statusCode == 403 { throw AttentionError.authRequired }
        guard http.statusCode == 200 else { throw AttentionError.transport(http.statusCode) }
        struct Identity: Decodable { struct User: Decodable { let id: String }; let user: User }
        guard let identity = try? JSONDecoder().decode(Identity.self, from: data), AttentionTarget.validID(identity.user.id) else {
            throw AttentionError.invalidResponse
        }
        return identity.user.id
    }

    public func fetch(scope: AttentionScope, endpoint: URL, kind: AttentionKind? = nil,
                      cursor: String? = nil) async throws -> AttentionPage {
        guard ["https", "http"].contains(endpoint.scheme?.lowercased() ?? ""), endpoint.host != nil,
              endpoint.user == nil, endpoint.password == nil, endpoint.query == nil, endpoint.fragment == nil,
              !scope.userID.isEmpty, !scope.workspaceID.isEmpty else { throw AttentionError.invalidResponse }
        guard let token = try await credential(scope), !token.isEmpty else { throw AttentionError.authRequired }
        try Task.checkCancellation()
        var components = URLComponents(url: endpoint.appendingPathComponent("api/me/attention"), resolvingAgainstBaseURL: false)!
        var query = [URLQueryItem(name: "limit", value: "100")]
        if let kind { query.append(.init(name: "kind", value: kind.rawValue)) }
        if let cursor { query.append(.init(name: "cursor", value: cursor)) }
        components.queryItems = query
        guard let url = components.url else { throw AttentionError.invalidResponse }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 20)
        request.httpShouldHandleCookies = false
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue(scope.workspaceID, forHTTPHeaderField: "x-steve-ws")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await session.data(for: request)
        try Task.checkCancellation()
        guard let http = response as? HTTPURLResponse else { throw AttentionError.invalidResponse }
        switch http.statusCode {
        case 200: break
        case 401, 403: throw AttentionError.authRequired
        case 404, 501: throw AttentionError.unsupported
        case 409: throw AttentionError.snapshotChanged
        default: throw AttentionError.transport(http.statusCode)
        }
        return try Self.decode(data, scope: scope)
    }

    /// Unknown optional fields/categories are ignored; v, scope, known category units and targets are strict.
    public static func decode(_ data: Data, scope: AttentionScope) throws -> AttentionPage {
        struct Raw: Decodable {
            struct Category: Decodable {
                struct Entry: Decodable {
                    struct Target: Decodable { let route: String; let entity_id: String? }
                    let id: String; let event_id: String; let created: Int64; let order: Int64?
                    let target: Target; let source_id: String?; let group_key: String?; let notify: Bool?
                }
                let kind: String; let count: Int64; let unit: String; let available: Bool
                let revision: String; let unchanged: Bool?; let entries: [Entry]?
                let next_cursor: String?; let snapshot: String?
            }
            struct CategoryEnvelope: Decodable {
                let category: Category?
                private enum CodingKeys: String, CodingKey { case kind }
                init(from decoder: Decoder) throws {
                    let fields = try decoder.container(keyedBy: CodingKeys.self)
                    let kind = try fields.decode(String.self, forKey: .kind)
                    category = AttentionKind(rawValue: kind) == nil ? nil : try Category(from: decoder)
                }
            }
            let v: Int; let epoch: String; let user_id: String; let workspace_id: String
            let history_policy: String; let categories: [CategoryEnvelope]
        }
        do {
            let raw = try JSONDecoder().decode(Raw.self, from: data)
            guard raw.v == 1, raw.history_policy == "since_attention_v1", !raw.epoch.isEmpty,
                  raw.user_id == scope.userID, raw.workspace_id == scope.workspaceID else {
                throw AttentionError.invalidResponse
            }
            var seen = Set<AttentionKind>()
            let categories = try raw.categories.compactMap(\.category).compactMap { c -> AttentionCategory? in
                guard let kind = AttentionKind(rawValue: c.kind) else { return nil }
                guard seen.insert(kind).inserted, c.count >= 0, c.unit == kind.unit.rawValue,
                      !c.revision.isEmpty, c.snapshot != "", c.next_cursor != "", c.unchanged != true,
                      !c.available || (c.entries != nil && (c.next_cursor != nil || c.snapshot != nil)) else {
                    throw AttentionError.invalidResponse
                }
                let entries = try (c.entries ?? []).map { e -> AttentionEntry in
                    guard validMetadataID(e.id), validMetadataID(e.event_id),
                          e.source_id.map(validMetadataID) != false, e.group_key.map(validMetadataID) != false else {
                        throw AttentionError.invalidResponse
                    }
                    return AttentionEntry(id: e.id, eventID: e.event_id, created: e.created, order: e.order,
                                          target: try .normalize(kind: kind, route: e.target.route, entityID: e.target.entity_id),
                                          sourceID: e.source_id, groupKey: e.group_key, notify: e.notify)
                }
                return AttentionCategory(kind: kind, count: c.count, available: c.available, revision: c.revision,
                                         entries: entries, nextCursor: c.next_cursor, snapshot: c.snapshot)
            }
            return AttentionPage(epoch: raw.epoch, userID: raw.user_id, workspaceID: raw.workspace_id, categories: categories)
        } catch { throw AttentionError.invalidResponse }
    }
    private static func validMetadataID(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 512 &&
            !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    }
}

public struct AttentionSnapshot: Sendable {
    public let epoch: String
    public let categories: [AttentionKind: AttentionCategory]
    public init(epoch: String, categories: [AttentionKind: AttentionCategory]) {
        self.epoch = epoch; self.categories = categories
    }
}

/// No partial publication. A cursor is only a snapshot page cursor, never an event checkpoint.
public enum AttentionSnapshotLoader {
    public static func load(api: any AttentionAPI, scope: AttentionScope, endpoint: URL) async throws -> AttentionSnapshot {
        // One immediate restart on 409/revision race; sustained churn backs off in the monitor.
        for attempt in 0...1 {
            do { return try await traverse(api: api, scope: scope, endpoint: endpoint) }
            catch AttentionError.snapshotChanged where attempt == 0 { continue }
        }
        throw AttentionError.snapshotChanged
    }
    private static func traverse(api: any AttentionAPI, scope: AttentionScope, endpoint: URL) async throws -> AttentionSnapshot {
        let first = try await api.fetch(scope: scope, endpoint: endpoint, kind: nil, cursor: nil)
        try validate(first, scope: scope)
        guard Set(first.categories.map(\.kind)).count == first.categories.count else { throw AttentionError.invalidResponse }
        var complete: [AttentionKind: AttentionCategory] = [:]
        for initial in first.categories {
            guard initial.count >= 0, initial.unit == initial.kind.unit else { throw AttentionError.invalidResponse }
            if !initial.available {
                complete[initial.kind] = initial; continue
            }
            var entries = initial.entries
            var next = initial.nextCursor
            var completionSnapshot = initial.snapshot
            var cursors = Set<String>()
            while let cursor = next {
                try Task.checkCancellation()
                guard !cursor.isEmpty, cursors.insert(cursor).inserted else { throw AttentionError.invalidResponse }
                let page = try await api.fetch(scope: scope, endpoint: endpoint, kind: initial.kind, cursor: cursor)
                try validate(page, scope: scope)
                guard page.epoch == first.epoch, page.categories.count == 1,
                      let c = page.categories.first, c.kind == initial.kind, c.available,
                      c.revision == initial.revision,
                      c.count == initial.count, c.unit == initial.unit else { throw AttentionError.snapshotChanged }
                entries.append(contentsOf: c.entries)
                next = c.nextCursor
                completionSnapshot = c.snapshot
            }
            // Only the token on the last page attests that this whole category was traversed.
            // We deliberately do full fetches (no if_snapshot) so no truncated cache can masquerade as unchanged.
            guard let completionSnapshot, !completionSnapshot.isEmpty,
                  Int64(entries.count) == initial.count,
                  Set(entries.map(\.id)).count == entries.count,
                  Set(entries.map(\.eventID)).count == entries.count else { throw AttentionError.invalidResponse }
            complete[initial.kind] = AttentionCategory(kind: initial.kind, count: initial.count, revision: initial.revision,
                                                       entries: entries, snapshot: completionSnapshot)
        }
        return AttentionSnapshot(epoch: first.epoch, categories: complete)
    }
    private static func validate(_ page: AttentionPage, scope: AttentionScope) throws {
        guard !page.epoch.isEmpty, page.userID == scope.userID, page.workspaceID == scope.workspaceID else {
            throw AttentionError.invalidResponse
        }
    }
}
