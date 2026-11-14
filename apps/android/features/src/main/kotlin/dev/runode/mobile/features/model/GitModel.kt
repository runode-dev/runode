package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.GitBranch
import dev.runode.mobile.protocol.GitFile
import dev.runode.mobile.protocol.GitFileDiff
import dev.runode.mobile.protocol.GitFileStatus
import dev.runode.mobile.protocol.GitRequest
import dev.runode.mobile.protocol.GitStatus
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.SessionId
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch

/** Git 页上一个文件的 diff：哪个文件、看的是暂存段还是未暂存段。 */
data class GitDiffTarget(val path: String, val staged: Boolean)

/**
 * 一个会话所在仓库的 Git 页：看分支和改动的文件、看一个文件的 diff、按文件暂存和撤回、提交、拉取推送、切分支。都经
 * `ClientMsg.Git` 请宿主在会话 shell 当前的目录里办，宿主按到达的先后一件一件办，改仓库的操作回改完后的状态。在主线程上用。
 *
 * 改仓库的操作一次只办一件（[running]），视图据此转圈、禁用别的按钮；读状态、读 diff、列分支不占它。宿主回带着请求编号的
 * `Error` 算这件没办成，接着重读一次状态。
 */
class GitModel(val sessionId: SessionId, private val link: HostLink) {
    /** 改仓库的操作，[running] 用它说正在办哪件。 */
    enum class Action {
        Stage,
        Unstage,
        Discard,
        Commit,
        Fetch,
        Pull,
        Push,
        Sync,
        Checkout,
    }

    private sealed class Pending {
        data object Status : Pending()

        data class Action(val action: GitModel.Action) : Pending()

        data class Diff(val target: GitDiffTarget) : Pending()

        data object Branches : Pending()
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** 仓库的状态；[loaded] 为真而它为空时，会话的目录不在 git 仓库里。 */
    var status by mutableStateOf<GitStatus?>(null)
        private set

    /** 读到过一次状态。 */
    var loaded by mutableStateOf(false)
        private set

    private var refreshState by mutableStateOf(false)

    /** 在等状态的回话。 */
    var isRefreshing: Boolean
        get() = refreshState
        private set(value) {
            refreshState = value
            if (value || refreshWaiters.isEmpty()) return
            val waiters = refreshWaiters.toList()
            refreshWaiters.clear()
            for (waiter in waiters) waiter.complete(Unit)
        }

    var running by mutableStateOf<Action?>(null)
        private set
    var linkState by mutableStateOf<LinkState>(LinkState.Idle)
        private set

    /** 连上了、能发请求（收到过这次连接的 `Ready`）。 */
    var isReady by mutableStateOf(false)
        private set

    /** 提交说明的输入框。 */
    var commitMessage by mutableStateOf("")
    var errorMessage by mutableStateOf<UiText?>(null)

    /** 分支列表，[loadBranches] 读来的。 */
    var branches by mutableStateOf<List<GitBranch>>(emptyList())
        private set
    var isLoadingBranches by mutableStateOf(false)
        private set

    /**
     * 打开着的 diff 和读到的内容；读到的为空、[isLoadingDiff] 为假、也没有 [diffError] 时，这个文件在那一段里已经没有改动
     * 了。
     */
    var diffTarget by mutableStateOf<GitDiffTarget?>(null)
        private set
    var diff by mutableStateOf<GitFileDiff?>(null)
        private set
    var isLoadingDiff by mutableStateOf(false)
        private set

    /** 打开着的 diff 没读成的原因，由 diff 页自己显示。 */
    var diffError by mutableStateOf<UiText?>(null)
        private set

    private var task: Job? = null
    private val pending = mutableMapOf<UInt, Pending>()

    /** [refreshAndWait] 在等的，状态读完（或者读不了、断线）时放开。 */
    private val refreshWaiters = mutableListOf<CompletableDeferred<Unit>>()

    /** 在办的提交交出去时输入框里的说明；提交成了时输入框还是它才清空，办着的时候用户改过的留着。 */
    private var submittedMessage: String? = null

    /** 仓库目录的名字，当页面标题。 */
    val repositoryName: String?
        get() {
            val root = status?.root
            if (root.isNullOrEmpty()) return null
            return root.trimEnd('/').substringAfterLast('/').ifEmpty { root }
        }

