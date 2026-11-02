import Foundation
import RunodeProtocol
import Testing

/// 电脑那边 `runode_protocol::speech` 的样例，两边的测试读同一批文件。
enum SpeechSamples {
    static let directory = PushSamples.repository.appending(path: "crates/protocol/tests/fixtures/speech")

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }
}

@Suite struct SpeechTests {
    @Test(arguments: [
        ("speech_hello.json", SpeechClientMsg.speechHello(version: speechVersion)),
        ("finish.json", .finish),
    ])
    func clientMessagesMatchTheSamples(_ name: String, _ message: SpeechClientMsg) throws {
        #expect(try sameJSON(JSONEncoder().encode(message), SpeechSamples.data(name)), "\(name)")
    }

    @Test(arguments: [
        ("ready.json", SpeechHostMsg.ready(installed: true)),
        ("incompatible.json", .incompatible(version: 1)),
        ("text.json", .text("打开 README.md 看看")),
        ("failed.json", .failed(reason: "no speech model downloaded")),
    ])
    func hostMessagesMatchTheSamples(_ name: String, _ message: SpeechHostMsg) throws {
        #expect(try JSONDecoder().decode(SpeechHostMsg.self, from: SpeechSamples.data(name)) == message, "\(name)")
    }

    @Test func audioFramesHaveKindFive() {
        #expect(FrameKind.audio.rawValue == 5)
    }

    /// 录音编成 16 位小端 PCM，超出 -1 到 1 的截到两头。
    @Test func audioIsLittleEndianPCM() {
        #expect(speechAudioPayload([0, 1, -1, 2]) == Data([0, 0, 0xff, 0x7f, 0x01, 0x80, 0xff, 0x7f]))
    }
}
