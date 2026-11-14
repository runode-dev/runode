package dev.runode.mobile.features.ui.git

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.DefaultsStore
import dev.runode.mobile.features.model.GitDiffTarget
import dev.runode.mobile.features.model.GitModel
import dev.runode.mobile.features.model.GitTreeItem
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.SharedPreferencesDefaults
import dev.runode.mobile.features.model.gitBranch
import dev.runode.mobile.features.model.gitBranchDetail
import dev.runode.mobile.features.model.gitDisplaySegments
import dev.runode.mobile.features.model.gitFileTree
import dev.runode.mobile.features.model.gitLetter
import dev.runode.mobile.features.model.gitOperation
import dev.runode.mobile.features.model.gitPathParts
import dev.runode.mobile.features.model.gitRunning
import dev.runode.mobile.features.model.gitStatusName
import dev.runode.mobile.features.model.gitUpstream
import dev.runode.mobile.features.model.gitWordChanges
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.CircleIconButton
import dev.runode.mobile.features.ui.ConfirmDialog
import dev.runode.mobile.features.ui.ConnectionStatusRow
import dev.runode.mobile.features.ui.DisclosureChevron
import dev.runode.mobile.features.ui.EmptyState
import dev.runode.mobile.features.ui.ErrorDialog
import dev.runode.mobile.features.ui.LeadingTitle
import dev.runode.mobile.features.ui.ListSectionHeader
import dev.runode.mobile.features.ui.LoadingState
import dev.runode.mobile.features.ui.ScreenPadding
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.ThemeColors
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.cardBackground
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.protocol.GitBranch
import dev.runode.mobile.protocol.GitFile
import dev.runode.mobile.protocol.GitFileDiff
import dev.runode.mobile.protocol.GitFileStatus
import dev.runode.mobile.protocol.GitHunk
import dev.runode.mobile.protocol.GitLine
import dev.runode.mobile.protocol.GitLineKind
import dev.runode.mobile.protocol.GitStatus
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable

