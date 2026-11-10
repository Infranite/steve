import SwiftUI
import WebKit

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
            webView.load(URLRequest(url: gateway))
            return
        }

        if let hash = coordinator.targetHashRoute {
            coordinator.consumeTargetHashRoute()
            let escaped = hash.replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "'", with: "\\'")
            webView.evaluateJavaScript("window.location.hash = '\(escaped)';", completionHandler: nil)
        }
    }

    @MainActor
    private static func makeBootstrapScript(platform: String, coordinator: SteveClientCoordinator) -> String {
        let activeJSON = encodeActiveHubJSON(coordinator: coordinator)
        return """
        (function() {
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
        if (window.__STEVE_NATIVE__) {
          window.__STEVE_NATIVE__.platform = '\(platform)';
          window.__STEVE_NATIVE__.activeHub = \(activeJSON);
        }
        """
    }

    @MainActor
    private static func encodeActiveHubJSON(coordinator: SteveClientCoordinator) -> String {
        let profile = coordinator.hubStore.activeProfile
        let endpoint = coordinator.summary.activeEndpoint.map(URLNormalizer.canonicalOrigin) ?? profile.primaryDisplayAddress
        let dict: [String: String] = [
            "id": profile.id,
            "name": profile.name,
            "endpoint": endpoint,
            "route": coordinator.summary.route.rawValue,
        ]
        guard let data = try? JSONEncoder().encode(dict),
              let str = String(data: data, encoding: .utf8) else {
            return "{}"
        }
        return str
    }

    public final class BridgeCoordinator: NSObject, WKScriptMessageHandler, WKNavigationDelegate {
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
                  let body = message.body as? [String: Any],
                  let action = body["action"] as? String else {
                return
            }
            Task { @MainActor in
                switch action {
                case "openHubSwitcher":
                    parent.coordinator.isHubSwitcherPresented = true
                case "openPairSheet":
                    parent.coordinator.isPairQRPresented = true
                case "openQRScanner":
                    parent.coordinator.isQRScannerPresented = true
                default:
                    break
                }
            }
        }

        public func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            if let url = navigationAction.request.url,
               url.scheme?.lowercased() == "steve" {
                Task { @MainActor in
                    _ = try? await parent.coordinator.pairWithQRCode(uri: url.absoluteString)
                }
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }
    }
}

#if os(macOS)
public typealias NSViewRepresentableFallback = NSViewRepresentable
#else
public typealias NSViewRepresentableFallback = UIViewRepresentable
#endif
