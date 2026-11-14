package dev.runode.mobile.features.ui.settings

import android.content.Context
import android.content.Intent
import android.provider.Settings
import android.text.format.Formatter
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
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
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.features.R
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.AppPreferences
import dev.runode.mobile.features.model.DictationModel
import dev.runode.mobile.features.model.DictationSource
import dev.runode.mobile.features.model.MachineListModel
import dev.runode.mobile.features.model.Presentation
import dev.runode.mobile.features.model.PushRegistration
import dev.runode.mobile.features.model.SettingsModel
import dev.runode.mobile.features.model.SizePreference
import dev.runode.mobile.features.ui.AppIcons
import dev.runode.mobile.features.ui.ConfirmDialog
import dev.runode.mobile.features.ui.DisclosureChevron
import dev.runode.mobile.features.ui.EmptyState
import dev.runode.mobile.features.ui.ErrorDialog
import dev.runode.mobile.features.ui.LeadingTitle
import dev.runode.mobile.features.ui.ListSectionHeader
import dev.runode.mobile.features.ui.ScreenPadding
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.theme.Shapes
import dev.runode.mobile.features.ui.theme.Type
import dev.runode.mobile.features.ui.theme.cardBackground
import dev.runode.mobile.features.ui.theme.resolve
import dev.runode.mobile.features.ui.theme.themeColors
import dev.runode.mobile.speech.SpeechModels
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.UUID
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 设置页：终端、语音输入、通知、配对过的电脑、报给电脑的设备名，以及版本和开源许可。改了马上生效、马上存（[SettingsModel]）。
 * 一台电脑的详情和许可在本页里往里翻，系统的返回键先退回上一层。
 */
@Composable
fun SettingsView(app: AppModel, onClose: () -> Unit) {
    var machineId by rememberSaveable { mutableStateOf<String?>(null) }
    var showingLicenses by rememberSaveable { mutableStateOf(false) }
    var license by rememberSaveable { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val machines = app.machineList
    val opened = machineId?.let(UUID::fromString)
    val shownLicense = license?.let { name -> licenses.firstOrNull { it.name == name } }
    when {
        opened != null -> {
            BackHandler { machineId = null }
            MachineSettingsView(app, machines, opened, rename = { name -> scope.launch { machines.rename(opened, name) } }) {
                machineId = null
            }
        }
        shownLicense != null -> {
            BackHandler { license = null }
            LicenseView(shownLicense) { license = null }
        }
        showingLicenses -> {
            BackHandler { showingLicenses = false }
            LicenseList(onOpen = { license = it.name }) { showingLicenses = false }
        }
        else -> SettingsList(app, onClose, onOpenMachine = { machineId = it.toString() }, onOpenLicenses = { showingLicenses = true })
    }
    // 设置页盖着首页，首页的提示弹不出来：电脑改名、删除出的错在这里说。
    ErrorDialog(machines.errorMessage?.resolve()) { machines.errorMessage = null }
}

@Composable
private fun SettingsList(app: AppModel, onClose: () -> Unit, onOpenMachine: (UUID) -> Unit, onOpenLicenses: () -> Unit) {
    val colors = themeColors
    val settings = app.settings
    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = onClose) { LeadingTitle(stringResource(R.string.settings)) }
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = ScreenPadding, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            section("terminal", R.string.settings_terminal, R.string.settings_terminal_footer) { TerminalRows(settings) }
            section("dictation", R.string.settings_dictation, R.string.settings_dictation_footer) { DictationRows(app.dictation, settings) }
            section("alerts", R.string.settings_notifications, R.string.settings_notifications_footer) {
                AlertRows(settings, app.push, app.machineList)
            }
            section("machines", R.string.home_computers, null) {
                for (machine in app.machineList.machines) {
                    MachineRow(machine, app.sessionList(machine.id)?.linkState ?: LinkState.Idle) { onOpenMachine(machine.id) }
                    RowDivider()
                }
                ButtonRow(stringResource(R.string.settings_pair_new), AppIcons.qrcode) {
                    app.pairsAfterSettings = true
                    onClose()
                }
            }
            section("device", R.string.settings_this_phone, R.string.settings_device_footer) {
                TextFieldRow(
                    initial = settings.preferences.deviceName ?: "",
                    placeholder = settings.systemDeviceName,
                    label = stringResource(R.string.settings_device_name),
                ) { settings.setDeviceName(it) }
            }
            section("about", R.string.settings_about, null) {
                val context = LocalContext.current
                ValueRow(stringResource(R.string.settings_version), remember(context) { version(context) })
                RowDivider()
                NavigationRow(stringResource(R.string.settings_licenses), onClick = onOpenLicenses)
            }
            item(key = "bottom") { Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars)) }
        }
    }
}

