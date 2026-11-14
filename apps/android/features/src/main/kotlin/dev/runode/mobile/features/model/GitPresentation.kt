package dev.runode.mobile.features.model

import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.GitBranch
import dev.runode.mobile.protocol.GitFile
import dev.runode.mobile.protocol.GitFileStatus
import dev.runode.mobile.protocol.GitHunk
import dev.runode.mobile.protocol.GitLineKind
import dev.runode.mobile.protocol.GitOperation
import dev.runode.mobile.protocol.GitStatus
import java.text.BreakIterator
import java.time.Instant
import java.time.ZoneId
import java.time.ZonedDateTime
import java.time.temporal.ChronoUnit
import java.util.Locale

// Git 页上给人看的文字。

/** 文件状态的单字母标记，要和电脑上 Git 面板用的字母相同。 */
fun Presentation.gitLetter(status: GitFileStatus): String = when (status) {
    GitFileStatus.Modified -> "M"
    GitFileStatus.Added -> "A"
    GitFileStatus.Deleted -> "D"
    GitFileStatus.Renamed -> "R"
    GitFileStatus.Untracked -> "U"
    GitFileStatus.Conflicted -> "!"
    is GitFileStatus.Unknown -> "?"
}

/** 文件状态给 TalkBack 读的名字。 */
fun Presentation.gitStatusName(status: GitFileStatus): UiText = when (status) {
    GitFileStatus.Modified -> UiText.res(R.string.git_file_modified)
    GitFileStatus.Added -> UiText.res(R.string.git_file_added)
    GitFileStatus.Deleted -> UiText.res(R.string.git_file_deleted)
    GitFileStatus.Renamed -> UiText.res(R.string.git_file_renamed)
    GitFileStatus.Untracked -> UiText.res(R.string.git_file_untracked)
    GitFileStatus.Conflicted -> UiText.res(R.string.git_file_conflicted)
    is GitFileStatus.Unknown -> UiText.Raw(status.text)
}

/** 文件名和它所在的目录（相对仓库根，根目录下的为空）。 */
fun Presentation.gitPathParts(path: String): Pair<String, String?> {
    val slash = path.lastIndexOf('/')
    if (slash < 0) return path to null
    return path.substring(slash + 1) to path.substring(0, slash)
}

/**
 * 分支列表里一个分支的小字：最近一次提交的标题和多久以前。宿主给了时间戳时按界面的语言写（取自 strings.xml，跟着界面文字
 * 走，不按系统的地区设置），没给时用 git 的英文原话。多久以前按 [zone] 的日历取最大的一个单位。
 */
fun Presentation.gitBranchDetail(
    branch: GitBranch,
    now: Instant = Instant.now(),
    zone: ZoneId = ZoneId.systemDefault(),
): UiText {
    val subject = branch.subject.takeIf { it.isNotEmpty() }?.let(UiText::Raw)
    val time = branch.time
    val whenText = if (time != null) relativeTime(Instant.ofEpochSecond(time), now, zone) else
        branch.date.takeIf { it.isNotEmpty() }?.let(UiText::Raw)
    return when {
        subject != null && whenText != null -> UiText.res(R.string.detail_separator, subject, whenText)
        else -> subject ?: whenText ?: UiText.Raw("")
    }
}

/** [then] 比 [now] 早多久，取最大的一个单位；比 [now] 晚（两边时钟不准）时当作刚刚。 */
private fun relativeTime(then: Instant, now: Instant, zone: ZoneId): UiText {
    val from = ZonedDateTime.ofInstant(minOf(then, now), zone)
    val to = ZonedDateTime.ofInstant(now, zone)
    for ((unit, id) in relativeUnits) {
        val count = unit.between(from, to)
        if (count > 0) return UiText.plural(id, count.toInt(), count.toInt())
    }
    return UiText.plural(R.plurals.time_seconds_ago, 0, 0)
}

private val relativeUnits = listOf(
    ChronoUnit.YEARS to R.plurals.time_years_ago,
    ChronoUnit.MONTHS to R.plurals.time_months_ago,
    ChronoUnit.WEEKS to R.plurals.time_weeks_ago,
    ChronoUnit.DAYS to R.plurals.time_days_ago,
    ChronoUnit.HOURS to R.plurals.time_hours_ago,
    ChronoUnit.MINUTES to R.plurals.time_minutes_ago,
    ChronoUnit.SECONDS to R.plurals.time_seconds_ago,
)

/** 分支那一行：分支名，分离头指针时写短哈希，还没有提交时写「还没有提交」。 */
fun Presentation.gitBranch(status: GitStatus): UiText {
    status.branch?.let { return UiText.Raw(it) }
    status.head?.let { return UiText.res(R.string.git_detached, it) }
    return UiText.res(R.string.git_no_commits)
}

