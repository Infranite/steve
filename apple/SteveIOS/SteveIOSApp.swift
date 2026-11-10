import SteveClientKit
import SwiftUI
import UIKit

@main
struct SteveIOSApp: App {
    @Environment(\.scenePhase) private var scenePhase
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
        let hubStore = HubStore(includeLocalDefault: false)
        _coordinator = StateObject(
            wrappedValue: SteveClientCoordinator(
                hubStore: hubStore,
                webRoot: webRoot,
                clientDescriptor: descriptor
            )
        )
    }

    var body: some Scene {
        WindowGroup {
            IOSRootView(coordinator: coordinator)
                .onAppear {
                    coordinator.startIfNeeded()
                }
                .onChange(of: scenePhase) { _, newPhase in
                    if newPhase == .active {
                        coordinator.handleAppDidBecomeActive()
                    }
                }
                .onOpenURL { url in
                    if url.scheme?.lowercased() == "steve" {
                        coordinator.requestExternalPairingConfirmation(uri: url.absoluteString)
                    }
                }
        }
    }
}
