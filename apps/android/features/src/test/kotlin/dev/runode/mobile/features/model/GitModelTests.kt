package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GitBranch
import dev.runode.mobile.protocol.GitFile
import dev.runode.mobile.protocol.GitFileDiff
import dev.runode.mobile.protocol.GitFileStatus
import dev.runode.mobile.protocol.GitHunk
import dev.runode.mobile.protocol.GitLine
import dev.runode.mobile.protocol.GitLineKind
import dev.runode.mobile.protocol.GitRequest
import dev.runode.mobile.protocol.GitStatus
import dev.runode.mobile.protocol.HostMsg
import java.time.Instant
import java.time.ZoneOffset
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.async
import kotlinx.coroutines.test.runTest
import org.junit.Rule

class GitModelTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()

    private fun status(
        staged: List<GitFile> = emptyList(),
        unstaged: List<GitFile> = emptyList(),
        head: String = "1a2b3c4",
    ) = GitStatus(
        root = "/Users/me/app",
        branch = "main",
        head = head,
        upstream = "origin/main",
        hasRemote = true,
        staged = staged,
        unstaged = unstaged,
    )

    private val changed = GitFile(path = "src/a.rs", status = GitFileStatus.Modified, added = 2u, removed = 1u)

    /** 连上的模型：已经收到 `Ready`，读状态的请求回了 [status]。 */
    private fun connectedModel(status: GitStatus?): GitModel {
        val model = GitModel(sessionA, link)
        model.handle(HostEvent.State(LinkState.Connected("mac", null)))
        model.handle(HostEvent.Ready(1u))
        val req = lastGit(GitRequest.Status)
        model.handle(HostMsg.GitStatus(req, sessionA, status))
        link.clearSent()
        return model
    }

    /** 最后发出去的一条 `Git` 的编号，要的是 [request]。 */
    private fun lastGit(request: GitRequest): UInt {
        val sent = assertIs<ClientMsg.Git>(link.sent.lastOrNull(), "expected a git request, got ${link.sent}")
        assertEquals(sessionA, sent.id)
        assertEquals(request, sent.request)
        return sent.req
    }

    /** 最后发出去的是不是要 [request] 的 `Git`。 */
    private fun sentLast(request: GitRequest): Boolean {
        val sent = link.sent.lastOrNull() as? ClientMsg.Git ?: return false
        return sent.id == sessionA && sent.request == request
    }

    private fun lost() = HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now()))

    @Test fun readsTheStatusOnceConnected() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        assertTrue(model.loaded)
        assertEquals(listOf(changed), model.status?.unstaged)
        assertEquals("app", model.repositoryName)
        assertFalse(model.isRefreshing)
    }

    @Test fun notARepository() {
        val model = connectedModel(null)
        assertTrue(model.loaded)
        assertNull(model.status)
    }

    /** 改仓库的操作一次一件：办着的时候别的操作不发，回了改完的状态才放开。 */
    @Test fun oneActionAtATime() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.stage(changed)
        val req = lastGit(GitRequest.Stage(listOf("src/a.rs")))
        assertEquals(GitModel.Action.Stage, model.running)
        model.push()
        assertEquals(1, link.sent.size)
        model.handle(HostMsg.GitStatus(req, sessionA, status(staged = listOf(changed))))
        assertNull(model.running)
        assertEquals(listOf(changed), model.status?.staged)
        assertEquals(emptyList(), model.status?.unstaged)
    }

    /** 撤回暂存的改名连同旧路径一起撤回。 */
    @Test fun unstagingARenameIncludesTheOldPath() {
        val renamed = GitFile(path = "new.rs", oldPath = "old.rs", status = GitFileStatus.Renamed)
        val model = connectedModel(status(staged = listOf(renamed)))
        model.unstage(renamed)
        lastGit(GitRequest.Unstage(listOf("new.rs", "old.rs")))
    }

    /** 放弃按路径请宿主办，占着 `running`。 */
    @Test fun discards() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.discard(listOf(changed))
        val req = lastGit(GitRequest.Discard(listOf("src/a.rs")))
        assertEquals(GitModel.Action.Discard, model.running)
        model.handle(HostMsg.GitStatus(req, sessionA, status()))
        assertTrue(model.running == null && model.isClean)
    }

    /** 一次选的文件多到一条消息装不下时不发，报出来。 */
    @Test fun tooManyFilesAreNotSent() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        val many = (0 until 40_000).map { GitFile(path = "src/generated/file-$it.rs", status = GitFileStatus.Modified) }
        model.stage(many)
        assertTrue(link.sent.isEmpty())
        assertNull(model.running)
        assertEquals("Too many files to send at once", model.errorMessage.english)
    }

    /** 没有暂存的改动时提交全部；提交成了才清空说明，没成时留着、报错并重读状态。 */
    @Test fun commits() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        assertFalse(model.canCommit)
        model.commitMessage = "  修好了 \n"
        assertTrue(model.canCommit && model.commitsEverything)
        model.commit()
        val failed = lastGit(GitRequest.Commit("修好了", stageAll = true))
        model.handle(HostMsg.Error(failed, null, "hook declined"))
        assertNull(model.running)
        assertEquals("Commit failed: hook declined", model.errorMessage.english)
        assertEquals("  修好了 \n", model.commitMessage)
        assertTrue(sentLast(GitRequest.Status))

        link.clearSent()
        model.errorMessage = null
        model.commit()
        val req = lastGit(GitRequest.Commit("修好了", stageAll = true))
        model.handle(HostMsg.GitStatus(req, sessionA, status(head = "5d6e7f8")))
        assertEquals("", model.commitMessage)
        assertTrue(model.isClean)
        assertFalse(model.canCommit)
    }

    /** 提交办着的时候改了说明（比如写下一条）：提交成了也不清掉改过的。 */
    @Test fun editsDuringACommitAreKept() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.commitMessage = "修好了"
        model.commit()
        val req = lastGit(GitRequest.Commit("修好了", stageAll = true))
        model.commitMessage = "下一条"
        model.handle(HostMsg.GitStatus(req, sessionA, status(head = "5d6e7f8")))
        assertNull(model.running)
        assertEquals("下一条", model.commitMessage)
    }

    /** 有暂存的改动时只提交暂存的。 */
    @Test fun commitsOnlyStagedChanges() {
        val model = connectedModel(status(staged = listOf(changed), unstaged = listOf(changed)))
        model.commitMessage = "只提交暂存的"
        assertFalse(model.commitsEverything)
        model.commit()
        lastGit(GitRequest.Commit("只提交暂存的", stageAll = false))
    }

    /** 打开 diff 读那一段的改动；回给已经关掉的 diff 的丢掉。 */
    @Test fun diffs() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        val target = GitDiffTarget("src/a.rs", staged = false)
        model.openDiff(target)
        val first = lastGit(GitRequest.Diff("src/a.rs", staged = false))
        assertTrue(model.isLoadingDiff)
        val other = GitDiffTarget("src/a.rs", staged = true)
        model.openDiff(other)
        val second = lastGit(GitRequest.Diff("src/a.rs", staged = true))
        val diff = GitFileDiff(changed, listOf(GitHunk("@@", listOf(GitLine(GitLineKind.Added, new = 1u, text = "x")))))
        model.handle(HostMsg.GitDiff(first, sessionA, diff))
        assertNull(model.diff)
        model.handle(HostMsg.GitDiff(second, sessionA, null))
        assertNull(model.diff)
        assertFalse(model.isLoadingDiff)
        assertEquals(other, model.diffTarget)
    }

    /** 读 diff 出错：错误归 diff 页显示，不当成没有改动；重读时清掉。 */
    @Test fun diffFailure() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.openDiff(GitDiffTarget("src/a.rs", staged = false))
        val req = lastGit(GitRequest.Diff("src/a.rs", staged = false))
        model.handle(HostMsg.Error(req, sessionA, "坏了"))
        assertNotNull(model.diffError)
        assertFalse(model.isLoadingDiff)
        assertNull(model.diff)
        assertNull(model.errorMessage)
        model.refresh()
        assertNull(model.diffError)
        assertTrue(model.isLoadingDiff)
    }

    /** 旧 diff 请求的错误不落到新打开的 diff 上，新的那个还在读。 */
    @Test fun staleDiffFailureIsIgnored() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.openDiff(GitDiffTarget("src/a.rs", staged = false))
        val first = lastGit(GitRequest.Diff("src/a.rs", staged = false))
        model.openDiff(GitDiffTarget("src/a.rs", staged = true))
        model.handle(HostMsg.Error(first, sessionA, "坏了"))
        assertNull(model.diffError)
        assertTrue(model.isLoadingDiff)
    }

    /** 数未暂存段里有冲突的文件，「全部暂存」据此先请用户确认。 */
    @Test fun countsUnstagedConflicts() {
        val conflicted = GitFile(path = "src/b.rs", status = GitFileStatus.Conflicted)
        val resolved = connectedModel(status(staged = listOf(conflicted), unstaged = listOf(changed)))
        assertEquals(0, resolved.unstagedConflicts)
        val other = GitFile(path = "src/c.rs", status = GitFileStatus.Conflicted)
        val model = connectedModel(status(unstaged = listOf(changed, conflicted, other)))
        assertEquals(2, model.unstagedConflicts)
    }

    /** 断着线打开 diff：不发请求，也不在读；diff 页据 `isReady` 显示没连上，不说没有改动。 */
    @Test fun diffWhileDisconnected() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.handle(lost())
        model.openDiff(GitDiffTarget("src/a.rs", staged = false))
        assertTrue(link.sent.isEmpty())
        assertFalse(model.isLoadingDiff)
        assertFalse(model.isReady)
    }

    @Test fun branchesAndCheckout() {
        val model = connectedModel(status())
        model.loadBranches()
        val req = lastGit(GitRequest.Branches)
        val main = GitBranch(name = "main", current = true)
        val feat = GitBranch(name = "origin/feat", remote = true)
        model.handle(HostMsg.GitBranches(req, sessionA, listOf(main, feat)))
        assertEquals(listOf(main, feat), model.branches)
        assertFalse(model.isLoadingBranches)

        link.clearSent()
        model.checkout(main)
        assertTrue(link.sent.isEmpty())
        model.checkout(feat)
        val checkout = lastGit(GitRequest.Checkout("origin/feat", remote = true))
        assertEquals(GitModel.Action.Checkout, model.running)
        model.handle(HostMsg.GitStatus(checkout, sessionA, status()))
        assertNull(model.running)
        assertEquals(emptyList(), model.branches)
    }

    /** 断线：在办的操作不会再有回话，放开并提醒；重新连上后重读。 */
    @Test fun connectionLoss() {
        val model = connectedModel(status(unstaged = listOf(changed)))
        model.pull()
        assertEquals(GitModel.Action.Pull, model.running)
        model.handle(lost())
        assertNull(model.running)
        assertFalse(model.isReady)
        assertNotNull(model.errorMessage)
        link.clearSent()
        model.refresh()
        assertTrue(link.sent.isEmpty())
        model.handle(HostEvent.Ready(2u))
        assertTrue(sentLast(GitRequest.Status))
    }

    /** 下拉刷新等到状态回话才返回。 */
    @Test fun refreshWaitsForTheAnswer() = runTest {
        val model = connectedModel(status())
        val refreshing = async { model.refreshAndWait() }
        assertTrue(eventually { sentLast(GitRequest.Status) })
        assertTrue(model.isRefreshing)
        assertFalse(refreshing.isCompleted)
        val req = lastGit(GitRequest.Status)
        model.handle(HostMsg.GitStatus(req, sessionA, status(unstaged = listOf(changed))))
        refreshing.await()
        assertEquals(listOf(changed), model.status?.unstaged)
    }

    /** 下拉刷新时断了线也返回，不一直转。 */
    @Test fun refreshStopsWaitingWhenTheLinkDrops() = runTest {
        val model = connectedModel(status())
        val refreshing = async { model.refreshAndWait() }
        assertTrue(eventually { model.isRefreshing })
        model.handle(lost())
        refreshing.await()
        assertFalse(model.isRefreshing)
    }

    /** 别的请求的回话和错误不归它管。 */
    @Test fun ignoresOtherReplies() {
        val model = connectedModel(status())
        assertFalse(model.handle(HostMsg.GitStatus(999u, sessionA, null)))
        assertFalse(model.handle(HostMsg.Error(999u, null, "x")))
        assertNull(model.errorMessage)
        assertNotNull(model.status)
    }

    /** 订阅连接，连上了就读状态。 */
    @Test fun openingSubscribesToTheLink() = runTest {
        val model = GitModel(sessionA, link)
        model.open()
        assertTrue(eventually { link.subscriberCount == 1 })
        link.emit(HostEvent.State(LinkState.Connected("mac", null)))
        link.emit(HostEvent.Ready(1u))
        assertTrue(eventually { sentLast(GitRequest.Status) })
        model.close()
    }
}

