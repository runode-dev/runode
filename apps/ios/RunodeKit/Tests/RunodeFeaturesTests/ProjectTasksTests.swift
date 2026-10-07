import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

/// 会话卡片长按菜单里的项目命令：按会话目录列一次，点了在会话里粘贴再回车，shell 不在提示符上时不发。
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

    /// 前台在跑别的程序、终端结束了时不发，节标题说明原因。
    @Test func onlyAShellAtItsPromptRunsTasks() async {
        let busy = info(sessionA, atPrompt: false, foreground: "vim")
        let gone = info(sessionB, exited: true)
        let model = model([busy, gone])
        #expect(!model.canRunProjectTask(in: busy))
        #expect(!(await model.runProjectTask(make.tasks[0], in: sessionA)))
        #expect(!(await model.runProjectTask(make.tasks[0], in: sessionB)))
        #expect(link.sent.isEmpty)
        #expect(Presentation.projectTasksHeader(busy, runnable: false) == "运行 · 前台在跑 vim，回到提示符后能用")
        #expect(Presentation.projectTasksHeader(gone, runnable: false) == "运行 · 终端已经结束")
        #expect(Presentation.projectTasksHeader(info(sessionA), runnable: true) == "运行")
    }

    /// 文件不在会话目录里时标出在哪一级。
    @Test func sourceTitlesTellWhereTheFileIs() {
        #expect(Presentation.taskSourceTitle(make, cwd: dir) == "Makefile")
        #expect(Presentation.taskSourceTitle(make, cwd: "/Users/ethan/dev/app/web/src") == "Makefile · ../..")
        #expect(Presentation.taskSourceTitle(make, cwd: "/Users/ethan/dev/application") == "Makefile")
        #expect(Presentation.taskSourceTitle(make, cwd: nil) == "Makefile")
    }
}
