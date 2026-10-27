import CoreGraphics
import Foundation
internal import GhosttyVt
import ImageIO
import RunodeProtocol

/// 手机这边的一份 libghostty-vt 状态机，只消费宿主转来的字节流（VT 重放和 PTY 输出）。
///
/// 权威的那份 VT 在宿主里，终端查询只由它应答：这里不注册写回 PTY 的回调，查询序列静默忽略。
/// 改 VT 状态的操作（改尺寸、换主题）都在宿主插进输出流的 `Resized`、`ThemeApplied` 处做，
/// 两份 VT 才不会分叉。按键、粘贴按这份 VT 当前的模式编码好后交给调用方，由它发给宿主。
///
/// 归谁管：libghostty 的句柄不能跨线程用，所以这是个非 `Sendable` 的类，由主 actor 上的
/// `TerminalModel` 持有，只在主 actor 上建、喂、读和释放；编译器保证它不会被送到别的隔离域。
public final class VTerminal {
    /// 回滚历史最多留多少行，和宿主的 `SCROLLBACK_LINES` 一致；另有字节上限，见 `scrollbackBytesCap`。
    static let scrollbackLines = 10_000
    /// 回滚历史的字节上限取宿主下发的 `TermSettings.scrollbackLimit` 和它中小的那个：这份 VT 从宿主的
    /// 重放起步，用不着和桌面留一样多；iPhone 内存紧，一个会话占太多会被 jetsam 杀掉。
    static let scrollbackBytesCap: UInt64 = 4 << 20
    /// 未知 OSC 最多留多少字节，和宿主的 `UNKNOWN_SEQUENCE_MAX_BYTES` 一致。
    static let unknownSequenceMaxBytes = 256 * 1024
    /// 每块屏幕最多存多少字节 Kitty 图片的像素，超了先丢最老的。比宿主存得少，因为 iPhone 内存紧；
    /// 这边丢了的只是画不出来，查询由宿主应答，两份 VT 不会因此分叉。
    static let kittyImageStorageBytes: UInt64 = 16 << 20

    private let handle: GhosttyTerminal
    private let renderState: GhosttyRenderState
    private let rowIterator: GhosttyRenderStateRowIterator
    private let rowCells: GhosttyRenderStateRowCells
    private let imageIterator: GhosttyRenderStateImageIterator
    /// 上次读图片列表时的版本（`GHOSTTY_RENDER_STATE_DATA_IMAGE_PLACEMENTS_GENERATION`）。
    private var imageList: UInt64?
    private let keyEncoder: GhosttyKeyEncoder
    private let keyEvent: GhosttyKeyEvent
    private let mouseEncoder: GhosttyMouseEncoder
    private let mouseEvent: GhosttyMouseEvent
    /// 读字素簇的缓冲，够大多数字素簇用，不够时临时分配。
    private let graphemeBuffer: UnsafeMutablePointer<UInt8>
    private static let graphemeCapacity = 64
    /// `readCell` 用 `ghostty_cell_get_multi` 一次取齐的格子属性：`cellFieldPointers` 按 `cellKeys` 的
    /// 顺序指着 `cellFields` 的各个字段，建一次一直用，读格子时不分配。
    private let cellFields: UnsafeMutablePointer<CellFields>
    private let cellFieldPointers: UnsafeMutablePointer<UnsafeMutableRawPointer?>
    private static let cellKeys = [
        GHOSTTY_CELL_DATA_CONTENT_TAG, GHOSTTY_CELL_DATA_WIDE, GHOSTTY_CELL_DATA_HAS_TEXT, GHOSTTY_CELL_DATA_HAS_STYLING,
    ]
    private struct CellFields {
        var tag = GHOSTTY_CELL_CONTENT_CODEPOINT
        var wide = GHOSTTY_CELL_WIDE_NARROW
        var hasText = false
        var styled = false
    }
    private let hold = RenderHold()
    /// 最近一次改尺寸、最近一次喂字节的时刻，见 `isRenderHeld`。
    private var resizedAt = ContinuousClock.now - .seconds(10)
    private var fedAt = ContinuousClock.now - .seconds(10)
    /// 平滑滚动时视口之外再往回看的零点几行：画的时候整屏往下错开这么多，露出视口上面那一行的
    /// 一部分。和桌面 `Session` 的 `scroll_offset` 一样，取值在 [0, 1)，视口在历史最顶上时为 0。
    private var scrollFraction: Double = 0

    public private(set) var size: GridSize
    public private(set) var settings: TermSettings

