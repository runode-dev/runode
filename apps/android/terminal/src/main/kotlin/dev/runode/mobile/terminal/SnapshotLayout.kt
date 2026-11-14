package dev.runode.mobile.terminal

/**
 * [VtJni.nativeSnapshot] 写进缓冲区的一帧，照抄 C 那边的 `SNAPSHOT_*`：整数按本机字节序，偏移以字节计。
 * 两边一致由 [signature] 和 [VtJni.nativeLayout] 相等守着，改布局时 [VERSION] 加一、两边和签名一起改。
 *
 * 一帧依次是：头（[HEADER_SIZE] 字节）、[ROW_COUNT] 个行记录、按 8 字节对齐的 [IMAGE_COUNT] 个图片记录、字素簇池。
 * 行记录是行号（视口上面那一行是 -1）加上 [COLUMNS] 个格子。格子的 [CELL_TEXT] 为 0 是没有字；不带
 * [CELL_TEXT_GRAPHEME] 时是唯一的码点，带着时低 31 位是字素簇池里的下标（以 int32 计），那里先是码点个数、
 * 再是各个码点。颜色带着 [COLOR_SET] 时低 24 位是 0xRRGGBB，没带是用默认色。
 */
internal object SnapshotLayout {
    const val VERSION = 1

    const val FLAGS = 0
    const val COLUMNS = 4
    const val ROWS = 8
    const val BACKGROUND = 12
    const val FOREGROUND = 16
    const val CURSOR_COLOR = 20
    const val ABOVE_COLUMNS = 24
    const val CURSOR_COLUMN = 28
    const val CURSOR_ROW = 32
    const val CURSOR_SHAPE = 36
    const val ROW_COUNT = 40
    const val IMAGE_COUNT = 44
    const val IMAGES_OFFSET = 48
    const val GRAPHEMES_OFFSET = 52
    const val NEXT_IMAGE_FRAME_MS = 56
    const val HEADER_SIZE = 64

    const val FLAG_FULL = 1 shl 0
    const val FLAG_ABOVE_CHANGED = 1 shl 1
    const val FLAG_IMAGES_CHANGED = 1 shl 2
    const val FLAG_CURSOR = 1 shl 3
    const val FLAG_CURSOR_BLINKING = 1 shl 4
    const val FLAG_CURSOR_WIDE_TAIL = 1 shl 5
    const val FLAG_CURSOR_COLOR = 1 shl 6
    const val FLAG_NEXT_IMAGE_FRAME = 1 shl 7

    const val ROW_Y = 0
    const val ROW_HEADER_SIZE = 4

    const val CELL_TEXT = 0
    const val CELL_FOREGROUND = 4
    const val CELL_BACKGROUND = 8
    const val CELL_ATTRIBUTES = 12
    const val CELL_UNDERLINE = 13
    const val CELL_WIDTH = 14
    const val CELL_SIZE = 16

    const val CELL_TEXT_GRAPHEME = Int.MIN_VALUE
    const val COLOR_SET = 0x01000000

    /** 格子的样式位就是 [CellAttributes] 的位；下划线、宽度、光标形状、图片的层就是对应枚举的 `ordinal`。 */
    private val attributeBits = listOf(
        CellAttributes.bold, CellAttributes.italic, CellAttributes.faint, CellAttributes.inverse,
        CellAttributes.invisible, CellAttributes.strikethrough, CellAttributes.blink,
    ).map { it.rawValue }

    const val IMAGE_ID = 0
    const val IMAGE_LAYER = 4
    const val IMAGE_Z = 8
    const val IMAGE_COLUMN = 12
    const val IMAGE_ROW = 16
    const val IMAGE_OFFSET_X = 20
    const val IMAGE_OFFSET_Y = 24
    const val IMAGE_WIDTH = 28
    const val IMAGE_HEIGHT = 32
    const val IMAGE_SOURCE_X = 36
    const val IMAGE_SOURCE_Y = 40
    const val IMAGE_SOURCE_WIDTH = 44
    const val IMAGE_SOURCE_HEIGHT = 48
    const val IMAGE_GENERATION = 56
    const val IMAGE_SIZE = 64

    /** 和 C 那边的 `layout_signature` 逐项相等。 */
    val signature: IntArray
        get() = (
            listOf(
                VERSION, HEADER_SIZE, FLAGS, COLUMNS, ROWS, BACKGROUND, FOREGROUND, CURSOR_COLOR, ABOVE_COLUMNS,
                CURSOR_COLUMN, CURSOR_ROW, CURSOR_SHAPE, ROW_COUNT, IMAGE_COUNT, IMAGES_OFFSET, GRAPHEMES_OFFSET,
                NEXT_IMAGE_FRAME_MS,
                FLAG_FULL, FLAG_ABOVE_CHANGED, FLAG_IMAGES_CHANGED, FLAG_CURSOR, FLAG_CURSOR_BLINKING,
                FLAG_CURSOR_WIDE_TAIL, FLAG_CURSOR_COLOR, FLAG_NEXT_IMAGE_FRAME,
                ROW_Y, ROW_HEADER_SIZE,
                CELL_TEXT, CELL_FOREGROUND, CELL_BACKGROUND, CELL_ATTRIBUTES, CELL_UNDERLINE, CELL_WIDTH, CELL_SIZE,
                CELL_TEXT_GRAPHEME, COLOR_SET,
            ) +
                attributeBits +
                UnderlineStyle.entries.map { it.ordinal } +
                listOf(CellWidth.Narrow, CellWidth.Wide, CellWidth.SpacerTail, CellWidth.SpacerHead).map { it.ordinal } +
                listOf(CursorShape.Block, CursorShape.BlockHollow, CursorShape.Bar, CursorShape.Underline)
                    .map { it.ordinal } +
                listOf(
                    IMAGE_ID, IMAGE_LAYER, IMAGE_Z, IMAGE_COLUMN, IMAGE_ROW, IMAGE_OFFSET_X, IMAGE_OFFSET_Y,
                    IMAGE_WIDTH, IMAGE_HEIGHT, IMAGE_SOURCE_X, IMAGE_SOURCE_Y, IMAGE_SOURCE_WIDTH,
                    IMAGE_SOURCE_HEIGHT, IMAGE_GENERATION, IMAGE_SIZE,
                ) +
                listOf(ImageLayer.BelowBackground, ImageLayer.BelowText, ImageLayer.AboveText).map { it.ordinal }
            ).toIntArray()
}
