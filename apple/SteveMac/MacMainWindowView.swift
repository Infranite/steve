import SteveClientKit
import SwiftUI

struct MacMainWindowView: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @State private var showAttention = false
    @State private var showConnection = false

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
    }

    var body: some View {
        Group {
            if coordinator.localGatewayURL != nil {
                SteveWebView(coordinator: coordinator, platformName: "macos")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
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
                        .frame(maxWidth: 420)
                    Button(L10n.tr("重试", "Retry")) {
                        coordinator.retryGatewayStartup()
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding(24)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView(L10n.tr("正在启动 Steve 客户端…", "Starting Steve client…"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationTitle(windowTitle)
        .toolbar {
            ToolbarItem(placement: .navigation) { MacHubMenu(coordinator: coordinator) }
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(MacShellContext.title(coordinator)).font(.headline).lineLimit(1)
                    Text(MacShellContext.workspace(coordinator)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                .accessibilityElement(children: .combine)
            }
            ToolbarItemGroup(placement: .primaryAction) {
                Button { showAttention.toggle() } label: {
                    Label(L10n.tr("提醒", "Attention"), systemImage: "bell")
                }
                .help(L10n.tr("提醒来源与入口（提醒尚不可用）", "Attention sources (reminders not yet available)"))
                .popover(isPresented: $showAttention) {
                    MacAttentionView(coordinator: coordinator) { route in
                        showAttention = false
                        coordinator.navigate(hashRoute: route)
                    }.padding(16).frame(width: 330)
                }
                Button { showConnection.toggle() } label: { MacConnectionLabel(phase: coordinator.summary.phase) }
                    .popover(isPresented: $showConnection) { MacConnectionDetails(coordinator: coordinator).padding(16).frame(width: 360) }
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            if let error = coordinator.shellNavigationError {
                HStack {
                    Label(error, systemImage: "exclamationmark.triangle").font(.callout)
                    Spacer()
                    Button(L10n.tr("关闭", "Dismiss")) { coordinator.dismissShellNavigationError() }
                }.padding(10).background(.regularMaterial)
            }
        }
        .alert(
            SteveClientCoordinator.pairConfirmationTitle,
            isPresented: Binding(
                get: {
                    coordinator.isPairConfirmationPresented && coordinator.pendingExternalPairConfirmation != nil
                },
                set: { presented in
                    // Hides only. The buttons answer by id, so the order in which SwiftUI calls
                    // this setter and a button's action does not matter.
                    if !presented {
                        coordinator.hidePairConfirmationAlert()
                    }
                }
            ),
            presenting: coordinator.pendingExternalPairConfirmation
        ) { pending in
            Button(L10n.tr("取消", "Cancel"), role: .cancel) {
                coordinator.cancelPendingExternalPairing(id: pending.id)
            }
            Button(L10n.tr("确认配对", "Pair")) {
                coordinator.beginConfirmExternalPairing(id: pending.id)
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
                get: {
                    !coordinator.isPairConfirmationPresented
                        && coordinator.externalPairErrorMessage != nil
                },
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

    private var windowTitle: String {
        MacShellContext.title(coordinator)
    }
}