/** 列表里的一节：标题、并成一张卡片的几行、下面的说明。 */
private fun LazyListScope.section(key: String, header: Int, footer: Int?, rows: @Composable ColumnScope.() -> Unit) {
    item(key = key) {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            ListSectionHeader(stringResource(header))
            Column(Modifier.fillMaxWidth().cardBackground(themeColors, padding = 0.dp, shape = Shapes.group), content = rows)
            if (footer != null) Footer(stringResource(footer))
        }
    }
}

// 终端

@Composable
private fun TerminalRows(settings: SettingsModel) {
    val preferences = settings.preferences
    val context = LocalContext.current
    MenuPickerRow(
        stringResource(R.string.settings_default_size),
        SizePreference.entries,
        preferences.defaultSize,
        { Presentation.sizePreference(it).resolve() },
    ) { settings.preferences = settings.preferences.copy(defaultSize = it) }
    RowDivider()
    SwitchRow(stringResource(R.string.settings_font_follows), preferences.fontSize == null) { follows ->
        // 关掉跟随时从系统现在给的字号开始调。
        settings.setFontSize(if (follows) null else systemFontSize(context))
    }
    preferences.fontSize?.let { size ->
        RowDivider()
        FontSizeRow(size) { settings.setFontSize(it) }
        val previewSize = with(LocalDensity.current) { size.toFloat().dp.toSp() }
        val previewLabel = stringResource(R.string.settings_font_preview)
        Text(
            "ls -la ~/projects",
            fontFamily = FontFamily.Monospace,
            fontSize = previewSize,
            maxLines = 1,
            overflow = TextOverflow.Clip,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 10.dp)
                .clearAndSetSemantics { contentDescription = previewLabel },
        )
    }
    RowDivider()
    SwitchRow(stringResource(R.string.settings_bell), preferences.bellHaptics) {
        settings.preferences = settings.preferences.copy(bellHaptics = it)
    }
}

/** 关掉「字号跟随系统」时的起点：按现在的系统字体大小算的终端默认字号（dp）。 */
private fun systemFontSize(context: Context): Double =
    (13 * context.resources.configuration.fontScale).roundToInt().toDouble().coerceIn(11.0, 28.0)

/** 字号：现在的点数，两边减小和加大，到了 [AppPreferences.fontSizes] 的边上不能再按。 */
@Composable
private fun FontSizeRow(size: Double, onChange: (Double) -> Unit) {
    val colors = themeColors
    val value = stringResource(R.string.settings_font_points, size.roundToInt())
    val range = AppPreferences.fontSizes
    Row(
        Modifier.fillMaxWidth().defaultMinSize(minHeight = 52.dp).padding(start = 16.dp, end = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(stringResource(R.string.settings_font_size), style = Type.body, modifier = Modifier.weight(1f))
        Text(value, style = Type.body, color = colors.secondary, modifier = Modifier.padding(end = 8.dp))
        StepButton(AppIcons.minusCircle, stringResource(R.string.settings_font_smaller), size > range.start) { onChange(size - 1) }
        StepButton(AppIcons.plusCircle, stringResource(R.string.settings_font_larger), size < range.endInclusive) { onChange(size + 1) }
    }
}

@Composable
private fun StepButton(icon: ImageVector, label: String, enabled: Boolean, onClick: () -> Unit) {
    val colors = themeColors
    Box(
        Modifier
            .size(44.dp)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, null, tint = if (enabled) colors.primary else colors.tertiary, modifier = Modifier.size(24.dp))
    }
}