/**
 * 一个会话所在仓库的 Git 页：分支和同步、提交说明、已暂存和改动两组文件（每组并成一张卡片）。点文件看 diff，暂存、取消暂存
 * 经行尾的按钮，长按还有拷贝路径和放弃（先确认）；点分支那一行切分支。文件能按目录排成树，列表形式和收起的目录记在
 * [GitViewPreferences] 里。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun GitScreen(model: GitModel, onBack: () -> Unit) {
    var diffTarget by remember { mutableStateOf<GitDiffTarget?>(null) }
    val target = diffTarget
    if (target != null) {
        BackHandler { diffTarget = null }
        GitDiffScreen(model, target) { diffTarget = null }
        return
    }
    val colors = themeColors
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val store = remember(context) { GitViewPreferences.store(context) }
    var prefs by remember(store) { mutableStateOf(store.load() ?: GitViewPreferences()) }
    fun update(change: (GitViewPreferences) -> GitViewPreferences) {
        prefs = change(prefs)
        store.save(prefs)
    }
    var refreshing by remember { mutableStateOf(false) }
    var discarding by remember { mutableStateOf<List<GitFile>>(emptyList()) }
    var stagingConflicts by remember { mutableStateOf(0) }
    var showingBranches by rememberSaveable { mutableStateOf(false) }
    val status = model.status
    val subtitle = model.running?.let { Presentation.gitRunning(it).resolve() } ?: Presentation.directory(status?.root)
    val busy = model.running != null || !model.isReady

    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = onBack, actions = {
            GitMoreMenu(model, prefs.asTree, { update { it.copy(asTree = !it.asTree) } }, { showingBranches = true })
        }) {
            LeadingTitle(model.repositoryName ?: stringResource(R.string.git), subtitle)
        }
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                scope.launch {
                    refreshing = true
                    model.refreshAndWait()
                    refreshing = false
                }
            },
            modifier = Modifier.weight(1f).fillMaxWidth(),
        ) {
            LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(horizontal = ScreenPadding, vertical = 4.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                if (!model.linkState.isConnected) {
                    item(key = "link") {
                        ConnectionStatusRow(model.linkState, model::reconnect, Modifier.cardBackground(colors))
                    }
                }
                if (status != null) {
                    item(key = "branch") { BranchCard(model, status) { showingBranches = true } }
                    if (model.isClean) {
                        item(key = "clean") {
                            Row(
                                Modifier.fillMaxWidth().cardBackground(colors),
                                horizontalArrangement = Arrangement.spacedBy(10.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                Icon(AppIcons.check, null, tint = colors.secondary, modifier = Modifier.size(18.dp))
                                Text(stringResource(R.string.git_clean), style = Type.body, color = colors.secondary)
                            }
                        }
                    } else {
                        item(key = "commit") { CommitCard(model) }
                        for (staged in listOf(true, false)) {
                            val files = if (staged) status.staged else status.unstaged
                            if (files.isEmpty()) continue
                            item(key = if (staged) "staged" else "unstaged") {
                                val collapsedKey = "${status.root}\n${if (staged) "staged" else "unstaged"}"
                                FileGroup(
                                    model = model,
                                    files = files,
                                    staged = staged,
                                    asTree = prefs.asTree,
                                    collapsed = prefs.collapsed[collapsedKey] ?: emptySet(),
                                    busy = busy,
                                    context = context,
                                    onToggle = { path ->
                                        update {
                                            val old = it.collapsed[collapsedKey] ?: emptySet()
                                            val new = if (path in old) old - path else old + path
                                            it.copy(collapsed = it.collapsed + (collapsedKey to new))
                                        }
                                    },
                                    onOpen = { diffTarget = GitDiffTarget(it.path, staged) },
                                    onDiscard = { discarding = it },
                                    onStageAll = {
                                        when {
                                            staged -> model.unstageAll()
                                            model.unstagedConflicts > 0 -> stagingConflicts = model.unstagedConflicts
                                            else -> model.stageAll()
                                        }
                                    },
                                )
                            }
                        }
                    }
                }
                item(key = "bottom") { Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars)) }
            }
            if (model.loaded && status == null && model.linkState.isConnected) {
                EmptyState(
                    stringResource(R.string.git_not_repo_title),
                    stringResource(R.string.git_not_repo_detail),
                    Modifier.align(Alignment.Center),
                )
            } else if (!model.loaded && model.linkState.isConnected) {
                LoadingState(stringResource(R.string.git_loading), Modifier.align(Alignment.Center))
            }
        }
    }

    if (discarding.isNotEmpty()) {
        val files = discarding
        val title = if (files.size == 1) {
            stringResource(R.string.git_discard_one, Presentation.gitPathParts(files[0].path).first)
        } else {
            pluralStringResource(R.plurals.git_discard_many, files.size, files.size)
        }
        ConfirmDialog(
            title = title,
            message = stringResource(R.string.git_discard_detail),
            confirm = stringResource(R.string.git_action_discard),
            onConfirm = { model.discard(files) },
            onDismiss = { discarding = emptyList() },
        )
    }
    if (stagingConflicts > 0) {
        ConfirmDialog(
            title = pluralStringResource(R.plurals.git_conflicts_title, stagingConflicts, stagingConflicts),
            message = stringResource(R.string.git_conflicts_detail),
            confirm = stringResource(R.string.git_stage_all),
            onConfirm = { model.stageAll() },
            onDismiss = { stagingConflicts = 0 },
        )
    }
    if (showingBranches) {
        ModalBottomSheet(onDismissRequest = { showingBranches = false }, containerColor = colors.page) {
            GitBranchSheet(model) { showingBranches = false }
        }
    }
    ErrorDialog(model.errorMessage?.resolve()) { model.errorMessage = null }
}

/** Git 页的显示偏好：以树形式查看、各段收起的目录（键是仓库根加段名）。 */
@Serializable
internal data class GitViewPreferences(
    val asTree: Boolean = false,
    val collapsed: Map<String, Set<String>> = emptyMap(),
) {
    companion object {
        // shortcut: 改动没了的目录也一直记着，攒多了再在读到状态时清掉不在改动里的
        fun store(context: Context): DefaultsStore<GitViewPreferences> = DefaultsStore(
            "gitView",
            SharedPreferencesDefaults(context.getSharedPreferences("git", Context.MODE_PRIVATE)),
        )
    }
}

