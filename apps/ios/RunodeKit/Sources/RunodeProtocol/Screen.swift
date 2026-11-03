import Foundation

/// 远程桌面这套消息和画面格式的版本，和电脑那边 `runode_protocol::screen::SCREEN_VERSION` 一样。
public let screenVersion: UInt32 = 2

/// 远程桌面的修饰键。
public struct ScreenMods: Hashable, Sendable, Codable {
    public var shift = false
    public var ctrl = false
    public var alt = false
    /// Command 键。
    public var cmd = false

    public init(shift: Bool = false, ctrl: Bool = false, alt: Bool = false, cmd: Bool = false) {
        self.shift = shift
        self.ctrl = ctrl
        self.alt = alt
        self.cmd = cmd
    }

    public var isEmpty: Bool { self == ScreenMods() }
}

public enum ScreenMouseButton: String, Hashable, Sendable, Codable {
    case left, right, middle
}

/// 交给电脑的一次输入。坐标是画面上的比例，左上角 (0, 0)，右下角 (1, 1)。
public enum ScreenInput: Hashable, Sendable, Encodable {
    /// 指针挪到这里；按着键时是拖动。
    case mouseMove(x: Double, y: Double)
    case mouseButton(ScreenMouseButton, down: Bool, x: Double, y: Double, mods: ScreenMods = ScreenMods())
    /// 滚动的点数，和手指拖动的方向一致：正的是内容往下、往右走。
    case scroll(dx: Int32, dy: Int32)
    /// 按一下再松开。`key` 是电脑那边 `Key` 的名字（`ArrowUp`、`Enter`、`A`）。
    case key(String, mods: ScreenMods = ScreenMods())
    /// 一段文字，不管键盘布局。
    case text(String)

    private enum Keys: String, CodingKey {
        case type, x, y, button, down, mods, dx, dy, key, text
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .mouseMove(x, y):
            try c.encode("mouse_move", forKey: .type)
            try c.encode(x, forKey: .x)
            try c.encode(y, forKey: .y)
        case let .mouseButton(button, down, x, y, mods):
            try c.encode("mouse_button", forKey: .type)
            try c.encode(button, forKey: .button)
            try c.encode(down, forKey: .down)
            try c.encode(x, forKey: .x)
            try c.encode(y, forKey: .y)
            try c.encode(mods, forKey: .mods)
        case let .scroll(dx, dy):
            try c.encode("scroll", forKey: .type)
            try c.encode(dx, forKey: .dx)
            try c.encode(dy, forKey: .dy)
        case let .key(key, mods):
            try c.encode("key", forKey: .type)
            try c.encode(key, forKey: .key)
            try c.encode(mods, forKey: .mods)
        case let .text(text):
            try c.encode("text", forKey: .type)
            try c.encode(text, forKey: .text)
        }
    }
}

/// 手机发给电脑的远程桌面消息。
public enum ScreenClientMsg: Hashable, Sendable, Encodable {
    /// 过了门禁后的第一条，这条连接就接到远程桌面上。
    case screenHello(version: UInt32)
    /// 开始采集；采集中再发时按新的参数重新开始（切屏也是）。`display` 是 `ScreenDisplay.id`，为空时是主屏；
    /// `bitrate` 单位 bit/s。
    case start(display: UInt32?, width: UInt32, height: UInt32, fps: UInt32, bitrate: UInt32)
    /// 下一帧要关键帧。
    case keyframe
    case bitrate(UInt32)
    case input(ScreenInput)
    /// 电脑锁着屏时用登录密码解锁，电脑按美式键盘敲进密码框再回车；解没解开看随后的 `lock`。
    case unlock(password: String)

