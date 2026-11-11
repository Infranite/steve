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
        let coord = SteveClientCoordinator(
            hubStore: hubStore,
            webRoot: webRoot,
            clientDescriptor: descriptor
        )
        coord.pairConfirmationVisibilityProbe = { IOSPairConfirmation.isAlertOnScreen() }
        _coordinator = StateObject(wrappedValue: coord)
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

/// The pairing confirmation is an alert over the root view.
enum IOSPairConfirmation {
    /// Is the confirmation alert on screen: an alert controller with the confirmation's title that is
    /// in a window and not on its way out.
    @MainActor
    static func isAlertOnScreen() -> Bool {
        let title = SteveClientCoordinator.pairConfirmationTitle
        for scene in UIApplication.shared.connectedScenes {
            guard let windowScene = scene as? UIWindowScene else { continue }
            for window in windowScene.windows {
                var controller = window.rootViewController
                while let current = controller {
                    if let alert = current as? UIAlertController,
                       alert.title == title,
                       !alert.isBeingDismissed,
                       alert.viewIfLoaded?.window != nil {
                        return true
                    }
                    controller = current.presentedViewController
                }
            }
        }
        return false
    }
}
