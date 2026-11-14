package dev.runode.mobile.features.ui.sessions

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBars
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.DirectoryPickerModel
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.CircleIconButton
import dev.runode.mobile.features.ui.DisclosureChevron
import dev.runode.mobile.features.ui.EmptyState
import dev.runode.mobile.features.ui.LeadingTitle
import dev.runode.mobile.features.ui.ListSectionHeader
import dev.runode.mobile.features.ui.LoadingState
import dev.runode.mobile.features.ui.ScreenPadding
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.theme.ActionKind
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.SurfaceButton
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.cardBackground
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors

/**
 * 新建工作区时选电脑上的目录：同一页里换目录，盖在会话列表上。能顺手填工作区的名字，空着时按目录取。电脑上的 app 没开着
 * 窗口时开的是后台终端，页脚说明这一点，也不出名字的输入框。
 *
 * @param onChoose 选好的目录和填的名字（可能是空串）
 */
@Composable
internal fun DirectoryPickerView(
    picker: DirectoryPickerModel,
    hasDesktopWindow: Boolean,
    onCancel: () -> Unit,
    onChoose: (dir: String, name: String) -> Unit,
) {
    val colors = themeColors
    var name by rememberSaveable { mutableStateOf("") }
    BackHandler(onBack = onCancel)
    val path = picker.path
    val hiddenLabel = stringResource(R.string.picker_show_hidden)
    // 接住所有触摸：点在空白处时不落到下面盖着的会话列表上。
    Column(Modifier.fillMaxSize().background(colors.page).pointerInput(Unit) {}) {
        TopBar(onBack = onCancel, actions = {
            CircleIconButton(
                if (picker.showsHidden) AppIcons.eye else AppIcons.eyeOff,
                hiddenLabel,
                { picker.showsHidden = !picker.showsHidden },
                modifier = Modifier.semantics { toggleableState = ToggleableState(picker.showsHidden) },
                prominent = picker.showsHidden,
            )
        }) {
            LeadingTitle(stringResource(R.string.new_workspace))
        }
        Box(Modifier.weight(1f).fillMaxWidth()) {
            LazyColumn(
                Modifier.fillMaxSize().alpha(if (picker.isLoading) 0.5f else 1f),
                contentPadding = PaddingValues(horizontal = ScreenPadding, vertical = 4.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (path != null) {
                    item(key = "current") {
                        val description = stringResource(R.string.picker_now_in, path)
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Row(
                                Modifier
                                    .fillMaxWidth()
                                    .cardBackground(colors, shape = Shapes.group)
                                    .clearAndSetSemantics { contentDescription = description },
                                verticalAlignment = Alignment.CenterVertically,
                                horizontalArrangement = Arrangement.spacedBy(10.dp),
                            ) {
                                Icon(AppIcons.folder, null, tint = colors.primary, modifier = Modifier.size(20.dp))
                                Text(
                                    Presentation.directory(path) ?: path,
                                    style = Type.headline,
                                    fontFamily = FontFamily.Monospace,
                                    maxLines = 2,
                                    overflow = TextOverflow.StartEllipsis,
                                )
                            }
                            Footer(stringResource(if (hasDesktopWindow) R.string.picker_footer_workspace else R.string.picker_footer_background))
                        }
                    }
                    if (hasDesktopWindow) {
                        item(key = "name") {
                            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                ListSectionHeader(stringResource(R.string.name_field))
                                NameField(name, path.trimEnd('/').substringAfterLast('/').ifEmpty { path }) { name = it }
                            }
                        }
                    }
                }
                item(key = "dirs") {
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.padding(top = 8.dp)) {
                        val parent = picker.parent
                        val rows = (if (parent != null) 1 else 0) + picker.visibleDirs.size
                        if (rows > 0) {
                            Column(Modifier.fillMaxWidth().cardBackground(colors, padding = 0.dp, shape = Shapes.group)) {
                                if (parent != null) {
                                    val hint = stringResource(R.string.picker_back_to, parent)
                                    FolderRow(stringResource(R.string.picker_parent), AppIcons.parentFolder, hint, false, !picker.isLoading) {
                                        picker.goUp()
                                    }
                                    if (picker.visibleDirs.isNotEmpty()) Divider()
                                }
                                picker.visibleDirs.forEachIndexed { index, dir ->
                                    FolderRow(dir, AppIcons.folder, null, true, !picker.isLoading) { picker.enter(dir) }
                                    if (index < picker.visibleDirs.lastIndex) Divider()
                                }
                            }
                        }
                        if (picker.truncated) Footer(stringResource(R.string.picker_truncated))
                    }
                }
            }
            val error = picker.errorMessage
            when {
                error != null -> Box(Modifier.fillMaxSize().background(colors.page), contentAlignment = Alignment.Center) {
                    EmptyState(stringResource(R.string.picker_error_title), error.resolve()) {
                        Row(Modifier.padding(top = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            SurfaceButton(ActionKind.Raised, onClick = { picker.retry() }, contentPadding = 16.dp) {
                                Text(stringResource(R.string.action_retry), style = Type.subheadline)
                            }
                            if (path != null) {
                                SurfaceButton(ActionKind.Raised, onClick = { picker.dismissError() }, contentPadding = 16.dp) {
                                    Text(stringResource(R.string.picker_back_to_last), style = Type.subheadline)
                                }
                            }
                        }
                    }
                }
                picker.isLoading && path == null -> LoadingState(stringResource(R.string.picker_loading), Modifier.align(Alignment.Center))
                !picker.isLoading && path != null && picker.visibleDirs.isEmpty() ->
                    EmptyState(stringResource(R.string.picker_empty), modifier = Modifier.align(Alignment.Center))
            }
        }
        SurfaceButton(
            ActionKind.Primary,
            onClick = { if (picker.canChoose) path?.let { onChoose(it, name) } },
            enabled = picker.canChoose,
            height = 50.dp,
            cornerRadius = 15.dp,
            modifier = Modifier
                .fillMaxWidth()
                .windowInsetsPadding(WindowInsets.navigationBars)
                .padding(horizontal = ScreenPadding, vertical = 10.dp),
        ) {
            Text(stringResource(R.string.picker_create), style = Type.headline)
        }
    }
}

