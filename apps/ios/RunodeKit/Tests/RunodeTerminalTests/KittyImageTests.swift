import CoreGraphics
import Foundation
import RunodeProtocol
import Testing

@testable import RunodeTerminal

/// 程序用 Kitty 图形协议放的图片进帧、按 id 和版本读得出像素，经真的 libghostty-vt。
@Suite struct KittyImageTests {
    let size = GridSize(cols: 20, rows: 5, cellWidthPx: 8, cellHeightPx: 16)

    /// 2×2 像素的 RGBA：红、绿、蓝、白。
    static let pixels: [UInt8] = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]

    func frame(after bytes: String) throws -> (VTerminal, ScreenFrame) {
        let vt = try VTerminal(size: size, settings: .default)
        vt.feed(Array(bytes.utf8))
        var frame = ScreenFrame()
        _ = vt.refresh(&frame)
        return (vt, frame)
    }

    func rgba(_ image: CGImage?) -> [UInt8]? {
        (image?.dataProvider?.data as Data?).map { Array($0) }
    }

    @Test func aPlacedImageIsInTheFrame() throws {
        let (vt, frame) = try frame(
            after: "ab\u{1b}_Ga=T,f=32,i=1,s=2,v=2,c=2,r=1,q=2;/wAA/wD/AP8AAP///////w==\u{1b}\\")
        let image = try #require(frame.images.first)
        #expect(frame.images.count == 1)
        #expect((image.x, image.y, image.width, image.height) == (2, 0, 2, 1))
        #expect((image.sourceWidth, image.sourceHeight, image.layer) == (2, 2, .aboveText))
        #expect(rgba(vt.image(id: image.id, generation: image.generation)) == Self.pixels)
        #expect(vt.image(id: image.id, generation: image.generation + 1) == nil)
    }

    /// PNG 由 ImageIO 解码，透明度不预乘。
    @Test func pngImagesAreDecoded() throws {
        // 1×1、红色、alpha 128 的 PNG。
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DQAAAEgQGALFXOsAAAAABJRU5ErkJggg=="
        let (vt, frame) = try frame(after: "\u{1b}_Ga=T,f=100,i=1,q=2;\(png)\u{1b}\\")
        let image = try #require(frame.images.first)
        let decoded = try #require(rgba(vt.image(id: image.id, generation: image.generation)))
        #expect(decoded == [255, 0, 0, 128])
    }

    /// 从文件读的图片宿主不收，这份也不收，两边才对得上。
    @Test func imagesFromFilesAreRefused() throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "runode-kitty-\(UUID()).rgba")
        try Data(Self.pixels).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let path = Data(url.path.utf8).base64EncodedString()
        let (_, frame) = try frame(after: "\u{1b}_Ga=T,t=f,f=32,i=1,s=2,v=2,q=2;\(path)\u{1b}\\")
        #expect(frame.images.isEmpty)
    }

    @Test func aPlayingAnimationSchedulesTheNextFrame() throws {
        let (vt, frame) = try frame(
            after: "\u{1b}_Ga=T,f=32,i=1,s=1,v=1,q=2;/wAA/w==\u{1b}\\"
                + "\u{1b}_Ga=f,f=32,i=1,s=1,v=1,z=60,q=2;AAD//w==\u{1b}\\"
                + "\u{1b}_Ga=a,i=1,r=1,z=25,q=2\u{1b}\\\u{1b}_Ga=a,i=1,s=3,q=2\u{1b}\\")
        let next = try #require(frame.nextImageFrame)
        #expect(next <= .milliseconds(25))
        vt.feed(Array("\u{1b}_Ga=a,i=1,s=1,q=2\u{1b}\\".utf8))
        var stopped = frame
        _ = vt.refresh(&stopped)
        #expect(stopped.nextImageFrame == nil)
    }
}
