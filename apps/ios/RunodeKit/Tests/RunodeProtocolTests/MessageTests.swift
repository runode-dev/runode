import Foundation
import Testing

@testable import RunodeProtocol

/// 宿主（Rust）序列化出来的样例：`crates/protocol/tests/fixtures/messages.json`，按名字存着各条消息的
/// JSON，由 `runode_protocol` 的测试 `message_fixture` 生成。
enum RustSamples {
    static let url: URL = URL(filePath: #filePath)
        .deletingLastPathComponent()  // RunodeProtocolTests
        .deletingLastPathComponent()  // Tests
        .deletingLastPathComponent()  // RunodeKit
        .deletingLastPathComponent()  // ios
        .deletingLastPathComponent()  // apps
        .deletingLastPathComponent()  // 仓库根
        .appending(path: "crates/protocol/tests/fixtures/messages.json")

    /// 每条样例重新编成的 JSON，按名字。
    static let messages: [String: Data] = {
        guard let data = try? Data(contentsOf: url),
            let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let messages = root["messages"] as? [String: Any]
        else { return [:] }
        return messages.compactMapValues { try? JSONSerialization.data(withJSONObject: $0, options: .fragmentsAllowed) }
    }()

    static func data(_ name: String) throws -> Data {
        try #require(messages[name], "no sample \(name)")
    }
}

/// 两段 JSON 解出来的值相等（键的顺序、空白不算）。
func sameJSON(_ a: Data, _ b: Data) throws -> Bool {
    let left = try JSONSerialization.jsonObject(with: a, options: .fragmentsAllowed) as AnyObject
    let right = try JSONSerialization.jsonObject(with: b, options: .fragmentsAllowed) as AnyObject
    return left.isEqual(right)
}

@Suite struct ClientMessageTests {
    let id = SessionId("0123456789abcdef0011223344556677")!
    let size = GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32)

    @Test(arguments: [
        "hello", "list", "layout_request", "open", "open_workspace", "open_workspace_named", "rename_workspace",
        "list_dirs", "list_dirs_home",
        "list_project_tasks", "spawn",
        "attach_vt", "attach_meta", "attach_size", "detach", "resize", "focus", "kill",
        "read_screen", "read_screen_command", "send_keys", "paste",
        "push_register", "push_unregister",
    ])
    func matchesTheHost(_ name: String) throws {
        let message: ClientMsg =
            switch name {
            case "hello":
                .hello(
                    protocol: 4, build: "ios-0.1.0+1", client: .mobile, caps: Caps(snapshot: false, vtReplay: true),
                    session: nil, device: "Ethan 的 iPhone")
            case "list": .listSessions
            case "layout_request": .layout(req: 0)
            case "open": .open(req: 7, placement: .tab, near: nil, cwd: nil, focus: false)
            case "open_workspace": .openWorkspace(req: 8, dir: "/Users/ethan/dev/中文", focus: false)
            case "open_workspace_named":
                .openWorkspace(req: 8, dir: "/Users/ethan/dev/中文", focus: false, name: "后端")
            case "rename_workspace": .renameWorkspace(req: 9, window: 1, workspace: 2, name: "前端")
            case "list_dirs": .listDirs(req: 5, path: "/Users/ethan")
            case "list_dirs_home": .listDirs(req: 5, path: nil)
            case "list_project_tasks": .listProjectTasks(req: 5, dir: "/Users/ethan/dev/中文")
            case "spawn": .spawn(req: 3, size: size, cwd: nil, integration: .detect, start: true)
            case "attach_vt": .attach(id: id, size: nil, mode: .vtReplay)
            case "attach_meta": .attach(id: id, size: nil, mode: .metaOnly)
            case "attach_size": .attach(id: id, size: size, mode: .vtReplay)
            case "detach": .detach(id: id)
            case "resize": .resize(id: id, size: size)
            case "focus": .focus(id: id, focused: true)
            case "kill": .kill(id: id)
            case "read_screen": .readScreen(id: id, lines: 8)
            case "read_screen_command": .readScreen(id: id, lines: nil, command: 1)
            case "send_keys": .sendKeys(req: 5, id: id, keys: ["1", "enter", "esc", "up"])
            case "paste": .paste(req: 6, id: id, text: "继续，用方案 2\n")
            case "push_register":
                .pushRegister(
                    req: 10, token: "80f0c8b3a4e2d1c0ffeeddccbbaa99887766554433221100aabbccddeeff0011",
                    env: .production, bundle: "cn.barey.runode", machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF",
                    machineName: "Ethan 的 MacBook Pro")
            case "push_unregister":
                .pushRegister(
                    req: 11, token: nil, env: .development, bundle: "cn.barey.runode",
                    machine: "6F9619FF-8B86-D011-B42D-00C04FC964FF", machineName: "Ethan 的 MacBook Pro")
            default: .listSessions
            }
        #expect(try sameJSON(JSONEncoder().encode(message), RustSamples.data(name)))
    }

