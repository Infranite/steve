import SwiftUI
import WebKit

#if canImport(AppKit)
import AppKit
#elseif canImport(UIKit)
import UIKit
#endif

public struct SteveWebView: NSViewRepresentableFallback {
    @ObservedObject private var coordinator: SteveClientCoordinator
    private let platformName: String

    public init(coordinator: SteveClientCoordinator, platformName: String) {
        self.coordinator = coordinator
        self.platformName = platformName
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
    private func buildWebView(bridge: BridgeCoordinator) -> WKWebView {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .default()
        config.preferences.javaScriptCanOpenWindowsAutomatically = false

        let userContent = WKUserContentController()
        userContent.add(bridge, name: BridgeCoordinator.handlerName)
        let bootstrapJS = Self.makeBootstrapScript(platform: platformName, coordinator: coordinator)
        userContent.addUserScript(
            WKUserScript(
                source: bootstrapJS,
                injectionTime: .atDocumentStart,
                forMainFrameOnly: true
            )
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
        guard let gateway = coordinator.localGatewayURL else { return }

        // Keep dynamic bridge state synchronized whenever active Hub or route changes
        let syncJS = Self.makeActiveHubSyncScript(platform: platformName, coordinator: coordinator)
        webView.evaluateJavaScript(syncJS, completionHandler: nil)

        if bridge.loadedNonce != coordinator.webReloadNonce || bridge.loadedGateway != gateway {
            bridge.loadedNonce = coordinator.webReloadNonce
            bridge.loadedGateway = gateway
            let token = coordinator.proxy.proxyAuthToken
            Self.plantProxyCookie(in: webView, gateway: gateway, token: token) { [weak webView] in
                webView?.load(URLRequest(url: gateway))
            }
            return
        }

        applyPendingHashRouteIfNeeded(in: webView)
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

    @MainActor
    private static func plantProxyCookie(
        in webView: WKWebView,
        gateway: URL,
        token: String,
        completion: @escaping @MainActor () -> Void
    ) {
        let props: [HTTPCookiePropertyKey: Any] = [
            .name: HubLoopbackProxy.proxyAuthCookieName,
            .value: token,
            .originURL: gateway,
            .domain: gateway.host ?? "127.0.0.1",
            .path: "/",
            HTTPCookiePropertyKey("HttpOnly"): "TRUE",
            .sameSitePolicy: HTTPCookieStringPolicy.sameSiteLax,
        ]
        guard let cookie = HTTPCookie(properties: props) else {
            completion()
            return
        }
        webView.configuration.websiteDataStore.httpCookieStore.setCookie(cookie) {
            Task { @MainActor in
                completion()
            }
        }
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

    @MainActor
    private static func makeBootstrapScript(platform: String, coordinator: SteveClientCoordinator) -> String {
        let activeJSON = encodeActiveHubJSON(coordinator: coordinator)
        return """
        (function() {
          if (window.location.hostname !== '' && window.location.hostname !== '127.0.0.1') { return; }
          try { localStorage.removeItem('steve.hub'); } catch (_) {}
          const post = (action) => {
            try { window.webkit.messageHandlers.\(BridgeCoordinator.handlerName).postMessage({ action }); } catch (_) {}
          };
          window.__STEVE_NATIVE__ = {
            platform: '\(platform)',
            activeHub: \(activeJSON),
            openHubSwitcher: () => post('openHubSwitcher'),
            openPairSheet: () => post('openPairSheet'),
            openQRScanner: \(platform == "ios" ? "() => post('openQRScanner')" : "undefined")
          };
        })();
        """
    }

    @MainActor
    private static func makeActiveHubSyncScript(platform: String, coordinator: SteveClientCoordinator) -> String {
        let activeJSON = encodeActiveHubJSON(coordinator: coordinator)
        return """
        if (window.__STEVE_NATIVE__ && window.location.hostname === '127.0.0.1') {
          window.__STEVE_NATIVE__.platform = '\(platform)';
          window.__STEVE_NATIVE__.activeHub = \(activeJSON);
        }
        """
    }

    @MainActor
    private static func encodeActiveHubJSON(coordinator: SteveClientCoordinator) -> String {
        guard let profile = coordinator.hubStore.currentProfile else {
            return "null"
        }
        let endpoint = coordinator.summary.activeEndpoint.map(URLNormalizer.canonicalOrigin) ?? profile.primaryDisplayAddress
        let dict: [String: String] = [
            "id": profile.id,
            "name": profile.name,
            "endpoint": endpoint,
            "route": coordinator.summary.route.rawValue,
        ]
        guard let data = try? JSONEncoder().encode(dict),
              let str = String(data: data, encoding: .utf8) else {
            return "null"
        }
        return str
    }

    public final class BridgeCoordinator: NSObject, WKScriptMessageHandler, WKNavigationDelegate, WKUIDelegate {
        public static let handlerName = "steveNative"

        var parent: SteveWebView
        weak var webView: WKWebView?
        var loadedNonce: UUID?
        var loadedGateway: URL?

        init(parent: SteveWebView) {
            self.parent = parent
        }

        func attach(_ webView: WKWebView) {
            self.webView = webView
        }

        public func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
            guard message.name == Self.handlerName,
                  message.frameInfo.isMainFrame,
                  let body = message.body as? [String: Any],
                  let action = body["action"] as? String else {
                return
            }
            let originProto = message.frameInfo.securityOrigin.`protocol`.lowercased()
            let originHost = message.frameInfo.securityOrigin.host.lowercased()
            let originPort = message.frameInfo.securityOrigin.port

            Task { @MainActor in
                guard let gateway = self.loadedGateway ?? self.parent.coordinator.localGatewayURL,
                      let expectedPort = gateway.port,
                      originProto == (gateway.scheme?.lowercased() ?? "http"),
                      originHost == (gateway.host?.lowercased() ?? "127.0.0.1"),
                      originPort == expectedPort else {
                    return
                }
                switch action {
                case "openHubSwitcher":
                    self.parent.coordinator.isHubSwitcherPresented = true
                case "openPairSheet":
                    self.parent.coordinator.isPairQRPresented = true
                case "openQRScanner":
                    self.parent.coordinator.isQRScannerPresented = true
                default:
                    break
                }
            }
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

            let scheme = url.scheme?.lowercased() ?? ""

            // Never initiate pairing from web content; custom `steve:` links in web pages are ignored.
            if scheme == "steve" {
                decisionHandler(.cancel)
                return
            }

            // Allow `about:blank` / `about:srcdoc` (used by sandboxed markdown/mail HTML preview iframes).
            if scheme == "about" {
                decisionHandler(.allow)
                return
            }

            let gateway = loadedGateway ?? parent.coordinator.localGatewayURL

            // `target="_blank"` links have `targetFrame == nil` — open http(s) targets in the system browser.
            if navigationAction.targetFrame == nil {
                if scheme == "http" || scheme == "https" {
                    openInSystemBrowser(url, gateway: gateway)
                }
                decisionHandler(.cancel)
                return
            }

            // Confine in-webview http(s) navigations strictly to the loopback gateway origin.
            if let gateway, SteveWebView.isGatewayOrigin(url, gateway: gateway) {
                decisionHandler(.allow)
                return
            }

            if scheme == "http" || scheme == "https" {
                openInSystemBrowser(url, gateway: gateway)
                decisionHandler(.cancel)
                return
            }

            if (scheme == "blob" || scheme == "data") && navigationAction.targetFrame?.isMainFrame == false {
                decisionHandler(.allow)
                return
            }

            decisionHandler(.cancel)
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
               scheme == "http" || scheme == "https" {
                let gateway = loadedGateway ?? parent.coordinator.localGatewayURL
                openInSystemBrowser(url, gateway: gateway)
            }
            return nil
        }

        @MainActor
        public func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            parent.applyPendingHashRouteIfNeeded(in: webView)
        }

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