class GitFileTreeTests {
    private fun files(vararg paths: String) = paths.map { GitFile(path = it, status = GitFileStatus.Modified) }

    private fun shape(items: List<GitTreeItem>): List<String> = items.map {
        when (it) {
            is GitTreeItem.Directory -> "${it.depth} ${it.name}/ ${it.files.size}${if (it.expanded) "" else " +"}"
            is GitTreeItem.File -> "${it.depth} ${it.file.path}"
        }
    }

    @Test fun groupsFilesByDirectory() {
        val tree = Presentation.gitFileTree(
            files("src/main.rs", "README.md", "src/a/b.rs", "Cargo.toml", "src/A.rs", "docs/x.md"),
            collapsed = emptySet(),
        )
        // 目录在前、文件在后，各自按名字排，不分大小写；只有一个文件的目录不并。
        assertEquals(
            listOf(
                "0 docs/ 1", "1 docs/x.md", "0 src/ 3", "1 a/ 1", "2 src/a/b.rs", "1 src/A.rs", "1 src/main.rs",
                "0 Cargo.toml", "0 README.md",
            ),
            shape(tree),
        )
    }

    @Test fun compactsSingleChildDirectoriesAndCollapses() {
        val changed = files("crates/desktop/src/a.rs", "crates/desktop/src/b/c.rs", "crates/desktop/src/b/d.rs")
        assertEquals(
            listOf(
                "0 crates/desktop/src/ 3", "1 b/ 2", "2 crates/desktop/src/b/c.rs", "2 crates/desktop/src/b/d.rs",
                "1 crates/desktop/src/a.rs",
            ),
            shape(Presentation.gitFileTree(changed, collapsed = emptySet())),
        )
        // 收起按并成一行后最深的那个目录认，下面的不排。
        assertEquals(
            listOf("0 crates/desktop/src/ 3 +"),
            shape(Presentation.gitFileTree(changed, collapsed = setOf("crates/desktop/src"))),
        )
    }
}