    @Test func settingsMatchTheHost() throws {
        #expect(try sameJSON(JSONEncoder().encode(TermSettings.default), RustSamples.data("settings_default")))
        var custom = TermSettings.default
        custom.cursorColor = .rgb(Rgb(1, 2, 3))
        custom.cursorText = .cellBackground
        custom.cursorBlink = false
        custom.cursorStyle = .blockHollow
        custom.optionAsAlt = .left
        custom.palette = [.init(index: 1, color: Rgb(255, 0, 0)), .init(index: 200, color: Rgb(0, 0, 255))]
        #expect(try sameJSON(JSONEncoder().encode(custom), RustSamples.data("settings_custom")))
        #expect(try JSONDecoder().decode(TermSettings.self, from: RustSamples.data("settings_custom")) == custom)
    }

    @Test func integrationModeMatchesTheHost() throws {
        #expect(try sameJSON(JSONEncoder().encode(IntegrationMode.force(.zsh)), RustSamples.data("integration_force")))
    }

    /// 手机能发的消息种类就是这些。宿主对手机发的 `shutdown`、交接、`ui_reply`、`set_options`、
    /// `set_theme` 只回错误，这里一个都不能有。给 `ClientMsg` 加种类时下面的 `switch` 编不过，逼着
    /// 回来对一遍这张表。
    @Test func mobileSendsOnlyAllowedKinds() throws {
        let samples: [ClientMsg] = [
            .hello(protocol: 4, build: "b", client: .mobile, caps: Caps(snapshot: false, vtReplay: true), session: nil, device: nil),
            .listSessions, .layout(req: 0), .open(req: 1, placement: .tab, near: id, cwd: "/tmp", focus: false),
            .openWorkspace(req: 1, dir: "/tmp", focus: false),
            .renameWorkspace(req: 1, window: 1, workspace: 1, name: "a"), .listDirs(req: 1, path: nil),
            .listProjectTasks(req: 1, dir: "/tmp"),
            .spawn(req: 1, size: size, cwd: nil, integration: .detect, start: true),
            .attach(id: id, size: nil, mode: .vtReplay), .detach(id: id), .resize(id: id, size: size),
            .focus(id: id, focused: true), .kill(id: id), .readScreen(id: id, lines: 3),
            .sendKeys(req: 1, id: id, keys: ["enter"]), .paste(req: 2, id: id, text: "y"),
            .git(req: 3, id: id, request: .status),
            .pushRegister(req: 4, token: "00", env: .production, bundle: "b", machine: "m", machineName: "n"),
        ]
        func covered(_ message: ClientMsg) -> Bool {
            switch message {
            case .hello, .listSessions, .layout, .open, .openWorkspace, .renameWorkspace, .listDirs, .listProjectTasks,
                .spawn, .attach, .detach, .resize, .focus, .kill, .readScreen, .sendKeys, .paste, .git, .pushRegister:
                true
            }
        }
        let forbidden: Set<String> = [
            "shutdown", "handoff", "handoff_ready", "handoff_abort", "handoff_done", "ui_reply", "set_options",
            "set_theme",
        ]
        for message in samples where covered(message) {
            let object = try JSONSerialization.jsonObject(with: JSONEncoder().encode(message)) as? [String: Any]
            let type = try #require(object?["type"] as? String)
            #expect(!forbidden.contains(type), "\(type)")
        }
    }

    @Test func helloGoesInAControlFrameOnChannelZero() throws {
        let frame = try Frame.control(ClientMsg.listSessions)
        #expect(frame.kind == .control)
        #expect(frame.channel == 0)
        #expect(String(decoding: frame.payload, as: UTF8.self) == #"{"type":"list_sessions"}"#)
    }
}

