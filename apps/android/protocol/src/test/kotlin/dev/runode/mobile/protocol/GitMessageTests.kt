package dev.runode.mobile.protocol

import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import org.junit.Test

/** 读写 git 的消息和宿主序列化出来的一样，样例是 `messages.json` 里的 `git_*`。 */
class GitMessageTests {
    private val id = SessionId("0123456789abcdef0011223344556677")!!

    @Test
    fun requestsMatchTheHost() {
        val cases = listOf(
            Triple("git_status_request", 1u, GitRequest.Status),
            Triple("git_diff_request", 2u, GitRequest.Diff(path = "src/a.rs", staged = true)),
            Triple("git_unstage_request", 3u, GitRequest.Unstage(paths = listOf("b.rs", "a.rs"))),
            Triple("git_commit_request", 4u, GitRequest.Commit(message = "修好了", stageAll = true)),
            Triple("git_checkout_request", 5u, GitRequest.Checkout(branch = "origin/feat", remote = true)),
            Triple("git_discard_request", 6u, GitRequest.Discard(paths = listOf("a.rs"))),
        )
        for ((name, req, request) in cases) {
            val encoded = encodeToJson(ClientMsg.serializer(), ClientMsg.Git(req = req, id = id, request = request))
            assertTrue(sameJson(encoded, Samples.message(name)), "$name: $encoded")
        }
    }

    /** 没有参数的操作只写 `op`，和宿主的 `GitRequest` 一样。 */
    @Test
    fun bareOperations() {
        val cases = listOf(
            GitRequest.StageAll to "stage_all",
            GitRequest.UnstageAll to "unstage_all",
            GitRequest.Fetch to "fetch",
            GitRequest.Pull to "pull",
            GitRequest.Push to "push",
            GitRequest.Sync to "sync",
            GitRequest.Branches to "branches",
        )
        for ((request, op) in cases) {
            assertEquals(buildJsonObject { put("op", JsonPrimitive(op)) }, encodeToJson(GitRequest.serializer(), request))
        }
    }

    private fun decode(name: String): HostMsg = decode(HostMsg.serializer(), Samples.message(name))

    @Test
    fun status() {
        val message = assertIs<HostMsg.GitStatus>(decode("git_status"))
        assertEquals(1u, message.req)
        assertEquals(id, message.id)
        val status = assertNotNull(message.status)
        assertEquals("/Users/me/app", status.root)
        assertTrue(status.branch == "main" && status.head == "1a2b3c4" && status.upstream == "origin/main")
        assertTrue(status.ahead == 2u && status.behind == 1u && status.hasRemote)
        assertEquals(GitOperation.CherryPick, status.operation)
        assertEquals(
            listOf(
                GitFile(
                    path = "src/new.rs",
                    oldPath = "src/old.rs",
                    status = GitFileStatus.Renamed,
                    added = 3u,
                    removed = 1u,
                ),
            ),
            status.staged,
        )
        assertEquals(listOf(GitFile(path = "notes.txt", status = GitFileStatus.Untracked, added = 1u)), status.unstaged)
        assertEquals(HostMsg.GitStatus(req = 2u, id = id, status = null), decode("git_status_none"))
    }

    @Test
    fun diff() {
        val message = assertIs<HostMsg.GitDiff>(decode("git_diff"))
        assertEquals(3u, message.req)
        val diff = assertNotNull(message.diff)
        assertTrue(diff.file.path == "a.txt" && diff.file.status == GitFileStatus.Modified)
        assertFalse(diff.truncated)
        assertEquals("@@ -1,2 +1,2 @@ fn main", diff.hunks.first().header)
        assertEquals(
            listOf(
                GitLine(kind = GitLineKind.Context, old = 1u, new = 1u, text = "\tkeep"),
                GitLine(kind = GitLineKind.Removed, old = 2u, text = "old"),
                GitLine(kind = GitLineKind.Added, new = 2u, text = "new"),
            ),
            diff.hunks.first().lines,
        )
    }

    @Test
    fun branches() {
        assertEquals(
            HostMsg.GitBranches(
                req = 4u,
                id = id,
                branches = listOf(
                    GitBranch(
                        name = "main",
                        current = true,
                        upstream = "origin/main",
                        subject = "init",
                        date = "2 days ago",
                        time = 1_700_000_000,
                    ),
                    GitBranch(name = "origin/feat", remote = true, subject = "wip", date = "1 hour ago"),
                ),
            ),
            decode("git_branches"),
        )
    }

    /** 宿主新加的状态、操作、行的种类读成 `Unknown`，不让整条消息读不了。 */
    @Test
    fun newerValuesAreTolerated() {
        val json =
            """{"path":"x","status":"typechange","old_path":null,"added":0,"removed":0,"binary":false,"gitlink":false}"""
        assertEquals(GitFileStatus.Unknown("typechange"), decode(GitFile.serializer(), json).status)
        assertEquals(GitOperation.Unknown("bisect"), decode(GitOperation.serializer(), "\"bisect\""))
        assertEquals(GitLineKind.Unknown("moved"), decode(GitLineKind.serializer(), "\"moved\""))
    }
}
