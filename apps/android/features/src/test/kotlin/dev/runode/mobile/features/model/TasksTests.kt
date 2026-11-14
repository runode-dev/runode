package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.Placement
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionMeta
import dev.runode.mobile.protocol.TaskEntry
import dev.runode.mobile.protocol.TaskSource
import java.time.Instant
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import org.junit.Rule

/** 菜单里的命令：按会话目录列一次，点了在会话里粘贴再回车，前台在跑别的程序时在新终端里跑。 */
class TasksTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()
    private val dir = "/Users/ethan/dev/app"

    private fun info(id: SessionId, atPrompt: Boolean = true, foreground: String? = null, exited: Boolean = false) =
        SessionInfo(
            id,
            smallGrid,
            SessionMeta(title = "t", cwd = dir, foregroundIsShell = atPrompt, foreground = foreground),
            exited = exited,
        )

    private fun model(sessions: List<SessionInfo>): SessionListModel {
        val model = SessionListModel(machineRecord(), link)
        model.handle(HostEvent.Ready(1u))
        model.handle(HostEvent.Message(HostMsg.SessionList(sessions)))
        link.clearSent()
        return model
    }

    private fun lost() = HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now()))

    private val make = TaskSource(
        TaskSource.Kind.Makefile,
        "/Users/ethan/dev/app/Makefile",
        tasks = listOf(TaskEntry("build", "make build", "编译")),
    )

    /** 在新终端里打的字：从第一个粘贴起。 */
    private fun typed() = link.sent.dropWhile { it !is ClientMsg.Paste }

    /** 一个目录只列一次：回话前、回话后再要都不发；下拉刷新时再列。 */
    @Test fun eachDirectoryIsListedOnce() {
        val model = model(listOf(info(sessionA)))
        model.loadTasks(dir)
        model.loadTasks(dir)
        assertEquals(1, link.sent.size)
        val list = assertIs<ClientMsg.ListTasks>(link.sent.first())
        assertEquals(dir, list.dir)
        model.handle(HostEvent.Message(HostMsg.Tasks(list.req, dir, listOf(make))))
        assertEquals(listOf(make), model.tasks(info(sessionA)))
        link.clearSent()
        model.loadTasks(dir)
        assertTrue(link.sent.isEmpty())
        model.refreshTasks()
        assertEquals(dir, assertIs<ClientMsg.ListTasks>(link.sent.first()).dir)
    }

    /** 断线丢了回话的目录重新连上后再列；有结果的不重发。 */
    @Test fun reconnectingRelistsLostTasks() = runTest {
        val model = model(listOf(info(sessionA)))
        model.loadTasks(dir)
        assertIs<ClientMsg.ListTasks>(link.sent.first())
        model.handle(lost())
        link.clearSent()
        model.handle(HostEvent.Ready(2u))
        assertTrue(eventually { link.sent.any { it is ClientMsg.ListTasks && it.dir == dir } })
        val req = link.sent.filterIsInstance<ClientMsg.ListTasks>().last().req
        model.handle(HostEvent.Message(HostMsg.Tasks(req, dir, listOf(make))))

        model.handle(lost())
        link.clearSent()
        model.handle(HostEvent.Ready(3u))
        delay(50)
        assertTrue(ClientMsg.ListSessions in link.sent)
        assertTrue(link.sent.none { it is ClientMsg.ListTasks })
    }

    /** 列不出来的目录记成空的，不再要。 */
    @Test fun errorsLeaveTheDirectoryEmpty() {
        val model = model(listOf(info(sessionA)))
        model.loadTasks(dir)
        val req = assertIs<ClientMsg.ListTasks>(link.sent.first()).req
        model.handle(HostEvent.Message(HostMsg.Error(req, null, "not a directory")))
        assertEquals(emptyList(), model.tasks[dir])
        assertNull(model.errorMessage)
        link.clearSent()
        model.loadTasks(dir)
        assertTrue(link.sent.isEmpty())
    }

    /** 点一条命令：粘贴命令行再回车。 */
    @Test fun runningPastesTheCommandAndPressesEnter() {
        val model = model(listOf(info(sessionA)))
        assertTrue(model.runTask(make.tasks[0], id = sessionA))
        assertEquals(2, link.sent.size)
        val paste = assertIs<ClientMsg.Paste>(link.sent.first())
        assertEquals(sessionA to "make build", paste.id to paste.text)
        val enter = assertIs<ClientMsg.SendKeys>(link.sent.last())
        assertEquals(sessionA to listOf("enter"), enter.id to enter.keys)
    }

    /**
     * 项目的命令要在项目目录里跑：会话在项目的子目录里时，即使停在提示符上也在旁边开一个项目目录的新终端跑，命令行原样
     * 粘贴；会话就在项目目录里时照旧在这个会话里跑。
     */
    @Test fun tasksRunInTheProjectDirectory() {
        val model = model(listOf(info(sessionA)))
        val task = TaskEntry("lint", "cargo clippy # slow", "cargo clippy # slow")
        assertTrue(model.runTask(task, dir, sessionA))
        link.clearSent()

        assertFalse(model.runTask(task, "/Users/ethan/dev", sessionA))
        assertEquals(1, link.sent.size)
        val open = assertIs<ClientMsg.Open>(link.sent.first())
        assertEquals(ClientMsg.Open(open.req, Placement.Right, sessionA, "/Users/ethan/dev", false), open)
        link.clearSent()
        model.handle(HostEvent.Message(HostMsg.Opened(open.req, sessionC)))
        val paste = assertIs<ClientMsg.Paste>(typed().first())
        assertEquals(sessionC to "cargo clippy # slow", paste.id to paste.text)
    }

    /** 前台在跑别的程序（agent、vim）时在旁边开一个同目录的新终端跑，开好后打开它；终端结束了时不发。 */
    @Test fun aBusySessionRunsTasksInANewTerminal() {
        val busy = info(sessionA, atPrompt = false, foreground = "claude")
        val gone = info(sessionB, exited = true)
        val model = model(listOf(busy, gone))
        val spawned = mutableListOf<SessionId>()
        model.onSpawned = { spawned += it }
        assertFalse(model.canRunTask(gone))
        assertFalse(model.runTask(make.tasks[0], id = sessionB))
        assertTrue(link.sent.isEmpty())

        assertFalse(model.runTask(make.tasks[0], id = sessionA))
        assertEquals(1, link.sent.size)
        val open = assertIs<ClientMsg.Open>(link.sent.first())
        assertEquals(ClientMsg.Open(open.req, Placement.Right, sessionA, dir, false), open)
        link.clearSent()
        model.handle(HostEvent.Message(HostMsg.Opened(open.req, sessionC)))
        val paste = assertIs<ClientMsg.Paste>(typed()[0])
        assertEquals(sessionC to "make build", paste.id to paste.text)
        val enter = assertIs<ClientMsg.SendKeys>(typed()[1])
        assertEquals(sessionC to listOf("enter"), enter.id to enter.keys)
        assertEquals(listOf(sessionC), spawned)
        assertFalse(model.isSpawning)
        assertEquals("Run · claude is in the foreground, runs in a new terminal", Presentation.tasksHeader(busy).english)
        assertEquals("Run · terminal has ended", Presentation.tasksHeader(gone).english)
        assertEquals("Run", Presentation.tasksHeader(info(sessionA)).english)
    }

    /** 电脑上的 app 没开着窗口时退回在会话目录里开后台会话，命令照样在那里跑。 */
    @Test fun aBusySessionFallsBackToASpawnInItsDirectory() {
        val model = model(listOf(info(sessionA, atPrompt = false, foreground = "claude")))
        model.runTask(make.tasks[0], id = sessionA)
        val openReq = assertIs<ClientMsg.Open>(link.sent.first()).req
        link.clearSent()
        model.handle(HostEvent.Message(HostMsg.Error(openReq, null, "there is no Runode window to do this in")))
        val spawn = assertIs<ClientMsg.Spawn>(link.sent.first())
        assertEquals(dir, spawn.cwd)
        assertTrue(spawn.start)
        link.clearSent()
        model.handle(HostEvent.Message(HostMsg.Spawned(spawn.req, sessionC)))
        assertTrue(link.sent.any { it is ClientMsg.Paste && it.id == sessionC && it.text == "make build" })
    }

    /** 文件不在会话目录里时标出在哪一级。 */
    @Test fun sourceTitlesTellWhereTheFileIs() {
        assertEquals("Makefile", Presentation.taskSourceTitle(make, dir).english)
        assertEquals("Makefile · ../..", Presentation.taskSourceTitle(make, "/Users/ethan/dev/app/web/src").english)
        assertEquals("Makefile", Presentation.taskSourceTitle(make, "/Users/ethan/dev/application").english)
        assertEquals("Makefile", Presentation.taskSourceTitle(make, null).english)
        val custom = TaskSource(TaskSource.Kind.Custom, "/Users/ethan/.runode/tasks.json", tasks = emptyList())
        assertEquals("My Commands", Presentation.taskSourceTitle(custom, "/Users/ethan/dev/app/web").english)
        val global = TaskSource(TaskSource.Kind.Global, "/Users/ethan/.runode/tasks.json", tasks = emptyList())
        assertEquals("General Commands", Presentation.taskSourceTitle(global, dir).english)
    }
}
