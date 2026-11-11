import CryptoKit
import Foundation
import Network
import Security

/// Serves the bundled Steve web UI on `http://127.0.0.1:<port>` and proxies `/api/*`, `/auth/*`, and
/// `/healthz` to the currently selected Hub endpoint while enforcing `Host`/`Origin`/per-launch cookie
/// authentication, isolating cookies per Hub, injecting Keychain `Authorization: Bearer` credentials,
/// and streaming `/api/stream` Server-Sent Events without buffering.
public final class HubLoopbackProxy: @unchecked Sendable {
    public static let proxyAuthCookieName = "steve_proxy_key"

    public struct ActiveTarget: Sendable, Equatable {
        public var hubID: String
        public var baseURL: URL?
        public var credential: String?

        public init(hubID: String = "", baseURL: URL? = nil, credential: String? = nil) {
            self.hubID = hubID
            self.baseURL = baseURL
            self.credential = credential
        }
    }

    public enum ProxyAuthEvent: Sendable, Equatable {
        case signedIn(PairSignInResponse)
        case signedOut
    }

    private final class NoRedirectDelegate: NSObject, URLSessionTaskDelegate, Sendable {
        func urlSession(
            _ session: URLSession,
            task: URLSessionTask,
            willPerformHTTPRedirection response: HTTPURLResponse,
            newRequest request: URLRequest,
            completionHandler: @escaping (URLRequest?) -> Void
        ) {
            // Never auto-follow upstream redirects or forward Authorization headers across origins.
            completionHandler(nil)
        }
    }

    public let proxyAuthToken: String
    public var authCookieHeader: String {
        "\(Self.proxyAuthCookieName)=\(proxyAuthToken)"
    }

    private let webRoot: URL?
    private let preferredPortDefaults: UserDefaults?
    private let preferredPortKey: String
    private let queue = DispatchQueue(label: "app.steve.client.loopback-proxy", qos: .userInitiated)
    private let lock = NSLock()
    private var target = ActiveTarget()
    private var authEventHandler: (@Sendable (String, ProxyAuthEvent) -> Void)?
    private var listener: NWListener?
    private var isListening = false
    private var lastBoundPort: UInt16 = 0
    private let redirectDelegate = NoRedirectDelegate()
    private let session: URLSession

    public private(set) var port: UInt16 {
        get {
            lock.lock()
            defer { lock.unlock() }
            return isListening ? lastBoundPort : 0
        }
        set {
            lock.lock()
            lastBoundPort = newValue
            lock.unlock()
        }
    }

    public var localOriginURL: URL? {
        let p = port
        guard p > 0 else { return nil }
        return URL(string: "http://127.0.0.1:\(p)")
    }

    public init(
        webRoot: URL? = nil,
        preferredPortDefaults: UserDefaults? = nil,
        preferredPortKey: String = "steve.apple.preferredProxyPort"
    ) {
        self.webRoot = webRoot
        self.preferredPortDefaults = preferredPortDefaults
        self.preferredPortKey = preferredPortKey
        self.proxyAuthToken = Self.generateRandomHexToken()

        let cfg = URLSessionConfiguration.ephemeral
        cfg.timeoutIntervalForRequest = 60
        cfg.timeoutIntervalForResource = 86_400
        cfg.httpCookieAcceptPolicy = .never
        cfg.httpShouldSetCookies = false
        cfg.httpCookieStorage = nil
        self.session = URLSession(configuration: cfg, delegate: redirectDelegate, delegateQueue: nil)
    }

    public func updateTarget(_ newTarget: ActiveTarget) {
        lock.lock()
        target = newTarget
        lock.unlock()
    }

    public func setAuthEventHandler(_ handler: (@Sendable (String, ProxyAuthEvent) -> Void)?) {
        lock.lock()
        authEventHandler = handler
        lock.unlock()
    }

    public func currentTarget() -> ActiveTarget {
        lock.lock()
        defer { lock.unlock() }
        return target
    }

    @discardableResult
    public func start() async throws -> URL {
        try await ensureRunning()
    }

