package dev.runode.mobile.protocol

import kotlinx.serialization.Required
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonObjectBuilder
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * 请宿主在一个会话所在的仓库里办的 git 操作，宿主的 `protocol::git::GitRequest`，放在 [ClientMsg.Git] 里发。
 * 宿主按会话 shell 当前的目录找仓库；改仓库的操作办完回改完后的 [GitStatus]。
 */
@Serializable(with = GitRequestSerializer::class)
sealed class GitRequest {
    data object Status : GitRequest()

    /** 一个文件在暂存段（[staged]）或未暂存段的逐行改动，回 [HostMsg.GitDiff]。 */
    data class Diff(val path: String, val staged: Boolean) : GitRequest()

    data class Stage(val paths: List<String>) : GitRequest()

    /** 暂存了的改名要连同旧路径一起给。 */
    data class Unstage(val paths: List<String>) : GitRequest()

    /** 丢掉这些文件还没暂存的改动，未跟踪的从电脑上删掉。宿主只认未暂存段里确实有的文件。 */
    data class Discard(val paths: List<String>) : GitRequest()

    data object StageAll : GitRequest()

    data object UnstageAll : GitRequest()

    /** [stageAll] 时先暂存所有改动再提交。 */
    data class Commit(val message: String, val stageAll: Boolean) : GitRequest()

    data object Fetch : GitRequest()

    data object Pull : GitRequest()

    data object Push : GitRequest()

    /** 先拉取再推送。 */
    data object Sync : GitRequest()

    /** 回 [HostMsg.GitBranches]。 */
    data object Branches : GitRequest()

    data class Checkout(val branch: String, val remote: Boolean) : GitRequest()
}

internal object GitRequestSerializer : EncodeOnlySerializer<GitRequest>("GitRequest") {
    override fun toJson(value: GitRequest): JsonElement = when (value) {
        GitRequest.Status -> op("status")
        is GitRequest.Diff -> op("diff") {
            put("path", value.path)
            put("staged", value.staged)
        }
        is GitRequest.Stage -> op("stage") { putStrings("paths", value.paths) }
        is GitRequest.Unstage -> op("unstage") { putStrings("paths", value.paths) }
        is GitRequest.Discard -> op("discard") { putStrings("paths", value.paths) }
        GitRequest.StageAll -> op("stage_all")
        GitRequest.UnstageAll -> op("unstage_all")
        is GitRequest.Commit -> op("commit") {
            put("message", value.message)
            put("stage_all", value.stageAll)
        }
        GitRequest.Fetch -> op("fetch")
        GitRequest.Pull -> op("pull")
        GitRequest.Push -> op("push")
        GitRequest.Sync -> op("sync")
        GitRequest.Branches -> op("branches")
        is GitRequest.Checkout -> op("checkout") {
            put("branch", value.branch)
            put("remote", value.remote)
        }
    }

    private fun op(name: String, fields: JsonObjectBuilder.() -> Unit = {}): JsonObject =
        buildJsonObject {
            put("op", name)
            fields()
        }
}

/** 仓库此刻的样子。 */
@Serializable
data class GitStatus(
    /** 仓库根目录，电脑上的绝对路径。 */
    val root: String,
    /** 当前分支；分离头指针时为空。 */
    val branch: String? = null,
    /** HEAD 的短哈希；还没有提交时为空。 */
    val head: String? = null,
    /** 上游分支，如 `origin/main`。 */
    val upstream: String? = null,
    val ahead: UInt = 0u,
    val behind: UInt = 0u,
    /** 配了至少一个远端。 */
    @SerialName("has_remote") val hasRemote: Boolean = false,
    /** 做到一半的合并之类。 */
    val operation: GitOperation? = null,
    /** 已暂存、未暂存（含未跟踪）两段的文件；部分暂存的文件两段里都有。 */
    val staged: List<GitFile> = emptyList(),
    val unstaged: List<GitFile> = emptyList(),
)

/** 做到一半、等着继续或放弃的操作。宿主新加的读成 [Unknown]。 */
@Serializable(with = GitOperationSerializer::class)
sealed class GitOperation(val wireName: String) {
    data object Merge : GitOperation("merge")

