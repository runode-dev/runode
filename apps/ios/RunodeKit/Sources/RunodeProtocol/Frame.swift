import Foundation

/// 帧的类型，和宿主的 `runode_protocol::FrameKind` 一一对应。帧头格式是冻结的：
/// `u32 LE 载荷长度 | u8 类型 | u32 LE 通道 | 载荷`。
public enum FrameKind: UInt8, Sendable, Hashable {
    /// 宿主发来的 PTY 输出，原样的字节。
    case output = 0
    /// 发给宿主、要写进 PTY 的输入。
    case input = 1
    /// 控制消息，JSON。
    case control = 2
    /// 快照或 VT 重放的一段，以 `HostMsg.snapshotEnd` 结束。
    case snapshot = 3
}

/// 一帧。
public struct Frame: Sendable, Equatable {
    /// 帧头的字节数：载荷长度、类型、通道。
    public static let headerLength = 9
    /// 载荷上限，和宿主的 `MAX_PAYLOAD` 一样；再长就是对面出了错。
    public static let maxPayload: UInt32 = 64 << 20
    /// 门禁阶段单帧载荷的上限，见远程访问规格。
    public static let gateMaxPayload: UInt32 = 16 << 10

    public var kind: FrameKind
    /// 会话在这条连接上的通道；控制消息一律是 0。
    public var channel: UInt32
    public var payload: Data

    public init(kind: FrameKind, channel: UInt32, payload: Data) {
        self.kind = kind
        self.channel = channel
        self.payload = payload
    }

    /// 一条控制消息，走通道 0。
    public static func control(_ message: some Encodable) throws -> Frame {
        Frame(kind: .control, channel: 0, payload: try JSONEncoder().encode(message))
    }

    /// 把控制帧的载荷解成消息。
    public func message<M: Decodable>(_ type: M.Type) throws -> M {
        try JSONDecoder().decode(type, from: payload)
    }

    /// 帧头加载荷。载荷超过 `maxPayload` 时报错，什么都不写。
    public func encoded() throws(FrameError) -> Data {
        guard let length = UInt32(exactly: payload.count), length <= Frame.maxPayload else {
            throw .tooLong(UInt64(payload.count))
        }
        var data = Data(capacity: Frame.headerLength + payload.count)
        withUnsafeBytes(of: length.littleEndian) { data.append(contentsOf: $0) }
        data.append(kind.rawValue)
        withUnsafeBytes(of: channel.littleEndian) { data.append(contentsOf: $0) }
        data.append(payload)
        return data
    }
}

/// 读帧失败的原因。出现任何一种时连接上的数据都已经对不齐了，只能断开。
public enum FrameError: Error, Equatable, Sendable {
    /// 载荷超过上限，带着对面声明的长度。
    case tooLong(UInt64)
    /// 不认识的帧类型，对面多半是别的协议版本。
    case unknownKind(UInt8)
    /// 连接在一帧中间断了。
    case truncated
}

/// 从字节流里切帧：网络上收到多少就 `append` 多少，再反复 `next` 直到返回 `nil`。
/// 和宿主的 `read_frame` 一样先查长度再查类型，载荷按实际到达的字节攒，不照着声明的长度预先分配。
public struct FrameDecoder: Sendable {
    /// 单帧载荷的上限：门禁阶段是 `Frame.gateMaxPayload`，之后是 `Frame.maxPayload`。
    public var maxPayload: UInt32
    private var buffer: [UInt8] = []
    /// `buffer` 里已经切走的字节数，攒多了再一起挪掉，免得每帧都搬一次。
    private var start = 0

    public init(maxPayload: UInt32 = Frame.maxPayload) {
        self.maxPayload = maxPayload
    }

    /// 还没切成帧的字节数。
    public var bufferedCount: Int { buffer.count - start }

    public mutating func append(_ data: Data) {
        buffer.append(contentsOf: data)
    }

    /// 切出下一帧；数据还不够一帧时返回 `nil`。
    public mutating func next() throws(FrameError) -> Frame? {
        let available = buffer.count - start
        guard available >= Frame.headerLength else { return nil }
        let length = readU32(at: start)
        if length > maxPayload {
            throw .tooLong(UInt64(length))
        }
        let kindByte = buffer[start + 4]
        guard let kind = FrameKind(rawValue: kindByte) else {
            throw .unknownKind(kindByte)
        }
        let channel = readU32(at: start + 5)
        let total = Frame.headerLength + Int(length)
        guard available >= total else { return nil }
        let payloadStart = start + Frame.headerLength
        let payload = Data(buffer[payloadStart..<(payloadStart + Int(length))])
        start += total
        compactIfNeeded()
        return Frame(kind: kind, channel: channel, payload: payload)
    }

    /// 连接正常关闭时调：两帧之间关闭没事，帧读到一半就关了算截断。
    public func finish() throws(FrameError) {
        if bufferedCount > 0 {
            throw .truncated
        }
    }

    private func readU32(at index: Int) -> UInt32 {
        UInt32(buffer[index]) | UInt32(buffer[index + 1]) << 8 | UInt32(buffer[index + 2]) << 16
            | UInt32(buffer[index + 3]) << 24
    }

    private mutating func compactIfNeeded() {
        if start == buffer.count {
            buffer.removeAll(keepingCapacity: true)
            start = 0
        } else if start > 64 << 10, start * 2 > buffer.count {
            buffer.removeFirst(start)
            start = 0
        }
    }
}
