import SwiftUI
import WebKit

#if canImport(AppKit)
import AppKit
#elseif canImport(UIKit)
import UIKit
#endif

public struct SteveWebView: NSViewRepresentableFallback {
    internal typealias RuleListCompiler = @MainActor (Int) async throws -> WKContentRuleList

    @ObservedObject private var coordinator: SteveClientCoordinator
    private let platformName: String
    private let ruleListCompiler: RuleListCompiler
    private let useNonPersistentDataStore: Bool?

    public init(coordinator: SteveClientCoordinator, platformName: String) {
        self.coordinator = coordinator
        self.platformName = platformName
        self.ruleListCompiler = Self.compileLoopbackPortRuleList
        self.useNonPersistentDataStore = nil
    }

    internal init(
        coordinator: SteveClientCoordinator,
        platformName: String,
        useNonPersistentDataStore: Bool? = nil,
        ruleListCompiler: @escaping RuleListCompiler = Self.compileLoopbackPortRuleList
    ) {
        self.coordinator = coordinator
        self.platformName = platformName
        self.useNonPersistentDataStore = useNonPersistentDataStore
        self.ruleListCompiler = ruleListCompiler
    }

    #if os(macOS)
    public func makeCoordinator() -> BridgeCoordinator {
        BridgeCoordinator(parent: self)
    }

    public func makeNSView(context: Context) -> WKWebView {
        let webView = buildWebView(bridge: context.coordinator)
        context.coordinator.attach(webView)
        loadIfReady(in: webView, bridge: context.coordinator)
        return webView
    }

    public func updateNSView(_ webView: WKWebView, context: Context) {
        context.coordinator.parent = self
        loadIfReady(in: webView, bridge: context.coordinator)
    }
    #else
    public func makeCoordinator() -> BridgeCoordinator {
        BridgeCoordinator(parent: self)
    }

    public func makeUIView(context: Context) -> WKWebView {
        let webView = buildWebView(bridge: context.coordinator)
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        webView.isOpaque = false
        webView.backgroundColor = .clear
        context.coordinator.attach(webView)
        loadIfReady(in: webView, bridge: context.coordinator)
        return webView
    }

    public func updateUIView(_ webView: WKWebView, context: Context) {
        context.coordinator.parent = self
        loadIfReady(in: webView, bridge: context.coordinator)
    }
    #endif

    @MainActor
    internal func makeTestWebView(bridge: BridgeCoordinator) -> WKWebView {
        let webView = buildWebView(bridge: bridge)
        webView.frame = CGRect(x: 0, y: 0, width: 480, height: 360)
        bridge.attach(webView)
        loadIfReady(in: webView, bridge: bridge)
        return webView
    }

    @MainActor
    internal func updateTestWebView(_ webView: WKWebView, bridge: BridgeCoordinator) {
        bridge.parent = self
        loadIfReady(in: webView, bridge: bridge)
    }

    @MainActor
    private func buildWebView(bridge: BridgeCoordinator) -> WKWebView {
        let config = WKWebViewConfiguration()
        if let useNonPersistentDataStore {
            config.websiteDataStore = useNonPersistentDataStore ? .nonPersistent() : .default()
        } else {
            #if DEBUG
            if ProcessInfo.processInfo.environment["STEVE_CLIENT_WEBKIT_EPHEMERAL"] == "1" {
                config.websiteDataStore = .nonPersistent()
            } else {
                config.websiteDataStore = .default()
            }
            #else
            config.websiteDataStore = .default()
            #endif
        }
        config.preferences.javaScriptCanOpenWindowsAutomatically = false

        let userContent = WKUserContentController()
        userContent.addScriptMessageHandler(
            bridge,
            contentWorld: .page,
            name: BridgeCoordinator.handlerName
        )
        config.userContentController = userContent

        let webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = bridge
        webView.uiDelegate = bridge
        #if os(macOS)
        webView.setValue(false, forKey: "drawsBackground")
        #endif
        return webView
    }