// 语音输入

/** 在哪里识别，手机上的模型下没下（下载、进度、删除）。 */
@Composable
private fun DictationRows(dictation: DictationModel, settings: SettingsModel) {
    val colors = themeColors
    val context = LocalContext.current
    val total = SpeechModels.downloadSize
    val size = remember(context) { Formatter.formatShortFileSize(context, total) }
    MenuPickerRow(
        stringResource(R.string.settings_dictation_source),
        DictationSource.entries,
        settings.preferences.dictationSource,
        {
            stringResource(
                when (it) {
                    DictationSource.Phone -> R.string.settings_dictation_phone
                    DictationSource.Computer -> R.string.settings_dictation_computer
                },
            )
        },
    ) { settings.preferences = settings.preferences.copy(dictationSource = it) }
    RowDivider()
    val title = stringResource(R.string.settings_phone_model)
    when (val download = dictation.download) {
        is DictationModel.Download.Running -> {
            val fraction = if (total > 0) (download.done.toFloat() / total).coerceIn(0f, 1f) else 0f
            val percent = "${(fraction * 100).toInt()}%"
            val state = stringResource(R.string.settings_model_downloading, percent)
            Row(
                Modifier.fillMaxWidth().defaultMinSize(minHeight = 52.dp).padding(start = 16.dp, end = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text(title, style = Type.body, modifier = Modifier.weight(1f))
                LinearProgressIndicator(
                    progress = { fraction },
                    color = colors.primary,
                    trackColor = colors.fill,
                    modifier = Modifier.width(100.dp).semantics {
                        stateDescription = state
                        progressBarRangeInfo = ProgressBarRangeInfo(fraction, 0f..1f)
                    },
                )
                Text(
                    stringResource(R.string.action_cancel),
                    style = Type.body,
                    modifier = Modifier
                        .clickable(role = Role.Button) { dictation.cancelDownload() }
                        .padding(horizontal = 8.dp, vertical = 12.dp),
                )
            }
        }
        DictationModel.Download.Idle -> if (dictation.installed) {
            ValueRow(title, stringResource(R.string.settings_model_downloaded, size))
            RowDivider()
            ButtonRow(stringResource(R.string.settings_model_delete), null, colors.failure) { dictation.remove() }
        } else {
            ModelDownloadRows(title, size, dictation, null)
        }
        is DictationModel.Download.Failed -> ModelDownloadRows(title, size, dictation, download.reason.resolve())
    }
}

@Composable
private fun ModelDownloadRows(title: String, size: String, dictation: DictationModel, failure: String?) {
    val colors = themeColors
    ValueRow(title, stringResource(R.string.settings_model_not_downloaded, size))
    RowDivider()
    ButtonRow(stringResource(R.string.dictation_download), AppIcons.download) { dictation.startDownload() }
    if (failure != null) {
        RowDivider()
        WarningRow(failure, colors.failure)
    }
}

// 通知

@Composable
private fun AlertRows(settings: SettingsModel, push: PushRegistration, machines: MachineListModel) {
    val colors = themeColors
    val context = LocalContext.current
    SwitchRow(stringResource(R.string.pairing_notify), settings.preferences.alertsBlockedAgents) {
        settings.preferences = settings.preferences.copy(alertsBlockedAgents = it)
    }
    if (!settings.preferences.alertsBlockedAgents) return
    if (!push.systemAllowed) {
        RowDivider()
        WarningRow(stringResource(R.string.settings_notifications_off), colors.failure) {
            context.startActivity(
                Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                    .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }
    } else {
        push.tokenError?.let { error ->
            RowDivider()
            WarningRow(stringResource(R.string.settings_push_token_failed, error.resolve()), colors.failure)
        }
    }
    for (machine in machines.machines) {
        val status = push.statuses[machine.id] as? PushRegistration.Status.Failed ?: continue
        RowDivider()
        ValueRow(machine.name, stringResource(R.string.settings_push_failed, status.message.resolve()))
    }
}

// 电脑

@Composable
private fun MachineRow(machine: MachineRecord, state: LinkState, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 56.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(machine.name, style = Type.body, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                Presentation.linkState(state, shownName = machine.name).resolve(),
                style = Type.footnote,
                color = themeColors.secondary,
                maxLines = 2,
            )
        }
        DisclosureChevron()
    }
}