    /// 按宿主给的尺寸和主题新建一份 VT。
    public init(size: GridSize, settings: TermSettings) throws(VTerminalError) {
        Self.installPngDecoder
        var terminal: GhosttyTerminal?
        guard ghostty_terminal_new(nil, &terminal, max(size.cols, 1), max(size.rows, 1)) == GHOSTTY_SUCCESS,
            let terminal
        else { throw .creationFailed }
        var state: GhosttyRenderState?
        var iterator: GhosttyRenderStateRowIterator?
        var cells: GhosttyRenderStateRowCells?
        var images: GhosttyRenderStateImageIterator?
        var encoder: GhosttyKeyEncoder?
        var event: GhosttyKeyEvent?
        var mouseEncoder: GhosttyMouseEncoder?
        var mouseEvent: GhosttyMouseEvent?
        guard ghostty_render_state_new(nil, &state) == GHOSTTY_SUCCESS, let state,
            ghostty_render_state_row_iterator_new(nil, &iterator) == GHOSTTY_SUCCESS, let iterator,
            ghostty_render_state_row_cells_new(nil, &cells) == GHOSTTY_SUCCESS, let cells,
            ghostty_render_state_image_iterator_new(nil, &images) == GHOSTTY_SUCCESS, let images,
            ghostty_key_encoder_new(nil, &encoder) == GHOSTTY_SUCCESS, let encoder,
            ghostty_key_event_new(nil, &event) == GHOSTTY_SUCCESS, let event,
            ghostty_mouse_encoder_new(nil, &mouseEncoder) == GHOSTTY_SUCCESS, let mouseEncoder,
            ghostty_mouse_event_new(nil, &mouseEvent) == GHOSTTY_SUCCESS, let mouseEvent
        else {
            ghostty_terminal_free(terminal)
            throw .creationFailed
        }
        handle = terminal
        renderState = state
        rowIterator = iterator
        rowCells = cells
        imageIterator = images
        keyEncoder = encoder
        keyEvent = event
        self.mouseEncoder = mouseEncoder
        self.mouseEvent = mouseEvent
        graphemeBuffer = .allocate(capacity: Self.graphemeCapacity)
        cellFields = .allocate(capacity: 1)
        cellFields.initialize(to: CellFields())
        cellFieldPointers = .allocate(capacity: Self.cellKeys.count)
        let fieldPointers: [UnsafeMutableRawPointer?] = [
            UnsafeMutableRawPointer(cellFields.pointer(to: \.tag)!),
            UnsafeMutableRawPointer(cellFields.pointer(to: \.wide)!),
            UnsafeMutableRawPointer(cellFields.pointer(to: \.hasText)!),
            UnsafeMutableRawPointer(cellFields.pointer(to: \.styled)!),
        ]
        cellFieldPointers.initialize(from: fieldPointers, count: fieldPointers.count)
        self.size = size
        self.settings = settings
        configureCommon(scrollbackBytes: settings.scrollbackLimit)
        ghostty_terminal_resize(
            handle, max(size.cols, 1), max(size.rows, 1), UInt32(size.cellWidthPx), UInt32(size.cellHeightPx))
        installRenderHold()
        // 多取视口上面一行：平滑滚动时画面往下错开，顶上要露出它的一部分。
        var overscan = GhosttyRenderStateOverscan(above: 1, below: 0)
        ghostty_render_state_set(renderState, GHOSTTY_RENDER_STATE_OPTION_OVERSCAN, &overscan)
        applyTheme(settings)
    }

    deinit {
        ghostty_mouse_event_free(mouseEvent)
        ghostty_mouse_encoder_free(mouseEncoder)
        ghostty_key_event_free(keyEvent)
        ghostty_key_encoder_free(keyEncoder)
        ghostty_render_state_row_cells_free(rowCells)
        ghostty_render_state_image_iterator_free(imageIterator)
        ghostty_render_state_row_iterator_free(rowIterator)
        ghostty_render_state_free(renderState)
        ghostty_terminal_free(handle)
        graphemeBuffer.deallocate()
        cellFields.deallocate()
        cellFieldPointers.deallocate()
    }

    // MARK: 喂字节、改尺寸、换主题

    /// 把宿主转来的字节（VT 重放或 PTY 输出）喂给 VT。
    public func feed(_ bytes: some Collection<UInt8>) {
        guard !bytes.isEmpty else { return }
        fedAt = .now
        if let done = bytes.withContiguousStorageIfAvailable({ buffer in
            ghostty_terminal_vt_write(handle, buffer.baseAddress, buffer.count)
        }) {
            return done
        }
        let copy = Array(bytes)
        copy.withUnsafeBufferPointer { ghostty_terminal_vt_write(handle, $0.baseAddress, $0.count) }
    }

    /// 在宿主改尺寸的位置（`HostMsg.resized`）改这份 VT 的尺寸。
    public func resize(_ newSize: GridSize) {
        guard newSize.cols > 0, newSize.rows > 0, newSize != size else { return }
        size = newSize
        resizedAt = .now
        scrollFraction = 0
        ghostty_terminal_resize(
            handle, newSize.cols, newSize.rows, UInt32(newSize.cellWidthPx), UInt32(newSize.cellHeightPx))
    }