/** 成对改动的行里改了哪些字，和电脑上 `changed_words` 的测试对得上。 */
class GitWordChangesTests {
    private fun changed(old: String, new: String): Pair<List<String>, List<String>> {
        val (a, b) = Presentation.gitChangedWords(old, new)
        return a.map { old.substring(it) } to b.map { new.substring(it) }
    }

    @Test fun marksOnlyTheChangedWords() {
        assertEquals(listOf("+ 1") to listOf("* 2"), changed("let total = count + 1;", "let total = count * 2;"))
        assertEquals(emptyList<String>() to listOf(", baz"), changed("foo(bar)", "foo(bar, baz)"))
    }

    @Test fun chineseIsComparedCharByChar() {
        assertEquals(
            listOf("只", "一") to listOf("两", "，再关掉"),
            changed("改动现在只有一处：实现。", "改动现在有两处：实现，再关掉。"),
        )
    }

    @Test fun comparesBytesNotCanonicalForms() {
        // é 写成一个字和写成 e 加尖音符，看着一样，字节不同，要标出来。
        val (a, b) = Presentation.gitChangedWords("let title = \"é\";", "let title = \"é\";")
        assertTrue(a.size == 1 && b.size == 1)
    }

    @Test fun tabsAreComparedAsWritten() {
        assertEquals(listOf("1") to listOf("2"), changed("\tx = 1", "\tx = 2"))
        // 缩进从空格换成制表符算改了。
        assertEquals(listOf("    ") to listOf("\t"), changed("    let x = 1;", "\tlet x = 1;"))
    }

