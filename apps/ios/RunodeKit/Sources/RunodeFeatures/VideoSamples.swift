import CoreMedia
import RunodeProtocol

/// 把 `VideoFrame` 做成能交给 `AVSampleBufferDisplayLayer` 的 `CMSampleBuffer`，由显示层自己解码。关键帧带的
/// 参数集建出格式，之后的帧沿用；每帧都标成马上显示，不按时间戳排队。
public struct VideoSampleBuilder {
    private var format: CMVideoFormatDescription?

    public init() {}

    /// 还没有格式（没收到过关键帧）或者建不出来时返回 `nil`，调用方该要一个关键帧。
    public mutating func sample(for frame: VideoFrame) -> CMSampleBuffer? {
        if frame.keyframe, !frame.parameterSets.isEmpty {
            format = Self.format(frame.parameterSets)
        }
        guard let format, !frame.data.isEmpty else { return nil }
        var block: CMBlockBuffer?
        let length = frame.data.count
        guard
            CMBlockBufferCreateWithMemoryBlock(
                allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: length,
                blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0, dataLength: length,
                flags: 0, blockBufferOut: &block) == noErr, let block
        else { return nil }
        let copied = frame.data.withUnsafeBytes { bytes in
            CMBlockBufferReplaceDataBytes(
                with: bytes.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: length)
        }
        guard copied == noErr else { return nil }
        var sample: CMSampleBuffer?
        var size = length
        guard
            CMSampleBufferCreateReady(
                allocator: kCFAllocatorDefault, dataBuffer: block, formatDescription: format, sampleCount: 1,
                sampleTimingEntryCount: 0, sampleTimingArray: nil, sampleSizeEntryCount: 1, sampleSizeArray: &size,
                sampleBufferOut: &sample) == noErr, let sample
        else { return nil }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
            CFArrayGetCount(attachments) > 0
        {
            let first = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                first, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }

    /// 解码器要的格式：HEVC，NAL 单元前 4 字节长度。
    static func format(_ parameterSets: [Data]) -> CMVideoFormatDescription? {
        let copies = parameterSets.map { set -> UnsafeMutablePointer<UInt8> in
            let pointer = UnsafeMutablePointer<UInt8>.allocate(capacity: max(set.count, 1))
            set.copyBytes(to: pointer, count: set.count)
            return pointer
        }
        defer { copies.forEach { $0.deallocate() } }
        var format: CMFormatDescription?
        let status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
            allocator: kCFAllocatorDefault, parameterSetCount: parameterSets.count,
            parameterSetPointers: copies.map { UnsafePointer($0) }, parameterSetSizes: parameterSets.map(\.count),
            nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &format)
        return status == noErr ? format : nil
    }
}