    /// 在宿主套用主题的位置（`HostMsg.themeApplied`）套用同一份主题，步骤和宿主的 `vt::apply_theme`
    /// 一致，两份 VT 才不会分叉。
    public func applyTheme(_ newSettings: TermSettings) {
        settings = newSettings
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, nil)
        var palette = [GhosttyColorRgb](repeating: GhosttyColorRgb(), count: 256)
        let readPalette = palette.withUnsafeMutableBytes { buffer in
            ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT, buffer.baseAddress)
        }
        guard readPalette == GHOSTTY_SUCCESS else { return }
        for entry in newSettings.palette {
            palette[Int(entry.index)] = Self.ghosttyColor(entry.color)
        }
        var background = Self.ghosttyColor(newSettings.background)
        var foreground = Self.ghosttyColor(newSettings.foreground)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &background)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &foreground)
        // 跟随单元格的光标色画的时候按光标所在格子解析，VT 里不设默认值。
        if case .rgb(let color) = newSettings.cursorColor {
            var cursor = Self.ghosttyColor(color)
            ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &cursor)
        } else {
            ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, nil)
        }
        setDefaultCursor(newSettings)
        palette.withUnsafeBytes { buffer in
            _ = ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, buffer.baseAddress)
        }
        configureCommon(scrollbackBytes: newSettings.scrollbackLimit)
    }

    /// 宿主的 `vt::set_default_cursor`：程序没用 DECSCUSR 设过形状时，libghostty 改默认值会把当前的
    /// 闪烁一起换掉；先换形状，看闪烁是不是变回了旧默认值，不一样说明程序用模式 12 改过，换完默认
    /// 闪烁后还原它。
    private func setDefaultCursor(_ settings: TermSettings) {
        let blinking = mode(12, ansi: false)
        var style = Self.ghosttyCursorStyle(settings.cursorStyle)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE, &style)
        let oldDefault = mode(12, ansi: false)
        // 没配置时默认闪烁；libghostty 的空值是不闪烁，所以显式给 true。
        var blink = settings.cursorBlink ?? true
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK, &blink)
        if blinking != oldDefault {
            var config = GhosttyTerminalModeConfig(mode: ghostty_mode_new(12, false), value: blinking)
            ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_MODE, &config)
        }
    }

    /// 宿主 `vt::configure_common` 里的选项，回滚的字节上限另按 `scrollbackBytesCap` 封顶，图片的
    /// 按 `kittyImageStorageBytes`。
    private func configureCommon(scrollbackBytes: UInt64) {
        var lines = Self.scrollbackLines
        var bytes = Int(clamping: min(scrollbackBytes, Self.scrollbackBytesCap))
        var unknown = Self.unknownSequenceMaxBytes
        var imageBytes = Self.kittyImageStorageBytes
        var no = false
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &lines)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &bytes)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_UNKNOWN_MAX_BYTES, &unknown)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &imageBytes)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, &no)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, nil)
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, &no)
    }

    /// 程序用同步输出（mode 2026）冻结屏幕时记下来，画的一方在冻结期间不刷新，免得画出半帧。
    private func installRenderHold() {
        let callback: GhosttyTerminalRenderHoldFn = { _, userdata, held in
            guard let userdata else { return }
            Unmanaged<RenderHold>.fromOpaque(userdata).takeUnretainedValue().set(held)
        }
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_USERDATA, Unmanaged.passUnretained(hold).toOpaque())
        ghostty_terminal_set(handle, GHOSTTY_TERMINAL_OPT_RENDER_HOLD, unsafeBitCast(callback, to: UnsafeRawPointer.self))
    }

    /// 先别刷新屏幕：程序用同步输出冻结着屏幕（有时限，免得程序出错时画面卡死），或者刚改过尺寸、
    /// 程序还没重画完。后者是因为 shell 集成说了会自己重画提示符（OSC 133 的 `redraw=1`）时，libghostty
    /// 改尺寸先把提示符擦掉，等 shell 收到 SIGWINCH 画回来要经宿主转一趟；改尺寸后短时间内等到改完以后
    /// 的输出再画，不画出擦掉了还没画回来的那一帧。
    public var isRenderHeld: Bool {
        let now = ContinuousClock.now
        if let since = hold.since, now - since < .seconds(1) { return true }
        return now - resizedAt < .milliseconds(300) && (fedAt < resizedAt || now - fedAt < .milliseconds(50))
    }

    // MARK: 模式和状态

    /// 读一个终端模式；`ansi` 为假是 DEC 私有模式（`?` 开头的）。
    public func mode(_ value: UInt16, ansi: Bool) -> Bool {
        var config = GhosttyTerminalModeConfig(mode: ghostty_mode_new(value, ansi), value: false)
        guard ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_MODE, &config) == GHOSTTY_SUCCESS else {
            return false
        }
        return config.value
    }

    /// 程序开着括号粘贴（mode 2004）。
    public var bracketedPaste: Bool { mode(2004, ansi: false) }

    // MARK: 视口滚动

    /// 回滚历史的位置：总行数、视口顶在第几行、视口多高。
    public struct Scrollbar: Sendable, Hashable {
        public var total: Int
        public var offset: Int
        public var length: Int

        /// 视口在最底下（跟着新输出走）。
        public var atBottom: Bool { offset + length >= total }
    }

    /// 视口停在最底下，也没有平滑滚动错开的零点几行。
    public var viewportAtBottom: Bool { scrollbar.atBottom && scrollFraction == 0 }

    public var scrollbar: Scrollbar {
        var bar = GhosttyTerminalScrollbar()
        guard ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_SCROLLBAR, &bar) == GHOSTTY_SUCCESS else {
            return Scrollbar(total: Int(size.rows), offset: 0, length: Int(size.rows))
        }
        return Scrollbar(total: Int(bar.total), offset: Int(bar.offset), length: Int(bar.len))
    }

    /// 回到最底下，跟着新输出走。
    public func scrollToBottom() {
        scrollFraction = 0
        var behavior = GhosttyTerminalScrollViewport()
        behavior.tag = GHOSTTY_SCROLL_VIEWPORT_BOTTOM
        ghostty_terminal_scroll_viewport(handle, behavior)
    }

    /// 按像素滚动回滚历史：`lines` 为正往回看更早的内容，可以是零点几行。凑够整行的部分挪视口，
    /// 剩下的记成错开量（`ScreenFrame.scrollOffset`），做法同桌面的 `Session::scroll_smoothly`。
    /// 返回画面是否变了；到了历史顶上或者已经在最底下、挪不动时为假。
    @discardableResult
    public func scrollSmoothly(lines: Double) -> Bool {
        let before = (scrollbar.offset, scrollFraction)
        var offset = scrollFraction + lines
        let whole = offset.rounded(.down)
        if whole != 0 {
            var behavior = GhosttyTerminalScrollViewport()
            behavior.tag = GHOSTTY_SCROLL_VIEWPORT_DELTA
            behavior.value.delta = -Int(whole)
            ghostty_terminal_scroll_viewport(handle, behavior)
            // 到了历史顶上或者已经在底部时视口挪不动，按实际挪了多少扣。
            offset -= Double(before.0 - scrollbar.offset)
        }
        // 视口上面没有行时不能往下错开；在底部继续往下滚时也不会错开成负的。
        let top = scrollbar.offset
        scrollFraction = top == 0 ? 0 : min(max(offset, 0), 0.999)
        return (top, scrollFraction) != before
    }

    /// 滚动归程序管：开着鼠标上报时滚轮发给它（全屏的 agent 界面多半这样自己滚）；在备用屏上开着
    /// 备用滚动（模式 1007）时换成上下方向键。两样都不是时滚的是这份 VT 的回滚历史。
    public var programScrolls: Bool {
        mouseTracking || (alternateScreen && mode(1007, ansi: false))
    }

    /// 程序开着鼠标上报（任何一种）。
    public var mouseTracking: Bool {
        var tracking = false
        guard ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, &tracking) == GHOSTTY_SUCCESS else {
            return false
        }
        return tracking
    }

    private var alternateScreen: Bool {
        var screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY
        ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen)
        return screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE
    }

    /// 在第 `row` 行第 `column` 列滚 `lines` 行（负数往上看更早的内容）要发给程序的字节：开着鼠标上报
    /// 时每行一个滚轮事件（同桌面的 `Session::scroll`），否则按备用滚动每行一个方向键。程序不管滚动时为空。
    public func encodeWheel(lines: Int, column: Int, row: Int) -> [UInt8] {
        guard lines != 0 else { return [] }
        guard mouseTracking else {
            guard programScrolls else { return [] }
            let arrow = encode(KeyInput(key: lines < 0 ? .up : .down))
            return Array([[UInt8]](repeating: arrow, count: abs(lines)).joined())
        }
        let button = lines < 0 ? GHOSTTY_MOUSE_BUTTON_FOUR : GHOSTTY_MOUSE_BUTTON_FIVE
        var bytes: [UInt8] = []
        for _ in 0..<abs(lines) {
            guard encodeMouse(GHOSTTY_MOUSE_ACTION_PRESS, button: button, column: column, row: row, into: &bytes)
            else { break }
        }
        return bytes
    }

    /// 在第 `row` 行第 `column` 列点一下（左键按下再松开）要发给程序的字节；程序没开鼠标上报时为空。
    public func encodeClick(column: Int, row: Int) -> [UInt8] {
        guard mouseTracking else { return [] }
        var bytes: [UInt8] = []
        for action in [GHOSTTY_MOUSE_ACTION_PRESS, GHOSTTY_MOUSE_ACTION_RELEASE] {
            guard encodeMouse(action, button: GHOSTTY_MOUSE_BUTTON_LEFT, column: column, row: row, into: &bytes)
            else { return [] }
        }
        return bytes
    }

    /// 按程序要的鼠标上报格式编码一个事件，追加到 `bytes`；编码失败时返回假。
    private func encodeMouse(
        _ action: GhosttyMouseAction, button: GhosttyMouseButton, column: Int, row: Int, into bytes: inout [UInt8]
    ) -> Bool {
        let cellWidth = UInt32(max(size.cellWidthPx, 1))
        let cellHeight = UInt32(max(size.cellHeightPx, 1))
        var encoderSize = GhosttyMouseEncoderSize()
        encoderSize.size = MemoryLayout<GhosttyMouseEncoderSize>.size
        encoderSize.screen_width = UInt32(size.cols) * cellWidth
        encoderSize.screen_height = UInt32(size.rows) * cellHeight
        encoderSize.cell_width = cellWidth
        encoderSize.cell_height = cellHeight
        ghostty_mouse_encoder_setopt_from_terminal(mouseEncoder, handle)
        ghostty_mouse_encoder_setopt(mouseEncoder, GHOSTTY_MOUSE_ENCODER_OPT_SIZE, &encoderSize)
        var pressed = action != GHOSTTY_MOUSE_ACTION_RELEASE
        ghostty_mouse_encoder_setopt(mouseEncoder, GHOSTTY_MOUSE_ENCODER_OPT_ANY_BUTTON_PRESSED, &pressed)
        ghostty_mouse_event_set_action(mouseEvent, action)
        ghostty_mouse_event_set_button(mouseEvent, button)
        ghostty_mouse_event_set_mods(mouseEvent, 0)
        // 落在格子中间，像素坐标换算成格子时不会因为舍入落到隔壁。
        let clampedColumn = min(max(column, 0), Int(size.cols) - 1)
        let clampedRow = min(max(row, 0), Int(size.rows) - 1)
        ghostty_mouse_event_set_position(
            mouseEvent,
            GhosttyMousePosition(
                x: (Float(clampedColumn) + 0.5) * Float(cellWidth), y: (Float(clampedRow) + 0.5) * Float(cellHeight)))
        var output = [CChar](repeating: 0, count: 64)
        var written = 0
        let result = output.withUnsafeMutableBufferPointer { buffer in
            ghostty_mouse_encoder_encode(mouseEncoder, mouseEvent, buffer.baseAddress, buffer.count, &written)
        }
        guard result == GHOSTTY_SUCCESS else { return false }
        bytes.append(contentsOf: output.prefix(written).map { UInt8(bitPattern: $0) })
        return true
    }

    // MARK: 读屏幕

    /// 按 render state 把变了的行抄进 `frame`，返回改了哪些。没变的行不碰。
    public func refresh(_ frame: inout ScreenFrame) -> FrameChange {
        let nextImageFrame = advanceAnimations()
        guard ghostty_render_state_update(renderState, handle) == GHOSTTY_SUCCESS else { return .none }
        var dirty = GHOSTTY_RENDER_STATE_DIRTY_FALSE
        ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_DIRTY, &dirty)
        var cols: UInt16 = 0
        var rows: UInt16 = 0
        ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_COLS, &cols)
        ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_ROWS, &rows)

        var colors = GhosttyRenderStateColors()
        colors.size = MemoryLayout<GhosttyRenderStateColors>.size
        ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_COLORS, &colors)
        let background = Self.rgb(colors.background)
        let foreground = Self.rgb(colors.foreground)
        let cursorColor = colors.cursor_has_value ? Self.rgb(colors.cursor) : nil

        var full = dirty == GHOSTTY_RENDER_STATE_DIRTY_FULL || Int(cols) != frame.columns || Int(rows) != frame.rows
            || background != frame.background || foreground != frame.foreground || cursorColor != frame.cursorColor
        if Int(cols) != frame.columns || Int(rows) != frame.rows {
            frame.columns = Int(cols)
            frame.rows = Int(rows)
            frame.cells = Array(
                repeating: Array(repeating: ScreenCell.blank, count: Int(cols)), count: Int(rows))
            full = true
        }
        frame.background = background
        frame.foreground = foreground
        frame.cursorColor = cursorColor

        // 视口在历史最顶上时取不到上面那一行，也就不能往下错开。
        var captured = GhosttyRenderStateOverscan()
        ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_OVERSCAN, &captured)
        let aboveCount = captured.above > 0 ? frame.columns : 0
        var aboveChanged = frame.above.count != aboveCount
        if aboveChanged {
            frame.above = Array(repeating: .blank, count: aboveCount)
        }
        frame.scrollOffset = aboveCount == 0 ? 0 : scrollFraction

        var changedRows: [Int] = []
        if dirty != GHOSTTY_RENDER_STATE_DIRTY_FALSE || full || aboveChanged {
            var iterator: GhosttyRenderStateRowIterator? = rowIterator
            ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &iterator)
            while ghostty_render_state_row_iterator_next(rowIterator) {
                var y: Int32 = 0
                ghostty_render_state_row_get(rowIterator, GHOSTTY_RENDER_STATE_ROW_DATA_VIEWPORT_Y, &y)
                var rowDirty = false
                ghostty_render_state_row_get(rowIterator, GHOSTTY_RENDER_STATE_ROW_DATA_DIRTY, &rowDirty)
                if y == -1, aboveCount > 0 {
                    if full || rowDirty || aboveChanged {
                        readRow(into: &frame.above, columns: frame.columns)
                        aboveChanged = true
                    }
                } else if y >= 0, Int(y) < frame.rows, full || rowDirty {
                    readRow(into: &frame.cells[Int(y)], columns: frame.columns)
                    changedRows.append(Int(y))
                }
            }
        }

        let cursor = readCursor()
        let cursorChanged = cursor != frame.cursor
        frame.cursor = cursor
        var list: UInt64 = 0
        ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_IMAGE_PLACEMENTS_GENERATION, &list)
        // 新的一帧（`full`）里还没有图片，列表没变也要读。
        let imagesChanged = full || imageList != list
        if imagesChanged {
            imageList = list
            frame.images = readImages()
        }
        frame.nextImageFrame = nextImageFrame
        ghostty_render_state_clean(renderState)
        return FrameChange(
            full: full, rows: full ? [] : changedRows, cursorChanged: cursorChanged, aboveChanged: aboveChanged,
            imagesChanged: imagesChanged)
    }

    /// 让在播的图片动画走到该放的那一帧，返回下一帧离现在多久；没有在播的为空。
    private func advanceAnimations() -> Duration? {
        guard let graphics = kittyGraphics else { return nil }
        let now = UInt64((ContinuousClock.now - Self.animationEpoch) / .milliseconds(1))
        var next: UInt64 = 0
        guard ghostty_kitty_graphics_animation_tick(graphics, now, &next) == GHOSTTY_SUCCESS else { return nil }
        return .milliseconds(next.subtractingReportingOverflow(now).overflow ? 0 : next - now)
    }

    /// 动画用的时钟的零点：libghostty 要求同一份 VT 一直用同一个零点。
    private static let animationEpoch = ContinuousClock.now

    private var kittyGraphics: GhosttyKittyGraphics? {
        var graphics: GhosttyKittyGraphics?
        guard ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &graphics) == GHOSTTY_SUCCESS
        else { return nil }
        return graphics
    }

    /// render state 里的图片，像素尺寸按单元格的像素尺寸折成格数，排成画的先后。
    private func readImages() -> [ScreenImage] {
        var iterator: GhosttyRenderStateImageIterator? = imageIterator
        guard ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR, &iterator)
            == GHOSTTY_SUCCESS
        else { return [] }
        let cellWidth = Double(max(size.cellWidthPx, 1))
        let cellHeight = Double(max(size.cellHeightPx, 1))
        var images: [(z: Int32, image: ScreenImage)] = []
        while ghostty_render_state_image_iterator_next(imageIterator) {
            var place = ImagePlacement()
            let read = withUnsafeMutablePointer(to: &place) { place in
                var values: [UnsafeMutableRawPointer?] = [
                    UnsafeMutableRawPointer(place.pointer(to: \.protocol)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.id)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.generation)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.layer)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.z)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.column)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.row)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.offsetX)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.offsetY)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.width)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.height)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.sourceX)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.sourceY)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.sourceWidth)!),
                    UnsafeMutableRawPointer(place.pointer(to: \.sourceHeight)!),
                ]
                return ghostty_render_state_image_get_multi(
                    imageIterator, Self.imageKeys.count, Self.imageKeys, &values, nil)
            }
            // 别的协议的图片和这个版本还不认识的层都跳过。
            guard read == GHOSTTY_SUCCESS, place.protocol == GHOSTTY_IMAGE_PROTOCOL_KITTY else { continue }
            let layer: ImageLayer
            switch place.layer {
            case GHOSTTY_RENDER_STATE_IMAGE_LAYER_BELOW_BACKGROUND: layer = .belowBackground
            case GHOSTTY_RENDER_STATE_IMAGE_LAYER_BELOW_TEXT: layer = .belowText
            case GHOSTTY_RENDER_STATE_IMAGE_LAYER_ABOVE_TEXT: layer = .aboveText
            default: continue
            }
            let image = ScreenImage(
                id: place.id, generation: place.generation,
                x: Double(place.column) + Double(place.offsetX) / cellWidth,
                y: Double(place.row) + Double(place.offsetY) / cellHeight,
                width: Double(place.width) / cellWidth, height: Double(place.height) / cellHeight,
                sourceX: place.sourceX, sourceY: place.sourceY, sourceWidth: place.sourceWidth,
                sourceHeight: place.sourceHeight, layer: layer)
            images.append((place.z, image))
        }
        return images.sorted { ($0.image.layer, $0.z) < ($1.image.layer, $1.z) }.map(\.image)
    }

    /// `readImages` 用 `ghostty_render_state_image_get_multi` 一次取齐的字段，顺序同 `imageKeys`。
    private struct ImagePlacement {
        var `protocol` = GHOSTTY_IMAGE_PROTOCOL_KITTY
        var id: UInt32 = 0
        var generation: UInt64 = 0
        var layer = GHOSTTY_RENDER_STATE_IMAGE_LAYER_ABOVE_TEXT
        var z: Int32 = 0
        var column: Int32 = 0
        var row: Int32 = 0
        var offsetX: UInt32 = 0
        var offsetY: UInt32 = 0
        var width: UInt32 = 0
        var height: UInt32 = 0
        var sourceX: UInt32 = 0
        var sourceY: UInt32 = 0
        var sourceWidth: UInt32 = 0
        var sourceHeight: UInt32 = 0
    }

    private static let imageKeys = [
        GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL, GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION, GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_Z, GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW, GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y, GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT, GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y, GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH,
        GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT,
    ]

    /// 帧里一块图片（`ScreenImage.id`、`ScreenImage.generation`）现在的像素。图片已经换了版本或删了
    /// 时为空，下一帧会带上新的。
    public func image(id: UInt32, generation: UInt64) -> CGImage? {
        guard let graphics = kittyGraphics, let image = ghostty_kitty_graphics_image(graphics, id) else { return nil }
        var current: UInt64 = 0
        var width: UInt32 = 0
        var height: UInt32 = 0
        var format = GHOSTTY_KITTY_IMAGE_FORMAT_RGBA
        var data: UnsafePointer<UInt8>?
        var length = 0
        guard ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_GENERATION, &current) == GHOSTTY_SUCCESS,
            current == generation,
            ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_WIDTH, &width) == GHOSTTY_SUCCESS,
            ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_HEIGHT, &height) == GHOSTTY_SUCCESS,
            ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_FORMAT, &format) == GHOSTTY_SUCCESS,
            ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR, &data) == GHOSTTY_SUCCESS,
            ghostty_kitty_graphics_image_get(image, GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN, &length) == GHOSTTY_SUCCESS,
            let data
        else { return nil }
        let pixels = UnsafeBufferPointer(start: data, count: length)
        let rgba: [UInt8]
        switch format {
        case GHOSTTY_KITTY_IMAGE_FORMAT_RGBA: rgba = Array(pixels)
        case GHOSTTY_KITTY_IMAGE_FORMAT_RGB:
            rgba = stride(from: 0, to: length - 2, by: 3).flatMap { [pixels[$0], pixels[$0 + 1], pixels[$0 + 2], 255] }
        case GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA:
            rgba = stride(from: 0, to: length - 1, by: 2).flatMap { [pixels[$0], pixels[$0], pixels[$0], pixels[$0 + 1]] }
        case GHOSTTY_KITTY_IMAGE_FORMAT_GRAY: rgba = pixels.flatMap { [$0, $0, $0, 255] }
        default: return nil
        }
        guard rgba.count == Int(width) * Int(height) * 4,
            let provider = CGDataProvider(data: Data(rgba) as CFData)
        else { return nil }
        return CGImage(
            width: Int(width), height: Int(height), bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: Int(width) * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
            provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }

    /// 回滚历史现在的字节上限。测试用。
    var scrollbackMaxBytes: Int {
        var bytes = 0
        ghostty_terminal_get(handle, GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_BYTES, &bytes)
        return bytes
    }

    /// 当前一屏的文字，一行一个，行尾空白去掉。测试和调试用。
    public func screenLines() -> [String] {
        var frame = ScreenFrame()
        _ = refresh(&frame)
        return frame.lines
    }

    private func readRow(into row: inout [ScreenCell], columns: Int) {
        var cells: GhosttyRenderStateRowCells? = rowCells
        guard ghostty_render_state_row_get(rowIterator, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &cells) == GHOSTTY_SUCCESS
        else { return }
        var x = 0
        while ghostty_render_state_row_cells_next(rowCells), x < columns {
            row[x] = readCell()
            x += 1
        }
        while x < columns {
            row[x] = .blank
            x += 1
        }
    }

    /// 一屏大多是空白格和不带样式的字，所以先从原始格子一次取齐有没有字、样式、单设的背景色，
    /// 没有的就不再一项项去问颜色和样式。
    private func readCell() -> ScreenCell {
        var raw: GhosttyCell = 0
        guard ghostty_render_state_row_cells_get(rowCells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, &raw) == GHOSTTY_SUCCESS,
            ghostty_cell_get_multi(raw, Self.cellKeys.count, Self.cellKeys, cellFieldPointers, nil) == GHOSTTY_SUCCESS
        else { return .blank }
        let fields = cellFields.pointee
        var cell = ScreenCell.blank
        switch fields.wide {
        case GHOSTTY_CELL_WIDE_WIDE: cell.width = .wide
        case GHOSTTY_CELL_WIDE_SPACER_TAIL: cell.width = .spacerTail
        case GHOSTTY_CELL_WIDE_SPACER_HEAD: cell.width = .spacerHead
        default: cell.width = .narrow
        }
        if fields.hasText {
            cell.text = readGrapheme()
        }
        // 前景色只来自样式，背景色来自样式或只有背景色的格子（`CONTENT_TAG` 是 `BG_COLOR_*`）。
        let onlyText = fields.tag == GHOSTTY_CELL_CONTENT_CODEPOINT || fields.tag == GHOSTTY_CELL_CONTENT_CODEPOINT_GRAPHEME
        guard fields.styled || !onlyText else { return cell }
        var color = GhosttyColorRgb()
        if ghostty_render_state_row_cells_get(rowCells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, &color)
            == GHOSTTY_SUCCESS
        {
            cell.foreground = Self.rgb(color)
        }
        if ghostty_render_state_row_cells_get(rowCells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, &color)
            == GHOSTTY_SUCCESS
        {
            cell.background = Self.rgb(color)
        }
        if fields.styled {
            var style = GhosttyStyle()
            style.size = MemoryLayout<GhosttyStyle>.size
            if ghostty_render_state_row_cells_get(rowCells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style)
                == GHOSTTY_SUCCESS
            {
                var attributes: CellAttributes = []
                if style.bold { attributes.insert(.bold) }
                if style.italic { attributes.insert(.italic) }
                if style.faint { attributes.insert(.faint) }
                if style.inverse { attributes.insert(.inverse) }
                if style.invisible { attributes.insert(.invisible) }
                if style.strikethrough { attributes.insert(.strikethrough) }
                if style.blink { attributes.insert(.blink) }
                cell.attributes = attributes
                cell.underline = UnderlineStyle(rawValue: UInt8(clamping: style.underline)) ?? .single
            }
        }
        return cell
    }

    private func readGrapheme() -> String {
        var buffer = GhosttyBuffer(ptr: graphemeBuffer, cap: Self.graphemeCapacity, len: 0)
        let result = ghostty_render_state_row_cells_get(
            rowCells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, &buffer)
        if result == GHOSTTY_SUCCESS {
            guard buffer.len > 0 else { return "" }
            return String(decoding: UnsafeBufferPointer(start: graphemeBuffer, count: buffer.len), as: UTF8.self)
        }
        guard result == GHOSTTY_OUT_OF_SPACE, buffer.len > 0 else { return "" }
        let needed = buffer.len
        let large = UnsafeMutablePointer<UInt8>.allocate(capacity: needed)
        defer { large.deallocate() }
        var retry = GhosttyBuffer(ptr: large, cap: needed, len: 0)
        guard ghostty_render_state_row_cells_get(
                rowCells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, &retry) == GHOSTTY_SUCCESS
        else { return "" }
        return String(decoding: UnsafeBufferPointer(start: large, count: retry.len), as: UTF8.self)
    }

    private func readCursor() -> ScreenCursor? {
        var cursor = GhosttyRenderStateCursor()
        cursor.size = MemoryLayout<GhosttyRenderStateCursor>.size
        guard ghostty_render_state_get(renderState, GHOSTTY_RENDER_STATE_DATA_CURSOR, &cursor) == GHOSTTY_SUCCESS,
            cursor.visible, cursor.viewport_has_value
        else { return nil }
        let shape: CursorShape =
            switch cursor.visual_style {
            case GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BAR: .bar
            case GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_UNDERLINE: .underline
            case GHOSTTY_RENDER_STATE_CURSOR_VISUAL_STYLE_BLOCK_HOLLOW: .blockHollow
            default: .block
            }
        return ScreenCursor(
            column: Int(cursor.viewport_x), row: Int(cursor.viewport_y), shape: shape, blinking: cursor.blinking,
            onWideTail: cursor.wide_tail)
    }

    // MARK: 输入编码

    /// 按这份 VT 当前的模式（应用光标键、Kitty 键盘协议、modifyOtherKeys 等）编码一次按键；
    /// 没有产生字节时返回空。
    public func encode(_ input: KeyInput) -> [UInt8] {
        let optionAsAlt: Bool =
            switch settings.optionAsAlt {
            case .yes: true
            case .left: !input.rightAlt
            case .right: input.rightAlt
            case .no, .auto: false
            }
        var consumed = input.consumedModifiers
        // Option 当作 Alt 时不能算「已消耗」，编码器才会改用未加修饰的字符并加 ESC 前缀。
        if optionAsAlt { consumed.remove(.alt) }
        ghostty_key_encoder_setopt_from_terminal(keyEncoder, handle)
        var option: GhosttyOptionAsAlt =
            switch settings.optionAsAlt {
            case .yes: GHOSTTY_OPTION_AS_ALT_TRUE
            case .left: GHOSTTY_OPTION_AS_ALT_LEFT
            case .right: GHOSTTY_OPTION_AS_ALT_RIGHT
            case .no, .auto: GHOSTTY_OPTION_AS_ALT_FALSE
            }
        ghostty_key_encoder_setopt(keyEncoder, GHOSTTY_KEY_ENCODER_OPT_MACOS_OPTION_AS_ALT, &option)
        ghostty_key_event_set_action(keyEvent, GHOSTTY_KEY_ACTION_PRESS)
        ghostty_key_event_set_key(keyEvent, input.key.ghosttyKey)
        ghostty_key_event_set_mods(keyEvent, input.modifiers.ghosttyMods(rightAlt: input.rightAlt))
        ghostty_key_event_set_consumed_mods(keyEvent, consumed.ghosttyMods(rightAlt: input.rightAlt))
        ghostty_key_event_set_composing(keyEvent, false)
        ghostty_key_event_set_unshifted_codepoint(keyEvent, input.unshifted?.unicodeScalars.first?.value ?? 0)
        // 文字只在编码期间借给 libghostty；控制字符不传，由编码器按逻辑键算。
        let text = input.text.flatMap { text in
            text.unicodeScalars.contains { $0.value < 0x20 || $0.value == 0x7F } ? nil : text
        }
        var output = [UInt8](repeating: 0, count: 128)
        var written = 0
        func run(_ utf8: UnsafePointer<CChar>?, _ length: Int) -> GhosttyResult {
            ghostty_key_event_set_utf8(keyEvent, utf8, length)
            defer { ghostty_key_event_set_utf8(keyEvent, nil, 0) }
            return output.withUnsafeMutableBufferPointer { buffer in
                buffer.withMemoryRebound(to: CChar.self) { chars in
                    ghostty_key_encoder_encode(keyEncoder, keyEvent, chars.baseAddress, chars.count, &written)
                }
            }
        }
        func attempt() -> GhosttyResult {
            guard let text else { return run(nil, 0) }
            return text.withCString { run($0, text.utf8.count) }
        }
        var result = attempt()
        if result == GHOSTTY_OUT_OF_SPACE {
            output = [UInt8](repeating: 0, count: written)
            result = attempt()
        }
        guard result == GHOSTTY_SUCCESS else { return [] }
        return Array(output.prefix(written))
    }

    /// 粘贴的字节：去掉不安全的控制字符，程序开着括号粘贴时套上括号，否则换行换成回车。
    public func encodePaste(_ text: String) -> [UInt8] {
        var data = Array(text.utf8).map { CChar(bitPattern: $0) }
        guard !data.isEmpty else { return [] }
        let bracketed = bracketedPaste
        var written = 0
        var output = [CChar](repeating: 0, count: data.count + 16)
        var result = data.withUnsafeMutableBufferPointer { input in
            output.withUnsafeMutableBufferPointer { out in
                ghostty_paste_encode(input.baseAddress, input.count, bracketed, out.baseAddress, out.count, &written)
            }
        }
        if result == GHOSTTY_OUT_OF_SPACE {
            data = Array(text.utf8).map { CChar(bitPattern: $0) }
            output = [CChar](repeating: 0, count: written)
            result = data.withUnsafeMutableBufferPointer { input in
                output.withUnsafeMutableBufferPointer { out in
                    ghostty_paste_encode(
                        input.baseAddress, input.count, bracketed, out.baseAddress, out.count, &written)
                }
            }
        }
        guard result == GHOSTTY_SUCCESS else { return [] }
        return output.prefix(written).map { UInt8(bitPattern: $0) }
    }

    // MARK: 转换

    private static func ghosttyColor(_ color: Rgb) -> GhosttyColorRgb {
        GhosttyColorRgb(r: color.r, g: color.g, b: color.b)
    }

    private static func rgb(_ color: GhosttyColorRgb) -> Rgb {
        Rgb(color.r, color.g, color.b)
    }

    private static func ghosttyCursorStyle(_ style: CursorStyle) -> GhosttyTerminalCursorStyle {
        switch style {
        case .block: GHOSTTY_TERMINAL_CURSOR_STYLE_BLOCK
        case .blockHollow: GHOSTTY_TERMINAL_CURSOR_STYLE_BLOCK_HOLLOW
        case .bar: GHOSTTY_TERMINAL_CURSOR_STYLE_BAR
        case .underline: GHOSTTY_TERMINAL_CURSOR_STYLE_UNDERLINE
        }
    }
}

