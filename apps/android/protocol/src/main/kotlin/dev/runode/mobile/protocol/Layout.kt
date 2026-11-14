package dev.runode.mobile.protocol

import kotlinx.serialization.Serializable

// 电脑上的 app 里各个终端摆在哪：窗口、工作区、标签和标签里的分屏，回 `Layout`。和宿主的
// `WindowLayout` 等一一对应；序号一律从 1 开始，窗口按打开的先后，工作区、标签按界面上的先后，
// 分屏按标签里从左到右、从上到下。

@Serializable
data class WindowLayout(
    val index: UInt,
    /** 是最前面的那个窗口。 */
    val front: Boolean = false,
    val workspaces: List<WorkspaceLayout>,
)

/** 窗口里的一个工作区。 */
@Serializable
data class WorkspaceLayout(
    val index: UInt,
    /** 侧栏里显示的名字。 */
    val name: String? = null,
    /** 工作区的目录；宿主没报时为空。 */
    val dir: String? = null,
    /** 是窗口当前显示的工作区。 */
    val active: Boolean = false,
    val tabs: List<TabLayout>,
) {
    /** 工作区里的会话：按标签的先后、标签里分屏的先后。 */
    val sessions: List<SessionId> get() = tabs.flatMap { tab -> tab.panes.map { it.id } }

    /** 在这个工作区里开新标签时挨着的会话：当前标签里有焦点的分屏，没标出来时退到第一个分屏。 */
    val anchor: SessionId?
        get() {
            val tab = tabs.firstOrNull { it.active } ?: tabs.firstOrNull()
            return (tab?.panes?.firstOrNull { it.focused } ?: tab?.panes?.firstOrNull())?.id
        }
}

/** 工作区里的一个标签。 */
@Serializable
data class TabLayout(
    val index: UInt,
    /** 是工作区当前显示的标签。 */
    val active: Boolean = false,
    val panes: List<PaneLayout>,
)

/** 标签里的一个分屏，即一个终端。 */
@Serializable
data class PaneLayout(
    val index: UInt,
    val id: SessionId,
    /** 在标签里的位置；读不出来时为空。 */
    @Serializable(with = LenientPaneRect::class) val rect: PaneRect? = null,
    /** 是标签里有焦点的那个分屏。 */
    val focused: Boolean = false,
    /** 浮在别的分屏上面；[rect] 是它浮着的位置，和别的分屏的位置会重叠。 */
    val floating: Boolean = false,
)

/** 分屏在标签区域里的位置，整个标签区域按 0..[extent] 归一化，和电脑上窗口的像素大小无关。 */
@Serializable
data class PaneRect(
    val x: UShort,
    val y: UShort,
    val width: UShort,
    val height: UShort,
) {
    companion object {
        /** 整个标签区域的边长。 */
        const val extent: UShort = 1000u
    }
}

internal object LenientPaneRect : LenientSerializer<PaneRect>(PaneRect.serializer())