/** 顶栏右边的「⋯」：刷新、列表和树形式、抓取拉取推送同步、切换分支。 */
@Composable
private fun GitMoreMenu(model: GitModel, asTree: Boolean, onToggleTree: () -> Unit, onBranches: () -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    val status = model.status
    Box {
        CircleIconButton(AppIcons.more, stringResource(R.string.git_actions), { expanded = true }, enabled = model.isReady)
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            MenuItem(stringResource(R.string.git_refresh), AppIcons.refresh) { expanded = false; model.refresh() }
            if (status != null && !model.isClean) {
                MenuItem(
                    stringResource(if (asTree) R.string.git_view_list else R.string.git_view_tree),
                    if (asTree) AppIcons.list else AppIcons.tree,
                ) { expanded = false; onToggleTree() }
            }
            if (status?.hasRemote == true) {
                HorizontalDivider(thickness = 1.dp, color = themeColors.hairline)
                val idle = model.running == null
                MenuItem(stringResource(R.string.git_action_fetch), AppIcons.download, idle) { expanded = false; model.fetch() }
                MenuItem(stringResource(R.string.git_action_pull), AppIcons.arrowDown, idle) { expanded = false; model.pull() }
                MenuItem(stringResource(R.string.git_action_push), AppIcons.arrowUp, idle) { expanded = false; model.push() }
                MenuItem(stringResource(R.string.git_action_sync), AppIcons.sync, idle) { expanded = false; model.sync() }
            }
            if (status != null) {
                HorizontalDivider(thickness = 1.dp, color = themeColors.hairline)
                MenuItem(stringResource(R.string.git_action_checkout), AppIcons.branch) { expanded = false; onBranches() }
            }
        }
    }
}

@Composable
private fun MenuItem(
    text: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    enabled: Boolean = true,
    color: Color = Color.Unspecified,
    onClick: () -> Unit,
) {
    DropdownMenuItem(
        text = { Text(text, color = color) },
        leadingIcon = { Icon(icon, null, Modifier.size(18.dp), tint = if (color == Color.Unspecified) themeColors.primary else color) },
        enabled = enabled,
        onClick = onClick,
    )
}

/** 分支卡片：分支名和上游，点了切分支；做到一半的操作的提醒；有远端时一排拉取、推送、同步。 */
@Composable
private fun BranchCard(model: GitModel, status: GitStatus, onBranches: () -> Unit) {
    val colors = themeColors
    val switchLabel = stringResource(R.string.git_action_checkout)
    Column(Modifier.fillMaxWidth().cardBackground(colors), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(
            Modifier
                .fillMaxWidth()
                .clickable(role = Role.Button, onClickLabel = switchLabel, onClick = onBranches)
                .semantics(mergeDescendants = true) {},
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(AppIcons.branch, null, tint = colors.primary, modifier = Modifier.size(20.dp))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(
                    Presentation.gitBranch(status).resolve(),
                    style = Type.headline,
                    fontFamily = FontFamily.Monospace,
                    maxLines = 1,
                    overflow = TextOverflow.MiddleEllipsis,
                )
                Text(Presentation.gitUpstream(status).resolve(), style = Type.subheadline, color = colors.secondary, maxLines = 1)
            }
            DisclosureChevron()
        }
        Presentation.gitOperation(status.operation)?.let { warning ->
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(AppIcons.warning, null, tint = colors.waiting, modifier = Modifier.size(14.dp))
                Text(warning.resolve(), style = Type.footnote, color = colors.waiting)
            }
        }
        if (status.hasRemote) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                RemoteButton(model, GitModel.Action.Pull, R.string.git_action_pull, AppIcons.arrowDown, status.behind.toInt()) { model.pull() }
                RemoteButton(model, GitModel.Action.Push, R.string.git_action_push, AppIcons.arrowUp, status.ahead.toInt()) { model.push() }
                RemoteButton(model, GitModel.Action.Sync, R.string.git_action_sync, AppIcons.sync, 0) { model.sync() }
            }
        }
    }
}

