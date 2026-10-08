#if os(iOS)
    import AVFoundation
    import SwiftUI
    import VisionKit

    /// 扫二维码的取景框：VisionKit 的 `DataScannerViewController` 只认二维码，包成 SwiftUI 视图。设备
    /// 不支持（模拟器）或者没给相机权限时 `onUnavailable` 报原因。
    struct QRScannerView: UIViewControllerRepresentable {
        let onCode: @MainActor (String) -> Void
        let onUnavailable: @MainActor (String) -> Void

        func makeCoordinator() -> Coordinator {
            Coordinator(self)
        }

        func makeUIViewController(context: Context) -> DataScannerViewController {
            let scanner = DataScannerViewController(
                recognizedDataTypes: [.barcode(symbologies: [.qr])], isGuidanceEnabled: false)
            scanner.delegate = context.coordinator
            context.coordinator.start(scanner)
            return scanner
        }

        func updateUIViewController(_ scanner: DataScannerViewController, context: Context) {
            context.coordinator.parent = self
        }

        static func dismantleUIViewController(_ scanner: DataScannerViewController, coordinator: Coordinator) {
            scanner.stopScanning()
        }

        @MainActor
        final class Coordinator: NSObject, DataScannerViewControllerDelegate {
            var parent: QRScannerView

            init(_ parent: QRScannerView) {
                self.parent = parent
            }

            func start(_ scanner: DataScannerViewController) {
                guard DataScannerViewController.isSupported else { return report(.unsupported) }
                Task { [weak scanner] in
                    // 头一次用时先问相机权限，问过了直接得到结果。
                    guard await AVCaptureDevice.requestAccess(for: .video) else { return report(.cameraRestricted) }
                    do {
                        try scanner?.startScanning()
                    } catch let error as DataScannerViewController.ScanningUnavailable {
                        report(error)
                    } catch {
                        report(.unsupported)
                    }
                }
            }

            func dataScanner(
                _ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]
            ) {
                for case .barcode(let barcode) in addedItems {
                    if let code = barcode.payloadStringValue { return parent.onCode(code) }
                }
            }

            func dataScanner(
                _ dataScanner: DataScannerViewController,
                becameUnavailableWithError error: DataScannerViewController.ScanningUnavailable
            ) {
                report(error)
            }

            private func report(_ reason: DataScannerViewController.ScanningUnavailable) {
                switch reason {
                case .cameraRestricted: parent.onUnavailable(String(localized: "没有相机权限，可以在设置里打开，或者粘贴配对链接"))
                case .unsupported: parent.onUnavailable(String(localized: "这台设备没有可用的摄像头，请粘贴配对链接"))
                @unknown default: parent.onUnavailable(String(localized: "相机不支持识别二维码，请粘贴配对链接"))
                }
            }
        }
    }
#endif
