import SteveClientKit
import SwiftUI

struct IOSRootView: View {
    @ObservedObject var coordinator: SteveClientCoordinator

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
    }

    var body: some View {
        Group {
            if coordinator.localGatewayURL != nil {
                SteveWebView(coordinator: coordinator, platformName: "ios")
                    .ignoresSafeArea(.container, edges: .bottom)
            } else if let startupError = coordinator.proxyStartupError {
                VStack(spacing: 14) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.largeTitle)
                        .foregroundStyle(.orange)
                    Text(L10n.tr("本地网关启动失败", "Failed to start local gateway"))
                        .font(.headline)
                    Text(startupError)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 24)
                    Button(L10n.tr("重试", "Retry")) {
                        coordinator.retryGatewayStartup()
                    }
                    .buttonStyle(.borderedProminent)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView(L10n.tr("正在启动 Steve…", "Starting Steve…"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .fullScreenCover(isPresented: $coordinator.isQRScannerPresented) {
            QRCodeScannerSheet(coordinator: coordinator)
        }
        .alert(
            L10n.tr("确认配对并连接到此 Hub？", "Pair and connect to this Hub?"),
            isPresented: Binding(
                get: { !coordinator.isQRScannerPresented && coordinator.pendingExternalPairConfirmation != nil },
                set: { if !$0 { coordinator.cancelPendingExternalPairing() } }
            ),
            presenting: coordinator.pendingExternalPairConfirmation
        ) { _ in
            Button(L10n.tr("取消", "Cancel"), role: .cancel) {
                coordinator.cancelPendingExternalPairing()
            }
            Button(L10n.tr("确认配对", "Pair")) {
                Task { await coordinator.confirmPendingExternalPairing() }
            }
        } message: { pending in
            Text(
                L10n.tr(
                    "名称：\(pending.displayName)\n目标地址：\n\(pending.endpointsSummary)",
                    "Name: \(pending.displayName)\nTarget endpoints:\n\(pending.endpointsSummary)"
                )
            )
        }
        .alert(
            L10n.tr("配对失败", "Pairing Failed"),
            isPresented: Binding(
                get: { coordinator.externalPairErrorMessage != nil },
                set: { if !$0 { coordinator.externalPairErrorMessage = nil } }
            )
        ) {
            Button(L10n.tr("好", "OK"), role: .cancel) {
                coordinator.externalPairErrorMessage = nil
            }
        } message: {
            Text(coordinator.externalPairErrorMessage ?? "")
        }
    }
}
