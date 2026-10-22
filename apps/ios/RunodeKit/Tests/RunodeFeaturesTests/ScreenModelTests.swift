import CoreMedia
import CoreVideo
import Foundation
import RunodeConnection
import RunodeProtocol
import Testing
import VideoToolbox

@testable import RunodeFeatures

@Suite struct ScreenModelTests {
    /// 画面不比手机能显示的大：长边超过上限时等比缩小，宽高取偶数（HEVC 的 4:2:0 要偶数）。
    @Test func captureSizeFitsTheLongSide() {
        let retina = ScreenDisplay(id: 1, width: 1728, height: 1117, pixelWidth: 3456, pixelHeight: 2234)
        #expect(ScreenModel.captureSize(retina) == (2560, 1654))
        let small = ScreenDisplay(id: 1, width: 1280, height: 801, pixelWidth: 1280, pixelHeight: 801)
        #expect(ScreenModel.captureSize(small) == (1280, 800))
    }

    /// 带着按住的修饰键输入单个字母、数字时按快捷键发，别的照文字发。
    @Test func textWithModsBecomesKeys() {
        let cmd = ScreenMods(cmd: true)
        #expect(ScreenModel.input(for: "c", mods: cmd) == .key("C", mods: cmd))
        #expect(ScreenModel.input(for: "1", mods: cmd) == .key("Digit1", mods: cmd))
        #expect(ScreenModel.input(for: "你", mods: cmd) == .text("你"))
        #expect(ScreenModel.input(for: "c", mods: ScreenMods()) == .text("c"))
        #expect(ScreenModel.input(for: "\n", mods: ScreenMods()) == .key("Enter", mods: ScreenMods()))
    }

    @Test func missingPermissionsAreNamed() {
        #expect(ScreenModel.missing(screenCapture: true, postEvent: true).isEmpty)
        #expect(ScreenModel.missing(screenCapture: false, postEvent: false).count == 2)
    }
}

/// 编码回调交出的样本；`VTCompressionSessionCompleteFrames` 返回前回调已经跑完，之后才读。
private final class SampleBox: @unchecked Sendable {
    var sample: CMSampleBuffer?
}

/// 用 VideoToolbox 现编一帧 HEVC，按电脑那边的格式拆成 `VideoFrame`：长度前缀的 NAL 和参数集。
private func encodedKeyframe() throws -> VideoFrame {
    var session: VTCompressionSession?
    try #require(
        VTCompressionSessionCreate(
            allocator: nil, width: 64, height: 64, codecType: kCMVideoCodecType_HEVC, encoderSpecification: nil,
            imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil, refcon: nil,
            compressionSessionOut: &session) == noErr)
    let compressor = try #require(session)
    var pixels: CVPixelBuffer?
    try #require(
        CVPixelBufferCreate(nil, 64, 64, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, nil, &pixels) == kCVReturnSuccess)
    let image = try #require(pixels)
    let encoded = SampleBox()
    VTCompressionSessionEncodeFrame(
        compressor, imageBuffer: image, presentationTimeStamp: .zero, duration: .invalid,
        frameProperties: nil, infoFlagsOut: nil
    ) { _, _, sample in encoded.sample = sample }
    VTCompressionSessionCompleteFrames(compressor, untilPresentationTimeStamp: .invalid)
    VTCompressionSessionInvalidate(compressor)
    let sample = try #require(encoded.sample)
    let format = try #require(CMSampleBufferGetFormatDescription(sample))
    var count = 0
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        format, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
        parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil)
    let sets = try (0..<count).map { index in
        var pointer: UnsafePointer<UInt8>?
        var size = 0
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
            format, parameterSetIndex: index, parameterSetPointerOut: &pointer, parameterSetSizeOut: &size,
            parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
        return Data(bytes: try #require(pointer), count: size)
    }
    let block = try #require(CMSampleBufferGetDataBuffer(sample))
    var data = Data(count: CMBlockBufferGetDataLength(block))
    data.withUnsafeMutableBytes { bytes in
        _ = CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: bytes.count, destination: bytes.baseAddress!)
    }
    return VideoFrame(data: data, keyframe: true, parameterSets: sets, ptsMicros: 0)
}

@Suite struct VideoSampleBuilderTests {
    /// 关键帧的参数集建出 64×64 的 HEVC 格式，帧的数据原样放进样本；之后不带参数集的帧沿用这个格式。
    @Test func keyframesSetTheFormatForLaterFrames() throws {
        let keyframe = try encodedKeyframe()
        var builder = VideoSampleBuilder()
        let delta = VideoFrame(data: keyframe.data, keyframe: false, parameterSets: [], ptsMicros: 1)
        let early = builder.sample(for: delta)
        #expect(early == nil)
        let built = builder.sample(for: keyframe)
        let sample = try #require(built)
        let format = try #require(CMSampleBufferGetFormatDescription(sample))
        #expect(CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_HEVC)
        let size = CMVideoFormatDescriptionGetDimensions(format)
        #expect((size.width, size.height) == (64, 64))
        #expect(CMSampleBufferGetTotalSampleSize(sample) == keyframe.data.count)
        let later = builder.sample(for: delta)
        #expect(later != nil)
    }

    @Test func garbageParameterSetsGiveNoFormat() {
        #expect(VideoSampleBuilder.format([Data([1, 2, 3])]) == nil)
    }
}

