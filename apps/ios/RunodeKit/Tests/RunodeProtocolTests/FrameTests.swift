import Foundation
import Testing

@testable import RunodeProtocol

@Suite struct FrameTests {
    @Test func encodesTheFrozenHeader() throws {
        let frame = Frame(kind: .input, channel: 0x0102_0304, payload: Data([0xAA, 0xBB]))
        let bytes = [UInt8](try frame.encoded())
        // u32 LE 长度 | u8 类型 | u32 LE 通道 | 载荷
        #expect(bytes == [2, 0, 0, 0, 1, 4, 3, 2, 1, 0xAA, 0xBB])
    }

    @Test func roundTripsSeveralFramesInOneChunk() throws {
        let frames = [
            Frame(kind: .control, channel: 0, payload: Data("{\"type\":\"list_sessions\"}".utf8)),
            Frame(kind: .output, channel: 7, payload: Data(repeating: 0x41, count: 1000)),
            Frame(kind: .snapshot, channel: 7, payload: Data()),
        ]
        var decoder = FrameDecoder()
        decoder.append(try frames.reduce(into: Data()) { $0.append(try $1.encoded()) })
        var decoded: [Frame] = []
        while let frame = try decoder.next() {
            decoded.append(frame)
        }
        #expect(decoded == frames)
        try decoder.finish()
    }

    @Test func waitsForTheRestOfAHalfFrame() throws {
        let bytes = try Frame(kind: .output, channel: 3, payload: Data("hello".utf8)).encoded()
        var decoder = FrameDecoder()
        // 帧头只到了一半。
        decoder.append(bytes.prefix(4))
        #expect(try decoder.next() == nil)
        // 帧头齐了，载荷还差。
        decoder.append(bytes[4..<11])
        #expect(try decoder.next() == nil)
        decoder.append(bytes[11...])
        #expect(try decoder.next()?.payload == Data("hello".utf8))
        #expect(try decoder.next() == nil)
    }

    @Test func aConnectionClosedMidFrameIsTruncated() throws {
        let bytes = try Frame(kind: .output, channel: 3, payload: Data("hello".utf8)).encoded()
        var decoder = FrameDecoder()
        decoder.append(bytes.prefix(12))
        #expect(try decoder.next() == nil)
        #expect(throws: FrameError.truncated) { try decoder.finish() }
    }

    @Test func rejectsOverlongFramesBeforeTheirPayloadArrives() {
        var header = Data()
        withUnsafeBytes(of: (Frame.maxPayload + 1).littleEndian) { header.append(contentsOf: $0) }
        header.append(0)
        header.append(contentsOf: [0, 0, 0, 0])
        var decoder = FrameDecoder()
        decoder.append(header)
        #expect(throws: FrameError.tooLong(UInt64(Frame.maxPayload) + 1)) { try decoder.next() }
    }

    @Test func gatePhaseHasA16KiBLimit() throws {
        let frame = Frame(kind: .control, channel: 0, payload: Data(repeating: 0x20, count: 16 * 1024 + 1))
        var decoder = FrameDecoder(maxPayload: Frame.gateMaxPayload)
        decoder.append(try frame.encoded())
        #expect(throws: FrameError.tooLong(16 * 1024 + 1)) { try decoder.next() }
    }

    @Test func rejectsUnknownKinds() {
        var decoder = FrameDecoder()
        decoder.append(Data([0, 0, 0, 0, 9, 0, 0, 0, 0]))
        #expect(throws: FrameError.unknownKind(9)) { try decoder.next() }
    }

    @Test func refusesToEncodeOverlongPayloads() {
        let frame = Frame(kind: .output, channel: 1, payload: Data(count: Int(Frame.maxPayload) + 1))
        #expect(throws: FrameError.tooLong(UInt64(Frame.maxPayload) + 1)) { try frame.encoded() }
    }
}