@Composable
private fun androidx.compose.foundation.layout.RowScope.RemoteButton(
    model: GitModel,
    action: GitModel.Action,
    title: Int,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    badge: Int,
    onClick: () -> Unit,
) {
    val name = stringResource(title)
    SurfaceButton(
        ActionKind.Raised,
        onClick = onClick,
        enabled = model.running == null && model.isReady,
        modifier = Modifier.weight(1f),
    ) {
        if (model.running == action) {
            CircularProgressIndicator(Modifier.size(14.dp), color = themeColors.secondary, strokeWidth = 2.dp)
        } else {
            Icon(icon, null, Modifier.size(16.dp))
        }
        Spacer(Modifier.width(4.dp))
        Text(
            if (badge > 0) stringResource(R.string.git_remote_badge, name, badge) else name,
            style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold),
            maxLines = 1,
        )
    }
}

/** 提交卡片：提交说明的输入框和提交按钮；没有暂存的文件时提交全部改动。 */
@Composable
private fun CommitCard(model: GitModel) {
    val colors = themeColors
    val focus = LocalFocusManager.current
    val placeholder = stringResource(R.string.git_commit_message)
    val status = model.status
    val title = when {
        status == null -> stringResource(R.string.git_action_commit)
        model.commitsEverything -> pluralStringResource(R.plurals.git_commit_all, status.unstaged.size, status.unstaged.size)
        else -> pluralStringResource(R.plurals.git_commit_staged, status.staged.size, status.staged.size)
    }
    Column(Modifier.fillMaxWidth().cardBackground(colors), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Box(Modifier.fillMaxWidth().background(colors.fill, Shapes.inner).padding(10.dp)) {
            if (model.commitMessage.isEmpty()) Text(placeholder, style = Type.body, color = colors.tertiary)
            BasicTextField(
                value = model.commitMessage,
                onValueChange = { model.commitMessage = it },
                textStyle = Type.body.copy(color = colors.primary),
                cursorBrush = SolidColor(colors.primary),
                maxLines = 6,
                modifier = Modifier.fillMaxWidth().semantics { contentDescription = placeholder },
            )
        }
        SurfaceButton(
            ActionKind.Primary,
            onClick = {
                focus.clearFocus()
                model.commit()
            },
            enabled = model.canCommit,
            modifier = Modifier.fillMaxWidth(),
        ) {
            if (model.running == GitModel.Action.Commit) {
                CircularProgressIndicator(Modifier.size(14.dp), color = colors.page, strokeWidth = 2.dp)
                Spacer(Modifier.width(6.dp))
            }
            Text(title, style = Type.headline)
        }
    }
}

/** 一段文件：段头（数目和全部暂存或全部取消暂存），下面并成一张卡片的各行；树形式时目录占一行，能收起。 */
@Composable
private fun FileGroup(
    model: GitModel,
    files: List<GitFile>,
    staged: Boolean,
    asTree: Boolean,
    collapsed: Set<String>,
    busy: Boolean,
    context: Context,
    onToggle: (String) -> Unit,
    onOpen: (GitFile) -> Unit,
    onDiscard: (List<GitFile>) -> Unit,
    onStageAll: () -> Unit,
) {
    val colors = themeColors
    val items = if (asTree) Presentation.gitFileTree(files, collapsed) else files.map { GitTreeItem.File(it, 0) }
    Column {
        Row(Modifier.fillMaxWidth().padding(bottom = 8.dp), verticalAlignment = Alignment.Bottom) {
            ListSectionHeader(
                stringResource(if (staged) R.string.git_staged_header else R.string.git_changes_header, files.size),
                Modifier.weight(1f),
            )
            Text(
                stringResource(if (staged) R.string.git_unstage_all else R.string.git_stage_all),
                style = Type.subheadline,
                color = if (busy) colors.tertiary else colors.primary,
                modifier = Modifier
                    .clickable(enabled = !busy, role = Role.Button, onClick = onStageAll)
                    .padding(start = 12.dp, top = 12.dp, end = 4.dp),
            )
        }
        Column(Modifier.fillMaxWidth().cardBackground(colors, padding = 0.dp, shape = Shapes.group)) {
            items.forEachIndexed { index, item ->
                when (item) {
                    is GitTreeItem.Directory -> DirectoryRow(item, model, staged, busy, context, onToggle, onDiscard)
                    is GitTreeItem.File -> FileRow(item.file, item.depth, asTree, model, staged, busy, context, onOpen, onDiscard)
                }
                if (index < items.lastIndex) HorizontalDivider(Modifier.padding(start = 48.dp), thickness = 1.dp, color = colors.hairline)
            }
        }
    }
}