    data object Rebase : GitOperation("rebase")

    data object CherryPick : GitOperation("cherry_pick")

    data object Revert : GitOperation("revert")

    data class Unknown(val text: String) : GitOperation(text)
}

internal object GitOperationSerializer : WireNameSerializer<GitOperation>(
    "GitOperation",
    known = listOf(GitOperation.Merge, GitOperation.Rebase, GitOperation.CherryPick, GitOperation.Revert),
    wireName = GitOperation::wireName,
    unknown = GitOperation::Unknown,
)

/** 有改动的一个文件。 */
@Serializable
data class GitFile(
    /** 相对仓库根；删掉的文件是原来的路径。 */
    val path: String,
    /** 改名前的路径。 */
    @SerialName("old_path") val oldPath: String? = null,
    val status: GitFileStatus,
    val added: UInt = 0u,
    val removed: UInt = 0u,
    val binary: Boolean = false,
    /** 子模块那样记着一个提交号的条目。 */
    val gitlink: Boolean = false,
)

/** 文件的状态。宿主新加的读成 [Unknown]。 */
@Serializable(with = GitFileStatusSerializer::class)
sealed class GitFileStatus(val wireName: String) {
    data object Modified : GitFileStatus("modified")

    data object Added : GitFileStatus("added")

    data object Deleted : GitFileStatus("deleted")

    data object Renamed : GitFileStatus("renamed")

    data object Untracked : GitFileStatus("untracked")

    data object Conflicted : GitFileStatus("conflicted")

    data class Unknown(val text: String) : GitFileStatus(text)
}

internal object GitFileStatusSerializer : WireNameSerializer<GitFileStatus>(
    "GitFileStatus",
    known = listOf(
        GitFileStatus.Modified,
        GitFileStatus.Added,
        GitFileStatus.Deleted,
        GitFileStatus.Renamed,
        GitFileStatus.Untracked,
        GitFileStatus.Conflicted,
    ),
    wireName = GitFileStatus::wireName,
    unknown = GitFileStatus::Unknown,
)

/** 一个文件在一段里的逐行改动。 */
@Serializable
data class GitFileDiff(
    val file: GitFile,
    val hunks: List<GitHunk>,
    /** 改动太多，或者是没读内容的未跟踪文件，[hunks] 不全。 */
    val truncated: Boolean = false,
)

@Serializable
data class GitHunk(
    /** `@@ -a,b +c,d @@` 以及后面的函数名之类。 */
    val header: String,
    val lines: List<GitLine>,
)

@Serializable
data class GitLine(
    val kind: GitLineKind,
    /** 在旧文件和新文件里的行号；新增的行没有旧行号，删掉的行没有新行号。 */
    val old: UInt? = null,
    val new: UInt? = null,
    /** 去掉开头的 `+`、`-` 或空格，制表符换成了空格。 */
    val text: String,
)

/** 一行改动的种类。宿主新加的读成 [Unknown]，当作上下文行显示。 */
@Serializable(with = GitLineKindSerializer::class)
sealed class GitLineKind(val wireName: String) {
    data object Context : GitLineKind("context")

    data object Added : GitLineKind("added")

    data object Removed : GitLineKind("removed")

    data class Unknown(val text: String) : GitLineKind(text)
}

internal object GitLineKindSerializer : WireNameSerializer<GitLineKind>(
    "GitLineKind",
    known = listOf(GitLineKind.Context, GitLineKind.Added, GitLineKind.Removed),
    wireName = GitLineKind::wireName,
    unknown = GitLineKind::Unknown,
)

/** 分支列表里的一项。 */
@Serializable
data class GitBranch(
    /** 本地分支是 `main` 这样的名字，远端分支带上远端名，如 `origin/main`。 */
    val name: String,
    @Required val remote: Boolean = false,
    val current: Boolean = false,
    val upstream: String? = null,
    /** 最近一次提交的标题和相对时间（git 按英文写，如 `2 days ago`）。 */
    val subject: String = "",
    val date: String = "",
    /** 最近一次提交的提交时间，Unix 秒；按本机语言写相对时间用它，宿主没给时退回 [date]。 */
    val time: Long? = null,
)