    @MainActor
    private func loadIfReady(in webView: WKWebView, bridge: BridgeCoordinator) {
        guard let gateway = coordinator.localGatewayURL,
              let gatewayPort = gateway.port else { return }

        if bridge.loadedNonce != coordinator.webReloadNonce || bridge.loadedGateway != gateway {
            let gatewayChanged = (bridge.loadedGateway != gateway)
            let previousToken = bridge.loadedProxyToken
            bridge.loadedNonce = coordinator.webReloadNonce
            bridge.loadedGateway = gateway
            bridge.isPageLoaded = false
            let loadGeneration = bridge.bumpLoadGeneration()
            let token = coordinator.proxy.proxyAuthToken
            let platform = platformName
            let compiler = ruleListCompiler

            // A page loaded with another gateway key is the previous Hub's page. It has to be gone
            // before the new key exists in the cookie store: `stopLoading()` ends loads, not the
            // document, and its timers would carry the new key to the newly selected Hub.
            let mustUnloadPreviousDocument = previousToken != nil && previousToken != token
            let unloadNavigation: WKNavigation? = mustUnloadPreviousDocument
                ? bridge.beginUnloadingDocument(in: webView)
                : nil

            Task { @MainActor [weak webView, weak bridge, weak coordinator] in
                guard let webView, let bridge, let coordinator,
                      bridge.loadGeneration == loadGeneration else { return }

                if mustUnloadPreviousDocument {
                    let unloaded = await bridge.waitForDocumentUnload(unloadNavigation)
                    guard bridge.loadGeneration == loadGeneration else { return }
                    guard unloaded else {
                        webView.stopLoading()
                        coordinator.failGatewayIsolation(
                            L10n.tr(
                                "无法卸载上一个 Hub 的页面，已停止本地网关。",
                                "The previous Hub's page could not be unloaded; the local gateway was stopped."
                            )
                        )
                        return
                    }
                    await bridge.loadStepObserver?(.previousDocumentUnloaded)
                    guard bridge.loadGeneration == loadGeneration else { return }
                }

                if gatewayChanged || !bridge.hasInstalledRuleList {
                    let userContent = webView.configuration.userContentController
                    userContent.removeAllUserScripts()
                    userContent.removeAllContentRuleLists()
                    bridge.hasInstalledRuleList = false
                    do {
                        let ruleList = try await compiler(gatewayPort)
                        guard bridge.loadGeneration == loadGeneration else { return }
                        userContent.add(ruleList)
                        let bootstrapJS = Self.makeBootstrapScript(platform: platform, gateway: gateway)
                        userContent.addUserScript(
                            WKUserScript(
                                source: bootstrapJS,
                                injectionTime: .atDocumentStart,
                                forMainFrameOnly: true
                            )
                        )
                        bridge.hasInstalledRuleList = true
                    } catch {
                        guard bridge.loadGeneration == loadGeneration else { return }
                        userContent.removeAllUserScripts()
                        userContent.removeAllContentRuleLists()
                        bridge.hasInstalledRuleList = false
                        webView.stopLoading()
                        coordinator.failGatewayIsolation(
                            L10n.tr(
                                "WebView 端口隔离规则编译失败：\(error.localizedDescription)",
                                "Failed to compile WebView loopback isolation rules: \(error.localizedDescription)"
                            )
                        )
                        return
                    }
                }

                coordinator.purgePendingWebCookies()
                let planted = await Self.plantProxyCookie(in: webView, gateway: gateway, token: token)
                guard bridge.loadGeneration == loadGeneration else { return }
                guard planted else {
                    // Without the key every request of the page is refused by the gateway: say so
                    // instead of showing a page that only gets 403s.
                    webView.stopLoading()
                    coordinator.failGatewayIsolation(
                        L10n.tr(
                            "无法为本地网关创建访问密钥 Cookie。",
                            "The local gateway's access cookie could not be created."
                        )
                    )
                    return
                }
                bridge.loadedProxyToken = token
                await bridge.loadStepObserver?(.proxyCookiePlanted)
                guard bridge.loadGeneration == loadGeneration else { return }
                webView.load(URLRequest(url: gateway))
                await bridge.loadStepObserver?(.gatewayLoadStarted)
            }
            return
        }

        if bridge.isPageLoaded {
            applyPendingHashRouteIfNeeded(in: webView)
        }
    }