    val isClean: Boolean
        get() {
            val status = status ?: return true
            return status.staged.isEmpty() && status.unstaged.isEmpty()
        }

    /** 能提交：有说明、有改动、没别的操作在办。没有暂存的改动时提交全部改动（[commitsEverything]）。 */
    val canCommit: Boolean
        get() = running == null && isReady && !isClean && commitMessage.isNotBlank()

    /** 提交时会先暂存全部改动：暂存段是空的。 */
    val commitsEverything: Boolean
        get() = status?.staged?.isEmpty() ?: true

    /** 未暂存段里还有冲突的文件数。不为零时视图先请用户确认再全部暂存：暂存了它们就算解决了，冲突标记会跟着提交。 */
    val unstagedConflicts: Int
        get() = status?.unstaged?.count { it.status == GitFileStatus.Conflicted } ?: 0

    /** 开始看这个仓库：订阅连接的事件，连上了就读状态。连接本身的开、停归会话列表管。 */
    fun open() {
        if (task != null) return
        task = scope.launch { link.events().collect { handle(it) } }
    }

    /** 不看了：停止订阅。还在宿主那边办的操作照样办完，回话没人收。 */
    fun close() {
        task?.cancel()
        task = null
        pending.clear()
        running = null
        isRefreshing = false
    }

    // 用户操作

    /** 断线时立刻重连。 */
    fun reconnect() {
        scope.launch { link.reconnectNow() }
    }

    /** 重读状态；打开着 diff 时也重读它。 */
    fun refresh() {
        if (!isReady) return
        isRefreshing = true
        request(GitRequest.Status, Pending.Status)
        diffTarget?.let(::requestDiff)
    }

    /** 重读状态，等到回话、读不了或者断线再返回，给下拉刷新用。 */
    suspend fun refreshAndWait() {
        refresh()
        if (!isRefreshing) return
        val waiter = CompletableDeferred<Unit>()
        refreshWaiters += waiter
        waiter.await()
    }

    fun stage(file: GitFile) = stage(listOf(file))

    /** 暂存几个文件，以树形式查看时对整个目录用。 */
    fun stage(files: List<GitFile>) = perform(Action.Stage, GitRequest.Stage(files.map { it.path }))

    fun unstage(file: GitFile) = unstage(listOf(file))

    /** 撤回暂存；改名连同旧路径一起撤回，不然只撤回新路径那一半。 */
    fun unstage(files: List<GitFile>) =
        perform(Action.Unstage, GitRequest.Unstage(files.flatMap { listOfNotNull(it.path, it.oldPath) }))

    /** 丢掉未暂存段里这些文件的改动，找不回来；视图先请用户确认。 */
    fun discard(files: List<GitFile>) = perform(Action.Discard, GitRequest.Discard(files.map { it.path }))

    fun stageAll() = perform(Action.Stage, GitRequest.StageAll)

    fun unstageAll() = perform(Action.Unstage, GitRequest.UnstageAll)

    /** 提交暂存的改动；暂存段是空的时先暂存全部改动。提交成了才清空说明，办着的时候说明改过就不清。 */
    fun commit() {
        if (!canCommit) return
        submittedMessage = commitMessage
        perform(Action.Commit, GitRequest.Commit(commitMessage.trim(), stageAll = commitsEverything))
    }

    fun fetch() = perform(Action.Fetch, GitRequest.Fetch)

    fun pull() = perform(Action.Pull, GitRequest.Pull)

    fun push() = perform(Action.Push, GitRequest.Push)

    fun sync() = perform(Action.Sync, GitRequest.Sync)

    fun loadBranches() {
        if (!isReady) return
        isLoadingBranches = true
        request(GitRequest.Branches, Pending.Branches)
    }

    fun checkout(branch: GitBranch) {
        if (branch.current) return
        perform(Action.Checkout, GitRequest.Checkout(branch.name, branch.remote))
    }

    /** 打开一个文件的 diff。 */
    fun openDiff(target: GitDiffTarget) {
        diffTarget = target
        diff = null
        diffError = null
        requestDiff(target)
    }

