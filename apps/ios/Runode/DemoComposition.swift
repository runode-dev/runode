#if DEBUG
    import Foundation
    import RunodeConnection
    import RunodeFeatures
    import RunodeProtocol
    import Synchronization

    /// 只在调试构建里有的演示模式：启动参数带 `-runode-demo` 时不连真的 Mac，换成一条本地的假连接，
    /// 列出两个会话、终端里放一段带颜色的内容，用来在模拟器里看界面和终端的画法。
    @MainActor
    enum DemoComposition {
        static let argument = "-runode-demo"
        static let machine = MachineRecord(
            id: UUID(uuidString: "00000000-0000-0000-0000-00000000DE70")!, name: "演示用的 Mac",
            hostName: "Demo MacBook", fingerprint: CertificateFingerprint(bytes: Data(repeating: 7, count: 32))!,
            port: 7866, addresses: ["127.0.0.1"], lastAddress: "127.0.0.1", deviceId: "00000000000000000000000000000000")

        static var requested: Bool {
            ProcessInfo.processInfo.arguments.contains(argument)
        }

        static func dependenciesIfRequested() -> AppDependencies? {
            guard requested else { return nil }
            let store = DemoMachineStore(machines: [machine])
            return AppDependencies(
                store: store, keyStore: DemoKeyStore(), pairing: DemoPairing(),
                makeLink: { _ in DemoLink() }, deviceName: "演示 iPhone")
        }

        /// 参数里还带着 `terminal` 时直接打开第一个会话的终端页，`sessions` 时打开会话列表，`pair` 时打开配对页。
        static func openIfRequested(_ app: AppModel) {
            guard requested else { return }
            let arguments = ProcessInfo.processInfo.arguments
            if arguments.contains("terminal") {
                app.path = [.machine(machine.id), .terminal(machine: machine.id, session: DemoLink.sessions[0].id)]
            } else if arguments.contains("sessions") {
                app.path = [.machine(machine.id)]
            } else if arguments.contains("pair") {
                app.startPairing()
            }
        }
    }

    private actor DemoMachineStore: MachineStore {
        var machines: [MachineRecord]
        init(machines: [MachineRecord]) { self.machines = machines }
        func all() -> [MachineRecord] { machines }
        func upsert(_ machine: MachineRecord) {
            machines.removeAll { $0.id == machine.id }
            machines.append(machine)
        }
        func remove(id: UUID) { machines.removeAll { $0.id == id } }
    }

    private struct DemoKeyStore: DeviceKeyStore {
        func save(_ key: StoredDeviceKey, for machine: UUID) throws {}
        func key(for machine: UUID) throws -> StoredDeviceKey? { nil }
        func deleteKey(for machine: UUID) throws {}
    }

    private struct DemoPairing: Pairing {
        func pair(with invitation: PairingInvitation, deviceName: String) async throws -> MachineRecord {
            throw LinkFailure.connectionFailed("演示模式不能配对")
        }
    }

    /// 假的宿主：收到什么就照协议回什么。
    private final class DemoLink: HostLink {
        static let sessions = [
            SessionInfo(
                id: SessionId("0123456789abcdef0011223344556677")!,
                size: GridSize(cols: 64, rows: 20, cellWidthPx: 16, cellHeightPx: 32),
                meta: SessionMeta(
                    title: "修协议的 bug", agent: Agent(kind: AgentKind("claude"), state: .working),
                    cwd: "/Users/ethan/dev/runode"),
                clients: 1, claimed: true, sizeOwner: "Ethan 的 MacBook Pro"),
            SessionInfo(
                id: SessionId("fedcba98765432100011223344556677")!,
                size: GridSize(cols: 120, rows: 40, cellWidthPx: 16, cellHeightPx: 32),
                meta: SessionMeta(
                    fallbackTitle: "zsh", agent: Agent(kind: AgentKind("codex"), state: .blocked), cwd: "/tmp"),
                clients: 0, claimed: false, sizeOwner: nil),
        ]

        private let subscribers = Mutex<[AsyncStream<HostEvent>.Continuation]>([])

        func events() async -> AsyncStream<HostEvent> {
            let (stream, continuation) = AsyncStream.makeStream(of: HostEvent.self)
            continuation.yield(.state(.connected(hostName: "Demo MacBook", address: "127.0.0.1")))
            continuation.yield(.ready(generation: 1))
            subscribers.withLock { $0.append(continuation) }
            return stream
        }

        private func emit(_ event: HostEvent) {
            for continuation in subscribers.withLock({ $0 }) {
                continuation.yield(event)
            }
        }

        func send(_ message: ClientMsg) {
            switch message {
            case .listSessions:
                emit(.message(.sessionList(Self.sessions)))
            case .attach(let id, _, .vtReplay):
                guard let session = Self.sessions.first(where: { $0.id == id }) else { return }
                emit(
                    .message(
                        .attached(
                            .init(
                                id: id, channel: 1, size: session.size, mode: .vtReplay, meta: session.meta,
                                settings: .default))))
                emit(.frame(Frame(kind: .snapshot, channel: 1, payload: Data(Self.screen.utf8)), generation: 1))
                emit(.message(.snapshotEnd(id: id)))
                emit(.message(.sizeOwner(id: id, mine: false, owner: "Ethan 的 MacBook Pro")))
            default:
                break
            }
        }

        func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
            // 回显，方便在模拟器里试键盘。
            emit(.frame(Frame(kind: .output, channel: channel, payload: data), generation: generation))
        }

        func start() async {}
        func stop() async {}
        func reconnectNow() async {}
        func nextRequestId() async -> UInt32 { 1 }

        static let screen = [
            "\u{1b}[1;32methan@mbp\u{1b}[0m:\u{1b}[1;34m~/dev/runode\u{1b}[0m$ git status --short",
            " \u{1b}[31mM\u{1b}[0m crates/protocol/src/remote.rs",
            "\u{1b}[32m??\u{1b}[0m apps/ios/",
            "\u{1b}[1;32methan@mbp\u{1b}[0m:\u{1b}[1;34m~/dev/runode\u{1b}[0m$ cargo test -p runode-protocol",
            "\u{1b}[1;32m   Compiling\u{1b}[0m runode-protocol v0.1.0",
            "\u{1b}[1;32m    Finished\u{1b}[0m `test` profile in 3.21s",
            "test remote::pairing_uri ... \u{1b}[32mok\u{1b}[0m",
            "test remote::signed_bytes ... \u{1b}[32mok\u{1b}[0m",
            "test result: \u{1b}[32mok\u{1b}[0m. 42 passed; 0 failed",
            "",
            "\u{1b}[7m 反显 \u{1b}[0m \u{1b}[1m粗体\u{1b}[0m \u{1b}[3m斜体\u{1b}[0m \u{1b}[4m下划线\u{1b}[0m \u{1b}[9m删除线\u{1b}[0m 中文宽字符 ✅",
            "\u{1b}[41m  \u{1b}[42m  \u{1b}[43m  \u{1b}[44m  \u{1b}[45m  \u{1b}[46m  \u{1b}[47m  \u{1b}[0m"
                + " \u{1b}[38;2;255;128;0m24 位色\u{1b}[0m \u{1b}[38;5;141m256 色\u{1b}[0m",
            // 圆头的 Powerline 段、细分隔符和辅助平面上的 Material Design 图标。
            "\u{1b}[38;2;221;48;255m\u{E0B6}\u{1b}[48;2;221;48;255;38;2;23;22;24m \u{F0399} node 22 \u{1b}[0m"
                + "\u{1b}[38;2;221;48;255m\u{E0B4}\u{1b}[0m  cpu \u{E0B1} mem \u{E0B3} \u{F0A1E} 12:30",
            // zsh 主题常见的 Powerline 提示符：段与段之间是填满格子的三角，图标来自 Nerd Font。
            "\u{1b}[38;2;23;22;24;48;2;45;154;255m \u{F179} \u{F07C} ~/dev/runode "
                + "\u{1b}[38;2;45;154;255;48;2;42;217;71m\u{E0B0}"
                + "\u{1b}[38;2;23;22;24m \u{E0A0} main \u{F113} "
                + "\u{1b}[0m\u{1b}[38;2;42;217;71m\u{E0B0}\u{1b}[0m ",
        ].joined(separator: "\r\n")
    }
#endif