/** 和上游差几个提交：`origin/main · ↑2 ↓1`、`origin/main · 已同步`；没有上游时说明能不能推。 */
fun Presentation.gitUpstream(status: GitStatus): UiText {
    val upstream = status.upstream
        ?: return UiText.res(if (status.hasRemote) R.string.git_no_upstream else R.string.git_no_remote)
    val counts = buildList {
        if (status.ahead > 0u) add("↑${status.ahead}")
        if (status.behind > 0u) add("↓${status.behind}")
    }
    val detail = if (counts.isEmpty()) UiText.res(R.string.git_synced) else UiText.Raw(counts.joinToString(" "))
    return UiText.res(R.string.detail_separator, upstream, detail)
}

/** 做到一半的操作的提醒；没有时为空。 */
fun Presentation.gitOperation(operation: GitOperation?): UiText? {
    val name = when (operation) {
        null -> return null
        GitOperation.Merge -> UiText.res(R.string.git_operation_merge)
        GitOperation.Rebase -> UiText.res(R.string.git_operation_rebase)
        GitOperation.CherryPick -> UiText.res(R.string.git_operation_cherry_pick)
        GitOperation.Revert -> UiText.res(R.string.git_operation_revert)
        is GitOperation.Unknown -> UiText.Raw(operation.text)
    }
    return UiText.res(R.string.git_operation_in_progress, name)
}

/** 改仓库的操作的名字。 */
fun Presentation.gitAction(action: GitModel.Action): UiText = UiText.res(
    when (action) {
        GitModel.Action.Stage -> R.string.git_action_stage
        GitModel.Action.Unstage -> R.string.git_action_unstage
        GitModel.Action.Discard -> R.string.git_action_discard
        GitModel.Action.Commit -> R.string.git_action_commit
        GitModel.Action.Fetch -> R.string.git_action_fetch
        GitModel.Action.Pull -> R.string.git_action_pull
        GitModel.Action.Push -> R.string.git_action_push
        GitModel.Action.Sync -> R.string.git_action_sync
        GitModel.Action.Checkout -> R.string.git_action_checkout
    },
)

/** 正在办的操作，转圈旁边的字。每个动作单写一句，别的语言里「正在」没法和动作名拼起来。 */
fun Presentation.gitRunning(action: GitModel.Action): UiText = UiText.res(
    when (action) {
        GitModel.Action.Stage -> R.string.git_running_stage
        GitModel.Action.Unstage -> R.string.git_running_unstage
        GitModel.Action.Discard -> R.string.git_running_discard
        GitModel.Action.Commit -> R.string.git_running_commit
        GitModel.Action.Fetch -> R.string.git_running_fetch
        GitModel.Action.Pull -> R.string.git_running_pull
        GitModel.Action.Push -> R.string.git_running_push
        GitModel.Action.Sync -> R.string.git_running_sync
        GitModel.Action.Checkout -> R.string.git_running_checkout
    },
)

/** Git 页以树形式查看时的一行。 */
sealed class GitTreeItem {
    /**
     * 目录：[path] 相对仓库根，并成一行的是链条最深的那个；[name] 是显示的名字（并成一行的写成 `a/b/c`）；[files] 是它
     * 下面所有改动的文件，对整个目录暂存、取消暂存用。
     */
    data class Directory(
        val path: String,
        val name: String,
        val depth: Int,
        val expanded: Boolean,
        val files: List<GitFile>,
    ) : GitTreeItem()

    data class File(val file: GitFile, val depth: Int) : GitTreeItem()
}

/**
 * 把一段改动的文件按目录分层排成行，排法要和电脑上 Git 面板的树形式（`file_tree`）相同：目录在前、文件在后，各自按名字
 * 排（不分大小写）；只有一个子目录、没有文件的目录和子目录并成一行。[collapsed] 里的目录（并成一行的按最深的那个认）收起，
 * 下面的不排。
 */
fun Presentation.gitFileTree(files: List<GitFile>, collapsed: Set<String>): List<GitTreeItem> {
    val root = GitTreeNode()
    for (file in files) {
        val parts = file.path.split('/').filter { it.isNotEmpty() }
        val name = parts.lastOrNull() ?: continue
        var node = root
        for (part in parts.dropLast(1)) node = node.child(part)
        node.files += name to file
    }
    val items = mutableListOf<GitTreeItem>()
    root.push("", 0, collapsed, items)
    return items
}

