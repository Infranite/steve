import AVFoundation
import SteveClientKit
import SwiftUI
import UIKit

struct QRCodeScannerSheet: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @Environment(\.dismiss) private var dismiss

    @State private var pastedURI = ""
    @State private var isPairing = false
    @State private var errorMessage: String?
    @State private var cameraPermissionDenied = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                ZStack {
                    if cameraPermissionDenied {
                        VStack(spacing: 12) {
                            Image(systemName: "camera.metering.unknown")
                                .font(.largeTitle)
                                .foregroundStyle(.secondary)
                            Text("未授予相机权限，可在系统设置中开启，或在下方粘贴配对链接。")
                                .font(.callout)
                                .foregroundStyle(.secondary)
                                .multilineTextAlignment(.center)
                                .padding(.horizontal, 24)
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 20))
                    } else {
                        CameraQRPreview(
                            onCodeScanned: { code in
                                guard !isPairing else { return }
                                Task { await handleScanned(code) }
                            },
                            onPermissionDenied: {
                                cameraPermissionDenied = true
                            }
                        )
                        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                        .overlay(
                            RoundedRectangle(cornerRadius: 20, style: .continuous)
                                .strokeBorder(Color.white.opacity(0.35), lineWidth: 1.5)
                        )
                        .overlay {
                            RoundedRectangle(cornerRadius: 18, style: .continuous)
                                .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2.5, dash: [18, 10]))
                                .frame(width: 220, height: 220)
                        }
                    }

                    if isPairing {
                        ZStack {
                            Color.black.opacity(0.45)
                                .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
                            VStack(spacing: 10) {
                                ProgressView()
                                    .tint(.white)
                                Text("正在配对并连接 Hub…")
                                    .font(.callout.weight(.medium))
                                    .foregroundStyle(.white)
                            }
                        }
                    }
                }
                .frame(maxHeight: 380)

                Text("将取景框对准 Mac 或网页端「设置 · 访问与客户端」中的配对二维码。有公网地址时默认走公网连接，否则自动走局域网连接。")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)

                if let errorMessage {
                    Text(errorMessage)
                        .font(.footnote)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                }

                Divider()

                VStack(alignment: .leading, spacing: 8) {
                    Text("或粘贴配对链接（steve://pair?…）")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    HStack(spacing: 8) {
                        TextField("steve://pair?v=1&code=stp_…", text: $pastedURI)
                            .textFieldStyle(.roundedBorder)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        Button("配对") {
                            Task { await handleScanned(pastedURI) }
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(isPairing || pastedURI.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }

                Spacer(minLength: 0)
            }
            .padding(20)
            .navigationTitle("扫码配对")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("取消") { dismiss() }
                }
            }
        }
    }

    private func handleScanned(_ raw: String) async {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard PairingPayload.parse(trimmed) != nil else {
            if raw == pastedURI {
                errorMessage = "配对链接格式无效，请扫描或粘贴有效的 steve://pair 二维码"
            }
            return
        }
        isPairing = true
        errorMessage = nil
        do {
            _ = try await coordinator.pairWithQRCode(uri: trimmed)
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
            UINotificationFeedbackGenerator().notificationOccurred(.error)
        }
        isPairing = false
    }
}

private struct CameraQRPreview: UIViewControllerRepresentable {
    var onCodeScanned: (String) -> Void
    var onPermissionDenied: () -> Void

    func makeUIViewController(context: Context) -> ScannerViewController {
        let vc = ScannerViewController()
        vc.onCodeScanned = onCodeScanned
        vc.onPermissionDenied = onPermissionDenied
        return vc
    }

    func updateUIViewController(_ uiViewController: ScannerViewController, context: Context) {
        uiViewController.onCodeScanned = onCodeScanned
        uiViewController.onPermissionDenied = onPermissionDenied
    }
}

private final class ScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCodeScanned: ((String) -> Void)?
    var onPermissionDenied: (() -> Void)?

    private let captureSession = AVCaptureSession()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private var lastDeliveredCode: String?
    private var lastDeliveredAt: Date = .distantPast

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configureCameraIfAuthorized()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if captureSession.isRunning {
            captureSession.stopRunning()
        }
    }

    private func configureCameraIfAuthorized() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            setupCaptureSession()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    if granted {
                        self?.setupCaptureSession()
                    } else {
                        self?.onPermissionDenied?()
                    }
                }
            }
        default:
            onPermissionDenied?()
        }
    }

    private func setupCaptureSession() {
        guard let videoDevice = AVCaptureDevice.default(for: .video),
              let videoInput = try? AVCaptureDeviceInput(device: videoDevice),
              captureSession.canAddInput(videoInput) else {
            return
        }
        captureSession.addInput(videoInput)

        let metadataOutput = AVCaptureMetadataOutput()
        guard captureSession.canAddOutput(metadataOutput) else { return }
        captureSession.addOutput(metadataOutput)
        metadataOutput.setMetadataObjectsDelegate(self, queue: .main)
        if metadataOutput.availableMetadataObjectTypes.contains(.qr) {
            metadataOutput.metadataObjectTypes = [.qr]
        }

        let layer = AVCaptureVideoPreviewLayer(session: captureSession)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        self.previewLayer = layer

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            self?.captureSession.startRunning()
        }
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard let readable = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              readable.type == .qr,
              let value = readable.stringValue else {
            return
        }
        let now = Date()
        if value == lastDeliveredCode, now.timeIntervalSince(lastDeliveredAt) < 2.5 {
            return
        }
        lastDeliveredCode = value
        lastDeliveredAt = now
        onCodeScanned?(value)
    }
}