    @MainActor
    fileprivate func applyPendingHashRouteIfNeeded(in webView: WKWebView) {
        if let hash = coordinator.targetHashRoute {
            coordinator.consumeTargetHashRoute()
            let escaped = hash.replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "'", with: "\\'")
            webView.evaluateJavaScript("window.location.hash = '\(escaped)';", completionHandler: nil)
        }
    }

    // MARK: - Loopback Cookie & Port Isolation (N3)

    /// Creates the per-launch `steve_proxy_key` cookie via RFC 6265 `Set-Cookie` parsing so `isHTTPOnly` is guaranteed `true`.
    internal static func makeProxyAuthCookie(gateway: URL, token: String) -> HTTPCookie? {
        let headerValue = "\(HubLoopbackProxy.proxyAuthCookieName)=\(token); Path=/; HttpOnly; SameSite=Lax"
        return HTTPCookie.cookies(
            withResponseHeaderFields: ["Set-Cookie": headerValue],
            for: gateway
        ).first(where: { $0.name == HubLoopbackProxy.proxyAuthCookieName })
    }

    @MainActor
    private static func plantProxyCookie(
        in webView: WKWebView,
        gateway: URL,
        token: String
    ) async -> Bool {
        guard let cookie = makeProxyAuthCookie(gateway: gateway, token: token) else { return false }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            webView.configuration.websiteDataStore.httpCookieStore.setCookie(cookie) {
                continuation.resume()
            }
        }
        return true
    }

    /// Builds a WebKit Content Blocker rule list that blocks WebView requests to any loopback host/port
    /// (`127.0.0.0/8`, `0.0.0.0`, `localhost`, `*.localhost`, `[::1]`) except `127.0.0.1:<gatewayPort>`.
    /// Uses only WebKit DFA-compatible regex syntax (no `|` alternation or `{m,n}` quantifiers).
    internal static func makeLoopbackPortIsolationRuleListJSON(gatewayPort: Int) throws -> String {
        let hostPatterns = [
            "127\\.[0-9]+\\.[0-9]+\\.[0-9]+[:/]",
            "0\\.0\\.0\\.0[:/]",
            "localhost[:/]",
            "[a-z0-9.-]+\\.localhost[:/]",
            "\\[::1\\][:/]",
        ]
        let schemePrefixes = ["^https?://", "^wss?://"]

        var rules: [[String: Any]] = []
        for scheme in schemePrefixes {
            for host in hostPatterns {
                rules.append([
                    "trigger": [
                        "url-filter": "\(scheme)\(host)",
                        "url-filter-is-case-sensitive": false,
                    ],
                    "action": [
                        "type": "block",
                    ],
                ])
            }
        }

        for allowPrefix in [
            "^http://127\\.0\\.0\\.1:\(gatewayPort)/",
            "^ws://127\\.0\\.0\\.1:\(gatewayPort)/",
        ] {
            rules.append([
                "trigger": [
                    "url-filter": allowPrefix,
                    "url-filter-is-case-sensitive": false,
                ],
                "action": [
                    "type": "ignore-previous-rules",
                ],
            ])
        }

        let data = try JSONSerialization.data(withJSONObject: rules)
        guard let json = String(data: data, encoding: .utf8), !rules.isEmpty else {
            throw BridgeCallError.operationFailed("Failed to serialize loopback isolation rule list")
        }
        return json
    }

    @MainActor
    internal static func compileLoopbackPortRuleList(gatewayPort: Int) async throws -> WKContentRuleList {
        guard (1...65535).contains(gatewayPort) else {
            throw BridgeCallError.operationFailed("Invalid gateway port for rule list")
        }
        let identifier = "steve-loopback-isolation-\(gatewayPort)"
        let json = try makeLoopbackPortIsolationRuleListJSON(gatewayPort: gatewayPort)
        guard let store = WKContentRuleListStore.default() else {
            throw BridgeCallError.operationFailed("WKContentRuleListStore unavailable")
        }
        return try await withCheckedThrowingContinuation { continuation in
            store.compileContentRuleList(forIdentifier: identifier, encodedContentRuleList: json) { list, error in
                if let list {
                    continuation.resume(returning: list)
                } else {
                    continuation.resume(throwing: error ?? BridgeCallError.operationFailed("Rule list compile failed"))
                }
            }
        }
    }

    // MARK: - Navigation Confinement & Authenticated Downloads (N5)

    internal enum NavigationDisposition: Equatable, Sendable {
        case allow
        case download
        case startGatewayDownload(URL)
        case cancel
        case openExternal(URL)
    }

    internal static func decideNavigationDisposition(
        url: URL,
        gateway: URL?,
        activeHubEndpoints: [URL] = [],
        shouldPerformDownload: Bool = false,
        targetFrameIsMain: Bool?,
        sourceFrameIsMain: Bool,
        navigationType: WKNavigationType
    ) -> NavigationDisposition {
        let scheme = url.scheme?.lowercased() ?? ""

        // Never initiate pairing directly from web navigation; pairing flows through `pair.claim`.
        if scheme == "steve" {
            return .cancel
        }

        // Allow `<a download>` on `blob:` / `data:` or gateway-origin resources.
        if shouldPerformDownload {
            if scheme == "blob" || scheme == "data" {
                return (sourceFrameIsMain || navigationType == .linkActivated) ? .download : .cancel
            }
            if let gateway, isGatewayOrigin(url, gateway: gateway) {
                return (sourceFrameIsMain || navigationType == .linkActivated) ? .download : .cancel
            }
            if (sourceFrameIsMain || navigationType == .linkActivated),
               let gwDownload = gatewayDownloadURLForAuthenticatedPath(
                   url,
                   gateway: gateway,
                   activeHubEndpoints: activeHubEndpoints
               ) {
                return .startGatewayDownload(gwDownload)
            }
            return .cancel
        }

        // Allow `about:blank` / `about:srcdoc` (used by sandboxed markdown/mail HTML preview iframes).
        if scheme == "about" {
            return .allow
        }

        // `target="_blank"` links have `targetFrame == nil` (`targetFrameIsMain == nil`).
        // Route same-Hub `/api/...` links through authenticated gateway download instead of an unauthenticated browser.
        if targetFrameIsMain == nil {
            if (scheme == "http" || scheme == "https") && (sourceFrameIsMain || navigationType == .linkActivated) {
                if let gwDownload = gatewayDownloadURLForAuthenticatedPath(
                    url,
                    gateway: gateway,
                    activeHubEndpoints: activeHubEndpoints
                ) {
                    return .startGatewayDownload(gwDownload)
                }
                return .openExternal(url)
            }
            return .cancel
        }

        // Subframe navigations (`targetFrame.isMainFrame == false`):
        // Allow gateway-origin embeds (PDF/mail iframes) and `blob:`/`data:`, but cancel scripted
        // external `http(s)` navigations without launching the system browser unless `.linkActivated`.
        if targetFrameIsMain == false {
            if let gateway, isGatewayOrigin(url, gateway: gateway) {
                return .allow
            }
            if scheme == "blob" || scheme == "data" {
                return .allow
            }
            if (scheme == "http" || scheme == "https") && navigationType == .linkActivated {
                if let gwDownload = gatewayDownloadURLForAuthenticatedPath(
                    url,
                    gateway: gateway,
                    activeHubEndpoints: activeHubEndpoints
                ) {
                    return .startGatewayDownload(gwDownload)
                }
                return .openExternal(url)
            }
            return .cancel
        }

        // Confine in-webview main-frame http(s) navigations strictly to the loopback gateway origin.
        if let gateway, isGatewayOrigin(url, gateway: gateway) {
            return .allow
        }

        if scheme == "blob" || scheme == "data" {
            return .download
        }

        if scheme == "http" || scheme == "https" {
            if let gwDownload = gatewayDownloadURLForAuthenticatedPath(
                url,
                gateway: gateway,
                activeHubEndpoints: activeHubEndpoints
            ) {
                return .startGatewayDownload(gwDownload)
            }
            return .openExternal(url)
        }

        return .cancel
    }

    /// Rewrites authenticated `/api/...` URLs targeting the gateway or active Hub to a gateway URL
    /// so `WKDownload` fetches them with `steve_proxy_key` + Keychain `Authorization: Bearer`.
    internal static func gatewayDownloadURLForAuthenticatedPath(
        _ url: URL,
        gateway: URL?,
        activeHubEndpoints: [URL]
    ) -> URL? {
        guard let gateway else { return nil }
        let path = url.path
        let isAPIPath = (path == "/api" || path.hasPrefix("/api/"))
        guard isAPIPath else { return nil }

        if isGatewayOrigin(url, gateway: gateway) {
            return url
        }

        let targetOrigin = URLNormalizer.canonicalOrigin(url)
        let matchesActiveHub = activeHubEndpoints.contains {
            URLNormalizer.canonicalOrigin($0) == targetOrigin
        }
        guard matchesActiveHub,
              var comps = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let gwComps = URLComponents(url: gateway, resolvingAgainstBaseURL: false) else {
            return nil
        }
        comps.scheme = gwComps.scheme
        comps.host = gwComps.host
        comps.port = gwComps.port
        return comps.url
    }

    internal static func shouldDownloadResponse(
        _ response: URLResponse,
        canShowMIMEType: Bool,
        gateway: URL?
    ) -> Bool {
        guard let url = response.url else { return false }
        let scheme = url.scheme?.lowercased() ?? ""
        let allowedOrigin = (scheme == "blob" || scheme == "data")
            || (gateway != nil && isGatewayOrigin(url, gateway: gateway!))
        guard allowedOrigin else { return false }

        if let http = response as? HTTPURLResponse {
            if http.statusCode >= 400 {
                return false
            }
            if let disposition = http.value(forHTTPHeaderField: "Content-Disposition")?.lowercased() {
                let trimmed = disposition.trimmingCharacters(in: .whitespaces)
                if trimmed == "attachment" || trimmed.hasPrefix("attachment;") {
                    return true
                }
            }
        }
        return !canShowMIMEType
    }

    internal static func sanitizeSuggestedFilename(_ raw: String) -> String {
        let trimmed = raw
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "\\", with: "_")
            .replacingOccurrences(of: "\0", with: "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed == "." || trimmed == ".." {
            return "download"
        }
        return trimmed
    }

    /// Chooses a non-conflicting file path inside `directory` (`name.ext`, `name (1).ext`, `name (2).ext`, ...).
    internal static func uniqueDownloadDestination(in directory: URL, suggestedFilename: String) -> URL {
        let safeName = sanitizeSuggestedFilename(suggestedFilename)
        let fm = FileManager.default
        let initial = directory.appendingPathComponent(safeName, isDirectory: false)
        if !fm.fileExists(atPath: initial.path) {
            return initial
        }

        let nsName = safeName as NSString
        let ext = nsName.pathExtension
        let stem = ext.isEmpty ? safeName : nsName.deletingPathExtension

        for index in 1...10_000 {
            let candidateName = ext.isEmpty ? "\(stem) (\(index))" : "\(stem) (\(index)).\(ext)"
            let candidate = directory.appendingPathComponent(candidateName, isDirectory: false)
            if !fm.fileExists(atPath: candidate.path) {
                return candidate
            }
        }
        return directory.appendingPathComponent("\(UUID().uuidString)-\(safeName)", isDirectory: false)
    }

    internal static func isGatewayOrigin(_ url: URL, gateway: URL) -> Bool {
        guard let urlScheme = url.scheme?.lowercased(),
              let gwScheme = gateway.scheme?.lowercased(),
              urlScheme == gwScheme,
              let urlHost = url.host?.lowercased(),
              let gwHost = gateway.host?.lowercased(),
              urlHost == gwHost,
              url.port == gateway.port else {
            return false
        }
        return true
    }

    internal static func rewriteExternalGatewayURL(_ url: URL, gateway: URL?, activeHubEndpoint: URL?) -> URL {
        guard let gateway,
              isGatewayOrigin(url, gateway: gateway),
              let activeHubEndpoint,
              var targetComps = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let hubComps = URLComponents(url: activeHubEndpoint, resolvingAgainstBaseURL: false) else {
            return url
        }
        targetComps.scheme = hubComps.scheme
        targetComps.host = hubComps.host
        targetComps.port = hubComps.port
        return targetComps.url ?? url
    }

    // MARK: - Native Bridge v2 Bootstrap (`window.__STEVE_NATIVE__`)

    internal static func makeBootstrapScript(platform: String, gateway: URL) -> String {
        let expectedOrigin = URLNormalizer.canonicalOrigin(gateway)
        let caps = SteveClientCoordinator.bridgeCapabilities(for: platform)
        let capsJSON = (try? String(data: JSONEncoder().encode(caps), encoding: .utf8)) ?? "[]"
        return """
        (function() {
          const expectedOrigin = '\(expectedOrigin)';
          if (window.location.origin !== 'null' && window.location.origin !== '' && window.location.origin !== expectedOrigin) { return; }
          try { localStorage.removeItem('steve.hub'); } catch (_) {}
          const can = Object.freeze(\(capsJSON));
          const bridge = Object.freeze({
            v: 2,
            platform: '\(platform)',
            can: can,
            call: function(op, args) {
              if (window.location.origin !== expectedOrigin) {
                return Promise.reject(new Error('Forbidden origin'));
              }
              const payloadArgs = (args && typeof args === 'object' && !Array.isArray(args)) ? args : {};
              return window.webkit.messageHandlers.\(BridgeCoordinator.handlerName).postMessage({
                op: String(op || ''),
                args: payloadArgs
              });
            }
          });
          try {
            Object.defineProperty(window, '__STEVE_NATIVE__', {
              value: bridge,
              configurable: false,
              enumerable: false,
              writable: false
            });
          } catch (_) {
            window.__STEVE_NATIVE__ = bridge;
          }
        })();
        """
    }

    // MARK: - Bridge, Navigation, Open Panel & Download Coordinator

    public final class BridgeCoordinator: NSObject, WKScriptMessageHandlerWithReply, WKNavigationDelegate, WKUIDelegate, WKDownloadDelegate {
        public static let handlerName = "steveNative"

        var parent: SteveWebView
        weak var webView: WKWebView?
        var loadedNonce: UUID?
        var loadedGateway: URL?
        var loadedProxyToken: String?
        var isPageLoaded = false
        var hasInstalledRuleList = false
        private(set) var loadGeneration: UInt64 = 0
        private var downloadDestinations: [ObjectIdentifier: URL] = [:]

        init(parent: SteveWebView) {
            self.parent = parent
        }

        func attach(_ webView: WKWebView) {
            self.webView = webView
            #if DEBUG && os(macOS)
            Self.debugLastAttachedWebView = webView
            #endif
        }

        #if DEBUG && os(macOS)
        @MainActor
        public static weak var debugLastAttachedWebView: WKWebView?
        @MainActor
        public static var debugNextOpenPanelURLs: [URL]?
        #endif

        @MainActor
        func bumpLoadGeneration() -> UInt64 {
            loadGeneration &+= 1
            // A newer load took over: whoever waits for the unload of the older one stops waiting.
            finishDocumentUnload(committed: false)
            return loadGeneration
        }

        // MARK: Unloading the previous Hub's document

        enum LoadStep: Equatable {
            case previousDocumentUnloaded
            case proxyCookiePlanted
            case gatewayLoadStarted
        }

        /// Observes the steps of a load, in order, each before the next one starts.
        var loadStepObserver: (@MainActor (LoadStep) async -> Void)?

        private static let blankURL = URL(string: "about:blank")!
        private var unloadNavigation: WKNavigation?
        private weak var blankNavigation: WKNavigation?
        private var unloadCommitted: Bool?
        private var unloadWaiter: CheckedContinuation<Bool, Never>?

        /// Replaces the current document with `about:blank`. The document is gone once this
        /// navigation commits; `waitForDocumentUnload` waits for that.
        @MainActor
        func beginUnloadingDocument(in webView: WKWebView) -> WKNavigation? {
            webView.stopLoading()
            unloadCommitted = nil
            unloadNavigation = webView.load(URLRequest(url: Self.blankURL))
            blankNavigation = unloadNavigation
            return unloadNavigation
        }

        /// True once the `about:blank` navigation has committed. False if it failed or a newer load
        /// replaced it.
        @MainActor
        func waitForDocumentUnload(_ navigation: WKNavigation?) async -> Bool {
            guard let navigation, navigation === unloadNavigation else { return false }
            if let unloadCommitted {
                unloadNavigation = nil
                return unloadCommitted
            }
            return await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
                unloadWaiter = continuation
            }
        }

        @MainActor
        private func finishDocumentUnload(committed: Bool) {
            guard unloadNavigation != nil else { return }
            if let waiter = unloadWaiter {
                unloadWaiter = nil
                unloadNavigation = nil
                unloadCommitted = nil
                waiter.resume(returning: committed)
            } else {
                unloadCommitted = committed
            }
        }

        @MainActor
        public func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
            if navigation === unloadNavigation {
                finishDocumentUnload(committed: true)
            }
        }

        @MainActor
        public func webView(
            _ webView: WKWebView,
            didFailProvisionalNavigation navigation: WKNavigation!,
            withError error: Error
        ) {
            if navigation === unloadNavigation {
                finishDocumentUnload(committed: false)
            }
        }

        @MainActor
        public func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            if navigation === unloadNavigation {
                finishDocumentUnload(committed: false)
            }
        }

        @MainActor
        public func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            // The process is gone and so is the document it held.
            finishDocumentUnload(committed: true)
        }

        @MainActor
        private func isTrustedGatewayFrame(_ frameInfo: WKFrameInfo) -> Bool {
            guard frameInfo.isMainFrame,
                  let gateway = loadedGateway ?? parent.coordinator.localGatewayURL,
                  let expectedPort = gateway.port else {
                return false
            }
            let originProto = frameInfo.securityOrigin.`protocol`.lowercased()
            let originHost = frameInfo.securityOrigin.host.lowercased()
            let originPort = frameInfo.securityOrigin.port
            return originProto == (gateway.scheme?.lowercased() ?? "http")
                && originHost == (gateway.host?.lowercased() ?? "127.0.0.1")
                && originPort == expectedPort
        }

        @MainActor
        public func userContentController(
            _ userContentController: WKUserContentController,
            didReceive message: WKScriptMessage,
            replyHandler: @escaping @MainActor @Sendable (Any?, String?) -> Void
        ) {
            guard message.name == Self.handlerName,
                  isTrustedGatewayFrame(message.frameInfo),
                  let body = message.body as? [String: Any],
                  let op = body["op"] as? String,
                  !op.isEmpty else {
                replyHandler(nil, "Forbidden bridge call")
                return
            }
            let args = (body["args"] as? [String: Any]) ?? [:]
            let platform = parent.platformName
            let coordinator = parent.coordinator

            Task { @MainActor in
                do {
                    let result = try await coordinator.handleBridgeCall(op: op, args: args, platform: platform)
                    replyHandler(result, nil)
                } catch {
                    replyHandler(nil, error.localizedDescription)
                }
            }
        }

        private var activeHubEndpoints: [URL] {
            var endpoints: [URL] = []
            if let active = parent.coordinator.summary.activeEndpoint {
                endpoints.append(active)
            }
            if let profileEndpoints = parent.coordinator.hubStore.currentProfile?.orderedEndpoints {
                endpoints.append(contentsOf: profileEndpoints)
            }
            return endpoints
        }

        @MainActor
        public func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let url = navigationAction.request.url else {
                decisionHandler(.cancel)
                return
            }

            let gateway = loadedGateway ?? parent.coordinator.localGatewayURL
            let disposition = SteveWebView.decideNavigationDisposition(
                url: url,
                gateway: gateway,
                activeHubEndpoints: activeHubEndpoints,
                shouldPerformDownload: navigationAction.shouldPerformDownload,
                targetFrameIsMain: navigationAction.targetFrame?.isMainFrame,
                sourceFrameIsMain: navigationAction.sourceFrame.isMainFrame,
                navigationType: navigationAction.navigationType
            )

            switch disposition {
            case .allow:
                decisionHandler(.allow)
            case .download:
                decisionHandler(.download)
            case .startGatewayDownload(let downloadURL):
                startAuthenticatedDownload(url: downloadURL, in: webView)
                decisionHandler(.cancel)
            case .cancel:
                decisionHandler(.cancel)
            case .openExternal(let externalURL):
                openInSystemBrowser(externalURL, gateway: gateway)
                decisionHandler(.cancel)
            }
        }

        @MainActor
        public func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationResponse: WKNavigationResponse,
            decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
        ) {
            let gateway = loadedGateway ?? parent.coordinator.localGatewayURL
            if SteveWebView.shouldDownloadResponse(
                navigationResponse.response,
                canShowMIMEType: navigationResponse.canShowMIMEType,
                gateway: gateway
            ) {
                decisionHandler(.download)
            } else {
                decisionHandler(.allow)
            }
        }

        @MainActor
        public func webView(
            _ webView: WKWebView,
            navigationAction: WKNavigationAction,
            didBecome download: WKDownload
        ) {
            download.delegate = self
        }

        @MainActor
        public func webView(
            _ webView: WKWebView,
            navigationResponse: WKNavigationResponse,
            didBecome download: WKDownload
        ) {
            download.delegate = self
        }

        @MainActor
        public func webView(
            _ webView: WKWebView,
            createWebViewWith configuration: WKWebViewConfiguration,
            for navigationAction: WKNavigationAction,
            windowFeatures: WKWindowFeatures
        ) -> WKWebView? {
            if let url = navigationAction.request.url,
               let scheme = url.scheme?.lowercased(),
               (scheme == "http" || scheme == "https"),
               (navigationAction.sourceFrame.isMainFrame || navigationAction.navigationType == .linkActivated) {
                let gateway = loadedGateway ?? parent.coordinator.localGatewayURL
                if let gwDownload = SteveWebView.gatewayDownloadURLForAuthenticatedPath(
                    url,
                    gateway: gateway,
                    activeHubEndpoints: activeHubEndpoints
                ) {
                    startAuthenticatedDownload(url: gwDownload, in: webView)
                } else {
                    openInSystemBrowser(url, gateway: gateway)
                }
            }
            return nil
        }

        #if os(macOS)
        @MainActor
        public func webView(
            _ webView: WKWebView,
            runOpenPanelWith parameters: WKOpenPanelParameters,
            initiatedByFrame frame: WKFrameInfo,
            completionHandler: @escaping ([URL]?) -> Void
        ) {
            // Allow gateway main frame and gateway subframes; reject any untrusted origin.
            guard let gateway = loadedGateway ?? parent.coordinator.localGatewayURL,
                  let expectedPort = gateway.port,
                  frame.securityOrigin.`protocol`.lowercased() == (gateway.scheme?.lowercased() ?? "http"),
                  frame.securityOrigin.host.lowercased() == (gateway.host?.lowercased() ?? "127.0.0.1"),
                  frame.securityOrigin.port == expectedPort else {
                completionHandler(nil)
                return
            }

            #if DEBUG
            if let injected = Self.debugNextOpenPanelURLs {
                Self.debugNextOpenPanelURLs = nil
                completionHandler(injected)
                return
            }
            #endif

            let panel = NSOpenPanel()
            panel.canChooseFiles = true
            panel.canChooseDirectories = parameters.allowsDirectories
            panel.allowsMultipleSelection = parameters.allowsMultipleSelection
            panel.canCreateDirectories = false

            if let window = webView.window {
                panel.beginSheetModal(for: window) { response in
                    completionHandler(response == .OK ? panel.urls : nil)
                }
            } else {
                panel.begin { response in
                    completionHandler(response == .OK ? panel.urls : nil)
                }
            }
        }
        #endif

        @MainActor
        public func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            // The blank document that stands in between two Hubs' pages is not the page.
            if let blankNavigation, navigation === blankNavigation { return }
            guard webView.url?.scheme?.lowercased() != "about" else { return }
            isPageLoaded = true
            parent.applyPendingHashRouteIfNeeded(in: webView)
        }

        @MainActor
        private func startAuthenticatedDownload(url: URL, in webView: WKWebView) {
            webView.startDownload(using: URLRequest(url: url)) { [weak self] download in
                Task { @MainActor [weak self] in
                    download.delegate = self
                }
            }
        }

        // MARK: - WKDownloadDelegate

        @MainActor
        public func download(
            _ download: WKDownload,
            decideDestinationUsing response: URLResponse,
            suggestedFilename: String,
            completionHandler: @escaping (URL?) -> Void
        ) {
            #if os(macOS)
            let baseDir: URL
            #if DEBUG
            if let overrideDir = ProcessInfo.processInfo.environment["STEVE_CLIENT_DOWNLOADS_DIR"],
               !overrideDir.isEmpty {
                let custom = URL(fileURLWithPath: overrideDir, isDirectory: true)
                try? FileManager.default.createDirectory(at: custom, withIntermediateDirectories: true)
                baseDir = custom
            } else {
                baseDir = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
                    ?? FileManager.default.temporaryDirectory
            }
            #else
            baseDir = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
                ?? FileManager.default.temporaryDirectory
            #endif
            try? FileManager.default.createDirectory(at: baseDir, withIntermediateDirectories: true)
            let dest = SteveWebView.uniqueDownloadDestination(in: baseDir, suggestedFilename: suggestedFilename)
            downloadDestinations[ObjectIdentifier(download)] = dest
            completionHandler(dest)
            #else
            let tempFolder = FileManager.default.temporaryDirectory
                .appendingPathComponent("SteveDownloads", isDirectory: true)
                .appendingPathComponent(UUID().uuidString, isDirectory: true)
            do {
                try FileManager.default.createDirectory(at: tempFolder, withIntermediateDirectories: true)
                let dest = SteveWebView.uniqueDownloadDestination(in: tempFolder, suggestedFilename: suggestedFilename)
                downloadDestinations[ObjectIdentifier(download)] = dest
                completionHandler(dest)
            } catch {
                completionHandler(nil)
            }
            #endif
        }

        @MainActor
        public func downloadDidFinish(_ download: WKDownload) {
            guard let fileURL = downloadDestinations.removeValue(forKey: ObjectIdentifier(download)) else {
                return
            }
            #if os(macOS)
            DistributedNotificationCenter.default().post(
                name: NSNotification.Name("com.apple.DownloadFileFinished"),
                object: fileURL.path
            )
            #elseif canImport(UIKit)
            presentIOSShareSheet(for: fileURL)
            #endif
        }

        @MainActor
        public func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
            if let fileURL = downloadDestinations.removeValue(forKey: ObjectIdentifier(download)) {
                #if !os(macOS)
                try? FileManager.default.removeItem(at: fileURL.deletingLastPathComponent())
                #else
                _ = fileURL
                #endif
            }
        }

        #if canImport(UIKit) && !os(macOS)
        @MainActor
        private func presentIOSShareSheet(for fileURL: URL) {
            guard let scene = UIApplication.shared.connectedScenes
                .compactMap({ $0 as? UIWindowScene })
                .first(where: { $0.activationState == .foregroundActive })
                ?? UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }).first,
                  let rootVC = scene.windows.first(where: \.isKeyWindow)?.rootViewController
                    ?? scene.windows.first?.rootViewController else {
                try? FileManager.default.removeItem(at: fileURL.deletingLastPathComponent())
                return
            }
            var presenter = rootVC
            while let presented = presenter.presentedViewController {
                presenter = presented
            }
            let activity = UIActivityViewController(activityItems: [fileURL], applicationActivities: nil)
            if let popover = activity.popoverPresentationController {
                popover.sourceView = presenter.view
                popover.sourceRect = CGRect(
                    x: presenter.view.bounds.midX,
                    y: presenter.view.bounds.midY,
                    width: 1,
                    height: 1
                )
                popover.permittedArrowDirections = []
            }
            activity.completionWithItemsHandler = { _, _, _, _ in
                try? FileManager.default.removeItem(at: fileURL.deletingLastPathComponent())
            }
            presenter.present(activity, animated: true)
        }
        #endif

        @MainActor
        private func openInSystemBrowser(_ url: URL, gateway: URL?) {
            let activeHub = parent.coordinator.summary.activeEndpoint
                ?? parent.coordinator.hubStore.currentProfile?.orderedEndpoints.first
            let target = SteveWebView.rewriteExternalGatewayURL(url, gateway: gateway, activeHubEndpoint: activeHub)
            #if canImport(AppKit)
            NSWorkspace.shared.open(target)
            #elseif canImport(UIKit)
            UIApplication.shared.open(target)
            #endif
        }
    }
}

#if os(macOS)
public typealias NSViewRepresentableFallback = NSViewRepresentable
#else
public typealias NSViewRepresentableFallback = UIViewRepresentable
#endif
