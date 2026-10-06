#if os(iOS)
    @preconcurrency import AVFoundation
    import SwiftUI
    import UIKit

    /// 扫二维码的取景框：AVFoundation 的相机预览加二维码识别，包成 SwiftUI 视图。没有摄像头
    /// （模拟器）或者没给权限时 `onUnavailable` 报原因。
    struct QRScannerView: UIViewRepresentable {
        let onCode: @MainActor (String) -> Void
        let onUnavailable: @MainActor (String) -> Void

        func makeUIView(context: Context) -> QRScannerUIView {
            let view = QRScannerUIView()
            view.onCode = onCode
            view.onUnavailable = onUnavailable
            view.start()
            return view
        }

        func updateUIView(_ view: QRScannerUIView, context: Context) {
            view.onCode = onCode
            view.onUnavailable = onUnavailable
        }

        static func dismantleUIView(_ view: QRScannerUIView, coordinator: ()) {
            view.stop()
        }
    }

    /// `AVCaptureSession` 的开、停会卡住调用的线程，放到它自己的队列上做；会话本身按 Apple 的说明可以
    /// 在别的线程上开停，这里只在 `queue` 上碰它。
    private final class CaptureSessionBox: @unchecked Sendable {
        let session = AVCaptureSession()
        let queue = DispatchQueue(label: "dev.runode.qr-capture")
    }

    final class QRScannerUIView: UIView, AVCaptureMetadataOutputObjectsDelegate {
        var onCode: @MainActor (String) -> Void = { _ in }
        var onUnavailable: @MainActor (String) -> Void = { _ in }
        private let box = CaptureSessionBox()
        private var previewLayer: AVCaptureVideoPreviewLayer?

        override func layoutSubviews() {
            super.layoutSubviews()
            previewLayer?.frame = bounds
        }

        func start() {
            backgroundColor = .black
            switch AVCaptureDevice.authorizationStatus(for: .video) {
            case .authorized:
                configure()
            case .notDetermined:
                Task { @MainActor [weak self] in
                    if await AVCaptureDevice.requestAccess(for: .video) {
                        self?.configure()
                    } else {
                        self?.onUnavailable("没有相机权限，可以在设置里打开，或者粘贴配对链接")
                    }
                }
            default:
                onUnavailable("没有相机权限，可以在设置里打开，或者粘贴配对链接")
            }
        }

        func stop() {
            let box = self.box
            box.queue.async { box.session.stopRunning() }
        }

        private func configure() {
            guard let device = AVCaptureDevice.default(for: .video),
                let input = try? AVCaptureDeviceInput(device: device), box.session.canAddInput(input)
            else {
                onUnavailable("这台设备没有可用的摄像头，请粘贴配对链接")
                return
            }
            box.session.addInput(input)
            let output = AVCaptureMetadataOutput()
            guard box.session.canAddOutput(output) else {
                onUnavailable("相机不支持识别二维码，请粘贴配对链接")
                return
            }
            box.session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]
            let preview = AVCaptureVideoPreviewLayer(session: box.session)
            preview.videoGravity = .resizeAspectFill
            preview.frame = bounds
            layer.addSublayer(preview)
            previewLayer = preview
            let box = self.box
            box.queue.async { box.session.startRunning() }
        }

        nonisolated func metadataOutput(
            _ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject],
            from connection: AVCaptureConnection
        ) {
            let codes = metadataObjects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
            guard let code = codes.first else { return }
            // 代理设在主队列上，这里一定在主线程。
            MainActor.assumeIsolated {
                onCode(code)
            }
        }
    }
#endif
