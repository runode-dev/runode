import Foundation
import Testing

@testable import RunodeSpeech

@Suite struct RecognitionTests {
    @Test func spacesOnlyBetweenLatinSentences() {
        var text = ""
        for sentence in ["Hello.", "World.", "你好。", "好的", "OK."] {
            append(sentence, to: &text)
        }
        #expect(text == "Hello. World.你好。好的OK.")
    }

    /// 少一个文件就不算装好，删的时候整个目录一起删。
    @Test func installedOnlyWithEveryFile() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: "speech-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        for asset in SpeechModels.assets.dropLast() {
            try FileManager.default.createDirectory(at: asset.path(in: root).deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data().write(to: asset.path(in: root))
        }
        #expect(!SpeechModels.isInstalled(in: root))
        let vad = SpeechModels.vad.path(in: root)
        try FileManager.default.createDirectory(at: vad.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data().write(to: vad)
        #expect(SpeechModels.isInstalled(in: root))
        try SpeechModels.remove(from: root)
        #expect(try FileManager.default.contentsOfDirectory(atPath: root.path).isEmpty)
    }

    @Test func downloadSizeIsTheSumOfTheFiles() {
        #expect(SpeechModels.downloadSize == 239_233_841 + 315_894 + 1_807_522)
    }
}
