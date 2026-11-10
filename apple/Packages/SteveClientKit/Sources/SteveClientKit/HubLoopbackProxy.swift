import Foundation
import Network

/// Serves the bundled Steve web UI on `http://127.0.0.1:<ephemeral>` and proxies `/api/*`, `/auth/*`, and
/// `/healthz` to the currently selected Hub endpoint while injecting Keychain `Authorization: Bearer` credentials
/// and streaming `/api/stream` Server-Sent Events without buffering.
public final class HubLoopbackProxy: @unchecked Sendable {
    public struct ActiveTarget: Sendable {
        public var baseURL: URL?
        public var credential: String?

        public init(baseURL: URL? = nil, credential: String? = nil) {
            self.baseURL = baseURL
            self.credential = credential
        }
    }

    private let webRoot: URL?
    private let queue = DispatchQueue(label: "app.steve.client.loopback-proxy", qos: .userInitiated)
    private let lock = NSLock()
    private var target = ActiveTarget()
    private var listener: NWListener?
    private let session: URLSession

    public private(set) var port: UInt16 = 0

    public var localOriginURL: URL? {
        guard port > 0 else { return nil }
        return URL(string: "http://127.0.0.1:\(port)")
    }

    public init(webRoot: URL? = nil) {
        self.webRoot = webRoot
        let cfg = URLSessionConfiguration.ephemeral
        cfg.timeoutIntervalForRequest = 60
        cfg.timeoutIntervalForResource = 86_400
        cfg.httpCookieAcceptPolicy = .always
        cfg.httpShouldSetCookies = true
        self.session = URLSession(configuration: cfg)
    }

    public func updateTarget(_ newTarget: ActiveTarget) {
        lock.lock()
        target = newTarget
        lock.unlock()
    }

    public func currentTarget() -> ActiveTarget {
        lock.lock()
        defer { lock.unlock() }
        return target
    }

    @discardableResult
    public func start() async throws -> URL {
        if let existing = localOriginURL {
            return existing
        }
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = true
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: .any)

        let nwListener = try NWListener(using: params)
        self.listener = nwListener

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

        self.port = boundPort
        return URL(string: "http://127.0.0.1:\(boundPort)")!
    }

    public func stop() {
        listener?.cancel()
        listener = nil
        port = 0
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

    private func handleConnection(_ connection: NWConnection) async {
        defer { connection.cancel() }
        do {
            guard let request = try await readHTTPRequest(from: connection) else {
                return
            }
            if Self.isProxiedPath(request.path) {
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

    private static func isProxiedPath(_ pathOnly: String) -> Bool {
        pathOnly == "/api"
            || pathOnly.hasPrefix("/api/")
            || pathOnly == "/auth"
            || pathOnly.hasPrefix("/auth/")
            || pathOnly == "/healthz"
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
        ]
        var hasAuthHeader = false
        for (name, value) in request.headers {
            let lower = name.lowercased()
            if hopByHop.contains(lower) { continue }
            if lower == "authorization" { hasAuthHeader = true }
            urlReq.setValue(value, forHTTPHeaderField: name)
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
            var outHeaders = Self.filteredUpstreamHeaders(from: http, stripContentLength: true)
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
            var outHeaders = Self.filteredUpstreamHeaders(from: http, stripContentLength: true)
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

    private static func joinUpstream(base: URL, rawTarget: String) -> URL? {
        let baseStr = URLNormalizer.canonicalOrigin(base)
        let suffix = rawTarget.hasPrefix("/") ? rawTarget : "/\(rawTarget)"
        return URL(string: baseStr + suffix)
    }

    private static func filteredUpstreamHeaders(from http: HTTPURLResponse, stripContentLength: Bool) -> [(String, String)] {
        let blocked: Set<String> = [
            "connection", "keep-alive", "transfer-encoding", "content-encoding",
            "strict-transport-security",
        ]
        var out: [(String, String)] = []
        for (rawK, rawV) in http.allHeaderFields {
            guard let k = rawK as? String, let v = rawV as? String else { continue }
            let lower = k.lowercased()
            if blocked.contains(lower) { continue }
            if stripContentLength && lower == "content-length" { continue }
            if lower == "set-cookie" {
                out.append((k, rewriteCookieForLoopback(v)))
            } else {
                out.append((k, v))
            }
        }
        return out
    }

    /// Strips `Secure` and `Domain=...` attributes when proxying over `http://127.0.0.1` so WebKit keeps session cookies too.
    private static func rewriteCookieForLoopback(_ cookieHeader: String) -> String {
        let parts = cookieHeader.split(separator: ";", omittingEmptySubsequences: false)
        var kept: [String] = []
        for (index, part) in parts.enumerated() {
            let trimmed = part.trimmingCharacters(in: .whitespaces)
            if index == 0 {
                kept.append(trimmed)
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
