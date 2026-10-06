import Foundation
import RunodeProtocol

/// 连接推给上层的事件，按发生的先后排在 `AsyncStream` 里。
public enum HostEvent: Sendable {
    /// 连接状态变了。订阅时先收到一次当前的状态。
    case state(LinkState)
    /// 门禁和 `Hello` 都过了，可以发消息了。每连上一次 `generation` 加一：断线重连以后通道编号
    /// 全部作废，各个页面要重新 `Attach`。
    case ready(generation: UInt64)
    /// 宿主发来的控制消息。
    case message(HostMsg)
    /// 宿主发来的输出或快照帧，带着收到它的那次连接的 `generation`。
    case frame(Frame, generation: UInt64)
}

/// 连接的状态。
public enum LinkState: Hashable, Sendable {
    case idle
    case connecting
    case connected(hostName: String, address: String?)
    /// 断了或者连不上，`retryAt` 时自动再试。
    case waiting(reason: String, retryAt: Date)
    /// 不会自动重试的失败（被拒绝、版本不兼容、私钥没了等），要用户处理后手动重试。
    case failed(LinkFailure)

    public var isConnected: Bool {
        if case .connected = self { return true }
        return false
    }
}

/// 连接或配对失败的原因，带给人看的中文说明。
public enum LinkFailure: Error, Hashable, Sendable, LocalizedError {
    case rejected(GateRejection)
    /// Mac 上的门禁版本这个 app 不认识。
    case gateVersion(UInt32)
    /// 宿主的协议版本和这个 app 不一样。
    case incompatible(String)
    /// 这台 Mac 的设备私钥不在 Keychain 里了。
    case missingKey
    /// Mac 出示的证书和配对时记下的指纹不一样。
    case fingerprintMismatch
    case timeout
    /// 一个能试的地址都没有。
    case noAddress
    /// 二维码过期了。
    case invitationExpired
    case protocolViolation(String)
    case connectionFailed(String)
    case closed(String)
    case keychain(String)

    /// 自动重试也没用的失败。
    public var isFatal: Bool {
        switch self {
        case .rejected(let reason):
            switch reason {
            // 设备被撤销、签名不对、口令作废、远程访问关了：再试也一样，Mac 还会把这些失败计入限速。
            case .unknownDevice, .badSignature, .pairingInvalid, .disabled: true
            case .rateLimited, .unknown: false
            }
        case .gateVersion, .incompatible, .missingKey, .invitationExpired, .keychain: true
        default: false
        }
    }

    public var errorDescription: String? {
        switch self {
        case .rejected(let reason):
            switch reason {
            case .unknownDevice: "这台 Mac 不认识这部手机了：配对可能已在 Mac 上撤销，请删除后重新配对"
            case .badSignature: "签名校验失败，请删除这台 Mac 后重新配对"
            case .pairingInvalid: "配对口令不对、已过期或已经用过，请在 Mac 上重新生成二维码"
            case .rateLimited: "尝试太频繁，Mac 暂时拒绝了连接，稍后自动重试"
            case .disabled: "Mac 上没有打开远程访问"
            case .unknown(let kind): "Mac 拒绝了连接（\(kind)）"
            }
        case .gateVersion(let version): "Mac 上的 runode 使用了更新的连接方式（版本 \(version)），请升级这个 app"
        case .incompatible(let reason): "Mac 上的 runode 和这个 app 版本不兼容：\(reason)"
        case .missingKey: "找不到这台 Mac 的设备密钥，请删除后重新配对"
        case .fingerprintMismatch: "对方的证书指纹和配对时的不一样，可能连到了别的设备"
        case .timeout: "连接超时"
        case .noAddress: "没有可以尝试的地址"
        case .invitationExpired: "二维码已经过期，请在 Mac 上重新生成"
        case .protocolViolation(let detail): "Mac 的回应不符合协议：\(detail)"
        case .connectionFailed(let detail): "连接失败：\(detail)"
        case .closed(let detail): "连接断开：\(detail)"
        case .keychain(let detail): "Keychain 出错：\(detail)"
        }
    }
}

/// 一台 Mac 上宿主的连接，各个视图模型经它收发。实现是 `HostConnection` 这个 actor；单元测试换成
/// 假的。`send`、`sendInput` 是同步的：消息排进一条队列，由连接按先后写出去，调用方不必等，
/// 也不会因为各自开的任务先后不定而乱序。
public protocol HostLink: AnyObject, Sendable {
    /// 订阅事件：先收到一次当前状态（已连上时还有一次 `ready`），之后的事件按先后到达。
    func events() async -> AsyncStream<HostEvent>
    /// 发一条控制消息；没连上时丢掉，连上后各页面在 `ready` 时重新发要的东西。
    func send(_ message: ClientMsg)
    /// 发输入；`generation` 不是当前这次连接的就丢掉，免得旧通道号落到新连接的别的会话上。
    func sendInput(_ data: Data, channel: UInt32, generation: UInt64)
    /// 要连接：没连着就开始连，断了自动重连。停在不能重试的失败（`LinkState.failed`）上时什么都不做，
    /// 要 `reconnectNow`。
    func start() async
    /// 不要连接了：断开，不再重连。
    func stop() async
    /// 正在等下一次重试时立刻重试；失败后停着的也重新开始。
    func reconnectNow() async
    /// `Spawn` 这类请求的编号，一条连接上不重复。
    func nextRequestId() async -> UInt32
}

/// 前端报给宿主的身份。
public struct ClientIdentity: Hashable, Sendable {
    /// 构建标识，宿主只用它判断能不能用快照；iOS 的构建和宿主永远不同。
    public var build: String
    /// 设备名，宿主告诉别的前端「尺寸由谁控制」时显示。
    public var deviceName: String

    public init(build: String, deviceName: String) {
        self.build = build
        self.deviceName = deviceName
    }

    /// 连上后的第一条消息：手机，只要 VT 重放。
    public var hello: ClientMsg {
        .hello(
            protocol: protocolVersion, build: build, client: .mobile, caps: Caps(snapshot: false, vtReplay: true),
            session: nil, device: deviceName)
    }
}