@Suite struct HostMessageTests {
    let id = SessionId("0123456789abcdef0011223344556677")!

    func decode(_ name: String) throws -> HostMsg {
        try JSONDecoder().decode(HostMsg.self, from: RustSamples.data(name))
    }

    @Test func welcomeAndIncompatible() throws {
        #expect(try decode("welcome") == .welcome)
        #expect(try decode("incompatible") == .incompatible(protocol: 5, build: "x", reason: "old"))
    }

    @Test func sessionListCarriesMeta() throws {
        guard case .sessionList(let sessions) = try decode("session_list") else {
            Issue.record("not a session list")
            return
        }
        let session = try #require(sessions.first)
        #expect(session.id == id)
        #expect(session.size == GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32))
        #expect(session.meta.title == "修 bug")
        #expect(session.meta.cwd == "/Users/ethan/中文")
        #expect(session.meta.agent == Agent(kind: AgentKind("claude"), state: .blocked))
        #expect(session.clients == 2)
        #expect(session.claimed)
        #expect(session.sizeOwner == "Ethan 的 MacBook")
    }

    @Test func attachedCarriesTheTheme() throws {
        guard case .attached(let attached) = try decode("attached") else {
            Issue.record("not attached")
            return
        }
        #expect(attached.channel == 7)
        #expect(attached.mode == .vtReplay)
        #expect(attached.settings == TermSettings.default)
        #expect(attached.meta.displayTitle == "修 bug")
    }

    @Test func streamMarkers() throws {
        let size = GridSize(cols: 80, rows: 24, cellWidthPx: 16, cellHeightPx: 32)
        #expect(try decode("snapshot_end") == .snapshotEnd(id: id))
        #expect(try decode("resized") == .resized(id: id, size: size))
        guard case .themeApplied(_, let settings) = try decode("theme_applied") else {
            Issue.record("not theme_applied")
            return
        }
        #expect(settings.cursorStyle == .blockHollow)
        #expect(settings.cursorText == .cellBackground)
        #expect(try decode("resync") == .resync(id: id, reason: "slow"))
        #expect(try decode("exited") == .exited(id: id, status: 1))
        #expect(try decode("exited_none") == .exited(id: id, status: nil))
        #expect(try decode("bell") == .bell(id: id))
        #expect(try decode("meta") == .meta(id: id, meta: SessionMeta()))
        #expect(try decode("size_owner") == .sizeOwner(id: id, mine: false, owner: "Ethan 的 MacBook"))
    }