    @discardableResult
    public func ensureRunning() async throws -> URL {
        if let existing = localOriginURL {
            return existing
        }

        let rememberedPort: UInt16? = {
            lock.lock()
            let mem = lastBoundPort
            lock.unlock()
            if (1024...65535).contains(Int(mem)) {
                return mem
            }
            if let saved = preferredPortDefaults?.integer(forKey: preferredPortKey),
               (1024...65535).contains(saved) {
                return UInt16(saved)
            }
            return nil
        }()

        let bound: (NWListener, UInt16)
        if let candidate = rememberedPort,
           let nwPort = NWEndpoint.Port(rawValue: candidate),
           let reused = try? await bindListener(port: nwPort) {
            bound = reused
        } else {
            bound = try await bindListener(port: .any)
        }

        let (nwListener, boundPort) = bound
        nwListener.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            switch state {
            case .failed, .cancelled:
                self.lock.lock()
                if self.listener === nwListener {
                    self.isListening = false
                    self.listener = nil
                }
                self.lock.unlock()
            default:
                break
            }
        }

        lock.withLock {
            listener?.cancel()
            listener = nwListener
            lastBoundPort = boundPort
            isListening = true
        }

        preferredPortDefaults?.set(Int(boundPort), forKey: preferredPortKey)
        return URL(string: "http://127.0.0.1:\(boundPort)")!
    }

    public func stop() {
        lock.lock()
        let active = listener
        listener = nil
        isListening = false
        lock.unlock()
        active?.cancel()
    }

    private func bindListener(port requestedPort: NWEndpoint.Port) async throws -> (NWListener, UInt16) {
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = (requestedPort == .any)
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: requestedPort)

        let nwListener = try NWListener(using: params)
        do {
            let boundPort: UInt16 = try await withCheckedThrowingContinuation { continuation in
                final class ResumeGate: @unchecked Sendable {
                    let lock = NSLock()
                    var done = false
                    func runOnce(_ block: () -> Void) {
                        lock.lock()
                        defer { lock.unlock() }
                        guard !done else { return }
                        done = true
                        block()
                    }
                }
                let gate = ResumeGate()

                nwListener.newConnectionHandler = { [weak self] conn in
                    self?.accept(connection: conn)
                }
                nwListener.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        if let p = nwListener.port?.rawValue, p > 0 {
                            gate.runOnce { continuation.resume(returning: p) }
                        }
                    case .failed(let err):
                        gate.runOnce { continuation.resume(throwing: err) }
                    case .cancelled:
                        gate.runOnce { continuation.resume(throwing: CancellationError()) }
                    default:
                        break
                    }
                }
                nwListener.start(queue: self.queue)
            }
            return (nwListener, boundPort)
        } catch {
            nwListener.cancel()
            throw error
        }
    }

    private func accept(connection: NWConnection) {
        connection.start(queue: queue)
        Task.detached(priority: .userInitiated) { [weak self] in
            guard let self else {
                connection.cancel()
                return
            }
            await self.handleConnection(connection)
        }
    }

    private var hasLocalWebBundle: Bool {
        guard let root = webRoot else { return false }
        return FileManager.default.fileExists(atPath: root.path)
    }

    private func handleConnection(_ connection: NWConnection) async {
        defer { connection.cancel() }
        do {
            guard let request = try await readHTTPRequest(from: connection) else {
                return
            }
            let proxied = Self.isProxiedPath(request.path)
            let requiresSecret = proxied || !hasLocalWebBundle
            if let rejection = validateCaller(request: request, requiresSecret: requiresSecret) {
                let body = Data("{\"error\":\"\(rejection)\"}".utf8)
                try await sendAll(
                    data: Self.serializeResponseHead(
                        status: 403,
                        reason: "Forbidden",
                        headers: [
                            ("Content-Type", "application/json; charset=utf-8"),
                            ("Content-Length", "\(body.count)"),
                            ("Cache-Control", "no-store"),
                            ("Connection", "close"),
                        ]
                    ) + body,
                    over: connection
                )
                return
            }

            if proxied {
                try await forwardToHub(request: request, over: connection)
            } else {
                try await serveStaticAsset(request: request, over: connection)
            }
        } catch {
            let body = Data("{\"error\":\"proxy_error\"}".utf8)
            try? await sendAll(
                data: Self.serializeResponseHead(
                    status: 502,
                    reason: "Bad Gateway",
                    headers: [
                        ("Content-Type", "application/json; charset=utf-8"),
                        ("Content-Length", "\(body.count)"),
                        ("Connection", "close"),
                    ]
                ) + body,
                over: connection
            )
        }
    }

    private func validateCaller(request: ParsedHTTPRequest, requiresSecret: Bool) -> String? {
        let currentPort = self.port
        guard currentPort > 0 else { return "proxy_stopped" }
        let expectedHost = "127.0.0.1:\(currentPort)"
        let expectedOrigin = "http://\(expectedHost)"

        let hostHeaders = request.headers.filter { $0.0.lowercased() == "host" }
        guard hostHeaders.count == 1,
              hostHeaders[0].1.lowercased() == expectedHost else {
            return "forbidden_host"
        }

        for (name, value) in request.headers where name.lowercased() == "origin" {
            if !value.isEmpty && value.lowercased() != expectedOrigin {
                return "forbidden_origin"
            }
        }

        for (name, value) in request.headers where name.lowercased() == "sec-fetch-site" {
            if value.lowercased() == "cross-site" {
                return "forbidden_fetch_site"
            }
        }

        if requiresSecret {
            let cookies = Self.parseRequestCookies(from: request.headers)
            guard let supplied = cookies[Self.proxyAuthCookieName],
                  Self.constantTimeEquals(supplied, proxyAuthToken) else {
                return "forbidden_proxy_key"
            }
        }
        return nil
    }

    private static func isProxiedPath(_ pathOnly: String) -> Bool {
        pathOnly == "/api"
            || pathOnly.hasPrefix("/api/")
            || pathOnly == "/auth"
            || pathOnly.hasPrefix("/auth/")
            || pathOnly == "/healthz"
    }

    // MARK: - Cookie Isolation & Per-Launch Secret Helpers

    public static func hubCookiePrefix(for hubID: String) -> String {
        guard !hubID.isEmpty else { return "steve_h_default_" }
        let digest = SHA256.hash(data: Data(hubID.utf8))
        let hex12 = digest.prefix(6).map { String(format: "%02x", $0) }.joined()
        return "steve_h_\(hex12)_"
    }

    private static func parseRequestCookies(from headers: [(String, String)]) -> [String: String] {
        var map: [String: String] = [:]
        for (name, value) in headers where name.lowercased() == "cookie" {
            for pair in value.split(separator: ";") {
                let trimmed = pair.trimmingCharacters(in: .whitespaces)
                guard let eq = trimmed.firstIndex(of: "=") else { continue }
                let k = String(trimmed[..<eq]).trimmingCharacters(in: .whitespaces)
                let v = String(trimmed[trimmed.index(after: eq)...]).trimmingCharacters(in: .whitespaces)
                if !k.isEmpty {
                    map[k] = v
                }
            }
        }
        return map
    }

    /// Extracts only cookies belonging to `hubID` (unwrapping their `steve_h_<hash>_` prefix) and never forwards `steve_proxy_key` or another Hub's cookies.
    private static func upstreamCookieHeader(from headers: [(String, String)], hubID: String) -> String? {
        let prefix = hubCookiePrefix(for: hubID)
        var pairs: [String] = []
        for (name, value) in headers where name.lowercased() == "cookie" {
            for pair in value.split(separator: ";") {
                let trimmed = pair.trimmingCharacters(in: .whitespaces)
                guard let eq = trimmed.firstIndex(of: "=") else { continue }
                let k = String(trimmed[..<eq]).trimmingCharacters(in: .whitespaces)
                let v = String(trimmed[trimmed.index(after: eq)...]).trimmingCharacters(in: .whitespaces)
                if k == proxyAuthCookieName { continue }
                guard k.hasPrefix(prefix) else { continue }
                let unwrappedName = String(k.dropFirst(prefix.count))
                if !unwrappedName.isEmpty {
                    pairs.append("\(unwrappedName)=\(v)")
                }
            }
        }
        return pairs.isEmpty ? nil : pairs.joined(separator: "; ")
    }

    /// Splits a CFNetwork-folded `Set-Cookie` header value into individual cookies without breaking `Expires=Wed, 21 Oct ...` dates.
    internal static func splitFoldedSetCookieHeader(_ headerValue: String) -> [String] {
        let chars = Array(headerValue)
        var results: [String] = []
        var segmentStart = 0
        var i = 0

        while i < chars.count {
            if chars[i] == "," {
                var j = i + 1
                while j < chars.count && (chars[j] == " " || chars[j] == "\t") {
                    j += 1
                }
                var k = j
                while k < chars.count && isCookieTokenChar(chars[k]) {
                    k += 1
                }
                if k > j && k < chars.count && chars[k] == "=" {
                    let piece = String(chars[segmentStart..<i]).trimmingCharacters(in: .whitespaces)
                    if !piece.isEmpty {
                        results.append(piece)
                    }
                    segmentStart = j
                    i = k
                    continue
                }
            }
            i += 1
        }
        let tail = String(chars[segmentStart..<chars.count]).trimmingCharacters(in: .whitespaces)
        if !tail.isEmpty {
            results.append(tail)
        }
        return results
    }

    private static func isCookieTokenChar(_ ch: Character) -> Bool {
        guard let scalar = ch.unicodeScalars.first, ch.unicodeScalars.count == 1 else { return false }
        let v = scalar.value
        if (v >= 0x30 && v <= 0x39) || (v >= 0x41 && v <= 0x5A) || (v >= 0x61 && v <= 0x7A) {
            return true
        }
        switch ch {
        case "!", "#", "$", "%", "&", "'", "*", "+", "-", ".", "^", "_", "`", "|", "~":
            return true
        default:
            return false
        }
    }

    /// Namespaces upstream cookies under `steve_h_<hash>_` and strips `Secure`/`Domain=...` attributes for `http://127.0.0.1`.
    internal static func rewriteCookieForLoopback(_ cookieHeader: String, hubID: String) -> String {
        let prefix = hubCookiePrefix(for: hubID)
        let parts = cookieHeader.split(separator: ";", omittingEmptySubsequences: false)
        var kept: [String] = []
        for (index, part) in parts.enumerated() {
            let trimmed = part.trimmingCharacters(in: .whitespaces)
            if index == 0 {
                if let eq = trimmed.firstIndex(of: "=") {
                    let rawName = String(trimmed[..<eq]).trimmingCharacters(in: .whitespaces)
                    let valPart = String(trimmed[eq...])
                    if rawName.hasPrefix(prefix) {
                        kept.append("\(rawName)\(valPart)")
                    } else {
                        kept.append("\(prefix)\(rawName)\(valPart)")
                    }
                } else {
                    kept.append(trimmed)
                }
                continue
            }
            let lower = trimmed.lowercased()
            if lower == "secure" || lower.hasPrefix("domain=") {
                continue
            }
            if lower.hasPrefix("samesite=none") {
                kept.append("SameSite=Lax")
                continue
            }
            kept.append(trimmed)
        }
        return kept.joined(separator: "; ")
    }

    private static func generateRandomHexToken() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        if status != errSecSuccess {
            let fallback = UUID().uuidString.replacingOccurrences(of: "-", with: "")
                + UUID().uuidString.replacingOccurrences(of: "-", with: "")
            return fallback.lowercased()
        }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    private static func constantTimeEquals(_ a: String, _ b: String) -> Bool {
        let ab = Array(a.utf8)
        let bb = Array(b.utf8)
        guard ab.count == bb.count else { return false }
        var diff: UInt8 = 0
        for i in 0..<ab.count {
            diff |= ab[i] ^ bb[i]
        }
        return diff == 0
    }

    // MARK: - Static Web Bundle Serving

    private func serveStaticAsset(request: ParsedHTTPRequest, over connection: NWConnection) async throws {
        // When no local webRoot is bundled, proxy static assets from the active Hub as well.
        guard let root = webRoot, FileManager.default.fileExists(atPath: root.path) else {
            try await forwardToHub(request: request, over: connection)
            return
        }

        let cleanPath = request.path.removingPercentEncoding ?? request.path
        let relative = cleanPath.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let rootStandard = root.standardizedFileURL.path

        var fileURL: URL?
        if !relative.isEmpty {
            let candidate = root.appendingPathComponent(relative).standardizedFileURL
            if candidate.path.hasPrefix(rootStandard + "/") {
                var isDir: ObjCBool = false
                if FileManager.default.fileExists(atPath: candidate.path, isDirectory: &isDir), !isDir.boolValue {
                    fileURL = candidate
                }
            }
        }

        // SPA fallback to index.html for client routes; return 404 for missing `/assets/...` or `/hub.json`
        if fileURL == nil {
            if cleanPath == "/hub.json" || cleanPath.hasPrefix("/assets/") {
                let notFound = Data("Not Found".utf8)
                let head = Self.serializeResponseHead(
                    status: 404,
                    reason: "Not Found",
                    headers: [
                        ("Content-Type", "text/plain; charset=utf-8"),
                        ("Content-Length", "\(notFound.count)"),
                        ("Connection", "close"),
                    ]
                )
                try await sendAll(data: head + notFound, over: connection)
                return
            }
            let indexCandidate = root.appendingPathComponent("index.html")
            if FileManager.default.fileExists(atPath: indexCandidate.path) {
                fileURL = indexCandidate
            }
        }

        guard let resolvedFile = fileURL,
              let data = try? Data(contentsOf: resolvedFile) else {
            let missing = Data("Web bundle missing".utf8)
            let head = Self.serializeResponseHead(
                status: 404,
                reason: "Not Found",
                headers: [
                    ("Content-Type", "text/plain; charset=utf-8"),
                    ("Content-Length", "\(missing.count)"),
                    ("Connection", "close"),
                ]
            )
            try await sendAll(data: head + missing, over: connection)
            return
        }

        let mime = Self.mimeType(for: resolvedFile.pathExtension)
        let cacheControl = resolvedFile.lastPathComponent == "index.html"
            ? "no-cache"
            : "public, max-age=31536000, immutable"
        let head = Self.serializeResponseHead(
            status: 200,
            reason: "OK",
            headers: [
                ("Content-Type", mime),
                ("Content-Length", "\(data.count)"),
                ("Cache-Control", cacheControl),
                ("Connection", "close"),
            ]
        )
        if request.method.uppercased() == "HEAD" {
            try await sendAll(data: head, over: connection)
        } else {
            try await sendAll(data: head + data, over: connection)
        }
    }

    // MARK: - Upstream Hub Forwarding (REST + SSE)

    private func forwardToHub(request: ParsedHTTPRequest, over connection: NWConnection) async throws {
        let active = currentTarget()
        guard let upstreamBase = active.baseURL,
              let upstreamURL = Self.joinUpstream(base: upstreamBase, rawTarget: request.rawTarget) else {
            let body = Data("{\"error\":\"Hub unreachable\"}".utf8)
            let head = Self.serializeResponseHead(
                status: 503,
                reason: "Service Unavailable",
                headers: [
                    ("Content-Type", "application/json; charset=utf-8"),
                    ("Content-Length", "\(body.count)"),
                    ("Connection", "close"),
                ]
            )
            try await sendAll(data: head + body, over: connection)
            return
        }

        var urlReq = URLRequest(url: upstreamURL)
        urlReq.httpMethod = request.method
        urlReq.cachePolicy = .reloadIgnoringLocalCacheData
        if !request.body.isEmpty {
            urlReq.httpBody = request.body
        }

        let hopByHop: Set<String> = [
            "host", "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
            "te", "trailer", "transfer-encoding", "upgrade", "content-length", "origin", "referer",
            "cookie", "accept-encoding",
        ]
        var hasAuthHeader = false
        for (name, value) in request.headers {
            let lower = name.lowercased()
            if hopByHop.contains(lower) { continue }
            if lower == "authorization" { hasAuthHeader = true }
            urlReq.setValue(value, forHTTPHeaderField: name)
        }
        urlReq.setValue("identity", forHTTPHeaderField: "Accept-Encoding")

        if let unwrappedCookies = Self.upstreamCookieHeader(from: request.headers, hubID: active.hubID) {
            urlReq.setValue(unwrappedCookies, forHTTPHeaderField: "Cookie")
        }
        if !hasAuthHeader, let cred = active.credential, !cred.isEmpty {
            urlReq.setValue("Bearer \(cred)", forHTTPHeaderField: "Authorization")
        }

        let isSSE = request.path == "/api/stream"
            || (request.headers.first(where: { $0.0.lowercased() == "accept" })?.1.contains("text/event-stream") == true)

        if isSSE {
            urlReq.timeoutInterval = 86_400
            let (bytes, response) = try await session.bytes(for: urlReq)
            guard let http = response as? HTTPURLResponse else {
                throw URLError(.badServerResponse)
            }
            var outHeaders = Self.filteredUpstreamHeaders(from: http, hubID: active.hubID, stripContentLength: true)
            outHeaders.append(("Cache-Control", "no-cache"))
            outHeaders.append(("Connection", "close"))
            let head = Self.serializeResponseHead(
                status: http.statusCode,
                reason: HTTPURLResponse.localizedString(forStatusCode: http.statusCode),
                headers: outHeaders
            )
            try await sendAll(data: head, over: connection)

            // Flush line-by-line so SSE frames arrive immediately in WKWebView's EventSource
            var lineBuffer = Data()
            lineBuffer.reserveCapacity(1024)
            for try await byte in bytes {
                lineBuffer.append(byte)
                if byte == 0x0A || lineBuffer.count >= 4096 {
                    try await sendAll(data: lineBuffer, over: connection)
                    lineBuffer.removeAll(keepingCapacity: true)
                }
            }
            if !lineBuffer.isEmpty {
                try await sendAll(data: lineBuffer, over: connection)
            }
        } else {
            let (data, response) = try await session.data(for: urlReq)
            guard let http = response as? HTTPURLResponse else {
                throw URLError(.badServerResponse)
            }
            observeAuthResponse(path: request.path, statusCode: http.statusCode, data: data, hubID: active.hubID)
            var outHeaders = Self.filteredUpstreamHeaders(from: http, hubID: active.hubID, stripContentLength: true)
            outHeaders.append(("Content-Length", "\(data.count)"))
            outHeaders.append(("Connection", "close"))
            let head = Self.serializeResponseHead(
                status: http.statusCode,
                reason: HTTPURLResponse.localizedString(forStatusCode: http.statusCode),
                headers: outHeaders
            )
            if request.method.uppercased() == "HEAD" {
                try await sendAll(data: head, over: connection)
            } else {
                try await sendAll(data: head + data, over: connection)
            }
        }
    }

    private func observeAuthResponse(path: String, statusCode: Int, data: Data, hubID: String) {
        guard !hubID.isEmpty else { return }
        if statusCode == 200 && (path == "/auth/token" || path == "/auth/pair"),
           let signed = try? JSONDecoder().decode(PairSignInResponse.self, from: data),
           let cred = signed.credential, !cred.isEmpty {
            let handler: (@Sendable (String, ProxyAuthEvent) -> Void)? = lock.withLock {
                if target.hubID == hubID {
                    target.credential = cred
                }
                return authEventHandler
            }
            handler?(hubID, .signedIn(signed))
        } else if (200...299).contains(statusCode) && (path == "/auth/client/logout" || path == "/auth/logout") {
            let handler: (@Sendable (String, ProxyAuthEvent) -> Void)? = lock.withLock {
                if target.hubID == hubID {
                    target.credential = nil
                }
                return authEventHandler
            }
            handler?(hubID, .signedOut)
        }
    }

    private static func joinUpstream(base: URL, rawTarget: String) -> URL? {
        let baseStr = URLNormalizer.canonicalOrigin(base)
        let suffix = rawTarget.hasPrefix("/") ? rawTarget : "/\(rawTarget)"
        return URL(string: baseStr + suffix)
    }

    /// Returns `true` when a cookie in `WKHTTPCookieStore` belongs to one of `purgedHubIDs`.
    public static func shouldDeleteWebCookie(named cookieName: String, purgedHubIDs: Set<String>) -> Bool {
        guard cookieName != proxyAuthCookieName, !purgedHubIDs.isEmpty else { return false }
        for id in purgedHubIDs {
            if cookieName.hasPrefix(hubCookiePrefix(for: id)) {
                return true
            }
        }
        return false
    }

    internal static func filteredUpstreamHeaders(
        from http: HTTPURLResponse,
        hubID: String,
        stripContentLength: Bool
    ) -> [(String, String)] {
        let blocked: Set<String> = [
            "connection", "keep-alive", "transfer-encoding", "content-encoding",
            "strict-transport-security", "clear-site-data",
        ]
        var out: [(String, String)] = []
        for (rawK, rawV) in http.allHeaderFields {
            guard let k = rawK as? String, let v = rawV as? String else { continue }
            let lower = k.lowercased()
            if blocked.contains(lower) { continue }
            if stripContentLength && lower == "content-length" { continue }
            if lower == "set-cookie" {
                for singleCookie in splitFoldedSetCookieHeader(v) {
                    out.append(("Set-Cookie", rewriteCookieForLoopback(singleCookie, hubID: hubID)))
                }
            } else {
                out.append((k, v))
            }
        }
        return out
    }

    // MARK: - Minimal HTTP/1.1 Parser & Serializer

    private struct ParsedHTTPRequest {
        var method: String
        var rawTarget: String
        var path: String
        var headers: [(String, String)]
        var body: Data
    }

    private func readHTTPRequest(from connection: NWConnection) async throws -> ParsedHTTPRequest? {
        var buffer = Data()
        let delimiter = Data([0x0D, 0x0A, 0x0D, 0x0A]) // \r\n\r\n
        var headerEndRange: Range<Data.Index>?

        while headerEndRange == nil {
            let chunk = try await receiveChunk(from: connection)
            if chunk.isEmpty {
                return nil
            }
            buffer.append(chunk)
            headerEndRange = buffer.range(of: delimiter)
            if buffer.count > 131_072 && headerEndRange == nil {
                throw URLError(.dataLengthExceedsMaximum)
            }
        }

        let headerEnd = headerEndRange!.lowerBound
        let bodyStart = headerEndRange!.upperBound
        guard let headerString = String(data: buffer[..<headerEnd], encoding: .utf8) else {
            return nil
        }

        let lines = headerString.components(separatedBy: "\r\n")
        guard let requestLine = lines.first else { return nil }
        let parts = requestLine.split(separator: " ", maxSplits: 2)
        guard parts.count >= 2 else { return nil }
        let method = String(parts[0])
        let rawTarget = String(parts[1])
        let pathOnly = rawTarget.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? "/"

        var headers: [(String, String)] = []
        var contentLength = 0
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = String(line[..<colon]).trimmingCharacters(in: .whitespaces)
            let value = String(line[line.index(after: colon)...]).trimmingCharacters(in: .whitespaces)
            headers.append((name, value))
            if name.lowercased() == "content-length", let len = Int(value), len > 0 {
                contentLength = len
            }
        }

        var body = Data(buffer[bodyStart...])
        while body.count < contentLength {
            let chunk = try await receiveChunk(from: connection)
            if chunk.isEmpty { break }
            body.append(chunk)
        }
        if body.count > contentLength {
            body = body.prefix(contentLength)
        }

        return ParsedHTTPRequest(
            method: method,
            rawTarget: rawTarget,
            path: pathOnly,
            headers: headers,
            body: body
        )
    }

    private func receiveChunk(from connection: NWConnection) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { content, _, isComplete, error in
                if let error {
                    continuation.resume(throwing: error)
                } else if let content, !content.isEmpty {
                    continuation.resume(returning: content)
                } else if isComplete {
                    continuation.resume(returning: Data())
                } else {
                    continuation.resume(returning: Data())
                }
            }
        }
    }

    private func sendAll(data: Data, over connection: NWConnection) async throws {
        guard !data.isEmpty else { return }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: data, completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume(returning: ())
                }
            })
        }
    }

    private static func serializeResponseHead(status: Int, reason: String, headers: [(String, String)]) -> Data {
        let cleanReason = reason.isEmpty ? "OK" : reason
        var text = "HTTP/1.1 \(status) \(cleanReason)\r\n"
        for (name, value) in headers {
            text += "\(name): \(value)\r\n"
        }
        text += "\r\n"
        return Data(text.utf8)
    }

    private static func mimeType(for ext: String) -> String {
        switch ext.lowercased() {
        case "html": return "text/html; charset=utf-8"
        case "js", "mjs": return "application/javascript; charset=utf-8"
        case "css": return "text/css; charset=utf-8"
        case "json", "map": return "application/json; charset=utf-8"
        case "svg": return "image/svg+xml"
        case "png": return "image/png"
        case "jpg", "jpeg": return "image/jpeg"
        case "webp": return "image/webp"
        case "ico": return "image/x-icon"
        case "woff2": return "font/woff2"
        case "woff": return "font/woff"
        case "ttf": return "font/ttf"
        case "wasm": return "application/wasm"
        default: return "application/octet-stream"
        }
    }
}
