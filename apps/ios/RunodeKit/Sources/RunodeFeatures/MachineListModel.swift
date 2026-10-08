import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 配对过的电脑：列出、改名、删除（连同 Keychain 里的设备私钥）。
@Observable
@MainActor
public final class MachineListModel {
    public private(set) var machines: [MachineRecord] = []
    public private(set) var loaded = false
    public var errorMessage: String?
    /// 正在改名的那台和输入框里的名字。
    public private(set) var renameTarget: UUID?
    public var renameText = ""
    /// 等用户确认删除的那台。
    public var deleteTarget: UUID?

    @ObservationIgnored private let store: any MachineStore
    @ObservationIgnored private let keyStore: any DeviceKeyStore
    /// 删掉一台电脑之前先断开它的连接、退出它的页面。
    @ObservationIgnored public var willDelete: @MainActor (UUID) -> Void = { _ in }
    /// 每次重新读完列表（配对、改名、删除之后都会读）以后调，`AppModel` 据此给每台电脑建好连接。
    @ObservationIgnored public var didLoad: @MainActor () -> Void = {}

    public init(store: any MachineStore, keyStore: any DeviceKeyStore) {
        self.store = store
        self.keyStore = keyStore
    }

    public func load() async {
        machines = await store.all().sorted { $0.pairedAt < $1.pairedAt }
        loaded = true
        didLoad()
    }

    public func machine(_ id: UUID) -> MachineRecord? {
        machines.first { $0.id == id }
    }

    /// 配对好的一台存下来；同一个证书指纹的旧记录（重新配对的）连同私钥一起换掉。
    public func add(_ machine: MachineRecord) async {
        do {
            for old in machines where old.fingerprint == machine.fingerprint && old.id != machine.id {
                willDelete(old.id)
                try? keyStore.deleteKey(for: old.id)
                try await store.remove(id: old.id)
            }
            try await store.upsert(machine)
        } catch {
            errorMessage = String(localized: "存不下这台电脑：\(error.localizedDescription)")
        }
        await load()
    }

    /// 改名的输入框开着；关掉时放弃。
    public var isRenaming: Bool {
        get { renameTarget != nil }
        set { if !newValue { renameTarget = nil } }
    }

    public func beginRename(_ id: UUID) {
        renameText = machine(id)?.name ?? ""
        renameTarget = id
    }

    public func rename(_ id: UUID, to name: String) async {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var machine = machine(id), !trimmed.isEmpty else { return }
        machine.name = trimmed
        do {
            try await store.upsert(machine)
        } catch {
            errorMessage = String(localized: "改不了名字：\(error.localizedDescription)")
        }
        await load()
    }

    public func delete(_ id: UUID) async {
        willDelete(id)
        do {
            try keyStore.deleteKey(for: id)
            try await store.remove(id: id)
        } catch {
            errorMessage = String(localized: "删不掉这台电脑：\(error.localizedDescription)")
        }
        await load()
    }
}