    @Test fun displayExpandsTabsToTabStops() {
        val text = "\tab\tc"
        assertEquals(listOf("    ab  c"), Presentation.gitDisplaySegments(text, emptyList()).map { it.text })
        // 改了的字跟着换到展开后的文字上，前面的制表位照算。
        val marked = Presentation.gitDisplaySegments(text, listOf(text.length - 1 until text.length))
        assertEquals(listOf("    ab  ", "c"), marked.map { it.text })
        assertEquals(listOf(false, true), marked.map { it.changed })
    }

    @Test fun rewrittenLinesAreNotMarked() {
        assertEquals(emptyList<String>() to emptyList(), changed("fn alpha() {}", "let beta = gamma;"))
    }

    @Test fun pairsRemovedWithAddedLines() {
        val hunk = GitHunk(
            "@@ -1,2 +1,2 @@",
            listOf(
                GitLine(GitLineKind.Removed, old = 1u, text = "a longer new line"),
                GitLine(GitLineKind.Added, new = 1u, text = "a much longer new line"),
                GitLine(GitLineKind.Context, old = 2u, new = 2u, text = "b"),
                GitLine(GitLineKind.Added, new = 3u, text = "c"),
            ),
        )
        val words = Presentation.gitWordChanges(hunk)
        assertEquals(listOf(0, 1, 0, 0), words.map { it.size })
        assertEquals("much ", hunk.lines[1].text.substring(words[1][0]))
    }
}