/**
 * 一台电脑的详情：改名（按完成或返回时存，[rename]）、配对信息、删除（先确认）。删除经 [MachineListModel.delete]，连着时先请
 * 电脑解除配对、注销推送。
 */
@Composable
private fun MachineSettingsView(
    app: AppModel,
    machines: MachineListModel,
    id: UUID,
    rename: (String) -> Unit,
    onBack: () -> Unit,
) {
    val colors = themeColors
    val scope = rememberCoroutineScope()
    val machine = machines.machine(id)
    var confirmingDelete by remember { mutableStateOf(false) }
    var name by remember(id) { mutableStateOf(machine?.name ?: "") }
    fun commit() {
        val trimmed = name.trim()
        val current = machines.machine(id) ?: return
        if (trimmed.isEmpty() || trimmed == current.name) {
            name = current.name
            return
        }
        rename(trimmed)
    }
    val back = {
        commit()
        onBack()
    }
    BackHandler(onBack = back)
    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = back) { LeadingTitle(machine?.name ?: stringResource(R.string.home_computers)) }
        if (machine == null) {
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { EmptyState(stringResource(R.string.machine_deleted)) }
            return@Column
        }
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = ScreenPadding, vertical = 4.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            section("name", R.string.name_field, R.string.machine_name_footer) {
                PlainTextField(name, machine.hostName, stringResource(R.string.name_field), { name = it }) { commit() }
            }
            section("info", R.string.machine_pairing_info, null) {
                ValueRow(stringResource(R.string.machine_host_name), machine.hostName)
                RowDivider()
                val paired = remember(machine.pairedAt) {
                    DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)
                        .format(machine.pairedAt.atZone(ZoneId.systemDefault()))
                }
                ValueRow(stringResource(R.string.machine_paired_at), paired)
                machine.lastAddress?.let { address ->
                    RowDivider()
                    ValueRow(stringResource(R.string.machine_last_address), "$address:${machine.port}", monospace = true)
                }
                RowDivider()
                ValueRow(stringResource(R.string.machine_device_id), machine.deviceId, monospace = true)
                RowDivider()
                ValueRow(stringResource(R.string.machine_fingerprint), machine.fingerprint.toString(), monospace = true)
            }
            item(key = "delete") {
                Column(Modifier.padding(top = 12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Column(Modifier.fillMaxWidth().cardBackground(colors, padding = 0.dp, shape = Shapes.group)) {
                        ButtonRow(stringResource(R.string.machine_delete), null, colors.failure) { confirmingDelete = true }
                    }
                    Footer(stringResource(R.string.machine_delete_footer))
                }
            }
            item(key = "bottom") { Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.navigationBars)) }
        }
    }
    if (confirmingDelete) {
        // 连着时删除前先请电脑解除配对；没连着时等它再连上，说明这一点。
        val connected = app.sessionList(id)?.linkState?.isConnected == true
        ConfirmDialog(
            title = stringResource(R.string.home_delete_title),
            message = stringResource(if (connected) R.string.machine_delete_footer else R.string.machine_delete_offline),
            confirm = stringResource(R.string.action_delete),
            onConfirm = {
                scope.launch {
                    machines.delete(id)
                    onBack()
                }
            },
            onDismiss = { confirmingDelete = false },
        )
    }
}

// 许可

/** 一份开源许可：名字和随 App 打包的许可文件（assets 里的路径）。 */
private class License(val name: String, val files: List<String>)

