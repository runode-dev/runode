import Foundation
import RunodeProtocol
import Testing

/// 电脑那边 `runode_protocol::media` 的样例，两边的测试读同一批文件。
enum MediaSamples {
    static let directory = PushSamples.repository.appending(path: "crates/protocol/tests/fixtures/media")

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: directory.appending(path: name))
    }
}

@Suite struct MediaTests {
    @Test(arguments: [
        ("media_hello.json", MediaClientMsg.mediaHello(version: mediaVersion)),
        ("media.json", .media(len: 245_760)),
    ])
    func clientMessagesMatchTheSamples(_ name: String, _ message: MediaClientMsg) throws {
        #expect(try sameJSON(JSONEncoder().encode(message), MediaSamples.data(name)), "\(name)")
    }

    @Test(arguments: [
        ("ready.json", MediaHostMsg.ready),
        ("incompatible.json", .incompatible(version: 1)),
        ("saved.json", .saved(path: "/Users/ethan/Library/Caches/runode/images/截图 1.png")),
        ("failed.json", .failed(reason: "the file is larger than 200 MiB")),
    ])
    func hostMessagesMatchTheSamples(_ name: String, _ message: MediaHostMsg) throws {
        #expect(try JSONDecoder().decode(MediaHostMsg.self, from: MediaSamples.data(name)) == message, "\(name)")
    }

    @Test func mediaFramesHaveKindSix() {
        #expect(FrameKind.media.rawValue == 6)
    }
}