/** 一个文件：点了看 diff，行尾暂存或取消暂存；长按有查看改动、暂存、拷贝路径，未暂存的还有放弃。 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun FileRow(
    file: GitFile,
    depth: Int,
    asTree: Boolean,
    model: GitModel,
    staged: Boolean,
    busy: Boolean,
    context: Context,
    onOpen: (GitFile) -> Unit,
    onDiscard: (List<GitFile>) -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    val hint = stringResource(R.string.git_view_changes)
    Box {
        Row(
            Modifier.fillMaxWidth().padding(start = 16.dp + 16.dp * depth, end = 6.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            GitFileLabel(
                file,
                showsDirectory = !asTree,
                modifier = Modifier
                    .weight(1f)
                    .combinedClickable(onClickLabel = hint, role = Role.Button, onLongClick = { menu = true }) { onOpen(file) },
            )
            StageButton(listOf(file), staged, model, busy)
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            MenuItem(hint, AppIcons.document) { menu = false; onOpen(file) }
            StageMenuItem(listOf(file), staged, model, busy) { menu = false }
            MenuItem(stringResource(R.string.git_copy_path), AppIcons.copy) { menu = false; copy(context, file.path) }
            if (!staged) {
                MenuItem(stringResource(R.string.git_action_discard), AppIcons.discard, !busy, themeColors.failure) {
                    menu = false
                    onDiscard(listOf(file))
                }
            }
        }
    }
}

/** 树形式下的一个目录：点了收起或展开，行尾对整个目录暂存；长按有暂存、拷贝路径，未暂存的还有放弃。 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DirectoryRow(
    item: GitTreeItem.Directory,
    model: GitModel,
    staged: Boolean,
    busy: Boolean,
    context: Context,
    onToggle: (String) -> Unit,
    onDiscard: (List<GitFile>) -> Unit,
) {
    val colors = themeColors
    var menu by remember { mutableStateOf(false) }
    val label = pluralStringResource(R.plurals.git_folder_label, item.files.size, item.files.size, item.path)
    val state = stringResource(if (item.expanded) R.string.git_expanded else R.string.git_collapsed)
    val action = stringResource(if (item.expanded) R.string.git_collapse else R.string.git_expand)
    Box {
        Row(
            Modifier.fillMaxWidth().padding(start = 16.dp + 16.dp * item.depth, end = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Row(
                Modifier
                    .weight(1f)
                    .defaultMinSize(minHeight = 44.dp)
                    .combinedClickable(onClickLabel = action, role = Role.Button, onLongClick = { menu = true }) { onToggle(item.path) }
                    .clearAndSetSemantics {
                        contentDescription = label
                        stateDescription = state
                        role = Role.Button
                    },
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Icon(
                    AppIcons.chevronRight,
                    null,
                    tint = colors.secondary,
                    modifier = Modifier.size(22.dp).padding(4.dp).rotate(if (item.expanded) 90f else 0f),
                )
                Icon(AppIcons.folder, null, tint = colors.secondary, modifier = Modifier.size(18.dp))
                Text(
                    item.name,
                    style = Type.body,
                    maxLines = 1,
                    overflow = TextOverflow.StartEllipsis,
                    modifier = Modifier.weight(1f),
                )
                Text(item.files.size.toString(), style = Type.caption, color = colors.secondary)
            }
            StageButton(item.files, staged, model, busy)
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            StageMenuItem(item.files, staged, model, busy) { menu = false }
            MenuItem(stringResource(R.string.git_copy_path), AppIcons.copy) { menu = false; copy(context, item.path) }
            if (!staged) {
                MenuItem(stringResource(R.string.git_action_discard), AppIcons.discard, !busy, colors.failure) {
                    menu = false
                    onDiscard(item.files)
                }
            }
        }
    }
}

@Composable
private fun StageButton(files: List<GitFile>, staged: Boolean, model: GitModel, busy: Boolean) {
    val label = stringResource(if (staged) R.string.git_action_unstage else R.string.git_action_stage)
    Box(
        Modifier
            .size(44.dp)
            .clickable(enabled = !busy, role = Role.Button) { if (staged) model.unstage(files) else model.stage(files) }
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        val tint = if (busy) themeColors.tertiary else themeColors.primary
        Icon(if (staged) AppIcons.minusCircle else AppIcons.plusCircle, null, tint = tint, modifier = Modifier.size(24.dp))
    }
}

@Composable
private fun StageMenuItem(files: List<GitFile>, staged: Boolean, model: GitModel, busy: Boolean, dismiss: () -> Unit) {
    MenuItem(
        stringResource(if (staged) R.string.git_action_unstage else R.string.git_action_stage),
        if (staged) AppIcons.minusCircle else AppIcons.plusCircle,
        !busy,
    ) {
        dismiss()
        if (staged) model.unstage(files) else model.stage(files)
    }
}

private fun copy(context: Context, text: String) {
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(text, text))
}

/** 一个改动的文件：状态字母、文件名、所在目录（树形式下不写）、增删的行数。 */
@Composable
internal fun GitFileLabel(file: GitFile, showsDirectory: Boolean, modifier: Modifier = Modifier) {
    val colors = themeColors
    val (name, directory) = Presentation.gitPathParts(file.path)
    val tint = statusTint(file.status, colors)
    val label = stringResource(
        R.string.git_file_label,
        file.path,
        Presentation.gitStatusName(file.status).resolve(),
        file.added.toInt(),
        file.removed.toInt(),
    )
    Row(
        modifier.defaultMinSize(minHeight = 44.dp).clearAndSetSemantics {
            contentDescription = label
            role = Role.Button
        },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Box(Modifier.size(22.dp).background(tint.copy(alpha = 0.14f), RoundedCornerShape(6.dp)), contentAlignment = Alignment.Center) {
            Text(
                Presentation.gitLetter(file.status),
                style = Type.caption.copy(fontWeight = FontWeight.Bold),
                fontFamily = FontFamily.Monospace,
                color = tint,
            )
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(name, style = Type.body, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
            if (showsDirectory && directory != null) {
                Text(
                    directory,
                    style = Type.caption,
                    color = colors.secondary,
                    maxLines = 1,
                    overflow = TextOverflow.StartEllipsis,
                )
            }
        }
        if (file.binary) {
            Text(stringResource(R.string.git_binary), style = Type.caption, color = colors.secondary)
        } else if (file.added + file.removed > 0u) {
            Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                if (file.added > 0u) Text("+${file.added}", style = Type.caption, fontFamily = FontFamily.Monospace, color = colors.diffAdded)
                if (file.removed > 0u) Text("−${file.removed}", style = Type.caption, fontFamily = FontFamily.Monospace, color = colors.diffRemoved)
            }
        }
    }
}