    /// 布局按窗口、工作区、标签、分屏一层层读出来，带分屏在标签里的位置；旧电脑不报的目录读成空。
    @Test func layoutDescribesWorkspaces() throws {
        guard case .layout(let req, let windows) = try decode("layout") else {
            Issue.record("not a layout")
            return
        }
        #expect(req == 0)
        let window = try #require(windows.first)
        #expect(window.index == 1 && window.front)
        #expect(window.workspaces.map(\.name) == ["runode", "blog"])
        #expect(window.workspaces.map(\.dir) == ["/Users/ethan/dev/runode", nil])
        let runode = window.workspaces[0]
        #expect(runode.active)
        #expect(runode.tabs.map(\.active) == [false, true])
        let a = SessionId("11111111111111111111111111111111")!
        let b = SessionId("22222222222222222222222222222222")!
        #expect(runode.sessions == [id, a, b])
        #expect(runode.tabs[1].panes.map(\.rect) == [
            PaneRect(x: 0, y: 0, width: 500, height: 1000), PaneRect(x: 500, y: 0, width: 500, height: 1000),
        ])
        // 开新标签挨着当前标签里有焦点的分屏。
        #expect(runode.anchor == b)
        #expect(window.workspaces[1].anchor == nil)
        #expect(try decode("layout_changed") == .layoutChanged)
    }

    /// 项目命令按来源分组读出来；没有说明的读成空。
    @Test func projectTasksAreGroupedBySource() throws {
        guard case .projectTasks(let req, let dir, var sources) = try decode("project_tasks") else {
            Issue.record("not project tasks")
            return
        }
        #expect(req == 6)
        #expect(dir == "/Users/ethan/dev/app/web")
        #expect(sources.map(\.kind) == [.custom, .makefile, .packageJson])
        let custom = sources.removeFirst()
        #expect(custom.project == "/Users/ethan/dev/app")
        #expect(custom.tasks == [ProjectTask(name: "lint", command: "cargo clippy # slow", description: "cargo clippy # slow")])
        #expect(sources[0].project == nil)
        #expect(
            sources[0].tasks == [
                ProjectTask(name: "build", command: "make -C .. build", description: "编译全部"),
                ProjectTask(name: "test", command: "make -C .. test"),
            ])
        #expect(sources[0].file == "/Users/ethan/dev/app/Makefile")
        #expect(!sources[0].truncated && sources[1].truncated)
        #expect(sources[1].tasks == [ProjectTask(name: "dev", command: "pnpm run dev", description: "vite")])
    }

    @Test func repliesAndGoodbyes() throws {
        #expect(try decode("done") == .done(req: 9))
        #expect(try decode("spawned") == .spawned(req: 3, id: id))
        #expect(try decode("opened") == .opened(req: 7, id: id))
        #expect(
            try decode("dirs") == .dirs(req: 5, path: "/Users/ethan", dirs: [".config", "dev", "中文"], truncated: true))
        #expect(try decode("error") == .error(req: nil, id: id, message: "no session"))
        #expect(try decode("goodbye") == .goodbye(.error("bye")))
        #expect(try decode("goodbye_handoff") == .goodbye(.handoff))
        #expect(try decode("screen_text") == .screenText(id: id, text: "a\n", truncated: false))
        #expect(try decode("command_finished") == .commandFinished(id: id))
    }

    @Test func unknownKindsAndValuesAreTolerated() throws {
        let unknown = try JSONDecoder().decode(HostMsg.self, from: Data(#"{"type":"from_the_future","x":1}"#.utf8))
        #expect(unknown == .unknown("from_the_future"))
        let goodbye = try JSONDecoder().decode(
            HostMsg.self, from: Data(#"{"type":"goodbye","reason":{"kind":"reboot"}}"#.utf8))
        #expect(goodbye == .goodbye(.unknown("reboot")))
        // 宿主已经不发的原因也读成 `unknown`。
        let idle = try JSONDecoder().decode(HostMsg.self, from: Data(#"{"type":"goodbye","reason":{"kind":"idle"}}"#.utf8))
        #expect(idle == .goodbye(.unknown("idle")))
        // 认识的种类多了不认识的字段：忽略。
        let bell = try JSONDecoder().decode(
            HostMsg.self, from: Data(#"{"type":"bell","id":"0123456789abcdef0011223344556677","extra":true}"#.utf8))
        #expect(bell == .bell(id: id))
        // 不认识的项目命令来源读成 `unknown`，缺的 `truncated` 读成假。
        let tasks = try JSONDecoder().decode(
            HostMsg.self,
            from: Data(
                #"{"type":"project_tasks","req":1,"dir":"/a","sources":[{"kind":"justfile","file":"/a/justfile","tasks":[]}]}"#
                    .utf8))
        if case .projectTasks(_, _, let sources) = tasks {
            #expect(sources.map(\.kind) == [.unknown] && !sources[0].truncated)
        } else {
            Issue.record("not project tasks")
        }
        // 新的 agent 状态不让整条消息失败。
        let meta = try JSONDecoder().decode(
            HostMsg.self,
            from: Data(
                #"{"type":"meta","id":"0123456789abcdef0011223344556677","meta":{"agent":{"kind":"newbot","state":"thinking"}}}"#
                    .utf8))
        #expect(meta == .meta(id: id, meta: SessionMeta(agent: Agent(kind: AgentKind("newbot"), state: .unknown("thinking")))))
    }

    @Test func knownKindsMissingRequiredFieldsFail() {
        #expect(throws: (any Error).self) {
            try JSONDecoder().decode(HostMsg.self, from: Data(#"{"type":"bell"}"#.utf8))
        }
    }

    @Test func sessionIdsAreValidated() {
        #expect(SessionId("0123456789ABCDEF0011223344556677")?.rawValue == "0123456789abcdef0011223344556677")
        #expect(SessionId("xyz") == nil)
        #expect(throws: (any Error).self) {
            try JSONDecoder().decode(HostMsg.self, from: Data(#"{"type":"bell","id":"short"}"#.utf8))
        }
    }
}