/** 编进 App 的第三方代码和图：libghostty-vt 和编进它的几个库，终端里画图标用的 Nerd Fonts，各家 agent 的 logo。 */
private val licenses = listOf(
    License("libghostty-vt", listOf("licenses/ghostty/LICENSE")),
    License("simdutf", listOf("licenses/simdutf/LICENSE")),
    License("Highway", listOf("licenses/highway/LICENSE")),
    License(
        "uucode",
        listOf("licenses/uucode/LICENSE.md", "licenses/uucode/LICENSE_unicode", "licenses/uucode/LICENSE_Bjoern_Hoehrmann"),
    ),
    License("Nerd Fonts Symbols Only", listOf("NerdFontsSymbolsOnly/LICENSE", "NerdFontsSymbolsOnly/README.md")),
    License("lobe-icons", listOf("licenses/agent-logos/LICENSE")),
)

@Composable
private fun LicenseList(onOpen: (License) -> Unit, onBack: () -> Unit) {
    val colors = themeColors
    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = onBack) { LeadingTitle(stringResource(R.string.settings_licenses)) }
        Column(
            Modifier
                .padding(horizontal = ScreenPadding, vertical = 8.dp)
                .fillMaxWidth()
                .cardBackground(colors, padding = 0.dp, shape = Shapes.group),
        ) {
            licenses.forEachIndexed { index, license ->
                NavigationRow(license.name) { onOpen(license) }
                if (index < licenses.lastIndex) RowDivider()
            }
        }
    }
}

@Composable
private fun LicenseView(license: License, onBack: () -> Unit) {
    val colors = themeColors
    val context = LocalContext.current
    val text by produceState("", license) {
        value = withContext(Dispatchers.IO) {
            license.files.joinToString("\n\n") { path ->
                runCatching { context.assets.open(path).use { it.readBytes().decodeToString() } }.getOrDefault("")
            }
        }
    }
    Column(Modifier.fillMaxSize().background(colors.page)) {
        TopBar(onBack = onBack) { LeadingTitle(license.name) }
        Text(
            text,
            style = Type.caption,
            fontFamily = FontFamily.Monospace,
            color = colors.secondary,
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = ScreenPadding, vertical = 8.dp),
        )
    }
}

// 行

@Composable
private fun RowDivider() {
    HorizontalDivider(Modifier.padding(start = 16.dp), thickness = 1.dp, color = themeColors.hairline)
}

@Composable
private fun Footer(text: String) {
    Text(text, style = Type.footnote, color = themeColors.secondary, modifier = Modifier.padding(horizontal = 4.dp))
}

/** 名字和右边的值。 */
@Composable
private fun ValueRow(title: String, value: String, monospace: Boolean = false) {
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 52.dp)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(title, style = Type.body)
        Text(
            value,
            style = if (monospace) Type.footnote else Type.body,
            fontFamily = if (monospace) FontFamily.Monospace else null,
            color = themeColors.secondary,
            textAlign = TextAlign.End,
            maxLines = if (monospace) 1 else 3,
            overflow = if (monospace) TextOverflow.MiddleEllipsis else TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

/** 开关的一行，整行都能点；开着时是成功的绿色，看得出开着。 */
@Composable
private fun SwitchRow(title: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    val colors = themeColors
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 52.dp)
            .toggleable(value = checked, role = Role.Switch, onValueChange = onChange)
            .padding(horizontal = 16.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = Type.body, modifier = Modifier.weight(1f).padding(end = 12.dp))
        Switch(
            checked = checked,
            onCheckedChange = null,
            colors = SwitchDefaults.colors(
                checkedTrackColor = colors.success,
                checkedThumbColor = Color.White,
                checkedBorderColor = Color.Transparent,
                uncheckedTrackColor = colors.fill,
                uncheckedThumbColor = colors.secondary,
                uncheckedBorderColor = Color.Transparent,
            ),
        )
    }
}

