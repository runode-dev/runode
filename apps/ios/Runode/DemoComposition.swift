#if DEBUG
    import Foundation
    import RunodeConnection
    import RunodeFeatures
    import RunodeProtocol
    import Synchronization

    /// 只在调试构建里有的演示模式：启动参数带 `-runode-demo` 时不连真的电脑，换成本地的假连接：一台
    /// 列出三个会话（等你回答的 Claude、在干活的 Codex、普通的 zsh），另一台一直连不上，用来在模拟器
    /// 里看界面。
    @MainActor
    enum DemoComposition {
        static let argument = "-runode-demo"
        static let machine = MachineRecord(
            id: UUID(uuidString: "00000000-0000-0000-0000-00000000DE70")!, name: "Ethan 的 MacBook Pro",
            hostName: "Ethan 的 MacBook Pro", fingerprint: CertificateFingerprint(bytes: Data(repeating: 7, count: 32))!,
            port: 7866, addresses: ["127.0.0.1"], lastAddress: "192.168.1.20", deviceId: "00000000000000000000000000000000")
        /// 连不上的那台。
        static let offlineMachine = MachineRecord(
            id: UUID(uuidString: "00000000-0000-0000-0000-00000000DE71")!, name: "homelab",
            hostName: "homelab", fingerprint: CertificateFingerprint(bytes: Data(repeating: 8, count: 32))!,
            port: 7866, addresses: ["127.0.0.2"], lastAddress: nil, deviceId: "00000000000000000000000000000001",
            pairedAt: .now + 1)

        static var requested: Bool {
            ProcessInfo.processInfo.arguments.contains(argument)
        }

        static func dependenciesIfRequested() -> AppDependencies? {
            guard requested else { return nil }
            let store = DemoMachineStore(machines: [machine, offlineMachine])
            return AppDependencies(
                store: store, keyStore: DemoKeyStore(), pairing: DemoPairing(),
                makeLink: { record -> any HostLink in record.id == offlineMachine.id ? DemoOfflineLink() : DemoLink() },
                deviceName: "演示 iPhone",
                // 上次打开的终端记在单独的一份设置里，和正式的分开；先带 `shell` 启动一次，再不带参数启动，
                // 首页就有「继续」。
                recents: UserDefaultsRecentTerminalStore(defaults: UserDefaults(suiteName: "demo") ?? .standard))
        }

        /// 参数里还带着 `terminal` 时直接打开等回答的那个会话的终端页，`shell` 时打开普通 shell 的，
        /// `list` 时打开会话列表，`git` 时打开普通 shell 所在仓库的 Git 页，`pair` 时打开配对页，`settings` 时打开设置页，不带参数停在首页。再带上 `offline` 时连上一会儿
        /// 后假装断线，带上 `light` 时终端用浅色主题。
        static func openIfRequested(_ app: AppModel) {
            guard requested else { return }
            let arguments = ProcessInfo.processInfo.arguments
            if arguments.contains("terminal") {
                app.path = [.machine(machine.id), .terminal(machine: machine.id, session: DemoLink.claude.id)]
            } else if arguments.contains("shell") {
                app.path = [.machine(machine.id), .terminal(machine: machine.id, session: DemoLink.shell.id)]
            } else if arguments.contains("list") {
                app.path = [.machine(machine.id)]
            } else if arguments.contains("git") {
                app.path = [.machine(machine.id), .git(machine: machine.id, session: DemoLink.shell.id)]
            } else if arguments.contains("pair") {
                app.startPairing()
            } else if arguments.contains("settings") {
                app.showingSettings = true
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

    /// 一直连不上的电脑：只报连接失败。
    private final class DemoOfflineLink: HostLink {
        func events() async -> AsyncStream<HostEvent> {
            let (stream, continuation) = AsyncStream.makeStream(of: HostEvent.self)
            continuation.yield(.state(.failed(.connectionFailed("找不到这台电脑"))))
            return stream
        }

        func send(_ message: ClientMsg) {}
        func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {}
        func start() async {}
        func stop() async {}
        func reconnectNow() async {}
        func setDeviceName(_ name: String) async {}
        func nextRequestId() async -> UInt32 { 0 }
    }

    /// 假的宿主：收到什么就照协议回什么。
    private final class DemoLink: HostLink {
        static let claude = SessionInfo(
            id: SessionId("0123456789abcdef0011223344556677")!,
            size: GridSize(cols: 64, rows: 20, cellWidthPx: 16, cellHeightPx: 32),
            meta: SessionMeta(
                title: "重构连接层", agent: Agent(kind: AgentKind("claude"), state: .blocked),
                cwd: "/Users/ethan/dev/runode"),
            clients: 1, claimed: true, sizeOwner: "Ethan 的 MacBook Pro")
        static let codex = SessionInfo(
            id: SessionId("fedcba98765432100011223344556677")!,
            size: GridSize(cols: 100, rows: 30, cellWidthPx: 16, cellHeightPx: 32),
            meta: SessionMeta(
                title: "补 iOS 的单元测试", agent: Agent(kind: AgentKind("codex"), state: .working),
                cwd: "/Users/ethan/dev/runode/apps/ios"),
            clients: 0, claimed: false, sizeOwner: nil)
        static let shell = SessionInfo(
            id: SessionId("aaaabbbbccccdddd0011223344556677")!,
            size: GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32),
            meta: SessionMeta(fallbackTitle: "zsh", cwd: "/Users/ethan/dev/runode", foregroundIsShell: true),
            clients: 0, claimed: false, sizeOwner: nil)
        static let sessions = [claude, codex, shell]

        private let subscribers = Mutex<[AsyncStream<HostEvent>.Continuation]>([])
        private let sizes = Mutex<[SessionId: GridSize]>([:])

        func events() async -> AsyncStream<HostEvent> {
            let (stream, continuation) = AsyncStream.makeStream(of: HostEvent.self)
            continuation.yield(.state(.connected(hostName: "Ethan 的 MacBook Pro", address: "192.168.1.20")))
            continuation.yield(.ready(generation: 1))
            subscribers.withLock { $0.append(continuation) }
            // 参数里带着 `offline`：连上一会儿后假装断线、等着重连，用来看断线的横幅。
            if ProcessInfo.processInfo.arguments.contains("offline") {
                Task {
                    try? await Task.sleep(for: .milliseconds(1500))
                    self.emit(.state(.waiting(reason: "电脑关闭了连接", retryAt: .now + 9)))
                }
            }
            return stream
        }

        private func emit(_ event: HostEvent) {
            for continuation in subscribers.withLock({ $0 }) {
                continuation.yield(event)
            }
        }

        private func size(of session: SessionInfo) -> GridSize {
            sizes.withLock { $0[session.id] } ?? session.size
        }

        func send(_ message: ClientMsg) {
            switch message {
            case .listSessions:
                emit(.message(.sessionList(Self.sessions.map { var info = $0; info.size = size(of: $0); return info })))
            case .attach(let id, let requested, .vtReplay):
                guard let session = Self.sessions.first(where: { $0.id == id }) else { return }
                if let requested, session.sizeOwner == nil {
                    sizes.withLock { $0[id] = requested }
                }
                emit(
                    .message(
                        .attached(
                            .init(
                                id: id, channel: 1, size: size(of: session), mode: .vtReplay, meta: session.meta,
                                settings: Self.theme))))
                emit(.frame(Frame(kind: .snapshot, channel: 1, payload: Data(Self.screen(for: id).utf8)), generation: 1))
                emit(.message(.snapshotEnd(id: id)))
                if let owner = session.sizeOwner {
                    emit(.message(.sizeOwner(id: id, mine: false, owner: owner)))
                }
            case .attach(let id, _, .metaOnly):
                // 和真的宿主一样，只看状态的 `Attach` 也带着主题，首页和列表据此上色。
                guard let session = Self.sessions.first(where: { $0.id == id }) else { return }
                emit(
                    .message(
                        .attached(
                            .init(
                                id: id, channel: 0, size: size(of: session), mode: .metaOnly, meta: session.meta,
                                settings: Self.theme))))
            case .resize(let id, let size):
                guard Self.sessions.first(where: { $0.id == id })?.sizeOwner == nil else { return }
                sizes.withLock { $0[id] = size }
                emit(.message(.resized(id: id, size: size)))
            case .focus(let id, true):
                guard Self.sessions.first(where: { $0.id == id })?.sizeOwner == nil else { return }
                emit(.message(.sizeOwner(id: id, mine: true, owner: "演示 iPhone")))
            case .readScreen(let id, _, let command):
                // 普通 shell 有 shell 集成：读最近一条命令的输出时只给输出；别的会话读屏幕。
                let text = command != nil && id == Self.shell.id ? Self.shellLastOutput : Self.plain(Self.screen(for: id))
                emit(.message(.screenText(id: id, text: text, truncated: false)))
            case .sendKeys(let req, _, _), .paste(let req, _, _):
                emit(.message(.done(req: req)))
            case .git(let req, let id, let request):
                emit(.message(git(req: req, id: id, request: request)))
            default:
                break
            }
        }

        /// 假仓库：暂存、提交、切分支都改它，读状态时照它回。
        private let repo = Mutex(
            GitStatus(
                root: "/Users/ethan/dev/runode", branch: "main", head: "581d7b4", upstream: "origin/main", ahead: 1,
                behind: 2, hasRemote: true,
                staged: [GitFile(path: "crates/protocol/src/git.rs", status: .added, added: 182)],
                unstaged: [
                    GitFile(path: "crates/host/src/server.rs", status: .modified, added: 34, removed: 3),
                    GitFile(path: "apps/ios/RunodeKit/Sources/RunodeFeatures/GitModel.swift", status: .untracked, added: 290),
                    GitFile(path: "docs/old-notes.md", status: .deleted, removed: 41),
                ]))

        private func git(req: UInt32, id: SessionId, request: GitRequest) -> HostMsg {
            func move(_ paths: [String], toStaged: Bool) {
                repo.withLock { status in
                    let from = toStaged ? status.unstaged : status.staged
                    let moving = from.filter { paths.contains($0.path) }
                    if toStaged {
                        status.unstaged.removeAll { paths.contains($0.path) }
                        status.staged = (status.staged + moving).sorted { $0.path < $1.path }
                    } else {
                        status.staged.removeAll { paths.contains($0.path) }
                        status.unstaged = (status.unstaged + moving).sorted { $0.path < $1.path }
                    }
                }
            }
            switch request {
            case .status, .fetch: break
            case .stage(let paths): move(paths, toStaged: true)
            case .unstage(let paths): move(paths, toStaged: false)
            case .stageAll: move(repo.withLock { $0.unstaged.map(\.path) }, toStaged: true)
            case .unstageAll: move(repo.withLock { $0.staged.map(\.path) }, toStaged: false)
            case .commit(_, let stageAll):
                repo.withLock { status in
                    if stageAll { status.unstaged = [] }
                    status.staged = []
                    status.ahead += 1
                    status.head = "9c0ffee"
                }
            case .pull: repo.withLock { $0.behind = 0 }
            case .push: repo.withLock { $0.ahead = 0 }
            case .sync: repo.withLock { $0.ahead = 0; $0.behind = 0 }
            case .checkout(let branch, _): repo.withLock { $0.branch = branch.replacingOccurrences(of: "origin/", with: "") }
            case .branches:
                let current = repo.withLock { $0.branch }
                let branches = [
                    GitBranch(name: "main", upstream: "origin/main", subject: "feat: iOS 加设置页", date: "2 hours ago"),
                    GitBranch(name: "mobile-git", subject: "wip: 手机端 Git", date: "5 minutes ago"),
                    GitBranch(name: "origin/main", remote: true, subject: "feat: iOS 加设置页", date: "2 hours ago"),
                    GitBranch(name: "origin/release", remote: true, subject: "chore: 0.4.0", date: "3 days ago"),
                ].map { branch in
                    var branch = branch
                    branch.current = branch.name == current
                    return branch
                }
                return .gitBranches(req: req, id: id, branches: branches)
            case .diff(let path, let staged):
                let file = repo.withLock { (staged ? $0.staged : $0.unstaged).first { $0.path == path } }
                guard let file else { return .gitDiff(req: req, id: id, diff: nil) }
                let lines: [GitLine] = [
                    GitLine(kind: .context, old: 940, new: 940, text: "            ClientMsg::Paste { req, id, text } => {"),
                    GitLine(kind: .context, old: 941, new: 941, text: "                self.deliver_done(req, id, DriveAction::Paste, Inbox::Paste(text))"),
                    GitLine(kind: .context, old: 942, new: 942, text: "            }"),
                    GitLine(kind: .added, new: 943, text: "            ClientMsg::Git { req, id, request } => self.git(req, id, request),"),
                    GitLine(kind: .removed, old: 943, text: "            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, reply),"),
                    GitLine(kind: .added, new: 944, text: "            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, *reply),"),
                    GitLine(kind: .context, old: 944, new: 945, text: "            // 读写剪贴板是宿主替会话里的程序请界面办的，前端不能直接要。"),
                ]
                return .gitDiff(
                    req: req, id: id,
                    diff: GitFileDiff(file: file, hunks: [GitHunk(header: "@@ -940,5 +940,6 @@ fn handle", lines: lines)]))
            }
            return .gitStatus(req: req, id: id, status: repo.withLock { $0 })
        }

        func sendInput(_ data: Data, channel: UInt32, generation: UInt64) {
            // 回显，方便在模拟器里试键盘。
            emit(.frame(Frame(kind: .output, channel: channel, payload: data), generation: generation))
        }

        func start() async {}
        func stop() async {}
        func reconnectNow() async {}
        func setDeviceName(_ name: String) async {}

        private let request = Mutex<UInt32>(0)
        func nextRequestId() async -> UInt32 {
            request.withLock {
                $0 += 1
                return $0
            }
        }

        /// 终端的主题：默认的深色，启动参数带 `light` 时换成浅色。
        static let theme: TermSettings = {
            guard ProcessInfo.processInfo.arguments.contains("light") else { return .default }
            var settings = TermSettings.default
            settings.background = Rgb(hex: 0xFBF8F1)
            settings.foreground = Rgb(hex: 0x3B3A36)
            return settings
        }()

        static func screen(for id: SessionId) -> String {
            switch id {
            case claude.id: claudeScreen
            case codex.id: codexScreen
            default: shellScreen
            }
        }

        /// 去掉 CSI 转义序列，当作读屏幕回来的纯文字。
        static func plain(_ screen: String) -> String {
            screen.replacingOccurrences(of: "\u{1b}\\[[0-9;?]*[A-Za-z]", with: "", options: .regularExpression)
                .replacingOccurrences(of: "\r", with: "")
        }

        static let claudeScreen = [
            "\u{1b}[38;2;215;119;87m●\u{1b}[0m 先跑一遍协议的测试，确认帧格式没被改坏。",
            "",
            "\u{1b}[38;2;215;119;87m●\u{1b}[0m \u{1b}[1mRead\u{1b}[0m(crates/protocol/src/remote.rs)",
            "  ⎿  Read 412 lines",
            "",
            "\u{1b}[2m────────────────────────────────────────────────────────────\u{1b}[0m",
            " \u{1b}[1mBash command\u{1b}[0m",
            "",
            "   cargo test -p runode-protocol",
            "   \u{1b}[2mRun the protocol crate's tests\u{1b}[0m",
            "",
            " Do you want to proceed?",
            " \u{1b}[38;2;215;119;87m❯ 1. Yes\u{1b}[0m",
            "   2. Yes, and don't ask again for cargo test",
            "   3. No, and tell Claude what to do differently \u{1b}[2m(esc)\u{1b}[0m",
        ].joined(separator: "\r\n")

        static let codexScreen = [
            "\u{1b}[1m›\u{1b}[0m 给 SessionListModel 的分组和预览节流补测试",
            "",
            "• Explored",
            "  └ Read SessionListModel.swift, ListModelTests.swift",
            "",
            "• Edited Tests/RunodeFeaturesTests/ListModelTests.swift (+48 -2)",
            "",
            "\u{1b}[1m•\u{1b}[0m Working \u{1b}[2m(12s • esc to interrupt)\u{1b}[0m",
        ].joined(separator: "\r\n")

        /// 普通 shell 最近一条命令（`eza --icons`）的输出，不含提示符。
        static let shellLastOutput = [
            "\u{F07B} apps  \u{E7A8} Cargo.toml  \u{F48A} README.md  \u{F07B} crates",
            "\u{F07B} docs  \u{F15C} LICENSE  \u{E615} Makefile  \u{F07B} vendor",
        ].joined(separator: "\n")

        static let shellScreen = [
            "\u{1b}[1;32methan@mbp\u{1b}[0m:\u{1b}[1;34m~/dev/runode\u{1b}[0m$ git status --short",
            " \u{1b}[31mM\u{1b}[0m crates/protocol/src/remote.rs",
            "\u{1b}[32m??\u{1b}[0m apps/ios/",
            "\u{1b}[7m 反显 \u{1b}[0m \u{1b}[1m粗体\u{1b}[0m \u{1b}[3m斜体\u{1b}[0m \u{1b}[4m下划线\u{1b}[0m 中文宽字符 ✅",
            "\u{1b}[34m\u{F07B} apps\u{1b}[0m  \u{E7A8} Cargo.toml  \u{F48A} README.md  \u{E0A0} main",
            "\u{1b}[38;2;23;22;24;48;2;45;154;255m \u{F179} \u{F07C} ~/dev/runode "
                + "\u{1b}[38;2;45;154;255;48;2;42;217;71m\u{E0B0}"
                + "\u{1b}[38;2;23;22;24m \u{E0A0} main \u{F113} "
                + "\u{1b}[0m\u{1b}[38;2;42;217;71m\u{E0B0}\u{1b}[0m ",
        ].joined(separator: "\r\n")
    }
#endif