    private enum Keys: String, CodingKey {
        case type, version, display, width, height, fps, bitrate, input, password
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .screenHello(version):
            try c.encode("screen_hello", forKey: .type)
            try c.encode(version, forKey: .version)
        case let .start(display, width, height, fps, bitrate):
            try c.encode("start", forKey: .type)
            try c.encodeIfPresent(display, forKey: .display)
            try c.encode(width, forKey: .width)
            try c.encode(height, forKey: .height)
            try c.encode(fps, forKey: .fps)
            try c.encode(bitrate, forKey: .bitrate)
        case .keyframe:
            try c.encode("keyframe", forKey: .type)
        case let .bitrate(bitrate):
            try c.encode("bitrate", forKey: .type)
            try c.encode(bitrate, forKey: .bitrate)
        case let .input(input):
            try c.encode("input", forKey: .type)
            try c.encode(input, forKey: .input)
        case let .unlock(password):
            try c.encode("unlock", forKey: .type)
            try c.encode(password, forKey: .password)
        }
    }
}

/// 被采集的那块屏幕。
public struct ScreenDisplay: Hashable, Sendable, Decodable, Identifiable {
    /// 电脑上这块屏幕的编号，插拔以后可能变。
    public var id: UInt32
    /// 点数。
    public var width: UInt32
    public var height: UInt32
    /// 像素数。
    public var pixelWidth: UInt32
    public var pixelHeight: UInt32

    public init(id: UInt32, width: UInt32, height: UInt32, pixelWidth: UInt32, pixelHeight: UInt32) {
        self.id = id
        self.width = width
        self.height = height
        self.pixelWidth = pixelWidth
        self.pixelHeight = pixelHeight
    }

    private enum CodingKeys: String, CodingKey {
        case id, width, height
        case pixelWidth = "pixel_width"
        case pixelHeight = "pixel_height"
    }
}

/// 远程桌面停下的原因归到哪类，线上写 snake_case。
public enum ScreenStopCause: String, Hashable, Sendable {
    /// 电脑屏幕上没人登录。
    case signedOut = "signed_out"
    /// 屏幕上没有 Runode 所用的那个用户登录着的桌面（X11 或 Wayland）。
    case noDesktop = "no_desktop"
    /// 屏幕上登录着别的用户。
    case otherUser = "other_user"
    /// Mac 没给 Runode 屏幕录制权限。
    case noScreenCapture = "no_screen_capture"
    /// 屏幕共享的授权框被拒绝了，或者没人点。
    case declined
    /// 建不出能编这个 app 解得了的格式的编码器，重连也看不了。
    case noEncoder = "no_encoder"
}

/// 电脑发给手机的远程桌面消息。
public enum ScreenHostMsg: Hashable, Sendable, Decodable {
    /// 握手后的第一条。`displays` 是开着的各块屏幕，主屏排第一。授权不够时采集（`screenCapture`）或输入
    /// （`postEvent`）办不了，要用户到电脑的系统设置里打开。
    case ready(displays: [ScreenDisplay], screenCapture: Bool, postEvent: Bool)
    /// 电脑的版本和这个 app 不同，随后断开。
    case incompatible(version: UInt32)
    /// 开始采集了，接下来第一帧是关键帧；`display` 是采的那块屏幕，之后的输入落在它上面。
    case started(display: UInt32, width: UInt32, height: UInt32)
    /// 采集停了或者开不起来。`reason` 是电脑给的原文；`cause` 是归了的类，电脑没归类或者分出了这个 app 不认得的类时为空。
    case stopped(reason: String, cause: ScreenStopCause?)
    /// 电脑锁没锁屏：`ready` 之后马上报一次，之后变了再报。
    case lock(locked: Bool)
    /// 用户在电脑上打开或关掉了输入授权，`postEvent` 的含义和 `ready` 里的一样。`screenCapture` 是 `ready` 时的值，
    /// 屏幕录制的授权要重新连接才认。
    case permissions(screenCapture: Bool, postEvent: Bool)
    /// `unlock` 没敲成（有美式键盘打不出的字符），`reason` 给日志看。
    case unlockFailed(reason: String)
    /// 这个 app 不认识的消息，忽略。
    case unknown