/** 从几项里选一项：右边是选着的那项，点开菜单换。 */
@Composable
private fun <T> MenuPickerRow(title: String, options: List<T>, selected: T, label: @Composable (T) -> String, onSelect: (T) -> Unit) {
    val colors = themeColors
    var expanded by remember { mutableStateOf(false) }
    val value = label(selected)
    Box {
        Row(
            Modifier
                .fillMaxWidth()
                .defaultMinSize(minHeight = 52.dp)
                .clickable(role = Role.DropdownList) { expanded = true }
                .semantics(mergeDescendants = true) {}
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(title, style = Type.body, modifier = Modifier.weight(1f))
            Text(value, style = Type.body, color = colors.secondary)
            Spacer(Modifier.width(6.dp))
            Icon(AppIcons.chevronRight, null, tint = colors.tertiary, modifier = Modifier.size(14.dp))
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }, modifier = Modifier.align(Alignment.TopEnd)) {
            for (option in options) {
                DropdownMenuItem(
                    text = { Text(label(option)) },
                    trailingIcon = if (option == selected) ({ Icon(AppIcons.check, null, Modifier.size(18.dp)) }) else null,
                    onClick = {
                        expanded = false
                        onSelect(option)
                    },
                )
            }
        }
    }
}

/** 一行按钮：可选的图标加文字，[color] 给删除这类危险操作用失败色。 */
@Composable
private fun ButtonRow(title: String, icon: ImageVector?, color: Color = Color.Unspecified, onClick: () -> Unit) {
    val tint = if (color == Color.Unspecified) themeColors.primary else color
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 52.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        if (icon != null) Icon(icon, null, tint = tint, modifier = Modifier.size(20.dp))
        Text(title, style = Type.body, color = tint)
    }
}

/** 往里翻一页的一行。 */
@Composable
private fun NavigationRow(title: String, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .defaultMinSize(minHeight = 52.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = Type.body, modifier = Modifier.weight(1f))
        DisclosureChevron()
    }
}

/** 一行提醒：警告图标加文字；给了 [onClick] 时整行能点，去补救。 */
@Composable
private fun WarningRow(text: String, color: Color, onClick: (() -> Unit)? = null) {
    Row(
        Modifier
            .fillMaxWidth()
            .then(if (onClick != null) Modifier.clickable(role = Role.Button, onClick = onClick) else Modifier)
            .semantics(mergeDescendants = true) {}
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(AppIcons.warning, null, tint = color, modifier = Modifier.size(18.dp))
        Text(text, style = Type.subheadline, color = color, modifier = Modifier.weight(1f))
    }
}

/** 一行输入框：按完成或离开输入框时交给 [onCommit]；[initial] 在第一次组合时填进去。 */
@Composable
private fun TextFieldRow(initial: String, placeholder: String, label: String, onCommit: (String) -> Unit) {
    var text by rememberSaveable { mutableStateOf(initial) }
    PlainTextField(text, placeholder, label, { text = it }) { onCommit(text) }
}

@Composable
private fun PlainTextField(value: String, placeholder: String, label: String, onChange: (String) -> Unit, onCommit: () -> Unit) {
    val colors = themeColors
    val focus = LocalFocusManager.current
    var focused by remember { mutableStateOf(false) }
    Box(Modifier.fillMaxWidth().defaultMinSize(minHeight = 52.dp).padding(horizontal = 16.dp, vertical = 14.dp)) {
        if (value.isEmpty()) Text(placeholder, style = Type.body, color = colors.tertiary, maxLines = 1)
        BasicTextField(
            value = value,
            onValueChange = onChange,
            singleLine = true,
            textStyle = Type.body.copy(color = colors.primary),
            cursorBrush = SolidColor(colors.primary),
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = { focus.clearFocus() }),
            modifier = Modifier
                .fillMaxWidth()
                .onFocusChanged {
                    if (focused && !it.isFocused) onCommit()
                    focused = it.isFocused
                }
                .semantics { contentDescription = label },
        )
    }
}

/** 「0.1.0 (1)」：版本名和版本号。 */
private fun version(context: Context): String {
    val info = runCatching { context.packageManager.getPackageInfo(context.packageName, 0) }.getOrNull() ?: return "—"
    val name = info.versionName ?: "—"
    return context.getString(R.string.settings_version_value, name, info.longVersionCode.toString())
}
