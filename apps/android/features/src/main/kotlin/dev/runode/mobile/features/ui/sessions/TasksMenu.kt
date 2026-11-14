package dev.runode.mobile.features.ui.sessions

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.SessionListModel
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.DisclosureChevron
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.TaskSource

/**
 * 菜单里会话目录的命令：一个来源（Makefile、package.json、自己加的）一项，点了交给 [onPick]，由菜单换成那个来源的命令
 * （[TaskListItems]）。终端结束了时点不开。没有命令时什么都不出。
 */
@Composable
internal fun TaskSourceItems(model: SessionListModel, session: SessionInfo, onPick: (TaskSource) -> Unit) {
    val sources = model.tasks(session).filter { it.tasks.isNotEmpty() }
    if (sources.isEmpty()) return
    val colors = themeColors
    val runnable = model.canRunTask(session)
    HorizontalDivider(thickness = 1.dp, color = colors.hairline)
    Text(
        Presentation.tasksHeader(session).resolve(),
        style = Type.footnote.copy(fontWeight = FontWeight.SemiBold),
        color = colors.secondary,
        modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp).semantics { heading() },
    )
    for (source in sources) {
        DropdownMenuItem(
            text = { Text(Presentation.taskSourceTitle(source, session.meta.cwd).resolve()) },
            leadingIcon = { Icon(taskSourceIcon(source), null, Modifier.size(18.dp)) },
            trailingIcon = { DisclosureChevron() },
            enabled = runnable,
            onClick = { onPick(source) },
        )
    }
}

/**
 * 一个来源里的命令，点一条就跑（[SessionListModel.runTask]）；最上面一项回到上一层（[onBack]）。[onRan] 收到真时命令在
 * 这个会话里跑了，假时在新终端里跑，由会话列表打开新终端。
 */
@Composable
internal fun TaskListItems(
    model: SessionListModel,
    session: SessionInfo,
    source: TaskSource,
    onBack: () -> Unit,
    onRan: (Boolean) -> Unit,
) {
    DropdownMenuItem(
        text = {
            Text(
                Presentation.taskSourceTitle(source, session.meta.cwd).resolve(),
                style = Type.subheadline.copy(fontWeight = FontWeight.SemiBold),
            )
        },
        leadingIcon = { Icon(AppIcons.back, null, Modifier.size(18.dp)) },
        onClick = onBack,
    )
    HorizontalDivider(thickness = 1.dp, color = themeColors.hairline)
    for (task in source.tasks) {
        DropdownMenuItem(
            text = {
                Column {
                    Text(task.name, fontFamily = FontFamily.Monospace, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    task.description?.let {
                        Text(it, style = Type.caption, color = themeColors.secondary, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    }
                }
            },
            enabled = model.canRunTask(session),
            onClick = { onRan(model.runTask(task, source.project, session.id)) },
        )
    }
}

/** 来源的图标，按 [Presentation.taskSourceSymbol] 说的那种。 */
private fun taskSourceIcon(source: TaskSource): ImageVector = when (Presentation.taskSourceSymbol(source)) {
    "star" -> AppIcons.star
    "globe" -> AppIcons.globe
    "hammer" -> AppIcons.hammer
    "shippingbox" -> AppIcons.box
    else -> AppIcons.terminal
}