extension VTerminal {
    /// 让 libghostty 收 PNG 格式的 Kitty 图片：用 ImageIO 解码。解码器整个进程共用，第一份 VT 建好
    /// 之前装上。
    static let installPngDecoder: Void = {
        let decode: GhosttySysDecodePngFn = { _, allocator, data, length, out in
            guard let data, let out else { return false }
            return decodePng(UnsafeBufferPointer(start: data, count: length), allocator: allocator, into: out)
        }
        ghostty_sys_set(GHOSTTY_SYS_OPT_DECODE_PNG, unsafeBitCast(decode, to: UnsafeRawPointer.self))
    }()

    /// 把 PNG 解成 libghostty 要的不预乘的 8 位 RGBA，缓冲区用它给的分配器分配（它限着单张图片的大小）。
    private static func decodePng(
        _ png: UnsafeBufferPointer<UInt8>, allocator: UnsafePointer<GhosttyAllocator>?,
        into out: UnsafeMutablePointer<GhosttySysImage>
    ) -> Bool {
        guard let source = CGImageSourceCreateWithData(Data(buffer: png) as CFData, nil),
            let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
        else { return false }
        let (width, height) = (image.width, image.height)
        let (length, overflow) = (width * 4).multipliedReportingOverflow(by: height)
        guard width > 0, height > 0, !overflow, let pixels = ghostty_alloc(allocator, length) else { return false }
        // 位图上下文只画得出预乘的 alpha，画完再除回去。
        guard let context = CGContext(
            data: pixels, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else {
            ghostty_free(allocator, pixels, length)
            return false
        }
        context.clear(CGRect(x: 0, y: 0, width: width, height: height))
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        for pixel in stride(from: 0, to: length, by: 4) {
            let alpha = UInt32(pixels[pixel + 3])
            guard alpha > 0, alpha < 255 else { continue }
            for channel in pixel..<pixel + 3 {
                let straight: UInt32 = (UInt32(pixels[channel]) * 255 + alpha / 2) / alpha
                pixels[channel] = UInt8(min(straight, 255))
            }
        }
        out.pointee = GhosttySysImage(width: UInt32(width), height: UInt32(height), data: pixels, data_len: length)
        return true
    }
}

public enum VTerminalError: Error, Sendable {
    case creationFailed
}

/// 同步输出的冻结状态，libghostty 的回调里改，和 `VTerminal` 在同一个线程上。
private final class RenderHold {
    var since: ContinuousClock.Instant?

    func set(_ held: Bool) {
        since = held ? .now : nil
    }
}