class GitBranchDetailTests {
    /** 宿主给了时间戳时按界面文字的语言写多久以前。 */
    @Test fun writesTheTimeInTheInterfaceLanguage() {
        val now = Instant.ofEpochSecond(1_700_000_000)
        val branch = GitBranch(name = "main", subject = "init", date = "3 days ago", time = 1_700_000_000L - 3 * 86_400)
        assertEquals("init · 3天前", Presentation.gitBranchDetail(branch, now, ZoneOffset.UTC).simplifiedChinese)
        assertEquals("init · 3 days ago", Presentation.gitBranchDetail(branch, now, ZoneOffset.UTC).english)
        val hour = branch.copy(time = 1_700_000_000L - 3_600)
        assertEquals("init · 1小时前", Presentation.gitBranchDetail(hour, now, ZoneOffset.UTC).simplifiedChinese)
        assertEquals("init · 1 hour ago", Presentation.gitBranchDetail(hour, now, ZoneOffset.UTC).english)
    }

    /** 宿主没给时间戳时用 git 的原话；什么都没有时只剩标题。 */
    @Test fun fallsBackToTheHostsWords() {
        assertEquals(
            "init · 3 days ago",
            Presentation.gitBranchDetail(GitBranch(name = "main", subject = "init", date = "3 days ago")).english,
        )
        assertEquals("init", Presentation.gitBranchDetail(GitBranch(name = "main", subject = "init")).english)
    }
}

class GitPresentationTests {
    @Test fun upstreamCountsAndFallbacks() {
        val synced = GitStatus(root = "/r", upstream = "origin/main", hasRemote = true)
        assertEquals("origin/main · Synced", Presentation.gitUpstream(synced).english)
        assertEquals("origin/main · ↑2 ↓1", Presentation.gitUpstream(synced.copy(ahead = 2u, behind = 1u)).english)
        assertEquals("No upstream yet; it is set when you push", Presentation.gitUpstream(synced.copy(upstream = null)).english)
        assertEquals("No remote configured", Presentation.gitUpstream(GitStatus(root = "/r")).english)
    }

    @Test fun branchLine() {
        assertEquals("main", Presentation.gitBranch(GitStatus(root = "/r", branch = "main", head = "abc")).english)
        assertEquals("Detached at abc", Presentation.gitBranch(GitStatus(root = "/r", head = "abc")).english)
        assertEquals("No commits yet", Presentation.gitBranch(GitStatus(root = "/r")).english)
    }

    @Test fun pathParts() {
        assertEquals("a.rs" to "src/x", Presentation.gitPathParts("src/x/a.rs"))
        assertEquals("a.rs" to null, Presentation.gitPathParts("a.rs"))
    }
}
