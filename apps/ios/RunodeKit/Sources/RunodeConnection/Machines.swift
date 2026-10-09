import Foundation
import RunodeProtocol

/// 一台配对过的电脑。私钥不在这里，在 `DeviceKeyStore` 里按 `id` 存。
public struct MachineRecord: Hashable, Sendable, Codable, Identifiable {
    public var id: UUID
    /// 列表里显示的名字，用户可以改；配对时取二维码里的主机名。
    public var name: String
    /// 门禁时电脑报的主机名。
    public var hostName: String
    public var fingerprint: CertificateFingerprint
    public var port: UInt16
    /// 二维码里的地址。
    public var addresses: [String]
    /// 上次连上时对方的地址，下次在 Bonjour 之后先试它。
    public var lastAddress: String?
    /// 配对时宿主发的设备编号。
    public var deviceId: String
    public var pairedAt: Date

    public init(
        id: UUID = UUID(), name: String, hostName: String, fingerprint: CertificateFingerprint, port: UInt16,
        addresses: [String], lastAddress: String?, deviceId: String, pairedAt: Date = .now
    ) {
        self.id = id
        self.name = name
        self.hostName = hostName
        self.fingerprint = fingerprint
        self.port = port
        self.addresses = addresses
        self.lastAddress = lastAddress
        self.deviceId = deviceId
        self.pairedAt = pairedAt
    }

    /// 发现之外要挨个试的地址：上次成功的在前，再是二维码里的，去掉重复。
    public var fallbackTargets: [TransportTarget] {
        var seen = Set<String>()
        return ([lastAddress].compactMap { $0 } + addresses).compactMap { address in
            guard seen.insert(address).inserted else { return nil }
            return TransportTarget.address(address, port: port)
        }
    }
}

/// 配对过的电脑存在哪里。
public protocol MachineStore: Sendable {
    func all() async -> [MachineRecord]
    /// 加一台，或者按 `id` 换掉已有的。
    func upsert(_ machine: MachineRecord) async throws
    /// 只改存着的那条的 `lastAddress`，别的字段（用户改过的名字）照旧；这条已经删了时什么也不做。
    func updateLastAddress(id: UUID, _ address: String) async throws
    func remove(id: UUID) async throws
}

/// 只记在内存里，测试和演示模式用。
public actor MemoryMachineStore: MachineStore {
    private var machines: [MachineRecord]

    public init(_ machines: [MachineRecord] = []) {
        self.machines = machines
    }

    public func all() -> [MachineRecord] { machines }

    public func upsert(_ machine: MachineRecord) {
        machines.removeAll { $0.id == machine.id }
        machines.append(machine)
    }

    public func updateLastAddress(id: UUID, _ address: String) {
        guard let index = machines.firstIndex(where: { $0.id == id }) else { return }
        machines[index].lastAddress = address
    }

    public func remove(id: UUID) { machines.removeAll { $0.id == id } }
}

/// 存在 app 自己目录下的一个 JSON 文件里；记录里没有秘密（私钥在 Keychain），文件照常加数据保护。
public actor FileMachineStore: MachineStore {
    private let url: URL
    private var cache: [MachineRecord]?

    public init(url: URL) {
        self.url = url
    }

    /// 默认位置：Application Support 下的 `machines.json`。
    public static func defaultURL() -> URL {
        URL.applicationSupportDirectory.appending(path: "machines.json")
    }

    public func all() -> [MachineRecord] {
        if let cache { return cache }
        let loaded = (try? Data(contentsOf: url)).flatMap { try? JSONDecoder().decode([MachineRecord].self, from: $0) }
        cache = loaded ?? []
        return cache ?? []
    }

    public func upsert(_ machine: MachineRecord) throws {
        var machines = all()
        if let index = machines.firstIndex(where: { $0.id == machine.id }) {
            machines[index] = machine
        } else {
            machines.append(machine)
        }
        try write(machines)
    }

    public func updateLastAddress(id: UUID, _ address: String) throws {
        var machines = all()
        guard let index = machines.firstIndex(where: { $0.id == id }) else { return }
        machines[index].lastAddress = address
        try write(machines)
    }

    public func remove(id: UUID) throws {
        try write(all().filter { $0.id != id })
    }

    private func write(_ machines: [MachineRecord]) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        #if os(iOS)
            try encoder.encode(machines).write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        #else
            try encoder.encode(machines).write(to: url, options: .atomic)
        #endif
        cache = machines
    }
}