/** 状态字母的颜色：加的绿、删的和冲突红、改的和改名蓝；琥珀色留给等你回答。 */
private fun statusTint(status: GitFileStatus, colors: ThemeColors): Color = when (status) {
    GitFileStatus.Added, GitFileStatus.Untracked -> colors.diffAdded
    GitFileStatus.Deleted, GitFileStatus.Conflicted -> colors.diffRemoved
    GitFileStatus.Modified, GitFileStatus.Renamed -> colors.working
    is GitFileStatus.Unknown -> colors.secondary
}

/**
 * 一个文件的 diff：逐行显示，成对改动的行里真正改了的字（[gitWordChanges]，在后台算）底色更深，长行折行。右上角暂存或取消
 * 暂存这个文件后回到 Git 页。
 */
@Composable
private fun GitDiffScreen(model: GitModel, target: GitDiffTarget, onBack: () -> Unit) {
    val colors = themeColors
    LaunchedEffect(target) { model.openDiff(target) }
    DisposableEffect(target) { onDispose { model.closeDiff() } }
    val diff = model.diff
    val words by produceState<Pair<GitFileDiff, List<List<List<IntRange>>>>?>(null, diff) {
        val current = diff ?: return@produceState
        value = current to withContext(Dispatchers.Default) { current.hunks.map(Presentation::gitWordChanges) }
    }
    val (name, directory) = Presentation.gitPathParts(target.path)
    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = onBack, actions = {
            SurfaceButton(
                ActionKind.Raised,
                onClick = {
                    val file = model.diff?.file ?: return@SurfaceButton
                    if (target.staged) model.unstage(file) else model.stage(file)
                    onBack()
                },
                enabled = model.diff != null && model.running == null && model.isReady,
                contentPadding = 14.dp,
            ) {
                Text(stringResource(if (target.staged) R.string.git_action_unstage else R.string.git_action_stage), style = Type.subheadline)
            }
        }) {
            LeadingTitle(name)
        }
        Box(Modifier.weight(1f).fillMaxWidth()) {
            when {
                diff != null && model.diffTarget == target -> when {
                    diff.file.binary -> EmptyState(
                        stringResource(R.string.git_binary_title),
                        stringResource(R.string.git_binary_detail),
                        Modifier.align(Alignment.Center),
                    )
                    diff.hunks.isEmpty() -> EmptyState(
                        stringResource(R.string.git_nothing_title),
                        stringResource(if (diff.truncated) R.string.git_too_large else R.string.git_no_line_changes),
                        Modifier.align(Alignment.Center),
                    )
                    else -> {
                        val hunkWords = words?.takeIf { it.first == diff }?.second
                        LazyColumn(Modifier.fillMaxSize()) {
                            if (directory != null) {
                                item(key = "path") {
                                    Text(
                                        target.path,
                                        style = Type.caption,
                                        fontFamily = FontFamily.Monospace,
                                        color = colors.secondary,
                                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                                    )
                                }
                            }
                            diff.hunks.forEachIndexed { ix, hunk ->
                                item(key = "hunk-$ix") { HunkHeader(hunk) }
                                val lineWords = hunkWords?.getOrNull(ix)
                                hunk.lines.forEachIndexed { line, value ->
                                    item(key = "line-$ix-$line") { DiffLine(value, lineWords?.getOrNull(line) ?: emptyList()) }
                                }
                            }
                            if (diff.truncated) {
                                item(key = "truncated") {
                                    Text(
                                        stringResource(R.string.git_diff_truncated),
                                        style = Type.footnote,
                                        color = colors.secondary,
                                        modifier = Modifier.padding(12.dp),
                                    )
                                }
                            }
                            item(key = "bottom") { Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars)) }
                        }
                    }
                }
                model.isLoadingDiff || model.diffTarget != target ->
                    LoadingState(stringResource(R.string.git_diff_loading), Modifier.align(Alignment.Center))
                // 没连上时读不了：重新连上后 `refresh` 会重读。
                !model.isReady -> ConnectionStatusRow(
                    model.linkState,
                    model::reconnect,
                    Modifier.align(Alignment.TopCenter).padding(ScreenPadding).cardBackground(colors),
                )
                model.diffError != null -> EmptyState(
                    stringResource(R.string.git_diff_error_title),
                    model.diffError?.resolve(),
                    Modifier.align(Alignment.Center),
                )
                else -> EmptyState(
                    stringResource(R.string.git_no_more_changes),
                    stringResource(if (target.staged) R.string.git_no_staged_left else R.string.git_no_unstaged_left),
                    Modifier.align(Alignment.Center),
                )
            }
        }
    }
}