@MainActor @Suite struct ScreenTapTests {
    /// 第二次点在第一次附近、隔得不久时落到第一次的位置上，电脑才认成双击。
    @Test func aQuickNearbySecondTapLandsOnTheFirst() {
        let model = ScreenModel(machine: machineRecord(), open: { _ in throw CancellationError() })
        let start = ContinuousClock.now
        #expect(model.tapTarget(x: 0.5, y: 0.5, view: CGPoint(x: 100, y: 100), now: start) == (0.5, 0.5))
        let second = model.tapTarget(x: 0.51, y: 0.5, view: CGPoint(x: 110, y: 100), now: start + .milliseconds(200))
        #expect(second == (0.5, 0.5))
        let later = model.tapTarget(x: 0.51, y: 0.5, view: CGPoint(x: 110, y: 100), now: start + .seconds(2))
        #expect(later == (0.51, 0.5))
    }
}

@MainActor @Suite struct ScreenRetryTests {
    struct Refused: Error {}

    /// 重连以后，上一次迟到的失败不把新的这次冲成失败。
    @Test func aStaleFailureDoesNotOverwriteTheRetry() async throws {
        let model = ScreenModel(machine: machineRecord()) { _ in
            // 被 `retry` 取消的那次不理会取消，照样报失败；新的那次一直连着不返回。
            try? await Task.sleep(for: .milliseconds(50))
            if Task.isCancelled { throw Refused() }
            try await Task.sleep(for: .seconds(60))
            throw Refused()
        }
        model.start()
        model.retry()
        try await Task.sleep(for: .milliseconds(200))
        #expect(model.phase == .connecting)
        model.close()
    }
}

@MainActor @Suite struct ScreenUnlockTests {
    let machine = machineRecord()
    let passwords = MemoryUnlockPasswords()

    func model() -> ScreenModel {
        let model = ScreenModel(machine: machine, open: { _ in throw CancellationError() }, passwords: passwords)
        model.unlockWait = .milliseconds(50)
        return model
    }

    /// 锁着屏、存过密码时自动发出去；电脑解开以后收尾，不请用户输。
    @Test func aStoredPasswordUnlocksAutomatically() async throws {
        try passwords.save("secret", for: machine.id)
        let model = model()
        model.handle(.message(.lock(locked: true)))
        #expect(await eventually { model.unlocking })
        model.handle(.message(.lock(locked: false)))
        #expect(!model.unlocking && !model.askingPassword && !model.locked)
    }

    /// 没存过时请用户输；勾了记住的，电脑解开以后才存下。
    @Test func aTypedPasswordIsSavedOnlyAfterItWorks() throws {
        let model = model()
        model.handle(.message(.lock(locked: true)))
        #expect(model.askingPassword)
        model.unlock(with: "secret", remember: true)
        #expect(model.unlocking && !passwords.hasPassword(for: machine.id))
        model.handle(.message(.lock(locked: false)))
        #expect(passwords.hasPassword(for: machine.id))
    }

    /// 发了存着的密码、电脑一直没解开：删掉它、请用户重输，同一次锁屏里不再自动试。
    @Test func aWrongStoredPasswordIsForgotten() async throws {
        try passwords.save("stale", for: machine.id)
        let model = model()
        model.handle(.message(.lock(locked: true)))
        #expect(await eventually { model.askingPassword })
        #expect(!model.unlocking && model.unlockError != nil && !passwords.hasPassword(for: machine.id))
        model.askingPassword = false
        model.handle(.message(.lock(locked: true)))
        #expect(!model.askingPassword)
    }

    /// 取密码（等 Face ID）时电脑已经解开了：回来的密码不再发，过了等待时间也不当成密码不对。
    @Test func aPasswordArrivingAfterTheUnlockIsDropped() async throws {
        let passwords = SlowPasswords(machine: machine.id)
        let model = ScreenModel(machine: machine, open: { _ in throw CancellationError() }, passwords: passwords)
        model.unlockWait = .milliseconds(50)
        model.handle(.message(.lock(locked: true)))
        model.handle(.message(.lock(locked: false)))
        try await Task.sleep(for: .milliseconds(300))
        #expect(!model.unlocking && !model.askingPassword && passwords.hasPassword(for: machine.id))
    }

    /// 解锁途中断开不算密码不对：存着的密码留着，也不请用户输。
    @Test func closingMidUnlockKeepsTheStoredPassword() async throws {
        try passwords.save("secret", for: machine.id)
        let model = model()
        model.handle(.message(.lock(locked: true)))
        #expect(await eventually { model.unlocking })
        model.close()
        try await Task.sleep(for: .milliseconds(200))
        #expect(!model.unlocking && !model.askingPassword && passwords.hasPassword(for: machine.id))
    }
}

/// 取密码要等一会儿，像在等 Face ID。
private final class SlowPasswords: UnlockPasswordStore {
    private let stored = MemoryUnlockPasswords()

    init(machine: UUID) { try? stored.save("secret", for: machine) }

    func hasPassword(for machine: UUID) -> Bool { stored.hasPassword(for: machine) }
    func password(for machine: UUID, reason: String) async throws -> String? {
        try await Task.sleep(for: .milliseconds(100))
        return try await stored.password(for: machine, reason: reason)
    }
    func save(_ password: String, for machine: UUID) throws { try stored.save(password, for: machine) }
    func deletePassword(for machine: UUID) { stored.deletePassword(for: machine) }
}
