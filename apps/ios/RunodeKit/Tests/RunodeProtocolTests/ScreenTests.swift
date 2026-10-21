import Foundation
import RunodeProtocol
import Testing

/// 电脑那边 `runode_protocol::screen` 照规格写出的远程桌面样例，两边的测试读同一批文件。
enum ScreenSamples {
    static let directory = PushSamples.repository.appending(path: "crates/protocol/tests/fixtures/screen")

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }

    /// 十六进制写的画面载荷。
    static func hex(_ name: String) throws -> Data {
        let text = try #require(String(data: data(name), encoding: .utf8)).trimmingCharacters(in: .whitespacesAndNewlines)
        var bytes = Data()
        var index = text.startIndex
        while index < text.endIndex {
            let next = text.index(index, offsetBy: 2)
            bytes.append(try #require(UInt8(text[index..<next], radix: 16)))
            index = next
        }
        return bytes
    }
}

@Suite struct ScreenTests {
    @Test(arguments: [
        ("screen_hello.json", ScreenClientMsg.screenHello(version: 2)),
        ("start.json", .start(display: 2, width: 2556, height: 1179, fps: 60, bitrate: 8_000_000)),
        ("keyframe.json", .keyframe),
        ("bitrate.json", .bitrate(2_000_000)),
        ("input_mouse_move.json", .input(.mouseMove(x: 0.5, y: 0.25))),
        ("input_mouse_button.json", .input(.mouseButton(.right, down: true, x: 0.75, y: 1, mods: ScreenMods(cmd: true)))),
        ("input_scroll.json", .input(.scroll(dx: 0, dy: -120))),
        ("input_key.json", .input(.key("ArrowUp", mods: ScreenMods(shift: true)))),
        ("input_text.json", .input(.text("你好 ok"))),
        ("unlock.json", .unlock(password: "p@ss word")),
    ])
    func clientMessagesMatchTheSamples(_ name: String, _ message: ScreenClientMsg) throws {
        #expect(try sameJSON(JSONEncoder().encode(message), ScreenSamples.data(name)), "\(name)")
    }

    @Test func hostMessagesMatchTheSamples() throws {
        func read(_ name: String) throws -> ScreenHostMsg {
            try JSONDecoder().decode(ScreenHostMsg.self, from: ScreenSamples.data(name))
        }
        let displays = [
            ScreenDisplay(id: 1, width: 1728, height: 1117, pixelWidth: 3456, pixelHeight: 2234),
            ScreenDisplay(id: 2, width: 2560, height: 1440, pixelWidth: 2560, pixelHeight: 1440),
        ]
        #expect(try read("ready.json") == .ready(displays: displays, screenCapture: true, postEvent: false))
        #expect(try read("incompatible.json") == .incompatible(version: 2))
        #expect(try read("started.json") == .started(display: 2, width: 2556, height: 1652))
        #expect(try read("stopped.json") == .stopped(reason: "display unplugged"))
        #expect(try read("lock.json") == .lock(locked: true))
        #expect(
            try read("unlock_failed.json") == .unlockFailed(reason: "the password has keys a US keyboard cannot type"))
    }

    @Test func videoPayloadsMatchTheSamples() throws {
        let keyframe = try #require(VideoFrame(payload: ScreenSamples.hex("video_keyframe.hex")))
        #expect(
            keyframe
                == VideoFrame(
                    data: Data([0, 0, 0, 2, 0x26, 1]), keyframe: true,
                    parameterSets: [Data([0x40, 1]), Data([0x42]), Data([0x44, 1, 2])], ptsMicros: -5))
        let delta = try #require(VideoFrame(payload: ScreenSamples.hex("video_delta.hex")))
        #expect(delta == VideoFrame(data: Data([0, 0, 0, 2, 2, 0x0a]), keyframe: false, parameterSets: [], ptsMicros: 1000))
    }

    /// 参数集的长度越过载荷结尾时整帧不认，不读出界。
    @Test func truncatedParameterSetsAreRejected() {
        var payload = Data([1])
        payload.append(contentsOf: [UInt8](repeating: 0, count: 8))
        payload.append(1)
        payload.append(contentsOf: [9, 0, 0, 0, 1, 2])
        #expect(VideoFrame(payload: payload) == nil)
    }
}