@Composable
private fun Footer(text: String) {
    Text(text, style = Type.footnote, color = themeColors.secondary, modifier = Modifier.padding(horizontal = 4.dp))
}

@Composable
private fun Divider() {
    HorizontalDivider(Modifier.padding(start = 48.dp), thickness = 1.dp, color = themeColors.hairline)
}

/** 目录列表的一行：图标、名字；[chevron] 时右边带个进入的小箭头。 */
@Composable
private fun FolderRow(
    title: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    hint: String?,
    chevron: Boolean,
    enabled: Boolean,
    onClick: () -> Unit,
) {
    val colors = themeColors
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 48.dp)
            .clickable(enabled = enabled, role = Role.Button, onClickLabel = hint, onClick = onClick)
            .semantics(mergeDescendants = true) { role = Role.Button }
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(icon, null, tint = colors.secondary, modifier = Modifier.size(20.dp))
        Text(title, style = Type.body, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
        if (chevron) DisclosureChevron()
    }
}

/** 工作区名字的输入框，[placeholder] 是空着时按目录取的名字。 */
@Composable
private fun NameField(value: String, placeholder: String, onChange: (String) -> Unit) {
    val colors = themeColors
    val label = stringResource(R.string.name_field)
    Box(
        Modifier
            .fillMaxWidth()
            .cardBackground(colors, padding = 14.dp, shape = Shapes.group),
    ) {
        if (value.isEmpty()) Text(placeholder, style = Type.body, color = colors.tertiary, maxLines = 1)
        BasicTextField(
            value = value,
            onValueChange = onChange,
            singleLine = true,
            textStyle = Type.body.copy(color = colors.primary),
            cursorBrush = SolidColor(colors.primary),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
            modifier = Modifier.fillMaxWidth().semantics { contentDescription = label },
        )
    }
}
