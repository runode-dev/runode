package dev.runode.mobile.features

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.key
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.core.view.WindowCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import dev.runode.mobile.features.model.AppModel
import dev.runode.mobile.features.model.Route
import dev.runode.mobile.features.ui.EmptyState
import dev.runode.mobile.features.ui.TopBar
import dev.runode.mobile.features.ui.git.GitScreen
import dev.runode.mobile.features.ui.home.HomeView
import dev.runode.mobile.features.ui.pairing.PairingView
import dev.runode.mobile.features.ui.screen.RemoteDesktopScreen
import dev.runode.mobile.features.ui.sessions.SessionListView
import dev.runode.mobile.features.ui.settings.SettingsView
import dev.runode.mobile.features.ui.terminal.TerminalScreen
import dev.runode.mobile.features.ui.theme.RunodeTheme
import dev.runode.mobile.features.ui.theme.themeColors

/**
 * App 的根界面：首页，往里是 [AppModel.path] 上的会话列表、终端页这些。配对页、设置页盖在整个导航栈上。系统的返回键依次
 * 关掉配对页、设置页，再退回导航栈的上一页；在首页时交给系统。终端页以外的界面跟电脑上终端的主题走（[AppModel.theme]），
 * 终端页按它那个会话的主题上色。App 进出前台时告诉 [AppModel.setActive]。
 */
@Composable
fun RunodeApp(app: AppModel) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle, app) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> app.setActive(true)
                Lifecycle.Event.ON_STOP -> app.setActive(false)
                else -> {}
            }
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    val pairing = app.pairing
    val top = app.path.lastOrNull()
    BackHandler(enabled = pairing != null || app.showingSettings || top != null) {
        when {
            pairing != null -> app.pairing = null
            app.showingSettings -> closeSettings(app)
            else -> app.path = app.path.dropLast(1)
        }
    }
    RunodeTheme(app.theme) {
        Box(Modifier.fillMaxSize().background(themeColors.page)) {
            when {
                pairing != null -> key(pairing) {
                    SystemBars(themeColors.isDark)
                    PairingView(pairing, app.settings) { machine ->
                        app.pairing = null
                        if (machine != null) app.path = listOf(Route.Machine(machine.id))
                    }
                }
                app.showingSettings -> {
                    SystemBars(themeColors.isDark)
                    SettingsView(app) { closeSettings(app) }
                }
                top == null -> {
                    SystemBars(themeColors.isDark)
                    HomeView(app)
                }
                else -> key(top) { Destination(app, top) }
            }
        }
    }
}

private fun closeSettings(app: AppModel) {
    app.showingSettings = false
    app.settingsDismissed()
}

/** 导航栈上的一页。 */
@Composable
private fun Destination(app: AppModel, route: Route) {
    val back = { app.path = app.path.dropLast(1) }
    when (route) {
        is Route.Machine -> {
            SystemBars(themeColors.isDark)
            val model = app.sessionList(route.machine)
            if (model == null) {
                MissingPage(stringResource(R.string.machine_not_found), back)
            } else {
                SessionListView(
                    model = model,
                    onBack = back,
                    onOpen = { app.openTerminal(route.machine, it) },
                    onOpenGit = { app.openGit(route.machine, it) },
                    onOpenScreen = if (app.canOpenScreen) ({ app.openScreen(route.machine) }) else null,
                    fontSize = app.settings.preferences.fontSize,
                )
            }
        }
        is Route.Terminal -> {
            val session = app.shownSession(route.machine, route.session)
            val model = app.terminal(route.machine, session)
            if (model == null) {
                SystemBars(themeColors.isDark)
                MissingPage(stringResource(R.string.terminal_not_found), back)
            } else {
                SystemBars(model.chromeTheme.isDark)
                TerminalScreen(
                    model = model,
                    preferences = app.settings.preferences,
                    sessions = app.sessionList(route.machine),
                    dictation = app.dictation,
                    machine = app.machineList.machine(route.machine),
                    onBack = back,
                    onOpenGit = { app.openGit(route.machine, session) },
                    onSelectPane = { app.switchTerminal(route.machine, it) },
                    onPaneEnded = { mates -> app.leaveEndedPane(route.machine, session, mates) },
                    onOpenSettings = { app.showingSettings = true },
                )
            }
        }
        is Route.Git -> {
            SystemBars(themeColors.isDark)
            val model = app.git(route.machine, route.session)
            if (model == null) MissingPage(stringResource(R.string.machine_not_found), back) else GitScreen(model, back)
        }
        is Route.Screen -> {
            SystemBars(dark = true)
            val model = app.screen(route.machine)
            if (model == null) MissingPage(stringResource(R.string.machine_not_found), back) else RemoteDesktopScreen(model, back)
        }
    }
}

/** 栈里这一页的电脑或会话已经没了。 */
@Composable
private fun MissingPage(title: String, onBack: () -> Unit) {
    Column(Modifier.fillMaxSize()) {
        TopBar(onBack = onBack) {}
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { EmptyState(title) }
    }
}

/** 状态栏、导航栏的图标按页面的明暗选深浅。 */
@Composable
private fun SystemBars(dark: Boolean) {
    val view = LocalView.current
    val activity = LocalContext.current.findActivity()
    if (view.isInEditMode || activity == null) return
    SideEffect {
        val controller = WindowCompat.getInsetsController(activity.window, view)
        controller.isAppearanceLightStatusBars = !dark
        controller.isAppearanceLightNavigationBars = !dark
    }
}

internal tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}