    fun closeDiff() {
        diffTarget = null
        diff = null
        diffError = null
        isLoadingDiff = false
    }

    // 收发

    private fun perform(action: Action, request: GitRequest) {
        if (running != null || !isReady) return
        // 超过一条控制消息的上限电脑会断开整条连接，重试又断：一次选太多文件时不发，报出来。
        val largest = ClientMsg.Git(UInt.MAX_VALUE, sessionId, request)
        val size = try {
            Frame.control(largest).payload.size
        } catch (_: Exception) {
            null
        }
        if (size == null || size > Frame.maxControlPayload) {
            errorMessage = UiText.res(R.string.git_too_many_files)
            return
        }
        running = action
        errorMessage = null
        request(request, Pending.Action(action))
    }

    private fun requestDiff(target: GitDiffTarget) {
        if (!isReady) return
        isLoadingDiff = true
        diffError = null
        request(GitRequest.Diff(target.path, target.staged), Pending.Diff(target))
    }

    private fun request(request: GitRequest, kind: Pending) {
        // 断着时不发：记下的请求永远等不到回话，读状态的那条还会让 isRefreshing 一直为真，下拉刷新转个不停。
        if (!isReady) return
        val req = link.nextRequestId()
        pending[req] = kind
        link.send(ClientMsg.Git(req, sessionId, request))
    }

    internal fun handle(event: HostEvent) {
        when (event) {
            is HostEvent.State -> {
                linkState = event.state
                if (!event.state.isConnected) connectionLost()
            }
            is HostEvent.Ready -> {
                isReady = true
                refresh()
            }
            is HostEvent.Message -> handle(event.message)
            is HostEvent.Frame -> {}
        }
    }

    /** 看是不是回给这里的请求的；是的话处理掉，返回 true。 */
    internal fun handle(message: HostMsg): Boolean {
        when (message) {
            is HostMsg.GitStatus -> {
                val kind = pending.remove(message.req) ?: return false
                if (kind is Pending.Action) {
                    running = null
                    if (kind.action == Action.Commit && commitMessage == submittedMessage) commitMessage = ""
                    if (kind.action == Action.Checkout) branches = emptyList()
                    // 改了仓库：打开着的 diff 可能变了。
                    diffTarget?.let(::requestDiff)
                } else {
                    isRefreshing = pending.values.any { it == Pending.Status }
                }
                status = message.status
                loaded = true
            }
            is HostMsg.GitDiff -> {
                val kind = pending[message.req] as? Pending.Diff ?: return false
                pending.remove(message.req)
                if (kind.target != diffTarget) return true
                diff = message.diff
                isLoadingDiff = false
            }
            is HostMsg.GitBranches -> {
                if (pending[message.req] != Pending.Branches) return false
                pending.remove(message.req)
                branches = message.branches
                isLoadingBranches = false
            }
            is HostMsg.Error -> {
                val req = message.req ?: return false
                val kind = pending.remove(req) ?: return false
                failed(kind, message.message)
            }
            else -> return false
        }
        return true
    }

    private fun failed(kind: Pending, message: String) {
        when (kind) {
            Pending.Status -> {
                isRefreshing = false
                loaded = true
                errorMessage = UiText.res(R.string.git_status_failed, message)
            }
            is Pending.Action -> {
                running = null
                errorMessage = UiText.res(R.string.git_action_failed, Presentation.gitAction(kind.action), message)
                // 失败的操作可能做了一半（拉取合出了冲突）：重读状态。
                refresh()
            }
            is Pending.Diff -> {
                // 旧 diff 请求的错误不落到新打开的 diff 上。
                if (kind.target != diffTarget) return
                isLoadingDiff = false
                diffError = UiText.res(R.string.git_diff_failed, message)
            }
            Pending.Branches -> {
                isLoadingBranches = false
                errorMessage = UiText.res(R.string.git_branches_failed, message)
            }
        }
    }

    /** 连接断了：还没回话的请求不会再有回音。重新连上后 `Ready` 时重读。 */
    private fun connectionLost() {
        isReady = false
        pending.clear()
        if (running != null) {
            running = null
            errorMessage = UiText.res(R.string.git_connection_lost)
        }
        isRefreshing = false
        isLoadingDiff = false
        isLoadingBranches = false
    }
}
