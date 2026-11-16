import Foundation
import ImageIO
import RunodeProtocol
import UniformTypeIdentifiers

/// 发给电脑之前把图片重新编码一遍：按 EXIF 的方向转正，长边不超过 `maxPixelSize`，写出的文件不带原图的
/// 元数据（拍摄地点、设备）。PNG 还是 PNG（截图里的字不糊），太大时和别的格式一样写成 JPEG；GIF 不超过
/// `maxMediaBytes` 时原样发，动图才不变成一帧。
public enum ImagePreparation {
    /// 写出的图片长边最多这么多像素。
    static let maxPixelSize = 2048
    /// PNG 重新编码后超过这么多字节时改写成 JPEG。
    static let pngLimit = 5 << 20
    static let jpegQuality = 0.85

    /// 读不出图片的数据报 `ImagePreparationError`。
    public static func prepare(_ data: Data) throws(ImagePreparationError) -> Data {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil), CGImageSourceGetCount(source) > 0 else {
            throw .unreadable
        }
        let type = CGImageSourceGetType(source) as String?
        if type == UTType.gif.identifier, data.count <= maxMediaBytes {
            return data
        }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
            kCGImageSourceShouldCacheImmediately: true,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            throw .unreadable
        }
        if type == UTType.png.identifier, let png = encode(image, as: .png), png.count <= pngLimit {
            return png
        }
        guard let jpeg = encode(image, as: .jpeg, properties: [kCGImageDestinationLossyCompressionQuality: jpegQuality])
        else { throw .unreadable }
        return jpeg
    }

    /// 只写像素，不带任何来自原图的属性。
    private static func encode(_ image: CGImage, as type: UTType, properties: [CFString: Any] = [:]) -> Data? {
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output, type.identifier as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return output as Data
    }
}

public enum ImagePreparationError: LocalizedError, Hashable, Sendable {
    /// 数据不是认得的图片格式，或者解不开。
    case unreadable

    public var errorDescription: String? { String(localized: "读不出这张图片") }
}