    private enum Keys: String, CodingKey {
        case type, displays, display, version, width, height, reason, locked
        case screenCapture = "screen_capture"
        case postEvent = "post_event"
        case noEncoder = "no_encoder"
        case signedOut = "signed_out"
        case cause
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "ready":
            self = .ready(
                displays: try c.decode([ScreenDisplay].self, forKey: .displays),
                screenCapture: try c.decode(Bool.self, forKey: .screenCapture),
                postEvent: try c.decode(Bool.self, forKey: .postEvent))
        case "incompatible":
            self = .incompatible(version: try c.decode(UInt32.self, forKey: .version))
        case "started":
            self = .started(
                display: try c.decode(UInt32.self, forKey: .display), width: try c.decode(UInt32.self, forKey: .width),
                height: try c.decode(UInt32.self, forKey: .height))
        case "stopped":
            // 不带 `cause` 的电脑（老版本，或者分了这边不认得的类）照两个标志归类。
            let cause = try c.decodeIfPresent(String.self, forKey: .cause).flatMap(ScreenStopCause.init(rawValue:))
            let signedOut = try c.decodeIfPresent(Bool.self, forKey: .signedOut) ?? false
            let noEncoder = try c.decodeIfPresent(Bool.self, forKey: .noEncoder) ?? false
            self = .stopped(
                reason: try c.decode(String.self, forKey: .reason),
                cause: cause ?? (signedOut ? .signedOut : noEncoder ? .noEncoder : nil))
        case "lock":
            self = .lock(locked: try c.decode(Bool.self, forKey: .locked))
        case "permissions":
            self = .permissions(
                screenCapture: try c.decode(Bool.self, forKey: .screenCapture),
                postEvent: try c.decode(Bool.self, forKey: .postEvent))
        case "unlock_failed":
            self = .unlockFailed(reason: try c.decode(String.self, forKey: .reason))
        default:
            self = .unknown
        }
    }
}

/// 编好的一帧 HEVC，`FrameKind.video` 的载荷。
public struct VideoFrame: Hashable, Sendable {
    /// NAL 单元，每个前面是 4 字节的大端长度，可以直接放进 `CMSampleBuffer`。
    public var data: Data
    /// 解码器能从这一帧开始解。
    public var keyframe: Bool
    /// 关键帧的 VPS、SPS、PPS（不带长度前缀），别的帧是空的。
    public var parameterSets: [Data]
    /// 采集时刻，微秒，只用来比先后。
    public var ptsMicros: Int64

    public init(data: Data, keyframe: Bool, parameterSets: [Data], ptsMicros: Int64) {
        self.data = data
        self.keyframe = keyframe
        self.parameterSets = parameterSets
        self.ptsMicros = ptsMicros
    }

    /// 解 `FrameKind.video` 的载荷：`u8 标志（第 0 位是关键帧）| i64 LE pts | u8 参数集个数 |
    /// 每个参数集 u32 LE 长度和内容 | 帧的数据`。格式不对时返回 `nil`。
    public init?(payload: Data) {
        let bytes = [UInt8](payload)
        var at = 0
        func take(_ count: Int) -> ArraySlice<UInt8>? {
            guard count >= 0, bytes.count - at >= count else { return nil }
            defer { at += count }
            return bytes[at..<at + count]
        }
        func little<T: FixedWidthInteger>(_ type: T.Type) -> T? {
            guard let raw = take(MemoryLayout<T>.size) else { return nil }
            return raw.reversed().reduce(T.zero) { $0 << 8 | T(truncatingIfNeeded: $1) }
        }
        guard let flags = take(1)?.first, let pts = little(Int64.self), let count = take(1)?.first else { return nil }
        var sets: [Data] = []
        for _ in 0..<count {
            guard let length = little(UInt32.self), let set = take(Int(length)) else { return nil }
            sets.append(Data(set))
        }
        self.init(data: Data(bytes[at...]), keyframe: flags & 1 != 0, parameterSets: sets, ptsMicros: pts)
    }
}
