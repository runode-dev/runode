import AVFoundation
import Foundation
import RunodeProtocol

/// 发给电脑之前把视频压一遍：缩到 1280×720 以内（`AVAssetExportPreset1280x720`），写成文件头在前、边收边能
/// 读的 MP4，不带拍摄地点、设备这些元数据。
public enum VideoPreparation {
    /// 把 `source` 压好，写到 `directory` 里一个新的 `.mp4` 文件，返回它，用完由调用方删掉。读不出、压不了
    /// 或压完仍超过 `maxMediaBytes` 时报 `VideoPreparationError`，不留下输出文件。
    public static func prepare(
        _ source: URL, in directory: URL = FileManager.default.temporaryDirectory
    ) async throws -> URL {
        let asset = AVURLAsset(url: source)
        guard let session = AVAssetExportSession(asset: asset, presetName: AVAssetExportPreset1280x720) else {
            throw VideoPreparationError.exportFailed(String(localized: "这个视频不能压成 720p"))
        }
        session.shouldOptimizeForNetworkUse = true
        session.metadataItemFilter = .forSharing()
        let output = directory.appending(path: "\(UUID().uuidString).mp4")
        do {
            do {
                try await session.export(to: output, as: .mp4)
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                throw VideoPreparationError.exportFailed(error.localizedDescription)
            }
            let size = try output.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
            guard size <= maxMediaBytes else { throw VideoPreparationError.tooLarge }
            return output
        } catch {
            try? FileManager.default.removeItem(at: output)
            throw error
        }
    }
}

public enum VideoPreparationError: LocalizedError, Hashable, Sendable {
    /// 读不出或压不了，带系统给的原因。
    case exportFailed(String)
    /// 压完还是超过 `maxMediaBytes`。
    case tooLarge

    public var errorDescription: String? {
        switch self {
        case .exportFailed(let reason): String(localized: "视频压缩失败：\(reason)")
        case .tooLarge: String(localized: "视频太大：压缩后仍超过 \(maxMediaBytes >> 20) MB")
        }
    }
}
