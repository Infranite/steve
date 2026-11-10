import SteveClientKit
import SwiftUI
import UIKit

@main
struct SteveIOSApp: App {
    @StateObject private var coordinator: SteveClientCoordinator

    init() {
        let deviceName = UIDevice.current.name
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0"
        let webRoot = Bundle.main.resourceURL?.appendingPathComponent("WebDist", isDirectory: true)
        let descriptor = ClientDescriptor(
            kind: "ios",
            name: deviceName,
            platform: "iOS",
            version: version
        )
        _coordinator = StateObject(wrappedValue: SteveClientCoordinator(webRoot: webRoot, clientDescriptor: descriptor))
    }

    var body: some Scene {
        WindowGroup {
            IOSRootView(coordinator: coordinator)
                .onAppear {
                    coordinator.startIfNeeded()
                }
                .onOpenURL { url in
                    if url.scheme?.lowercased() == "steve" {
                        Task {
                            _ = try? await coordinator.pairWithQRCode(uri: url.absoluteString)
                        }
                    }
                }
        }
    }
}
