import Foundation

/// 官方 App 为一台电脑向中转服务换推送句柄的请求体（宿主的 `push::RelayRegister`）。电脑只拿得到句柄，
/// 拿不到 device token。
public struct RelayRegister: Hashable, Sendable, Encodable {
    /// 宿主的 `push::RELAY_REGISTER`。
    public static let path = "/v1/register"
    /// 宿主的 `push::OFFICIAL_BUNDLE`：只有这个 App 的推送经中转服务发。
    public static let officialBundle = "dev.runode.mobile"

    public var v: UInt32 = 1
    public var env: ApnsEnv
    /// APNs 的 device token（十六进制），或者 FCM 的 registration token 原文。
    public var token: String
    /// 手机给这台电脑编的 UUID（`ClientMsg.pushRegister` 的 `machine`）。
    public var machine: String
    /// 推送经哪家送到手机；是 `apns` 时不写出来。
    public var service: PushService

    public init(env: ApnsEnv, token: String, machine: String, service: PushService = .apns) {
        self.env = env
        self.token = token
        self.machine = machine
        self.service = service
    }

    private enum CodingKeys: String, CodingKey {
        case v, env, token, machine, service
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(v, forKey: .v)
        try c.encode(env, forKey: .env)
        try c.encode(token, forKey: .token)
        try c.encode(machine, forKey: .machine)
        if service != .apns { try c.encode(service, forKey: .service) }
    }
}

/// 中转服务换句柄的回话（宿主的 `push::RelayHandle`）。
public struct RelayHandle: Hashable, Sendable, Decodable {
    public var handle: String
}

/// 中转服务出错时的响应体（宿主的 `push::PushError`）。
public struct PushError: Hashable, Sendable, Decodable {
    public var reason: String
}
