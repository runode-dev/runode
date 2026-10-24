import Foundation
import RunodeTerminal
import Synchronization
import Testing

@testable import RunodeFeatures
@testable import RunodeSpeech

/// 麦克风键：按住说话、点一下开始再点一下结束、滑出去取消；识别出的文字交给开始时给的 `insert`。
@MainActor @Suite struct DictationTests {
    final class FakeRecorder: DictationRecorder, @unchecked Sendable {
        var started = false
        var stopped = false
        func start() throws { started = true }
        func stop() throws -> [Float] {
            stopped = true
            return [0.1, 0.2]
        }
    }

    let recorder = FakeRecorder()
    let root: URL

    init() throws {
        root = FileManager.default.temporaryDirectory.appending(path: "dictation-\(UUID().uuidString)")
        for asset in SpeechModels.assets {
            try FileManager.default.createDirectory(at: asset.path(in: root).deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data().write(to: asset.path(in: root))
        }
    }

    func model() -> DictationModel {
        let recorder = recorder
        return DictationModel(
            root: root, openSpeech: nil, makeRecorder: { _ in recorder }, transcribe: { "第\($0.count)段" })
    }

    func settle(_ model: DictationModel) async {
        while model.phase == .transcribing { await Task.yield() }
    }

    @Test func holdingThenReleasingTranscribes() async throws {
        let model = model()
        var typed: [String] = []
        model.press(.down, machine: nil, source: .phone) { typed.append($0) }
        #expect(model.phase == .recording && recorder.started)
        try await Task.sleep(for: DictationModel.hold + .milliseconds(50))
        model.press(.up, machine: nil, source: .phone) { _ in }
        #expect(model.phase == .transcribing)
        await settle(model)
        #expect(typed == ["第2段"])
    }

    /// 点得快就一直录，再按一次才结束，松开那一下不算。
    @Test func aTapKeepsRecordingUntilTheNextPress() async {
        let model = model()
        var typed: [String] = []
        model.press(.down, machine: nil, source: .phone) { typed.append($0) }
        model.press(.up, machine: nil, source: .phone) { _ in }
        #expect(model.phase == .recording)
        model.press(.down, machine: nil, source: .phone) { _ in }
        #expect(model.phase == .transcribing)
        model.press(.up, machine: nil, source: .phone) { _ in }
        await settle(model)
        #expect(typed == ["第2段"])
    }

    @Test func slidingOffCancels() {
        let model = model()
        model.press(.down, machine: nil, source: .phone) { _ in Issue.record("nothing should be typed") }
        model.press(.cancel, machine: nil, source: .phone) { _ in }
        #expect(model.phase == .idle && recorder.stopped)
    }

    /// 手机上没模型、又没有电脑可交时不录，说清楚要去下载。
    @Test func withoutAnyModelItAsksToDownload() throws {
        try SpeechModels.remove(from: root)
        let model = model()
        model.press(.down, machine: nil, source: .phone) { _ in }
        #expect(model.phase == .idle && !recorder.started)
        #expect(model.errorMessage != nil)
    }
}