@Composable
private fun HunkHeader(hunk: GitHunk) {
    val colors = themeColors
    Text(
        hunk.header,
        style = Type.caption,
        fontFamily = FontFamily.Monospace,
        color = colors.secondary,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.fillMaxWidth().background(colors.card).padding(horizontal = 12.dp, vertical = 6.dp),
    )
}

/** diff 的一行：行号、加减号、内容；真正改了的字（[words]）垫更深的底色。 */
@Composable
private fun DiffLine(line: GitLine, words: List<IntRange>) {
    val colors = themeColors
    val tint = when (line.kind) {
        GitLineKind.Added -> colors.diffAdded
        GitLineKind.Removed -> colors.diffRemoved
        else -> null
    }
    val marker = when (line.kind) {
        GitLineKind.Added -> "+"
        GitLineKind.Removed -> "−"
        else -> " "
    }
    val kind = stringResource(
        when (line.kind) {
            GitLineKind.Added -> R.string.git_line_added
            GitLineKind.Removed -> R.string.git_line_removed
            else -> R.string.git_line_unchanged
        },
    )
    val description = stringResource(R.string.git_line_label, kind, line.text)
    val text: AnnotatedString = if (line.text.isEmpty()) {
        AnnotatedString(" ")
    } else {
        buildAnnotatedString {
            for (segment in Presentation.gitDisplaySegments(line.text, words)) {
                if (segment.changed && tint != null) {
                    withStyle(SpanStyle(background = tint.copy(alpha = 0.4f))) { append(segment.text) }
                } else {
                    append(segment.text)
                }
            }
        }
    }
    val style = Type.caption.copy(fontFamily = FontFamily.Monospace)
    Row(
        Modifier
            .fillMaxWidth()
            .background(tint?.copy(alpha = 0.14f) ?: Color.Transparent)
            .padding(horizontal = 8.dp, vertical = 1.dp)
            .clearAndSetSemantics { contentDescription = description },
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            (line.new ?: line.old)?.toString() ?: "",
            style = style,
            color = colors.tertiary,
            textAlign = TextAlign.End,
            modifier = Modifier.widthIn(min = 32.dp),
        )
        Text(marker, style = style, color = tint ?: colors.secondary)
        Text(text, style = style, modifier = Modifier.weight(1f))
    }
}

