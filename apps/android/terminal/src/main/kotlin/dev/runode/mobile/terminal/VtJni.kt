package dev.runode.mobile.terminal

import java.nio.ByteBuffer

/**
 * librunode_vt.so 的入口。句柄是 native 那边一份 VT 连同读屏幕、编码输入用的对象；同一时刻只能有一个线程拿它调，
 * [nativeFree] 之后不能再传进来，这些由 [VTerminal] 保证。
 */
internal object VtJni {
    init {
        System.loadLibrary("runode_vt")
    }

    /** 建不出来时返回 0。 */
    @JvmStatic
    external fun nativeNew(cols: Int, rows: Int): Long

    @JvmStatic
    external fun nativeFree(handle: Long)

    @JvmStatic
    external fun nativeFeed(handle: Long, bytes: ByteArray, offset: Int, length: Int)

    @JvmStatic
    external fun nativeResize(handle: Long, cols: Int, rows: Int, cellWidth: Int, cellHeight: Int)

    @JvmStatic
    external fun nativeConfigureCommon(
        handle: Long,
        scrollbackLines: Long,
        scrollbackBytes: Long,
        unknownMaxBytes: Long,
        imageStorageBytes: Long,
    )

    /**
     * 颜色是 0xRRGGBB，[cursor] 为 -1 时光标色跟随单元格；[palette] 每项是 `下标 shl 24 or 0xRRGGBB`，
     * [cursorStyle] 是 [GhosttyAbi] 的光标样式。读不出内置调色板、什么都没改时返回假。
     */
    @JvmStatic
    external fun nativeApplyTheme(
        handle: Long,
        background: Int,
        foreground: Int,
        cursor: Int,
        palette: IntArray,
        cursorStyle: Int,
        cursorBlink: Boolean,
    ): Boolean

    /** 程序用同步输出冻结屏幕的时刻（和 [System.nanoTime] 同一个时钟）；没冻结时为 -1。 */
    @JvmStatic
    external fun nativeRenderHeldSince(handle: Long): Long

    @JvmStatic
    external fun nativeMode(handle: Long, value: Int, ansi: Boolean): Boolean

    @JvmStatic
    external fun nativeMouseTracking(handle: Long): Boolean

    @JvmStatic
    external fun nativeAlternateScreen(handle: Long): Boolean

    @JvmStatic
    external fun nativeScrollbackMaxBytes(handle: Long): Long

    /** 总行数、视口顶在第几行、视口多高写进 [out]；读不出时返回假。 */
    @JvmStatic
    external fun nativeScrollbar(handle: Long, out: LongArray): Boolean

    @JvmStatic
    external fun nativeScrollToBottom(handle: Long)

    /** 视口挪 [delta] 行，负数往上看更早的内容。 */
    @JvmStatic
    external fun nativeScrollBy(handle: Long, delta: Long)

    /**
     * 按 [SnapshotLayout] 往 [buffer]（direct）写一帧。`frame*` 是调用方那一帧现在的样子，没有光标色为 -1。
     * 返回写了多少字节；更新失败时为 0；[buffer] 不够大时是负的所需字节数，扩容后带着 [retry] 再调。
     */
    @JvmStatic
    external fun nativeSnapshot(
        handle: Long,
        buffer: ByteBuffer,
        retry: Boolean,
        frameColumns: Int,
        frameRows: Int,
        frameBackground: Int,
        frameForeground: Int,
        frameCursorColor: Int,
        frameAboveColumns: Int,
    ): Int

    /** 一块 Kitty 图片现在不预乘的 RGBA，宽高写进 [size]；不是 [generation] 那一版或读不出时为空。 */
    @JvmStatic
    external fun nativeImagePixels(handle: Long, id: Int, generation: Long, size: IntArray): ByteArray?

    /** [key]、[mods]、[consumedMods]、[optionAsAlt] 是 [GhosttyAbi] 的取值；[unshifted] 没有时为 0。 */
    @JvmStatic
    external fun nativeEncodeKey(
        handle: Long,
        key: Int,
        mods: Int,
        consumedMods: Int,
        unshifted: Int,
        text: ByteArray?,
        optionAsAlt: Int,
    ): ByteArray

    /** [action]、[button] 是 [GhosttyAbi] 的取值；编码失败时为空。 */
    @JvmStatic
    external fun nativeEncodeMouse(
        handle: Long,
        action: Int,
        button: Int,
        column: Int,
        row: Int,
        columns: Int,
        rows: Int,
        cellWidth: Int,
        cellHeight: Int,
    ): ByteArray?

    @JvmStatic
    external fun nativeEncodePaste(text: ByteArray, bracketed: Boolean): ByteArray

    /** C 那边的 [SnapshotLayout.signature]。 */
    @JvmStatic
    external fun nativeLayout(): IntArray

    /** C 那边的 [GhosttyAbi.signature]。 */
    @JvmStatic
    external fun nativeGhosttyConstants(): IntArray
}