private class GitTreeNode {
    val dirs = LinkedHashMap<String, GitTreeNode>()
    val files = mutableListOf<Pair<String, GitFile>>()

    fun child(name: String): GitTreeNode = dirs.getOrPut(name) { GitTreeNode() }

    val allFiles: List<GitFile> get() = files.map { it.second } + dirs.values.flatMap { it.allFiles }

    fun push(dir: String, depth: Int, collapsed: Set<String>, items: MutableList<GitTreeItem>) {
        for (name in dirs.keys.sortedWith(byName)) {
            var node = dirs.getValue(name)
            var path = if (dir.isEmpty()) name else "$dir/$name"
            var label = name
            while (node.files.isEmpty() && node.dirs.size == 1) {
                val (next, child) = node.dirs.entries.first()
                path += "/$next"
                label += "/$next"
                node = child
            }
            val expanded = path !in collapsed
            items += GitTreeItem.Directory(path, label, depth, expanded, node.allFiles)
            if (expanded) node.push(path, depth + 1, collapsed, items)
        }
        for ((_, file) in files.sortedWith(compareBy(byName) { it.first })) {
            items += GitTreeItem.File(file, depth)
        }
    }

    private companion object {
        val byName = Comparator<String> { a, b ->
            val order = a.lowercase().compareTo(b.lowercase())
            if (order != 0) order else a.compareTo(b)
        }
    }
}

/** diff 里一行显示出来的一段，[changed] 时是改了的字。 */
data class GitSegment(val text: String, val changed: Boolean)

/**
 * 一块里紧跟着的一串删掉的行和一串加的行按先后一一配对，各自真正改了的字（[gitChangedWords]），和 `hunk.lines` 一一对应；
 * 配不上对的行为空。
 */
fun Presentation.gitWordChanges(hunk: GitHunk): List<List<IntRange>> {
    val lines = hunk.lines
    val words = MutableList(lines.size) { emptyList<IntRange>() }
    fun run(from: Int, kind: GitLineKind): IntRange {
        var end = from
        while (end < lines.size && lines[end].kind == kind) end += 1
        return from until end
    }
    var ix = 0
    while (ix < lines.size) {
        val removed = run(ix, GitLineKind.Removed)
        val added = run(removed.last + 1, GitLineKind.Added)
        for ((old, new) in removed.zip(added)) {
            val (a, b) = gitChangedWords(lines[old].text, lines[new].text)
            words[old] = a
            words[new] = b
        }
        ix = maxOf(added.last + 1, ix + 1)
    }
    return words
}

/**
 * diff 里一行显示出来的样子，切成几段，每段标着是不是改了的字（[words]，[text] 里的范围）：制表符按 [gitTabWidth] 列一个
 * 的制表位展开成空格，一个字（字位簇）算一列。
 */
fun Presentation.gitDisplaySegments(text: String, words: List<IntRange>): List<GitSegment> {
    var column = 0
    fun expand(part: String): String = buildString {
        for (c in graphemes(part)) {
            val cluster = part.substring(c)
            if (cluster == "\t") {
                val width = gitTabWidth - column % gitTabWidth
                repeat(width) { append(' ') }
                column += width
            } else {
                append(cluster)
                column += 1
            }
        }
    }
    val segments = mutableListOf<GitSegment>()
    var at = 0
    for (range in words) {
        if (at < range.first) segments += GitSegment(expand(text.substring(at, range.first)), false)
        segments += GitSegment(expand(text.substring(range)), true)
        at = range.last + 1
    }
    if (at < text.length) segments += GitSegment(expand(text.substring(at)), false)
    return segments
}

/** 制表符对齐到这么多列的倍数；要和电脑上的 `runode_preview::TAB_WIDTH` 相同，两边的 diff 才对得齐。 */
val Presentation.gitTabWidth: Int get() = 4

/** 中间不同的那段按词数相乘超过这么多时不细比，整段算改了。 */
private const val maxWordPairs = 250_000

/**
 * [old] 改成 [new] 时两边各自改了的范围：切成词，求最长公共子序列，没对上的就是改了的。规则要和电脑上的 `changed_words`
 * 相同，改一边要跟着改另一边。
 */