/** 切分支：本地分支在前，远端分支在后，当前的打勾。点一个就切过去并关掉。 */
@Composable
private fun GitBranchSheet(model: GitModel, onDismiss: () -> Unit) {
    val colors = themeColors
    LaunchedEffect(model) { model.loadBranches() }
    val local = model.branches.filter { !it.remote }
    val remote = model.branches.filter { it.remote }
    Column(Modifier.fillMaxWidth().padding(horizontal = ScreenPadding).padding(bottom = 24.dp)) {
        Text(
            stringResource(R.string.git_action_checkout),
            style = Type.headline,
            modifier = Modifier.align(Alignment.CenterHorizontally).semantics { heading() },
        )
        when {
            model.isLoadingBranches && model.branches.isEmpty() ->
                LoadingState(stringResource(R.string.git_branches_loading), Modifier.fillMaxWidth().padding(32.dp))
            !model.isLoadingBranches && model.branches.isEmpty() ->
                EmptyState(stringResource(R.string.git_no_branches), modifier = Modifier.fillMaxWidth())
            else -> LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                for ((title, branches) in listOf(R.string.git_local_branches to local, R.string.git_remote_branches to remote)) {
                    if (branches.isEmpty()) continue
                    item(key = "header-$title") { ListSectionHeader(stringResource(title)) }
                    item(key = "group-$title") {
                        Column(Modifier.fillMaxWidth().cardBackground(colors, padding = 0.dp, shape = Shapes.group)) {
                            branches.forEachIndexed { index, branch ->
                                BranchRow(branch, enabled = !branch.current && model.running == null) {
                                    model.checkout(branch)
                                    onDismiss()
                                }
                                if (index < branches.lastIndex) {
                                    HorizontalDivider(Modifier.padding(start = 16.dp), thickness = 1.dp, color = colors.hairline)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun BranchRow(branch: GitBranch, enabled: Boolean, onClick: () -> Unit) {
    val colors = themeColors
    val current = stringResource(R.string.git_current_branch)
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { selected = branch.current }
            .padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                branch.name,
                style = Type.body.copy(fontWeight = if (branch.current) FontWeight.SemiBold else FontWeight.Normal),
                fontFamily = FontFamily.Monospace,
                maxLines = 1,
                overflow = TextOverflow.MiddleEllipsis,
            )
            Text(Presentation.gitBranchDetail(branch).resolve(), style = Type.caption, color = colors.secondary, maxLines = 1)
        }
        if (branch.current) {
            Icon(AppIcons.check, current, tint = colors.primary, modifier = Modifier.size(18.dp))
        }
    }
}
