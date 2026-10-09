import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

/// 菜单里的项目命令：按会话目录列一次，点了在会话里粘贴再回车，前台在跑别的程序时在新终端里跑。
@MainActor
@Suite struct ProjectTasksTests {
    let link = FakeLink()
    let dir = "/Users/ethan/dev/app"

    func info(_ id: SessionId, atPrompt: Bool = true, foreground: String? = nil, exited: Bool = false) -> SessionInfo {
        SessionInfo(
            id: id, size: smallGrid,
            meta: SessionMeta(title: "t", cwd: dir, foregroundIsShell: atPrompt, foreground: foreground), exited: exited)
    }

    func model(_ sessions: [SessionInfo]) -> SessionListModel {
        let model = SessionListModel(machine: machineRecord(), link: link)
        model.handle(.ready(generation: 1))
        model.handle(.message(.sessionList(sessions)))
        link.clearSent()
        return model
    }

    let make = TaskSource(
        kind: .makefile, file: "/Users/ethan/dev/app/Makefile",
        tasks: [ProjectTask(name: "build", command: "make build", description: "编译")])

    /// 一个目录只列一次：回话前、回话后再要都不发；下拉刷新时再列。
    @Test func eachDirectoryIsListedOnce() async {
        let model = model([info(sessionA)])
        await model.loadProjectTasks(in: dir)
        await model.loadProjectTasks(in: dir)
        guard case .listProjectTasks(let req, dir)? = link.sent.first, link.sent.count == 1 else {
            Issue.record("expected one list_project_tasks, got \(link.sent)")
            return
        }
        model.handle(.message(.projectTasks(req: req, dir: dir, sources: [make])))
        #expect(model.projectTasks(for: info(sessionA)) == [make])
        link.clearSent()
        await model.loadProjectTasks(in: dir)
        #expect(link.sent.isEmpty)
        await model.refreshProjectTasks()
        guard case .listProjectTasks(_, dir)? = link.sent.first else {
            Issue.record("expected a refresh, got \(link.sent)")
            return
        }
    }

    /// 列不出来的目录记成空的，不再要。
    @Test func errorsLeaveTheDirectoryEmpty() async {
        let model = model([info(sessionA)])
        await model.loadProjectTasks(in: dir)
        guard case .listProjectTasks(let req, _)? = link.sent.first else {
            Issue.record("expected a list_project_tasks, got \(link.sent)")
            return
        }
        model.handle(.message(.error(req: req, id: nil, message: "not a directory")))
        #expect(model.projectTasks[dir] == [])
        #expect(model.errorMessage == nil)
        link.clearSent()
        await model.loadProjectTasks(in: dir)
        #expect(link.sent.isEmpty)
    }

    /// 旧电脑不认识 `ListProjectTasks`，回不带编号的「unknown message」：这次连着时不再要。
    @Test func anOldComputerIsNotAskedAgain() async {
        let model = model([info(sessionA)])
        await model.loadProjectTasks(in: dir)
        model.handle(.message(.error(req: nil, id: nil, message: HostMsg.unknownMessage)))
        link.clearSent()
        await model.loadProjectTasks(in: "/tmp")
        #expect(link.sent.isEmpty)
        #expect(model.errorMessage == nil)
    }

    /// 点一条命令：粘贴命令行再回车。
    @Test func runningPastesTheCommandAndPressesEnter() async {
        let model = model([info(sessionA)])
        let ran = await model.runProjectTask(make.tasks[0], in: sessionA)
        #expect(ran)
        guard case .paste(_, sessionA, "make build")? = link.sent.first,
            case .sendKeys(_, sessionA, ["enter"])? = link.sent.last, link.sent.count == 2
        else {
            Issue.record("expected a paste and an enter, got \(link.sent)")
            return
        }
    }