fun Presentation.gitChangedWords(old: String, new: String): Pair<List<IntRange>, List<IntRange>> {
    val rangesA = wordTokens(old)
    val rangesB = wordTokens(new)
    // 词换成编号再比，免得在最长公共子序列的内层循环里比字符串。按码元认：写法不同、看着一样的字（é 和 e 加尖音符）
    // 算不同，git 和电脑上都按字节，那种改动要标出来。
    val ids = HashMap<String, Int>()
    fun id(word: String): Int = ids.getOrPut(word) { ids.size }
    val a = rangesA.map { id(old.substring(it)) }
    val b = rangesB.map { id(new.substring(it)) }
    var prefix = 0
    while (prefix < minOf(a.size, b.size) && a[prefix] == b[prefix]) prefix += 1
    var suffix = 0
    while (suffix < minOf(a.size, b.size) - prefix && a[a.size - 1 - suffix] == b[b.size - 1 - suffix]) suffix += 1
    val keepA = BooleanArray(a.size)
    val keepB = BooleanArray(b.size)
    for (i in 0 until prefix) {
        keepA[i] = true
        keepB[i] = true
    }
    for (k in 0 until suffix) {
        keepA[a.size - 1 - k] = true
        keepB[b.size - 1 - k] = true
    }
    val n = a.size - suffix - prefix
    val m = b.size - suffix - prefix
    if (n.toLong() * m <= maxWordPairs) {
        // lcs[i * (m + 1) + j] 是中间段 a 从第 i 个、b 从第 j 个起的最长公共子序列有多长。
        val lcs = IntArray((n + 1) * (m + 1))
        for (i in n - 1 downTo 0) {
            for (j in m - 1 downTo 0) {
                lcs[i * (m + 1) + j] = if (a[prefix + i] == b[prefix + j]) {
                    lcs[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    maxOf(lcs[(i + 1) * (m + 1) + j], lcs[i * (m + 1) + j + 1])
                }
            }
        }
        var i = 0
        var j = 0
        while (i < n && j < m) {
            if (a[prefix + i] == b[prefix + j]) {
                keepA[prefix + i] = true
                keepB[prefix + j] = true
                i += 1
                j += 1
            } else if (lcs[(i + 1) * (m + 1) + j] >= lcs[i * (m + 1) + j + 1]) {
                i += 1
            } else {
                j += 1
            }
        }
    }
    val kept = rangesA.indices.filter { keepA[it] }.sumOf { utf8Length(old.substring(rangesA[it])) }
    if (kept * 4 < maxOf(utf8Length(old), utf8Length(new))) return emptyList<IntRange>() to emptyList()
    return changedRanges(old, rangesA, keepA) to changedRanges(new, rangesB, keepB)
}

/** 没留下的词并成连续的范围，中间只隔着空白的也并起来。 */
private fun changedRanges(text: String, words: List<IntRange>, keep: BooleanArray): List<IntRange> {
    val out = mutableListOf<IntRange>()
    for ((index, range) in words.withIndex()) {
        if (keep[index]) continue
        val last = out.lastOrNull()
        if (last != null && text.substring(last.last + 1, range.first).all(Char::isWhitespace)) {
            out[out.size - 1] = last.first..range.last
        } else {
            out += range
        }
    }
    return out
}

/** 切成词，汉字这类 UTF-8 三字节以上的字一个字算一个，不然一句话只是一个词。 */
private fun wordTokens(text: String): List<IntRange> {
    val chars = graphemes(text)
    val out = mutableListOf<IntRange>()
    var index = 0
    while (index < chars.size) {
        val kind = charKind(text.substring(chars[index]))
        var end = index + 1
        while (kind != 0 && end < chars.size && charKind(text.substring(chars[end])) == kind) end += 1
        out += chars[index].first..chars[end - 1].last
        index = end
    }
    return out
}

/** 一个字算哪类：1 是空白，2 是字母、数字和下划线（三字节以上的不算），0 是别的，各自成词。 */
private fun charKind(cluster: String): Int {
    val scalar = cluster.codePointAt(0)
    if (Character.isWhitespace(scalar) || Character.isSpaceChar(scalar)) return 1
    if (cluster == "_" || (isLetterOrNumber(scalar) && utf8Length(cluster) < 3)) return 2
    return 0
}

private fun isLetterOrNumber(scalar: Int): Boolean = Character.isLetterOrDigit(scalar) ||
    Character.getType(scalar).let { it == Character.LETTER_NUMBER.toInt() || it == Character.OTHER_NUMBER.toInt() }

private fun utf8Length(text: String): Int = text.encodeToByteArray().size

/** 按字位簇切开，一个用户看到的字（带组合符号的、emoji）一段。 */
private fun graphemes(text: String): List<IntRange> {
    val iterator = BreakIterator.getCharacterInstance(Locale.ROOT)
    iterator.setText(text)
    val out = mutableListOf<IntRange>()
    var start = iterator.first()
    var end = iterator.next()
    while (end != BreakIterator.DONE) {
        out += start until end
        start = end
        end = iterator.next()
    }
    return out
}
