package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.HostMsg

/**
 * 新建工作区时在手机上一级级浏览电脑上的目录：每进一个目录发一次 `ListDirs`，宿主回 `Dirs`，出错时回带着请求编号的
 * `Error`。只认最后一次请求的回话，点得快时前面的回话丢掉。选好以后由 [SessionListModel.createWorkspace] 去建。
 */
class DirectoryPickerModel(private val link: HostLink) {
    /** 现在列着的目录，宿主规范化过的绝对路径；第一次列好之前为空。 */
    var path by mutableStateOf<String?>(null)
        private set

    /** [path] 里的子目录名，按名字排好。 */
    var dirs by mutableStateOf<List<String>>(emptyList())
        private set

    /** 子目录太多，宿主只给了前面一部分。 */
    var truncated by mutableStateOf(false)
        private set

    var isLoading by mutableStateOf(false)
        private set

    var errorMessage by mutableStateOf<UiText?>(null)
        private set

    /** 连着电脑。会话列表在连接断开、连上时改它；只在连着时建选择器，所以一开始为真。 */
    var isConnected by mutableStateOf(true)
        internal set

    /** 显示以 `.` 开头的目录。 */
    var showsHidden by mutableStateOf(false)

    private var pending: UInt? = null

    /** 在等回话的那次要列的目录，出错时重试用。 */
    private var requested: String? = null

    /** 要显示的子目录：不显示隐藏目录时去掉以 `.` 开头的。 */
    val visibleDirs: List<String> get() = if (showsHidden) dirs else dirs.filterNot { it.startsWith(".") }

    /**
     * 上一级目录；已经在根目录（Windows 上是 `C:/` 这样的盘符根、`//server/share` 这样的共享根）或者还没列好时为空。
     * 按 `/` 自己切，UNC 路径开头的 `//` 原样留着。
     */
    val parent: String?
        get() {
            val path = path ?: return null
            if (path == "/" || isDriveRoot(path) || isShareRoot(path)) return null
            val slash = path.lastIndexOf('/')
            if (slash < 0) return null
            return path.substring(0, slash).ifEmpty { "/" }
        }

    /**
     * 能在 [path] 里新建：列好了、没在等回话、连着，也没有出错的提示盖着。进不去的子目录报错时 [path] 还是上次列好的
     * 目录，错误层盖住了列表，这时建下去会建在用户看不见的目录里。
     */
    val canChoose: Boolean get() = path != null && !isLoading && isConnected && errorMessage == null

    /** 列一个目录，[path] 为空时是家目录。 */
    fun load(path: String?) {
        requested = path
        if (!isConnected) {
            errorMessage = UiText.res(R.string.not_connected)
            return
        }
        val req = link.nextRequestId()
        pending = req
        isLoading = true
        errorMessage = null
        link.send(ClientMsg.ListDirs(req, path))
    }

    /** 进到现在这个目录里名叫 [name] 的子目录。 */
    fun enter(name: String) {
        val path = path ?: return
        load(if (path.endsWith("/")) path + name else "$path/$name")
    }

    fun goUp() {
        load(parent ?: return)
    }

    /** 收起出错的提示，回到上次列好的目录。 */
    fun dismissError() {
        errorMessage = null
    }

    /** 上次没列成：再列一次同一个目录。 */
    fun retry() {
        load(requested)
    }

    /** 连接断了：在等的回话不会来了，不再转圈，报出错，重新连上后可以重试。 */
    internal fun connectionLost() {
        isConnected = false
        if (pending == null) return
        pending = null
        isLoading = false
        errorMessage = UiText.res(R.string.not_connected)
    }

    /**
     * 是给自己的回话时处理掉并返回真。不认领不带编号的 `Error`：宿主对 `ListDirs` 出错一定回带编号的，不带编号的是
     * 同一条连接上别的请求的。
     */
    internal fun handle(message: HostMsg): Boolean {
        when (message) {
            is HostMsg.Dirs -> {
                if (pending == null || message.req != pending) return false
                pending = null
                isLoading = false
                path = message.path
                dirs = message.dirs
                truncated = message.truncated
                return true
            }
            is HostMsg.Error -> {
                if (pending == null || message.req != pending) return false
                pending = null
                isLoading = false
                errorMessage = Presentation.hostError(message)
                return true
            }
            else -> return false
        }
    }

    internal companion object {
        /** Windows 宿主回的盘符根：`C:/`。 */
        fun isDriveRoot(path: String): Boolean =
            path.length == 3 && path[0].let { it in 'a'..'z' || it in 'A'..'Z' } && path[1] == ':' && path[2] == '/'

        /** Windows 宿主回的共享根：`//server/share`（再往上不是能列的目录）。 */
        fun isShareRoot(path: String): Boolean =
            path.startsWith("//") && path.drop(2).split('/').filter { it.isNotEmpty() }.size <= 2
    }
}