    /// 项目的命令要在项目目录里跑：会话在项目的子目录里时，即使停在提示符上也在旁边开一个项目目录的
    /// 新终端跑，命令行原样粘贴；会话就在项目目录里时照旧在这个会话里跑。
    @Test func projectTasksRunInTheProjectDirectory() async {
        let model = model([info(sessionA)])
        let task = ProjectTask(name: "lint", command: "cargo clippy # slow", description: "cargo clippy # slow")
        #expect(await model.runProjectTask(task, at: dir, in: sessionA))
        link.clearSent()

        #expect(!(await model.runProjectTask(task, at: "/Users/ethan/dev", in: sessionA)))
        guard case .open(let req, .tab, sessionA, "/Users/ethan/dev", false)? = link.sent.first, link.sent.count == 1
        else {
            Issue.record("expected an open in the project directory, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.opened(req: req, id: sessionC)))
        let typed = link.sent.drop { if case .paste = $0 { false } else { true } }
        guard case .paste(_, sessionC, "cargo clippy # slow")? = typed.first else {
            Issue.record("expected the command pasted as written, got \(link.sent)")
            return
        }
    }

    /// 前台在跑别的程序（agent、vim）时在旁边开一个同目录的新终端跑，开好后打开它；终端结束了时不发。
    @Test func aBusySessionRunsTasksInANewTerminal() async {
        let busy = info(sessionA, atPrompt: false, foreground: "claude")
        let gone = info(sessionB, exited: true)
        let model = model([busy, gone])
        var spawned: [SessionId] = []
        model.onSpawned = { spawned.append($0) }
        #expect(!model.canRunProjectTask(in: gone))
        #expect(!(await model.runProjectTask(make.tasks[0], in: sessionB)))
        #expect(link.sent.isEmpty)

        #expect(!(await model.runProjectTask(make.tasks[0], in: sessionA)))
        guard case .open(let req, .tab, sessionA, dir, false)? = link.sent.first, link.sent.count == 1 else {
            Issue.record("expected an open beside the session, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.opened(req: req, id: sessionC)))
        let typed = link.sent.drop { if case .paste = $0 { false } else { true } }
        guard case .paste(_, sessionC, "make build")? = typed.first,
            case .sendKeys(_, sessionC, ["enter"])? = typed.dropFirst().first
        else {
            Issue.record("expected a paste and an enter in the new terminal, got \(link.sent)")
            return
        }
        #expect(spawned == [sessionC])
        #expect(!model.isSpawning)
        #expect(Presentation.projectTasksHeader(busy) == "运行 · 前台在跑 claude，在新终端里跑")
        #expect(Presentation.projectTasksHeader(gone) == "运行 · 终端已经结束")
        #expect(Presentation.projectTasksHeader(info(sessionA)) == "运行")
    }

    /// 电脑上的 app 没开着窗口时退回在会话目录里开后台会话，命令照样在那里跑。
    @Test func aBusySessionFallsBackToASpawnInItsDirectory() async {
        let model = model([info(sessionA, atPrompt: false, foreground: "claude")])
        await model.runProjectTask(make.tasks[0], in: sessionA)
        guard case .open(let openReq, _, _, _, _)? = link.sent.first else {
            Issue.record("expected an open, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.error(req: openReq, id: nil, message: "there is no runode window to do this in")))
        guard case .spawn(let req, _, dir, _, true)? = link.sent.first else {
            Issue.record("expected a spawn in the session's directory, got \(link.sent)")
            return
        }
        link.clearSent()
        model.handle(.message(.spawned(req: req, id: sessionC)))
        guard link.sent.contains(where: { if case .paste(_, sessionC, "make build") = $0 { true } else { false } }) else {
            Issue.record("expected a paste in the new terminal, got \(link.sent)")
            return
        }
    }

    /// 文件不在会话目录里时标出在哪一级。
    @Test func sourceTitlesTellWhereTheFileIs() {
        #expect(Presentation.taskSourceTitle(make, cwd: dir) == "Makefile")
        #expect(Presentation.taskSourceTitle(make, cwd: "/Users/ethan/dev/app/web/src") == "Makefile · ../..")
        #expect(Presentation.taskSourceTitle(make, cwd: "/Users/ethan/dev/application") == "Makefile")
        #expect(Presentation.taskSourceTitle(make, cwd: nil) == "Makefile")
        let custom = TaskSource(kind: .custom, file: "/Users/ethan/.runode/tasks.json", tasks: [])
        #expect(Presentation.taskSourceTitle(custom, cwd: "/Users/ethan/dev/app/web") == "我的命令")
        let global = TaskSource(kind: .global, file: "/Users/ethan/.runode/tasks.json", tasks: [])
        #expect(Presentation.taskSourceTitle(global, cwd: dir) == "通用命令")
    }
}
