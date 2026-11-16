package dev.runode.mobile.terminal

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.res.Configuration
import android.graphics.Canvas
import android.graphics.PointF
import android.graphics.Rect
import android.graphics.RenderNode
import android.graphics.drawable.Drawable
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.text.SpannableStringBuilder
import android.util.DisplayMetrics
import android.util.TypedValue
import android.view.ActionMode
import android.view.Choreographer
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.Menu
import android.view.MenuItem
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewTreeObserver
import android.view.WindowInsets
import android.view.WindowInsetsAnimation
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.Magnifier
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.TermSettings
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt

/** 终端视图要上层做的事。都在主线程上调。 */
interface TerminalViewDelegate {
    /** 用户打了字、按了键、粘贴，或者在程序管鼠标时滚动、点按。 */
    fun didInput(view: TerminalView, input: TerminalInput)

    /** 按视图现在的大小，「适配本机屏幕」该要多大的网格。视图大小、字号变了就报一次。 */
    fun fitSizeDidChange(view: TerminalView, size: GridSize)

    /** 视口离开了最底下（在看回滚历史），或者回到了最底下。 */
    fun didScrollBack(view: TerminalView, scrolledBack: Boolean)

    /** 软键盘弹出或收起。 */
    fun keyboardVisible(view: TerminalView, visible: Boolean)

    /** 按键栏上的麦克风键按下、松开或者滑出去放开。 */
    fun dictation(view: TerminalView, press: DictationPress)

    /** 按了按键栏上的图片键：要用户挑几张图片或几段视频发给电脑。 */
    fun requestMedia(view: TerminalView)

    /** 粘贴时剪贴板里没有文字、只有图片：[uri] 是这张图片，要发给电脑。 */
    fun pasteImage(view: TerminalView, uri: Uri)
}

/**
 * 终端视图：自己在 Canvas 上画 VT 的一屏，接软键盘（含输入法的组字）、硬件键盘和按键栏的输入，单指拖动看回滚
 * 历史，捏合改字号，长按选中文字、拖拖柄改选区、拷贝。网格比视图矮时贴着底边放，上面空出来的是终端背景色；比视图宽
 * 或高时拖着平移，有输出或打字时视口跟着光标。
 *
 * 画法：每一行录成一个 [RenderNode]，VT 报了哪些行变了（[VTerminal.refresh] 的 [FrameChange]）就只重录那几行，
 * 其余行的显示列表留着；光标、组字中的文字每次画时现画，光标闪烁不用重录。平滑滚动错开不足一行时整个网格往下挪，
 * 顶上露出另录的视口上面那一行（[ScreenFrame.above]）。画在不支持 [RenderNode] 的软件 Canvas 上时逐行现画。
 *
 * 只能在主线程上用。
 */
class TerminalView(context: Context) : View(context), TerminalDisplay {
    var delegate: TerminalViewDelegate? = null

    /** 正在画的那份 VT，由视图模型给；视图只读它和滚它的视口。 */
    private var terminal: VTerminal? = null

    /** 当前这一屏，[VTerminal.refresh] 就地改它。 */
    internal var screen = ScreenFrame()
        private set

    private val symbolTypeface = SymbolFont.typeface(context)
    private val renderer = TerminalRenderer(TerminalFont(fontSizes(resources.displayMetrics, null).base, symbolTypeface))
    private val font: TerminalFont get() = renderer.font

    private var rowNodes: Array<RenderNode> = emptyArray()
    private var dirtyRows = BooleanArray(0)
    private val aboveNode = RenderNode("terminal-above")
    private var aboveDirty = true

    private val mainHandler = Handler(Looper.getMainLooper())
    private var refreshScheduled = false

    /** 下一次刷新最早在这时候（[SystemClock.uptimeMillis]），见 [scheduleRefresh]。 */
    private var nextRefreshAt = 0L

    /** 上次刷新以来 [onDraw] 一共花的时间，[scheduleRefresh] 按它拉开两次刷新。 */
    private var drawNanos = 0L
    private val refreshTask = Runnable { runScheduledRefresh() }
    private val imageFrameTask = Runnable { scheduleRefresh() }
    private val heldTask = Runnable { scheduleRefresh() }

    /** 用户捏合定的字号（像素）；为空时按默认字号。 */
    private var userFontSize: Float? = null

    /** 按手机屏幕决定尺寸（网格铺满视图）；为假时跟随电脑，字号缩到网格正好一屏宽，但不小于可读字号。 */
    private var fitsPhone = false
    private var fitFontCache: TerminalFont? = null
    private var lastFitSize: GridSize? = null
    private var lastFontScale = resources.configuration.fontScale
    private var lastScrolledBack = false

    /** 网格比视图宽时往左挪了多少；网格顶边在视图里的位置。都是像素。 */
    private var panX = 0f
    private var gridTop = 0f

    /** 用户最近一次自己拖动、缩放的时刻：之后一会儿不自动跟着光标走，免得和手指抢。 */
    private var lastUserScroll = SystemClock.uptimeMillis() - 10_000

    internal enum class KeyboardMotion { Opening, Closing }

    /**
     * 软键盘正在弹出或收起；停稳了为空。这时视图的高度一帧一变，等停稳了再报「适配手机」的尺寸，宿主只改一次。
     */
    private var keyboardMotion: KeyboardMotion? = null
    private val keyboardSettle = Runnable { keyboardDidMove() }

    private enum class Drag { None, Scrollback, Pan }

    private var drag = Drag.None
    private val wheel = WheelAccumulator()

    /** 拖动开始时手指所在的格子，程序自己管滚动时滚轮事件报在这里。 */
    private var scrollAnchorColumn = 0
    private var scrollAnchorRow = 0

    /** 这一次触摸里捏合已经改了字号，拖动不再算数，抬手后报尺寸。 */
    private var zooming = false
    private var pinchStartSize = 0f
    private var pinchScale = 1f
    private var tapX = 0f
    private var tapY = 0f

    /** 松手后接着滚的速度（像素每秒，往下为正）；为 0 时没在滚。 */
    private var momentumVelocity = 0f
    private var momentumLastFrame = 0L
    private val momentumFrame = Choreographer.FrameCallback { stepMomentum(it) }

    /**
     * 在选中文字：画拖柄、弹「拷贝 / 粘贴 / 全选」。选区本身只存在 VT 里，每次刷新后经 [syncSelection] 和它对账。
     */
    private var selecting = false

    /** 正在拖的那个拖柄（长按选中后手指不抬接着拖算拖终点柄）；没在拖时为空。 */
    private var selectionDrag: SelectionEnd? = null

    /**
     * 这次触摸归拖柄管，直到抬手：途中选区被清掉（[selectionDrag] 跟着置空）时剩下的事件也不交给手势识别，
     * 它们没收到这次的按下。
     */
    private var handleTouch = false

    /** 这次拖动上一次把在拖的那端装到的格子；每次开始拖时清空。 */
    private var lastDragCell: GridCell? = null

    /** 上次刷新时选区两端在视口里的格子，画拖柄、认拖柄用。 */
    private var selectionBounds: SelectionBounds? = null

    /**
     * 弹着的浮动菜单（选中时的，或长按没取到词时只有「粘贴」的）。自己收菜单时先把它置空再 finish，
     * [ActionMode.Callback.onDestroyActionMode] 据此分出用户关的（返回键）和自己收的。
     */
    private var selectionMode: ActionMode? = null

    /** 拖柄时选区那端的格边（行中线）相对手指的偏移：手指按在拖柄图上，选区那端跟着格边走、不跟着指尖。 */
    private var dragOffsetX = 0f
    private var dragOffsetY = 0f

    /** 为假时是长按刚选中，手指挪出触摸容差才开始拖，按着不动时的抖动不改选区。 */
    private var dragArmed = true
    private var pressX = 0f
    private var pressY = 0f

    /** 最近一次拖到的位置（加上偏移后的，视图坐标），自动滚动每帧按它扩选区。 */
    private var dragX = 0f
    private var dragY = 0f

    private var edgeScrolling = false
    private var edgeLastFrame = 0L
    private val edgeFrame = Choreographer.FrameCallback { stepEdgeScroll(it) }
    private var magnifier: Magnifier? = null
    private val touchSlop = ViewConfiguration.get(context).scaledTouchSlop.toFloat()

    /** 系统主题里的选区拖柄图，左柄画起点、右柄画终点；主题里没有时不画，只按 [HANDLE_FALLBACK_DP] 认触摸。 */
    private val startHandle: Drawable? by lazy { themeDrawable(android.R.attr.textSelectHandleLeft) }
    private val endHandle: Drawable? by lazy { themeDrawable(android.R.attr.textSelectHandleRight) }

    /** 拷贝选中的文字时怎么写剪贴板；测试换掉它，不碰系统剪贴板。 */
    internal var writeClipboard: (String) -> Unit = { text ->
        context.getSystemService(ClipboardManager::class.java)
            ?.setPrimaryClip(ClipData.newPlainText(context.getString(R.string.terminal_label), text))
    }

    private var blinkOn = true
    private val blinkTask = Runnable {
        blinkOn = !blinkOn
        invalidate()
        scheduleBlink()
    }

    /** 输入法看到的文档，只装着组字中的文字，见 [TerminalInputConnection]。 */
    internal val document = SpannableStringBuilder()

    /** 硬件键盘上按过、还没和下一个字合成的死键附加符号。 */
    private var pendingAccent = 0
    private var lastBell = 0L

    private val layoutListener = ViewTreeObserver.OnGlobalLayoutListener { syncKeyboardVisible() }

    /** 视图顶上被叠着的东西（断线横幅）挡住的高度（像素）。网格上方的空白不够时让出这段，不改网格的尺寸。 */
    var topObstruction: Float = 0f
        set(value) {
            if (abs(value - field) <= 0.5f) return
            field = value
            positionGrid(stickToBottom = true)
            invalidate()
        }

    /** 用户在设置里定的字号（dp，不跟系统的字体缩放）；为空时跟随系统字体大小。 */
    var fontSizeOverride: Float? = null
        set(value) {
            if (value == field) return
            field = value
            contentSizeDidChange()
        }

    /** 程序响铃时震一下。 */
    var bellHaptics = true

    /** 能打字：会话结束了时为假，点终端不弹键盘，开着的键盘收起来。 */
    var acceptsInput = true
        set(value) {
            if (value == field) return
            field = value
            isFocusable = value
            isFocusableInTouchMode = value
            if (!value) hideKeyboard()
        }

    /** 按键栏上粘住的 Ctrl：下一个打的字、按的键带上 Ctrl 发。 */
    var controlLatched by mutableStateOf(false)

    /** 正在录音：按键栏上的麦克风键显示成按下去的样子。 */
    var dictating by mutableStateOf(false)

    /** 软键盘开着，且终端有输入焦点。 */
    var keyboardVisible by mutableStateOf(false)
        private set

    private val scaleDetector = ScaleGestureDetector(context, ScaleListener()).apply { isQuickScaleEnabled = false }
    private val gestureDetector = GestureDetector(context, GestureListener())

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_YES
        contentDescription = context.getString(R.string.terminal_label)
        setWindowInsetsAnimationCallback(KeyboardAnimation())
    }

    // 画

    /** 换一份 VT（重新 `Attach` 后新建的），整屏重画。 */
    override fun terminalDidReset(terminal: VTerminal?, settings: TermSettings) {
        stopMomentum()
        leaveSelection()
        // 组字中的文字是给原来那份 VT 的：视图换了会话或者重新连上时不上屏。
        discardMarkedText()
        this.terminal = terminal
        renderer.imageSource = terminal?.let { vt -> { id, generation -> vt.image(id, generation) } }
        renderer.settings = settings
        renderer.clearImages()
        screen = ScreenFrame()
        markAllDirty()
        refreshNow()
    }

    override fun terminalSettingsDidChange(settings: TermSettings) {
        renderer.settings = settings
        markAllDirty()
        scheduleRefresh()
    }

    override fun terminalContentDidChange() {
        scheduleRefresh()
    }

    override fun terminalDidRingBell() {
        if (!bellHaptics) return
        // 连着响铃时最多每 200 毫秒震一下。
        val now = SystemClock.uptimeMillis()
        if (now - lastBell <= BELL_INTERVAL_MS) return
        lastBell = now
        performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
    }

    /**
     * VT 变了：合并一轮里的多块输出，稍后再从 render state 抄变了的行。刷屏时两次刷新至少隔一帧，抄和画得慢时按
     * 它们耗时的两倍拉开，让抄和画最多占一半主线程，空档有上限；离上次刷新已经够久时（打字回显）马上刷。
     */
    fun scheduleRefresh() {
        if (refreshScheduled) return
        refreshScheduled = true
        mainHandler.postDelayed(refreshTask, max(0L, nextRefreshAt - SystemClock.uptimeMillis()))
    }

    private fun runScheduledRefresh() {
        refreshScheduled = false
        val started = System.nanoTime()
        refreshNow()
        val cost = (System.nanoTime() - started + drawNanos) / 1_000_000
        drawNanos = 0
        nextRefreshAt = SystemClock.uptimeMillis() + (cost * 2).coerceIn(MIN_REFRESH_MS, MAX_REFRESH_MS)
    }

    private fun refreshNow() {
        val terminal = terminal ?: run {
            invalidate()
            return
        }
        // 程序在同步输出、改尺寸后还在重画：留着上一帧，一会儿再看（见 VTerminal.isRenderHeld）。
        if (terminal.isRenderHeld) {
            mainHandler.removeCallbacks(heldTask)
            mainHandler.postDelayed(heldTask, HELD_RETRY_MS)
            return
        }
        val oldColumns = screen.columns
        val oldRows = screen.rows
        val change = terminal.refresh(screen)
        if (oldColumns != screen.columns || oldRows != screen.rows) {
            ensureRowNodes()
            if (!updateFont()) positionGrid()
        }
        if (change.imagesChanged) renderer.pruneImages(screen.images)
        scheduleImageFrame(screen.nextImageFrame?.inWholeMilliseconds)
        if (change.full || change.imagesChanged) {
            dirtyRows.fill(true)
        } else {
            for (row in change.rows) if (row in dirtyRows.indices) dirtyRows[row] = true
        }
        if (change.full || change.aboveChanged || change.imagesChanged) aboveDirty = true
        if (change.cursorChanged) resetBlink()
        invalidate()
        val scrolledBack = !terminal.viewportAtBottom
        if (scrolledBack != lastScrolledBack) {
            lastScrolledBack = scrolledBack
            delegate?.didScrollBack(this, scrolledBack)
        }
        followCursor()
        syncSelection(terminal)
    }

    /** 有在播的图片动画时到点再刷新一次；动画停了就不再刷。 */
    private fun scheduleImageFrame(dueMs: Long?) {
        mainHandler.removeCallbacks(imageFrameTask)
        if (dueMs != null) mainHandler.postDelayed(imageFrameTask, max(dueMs, MIN_IMAGE_FRAME_MS))
    }

    /** 有输出时视口跟着光标走；用户正在拖（含拖拖柄）、刚拖过，或者在看回滚历史时不动。 */
    private fun followCursor() {
        if (lastScrolledBack || drag != Drag.None || selectionDrag != null || momentumVelocity != 0f || zooming) return
        if (SystemClock.uptimeMillis() - lastUserScroll <= FOLLOW_PAUSE_MS) return
        revealCursor()
    }

    private fun ensureRowNodes() {
        if (rowNodes.size == screen.rows) return
        rowNodes.forEach { it.discardDisplayList() }
        rowNodes = Array(screen.rows) { RenderNode("terminal-row") }
        dirtyRows = BooleanArray(screen.rows) { true }
        aboveDirty = true
    }

    private fun markAllDirty() {
        dirtyRows.fill(true)
        aboveDirty = true
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val started = System.nanoTime()
        val background = if (screen.rows > 0) screen.background else renderer.settings.background
        // 宿主视图（Compose 的 AndroidView）不一定把子视图裁在边界里，自己裁，背景不盖到别的界面上。
        canvas.clipRect(0, 0, width, height)
        canvas.drawColor(TerminalRenderer.argb(background))
        if (screen.rows == 0 || screen.columns == 0 || font.cellHeight <= 0f) return
        val nodes = canvas.isHardwareAccelerated
        if (nodes) record()
        val cellHeight = font.cellHeight
        val shift = scrollShift()
        canvas.save()
        canvas.translate(gridLeft(), gridTop)
        if (shift != 0f) {
            // 错开时裁掉挪出网格底边的部分，顶上露出视口上面那一行。
            canvas.clipRect(0f, 0f, gridWidth(), gridHeight())
            canvas.translate(0f, shift)
            if (screen.above.isNotEmpty()) {
                if (nodes) {
                    canvas.drawRenderNode(aboveNode)
                } else {
                    canvas.save()
                    canvas.translate(0f, -cellHeight)
                    renderer.drawRow(canvas, screen, screen.above, -1)
                    canvas.restore()
                }
            }
        }
        for (row in 0 until screen.rows) {
            if (nodes) {
                canvas.drawRenderNode(rowNodes[row])
            } else {
                canvas.save()
                canvas.translate(0f, row * cellHeight)
                renderer.drawRow(canvas, screen, screen.cells[row], row)
                canvas.restore()
            }
        }
        if (cursorShown()) renderer.drawCursor(canvas, screen, isFocused)
        val marked = markedText
        if (marked.isNotEmpty()) renderer.drawMarkedText(canvas, screen, marked)
        canvas.restore()
        if (selecting) drawSelectionHandles(canvas)
        drawNanos += System.nanoTime() - started
    }

    /** 重录脏了的行。 */
    private fun record() {
        ensureRowNodes()
        val width = ceil(gridWidth()).toInt()
        val height = font.cellHeight.toInt()
        for (row in 0 until screen.rows) {
            if (!dirtyRows[row]) continue
            val node = rowNodes[row]
            node.setPosition(0, row * height, width, (row + 1) * height)
            val canvas = node.beginRecording(width, height)
            try {
                renderer.drawRow(canvas, screen, screen.cells[row], row)
            } finally {
                node.endRecording()
            }
            dirtyRows[row] = false
        }
        if (aboveDirty && screen.above.isNotEmpty()) {
            aboveNode.setPosition(0, -height, width, 0)
            val canvas = aboveNode.beginRecording(width, height)
            try {
                renderer.drawRow(canvas, screen, screen.above, -1)
            } finally {
                aboveNode.endRecording()
            }
            aboveDirty = false
        }
    }

    // 光标闪烁

    private fun cursorShown(): Boolean = blinkOn || screen.cursor?.blinking != true || !isFocused

    /** 光标动了、打了字：先显示出来，再接着闪。 */
    private fun resetBlink() {
        blinkOn = true
        mainHandler.removeCallbacks(blinkTask)
        scheduleBlink()
    }

    private fun scheduleBlink() {
        mainHandler.removeCallbacks(blinkTask)
        if (isAttachedToWindow && isFocused && screen.cursor?.blinking == true) {
            mainHandler.postDelayed(blinkTask, BLINK_MS)
        } else {
            blinkOn = true
        }
    }

    // 布局、字号

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        updateFont()
        positionGrid(stickToBottom = true)
        reportFitSize()
        syncKeyboardVisible()
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        if (newConfig.fontScale != lastFontScale) {
            lastFontScale = newConfig.fontScale
            contentSizeDidChange()
        }
    }

    /** 系统字体大小或设置里的字号改了：丢掉捏合定的字号，重新排网格、算「适配手机」的尺寸。 */
    private fun contentSizeDidChange() {
        userFontSize = null
        fitFontCache = null
        updateFont()
        positionGrid()
        reportFitSize()
    }

    private fun fontSizes(): FontSizes = fontSizes(resources.displayMetrics, fontSizeOverride)

    /**
     * 现在该用的字号：捏合定过就用它；适配手机时用默认字号；跟随电脑时网格比屏幕宽就缩到正好一屏宽，但不小于可读
     * 字号，再宽的横着拖。
     */
    private fun currentFontSize(): Float {
        val sizes = fontSizes()
        userFontSize?.let { return it.coerceIn(sizes.readable, sizes.base * MAX_ZOOM) }
        if (fitsPhone || screen.columns == 0 || width == 0) return sizes.base
        val widthAtBase = screen.columns * font.cellWidth * sizes.base / font.size
        if (widthAtBase <= width) return sizes.base
        return max(sizes.readable, sizes.base * width / widthAtBase)
    }

    /** 按 [currentFontSize] 换字体；换了时整屏重录并重新摆网格，返回真。 */
    private fun updateFont(): Boolean {
        val size = (currentFontSize() * 2).roundToInt() / 2f
        if (abs(size - font.size) < 0.01f) return false
        renderer.font = TerminalFont(size, symbolTypeface)
        markAllDirty()
        positionGrid()
        return true
    }

    private fun gridWidth(): Float = screen.columns * font.cellWidth

    private fun gridHeight(): Float = screen.rows * font.cellHeight

    private fun gridLeft(): Float {
        val gridWidth = gridWidth()
        return if (gridWidth <= width) (width - gridWidth) / 2 else -panX
    }

    /** 平滑滚动的错开量，取整到像素：网格和上面那一行在半个像素上各自取整时，中间会漏出一条背景色的缝。 */
    private fun scrollShift(): Float = (screen.scrollOffset * font.cellHeight).toFloat().roundToInt().toFloat()

    /**
     * 网格放得下时网格顶边该在哪：比视图矮出整行时贴着底边，不足一行的零头留在底下。适配手机时多出的整行只是在等
     * 宿主改尺寸：上面有回滚历史时照样贴底边；没有历史、或键盘正在弹出时贴着顶边，免得网格先往下跳又跳回来。
     */
    private fun restingTop(): Float {
        val cellHeight = font.cellHeight
        val waitsForRows = fitsPhone && (screen.above.isEmpty() || keyboardMotion == KeyboardMotion.Opening)
        var slack = if (waitsForRows) 0f else height - gridHeight()
        if (cellHeight > 0f) slack = floor(slack / cellHeight) * cellHeight
        return maxOf(0f, slack, topObstruction)
    }

    /**
     * 摆网格。放不下时：[stickToBottom] 为真（视图大小变了）且整个网格往下挪出顶上让出的高度后光标还看得见，就这样
     * 挪；否则停在最底下。
     */
    private fun positionGrid(stickToBottom: Boolean = false) {
        val gridWidth = gridWidth()
        val gridHeight = gridHeight()
        panX = if (gridWidth <= width) 0f else panX.coerceIn(0f, gridWidth - width)
        val top = restingTop()
        gridTop = when {
            gridHeight + top <= height + 0.5f -> top
            stickToBottom -> {
                val cursorBottom = renderer.cursorRect(screen)?.bottom ?: Float.POSITIVE_INFINITY
                if (cursorBottom + top <= height) top else height - gridHeight
            }
            else -> gridTop.coerceIn(height - gridHeight, top)
        }
    }

    /** 拖着平移网格（网格比视图宽或高时）。 */
    private fun panBy(dx: Float, dy: Float) {
        val gridWidth = gridWidth()
        if (gridWidth > width) panX = (panX + dx).coerceIn(0f, gridWidth - width)
        val gridHeight = gridHeight()
        val top = restingTop()
        if (gridHeight + top > height) gridTop = (gridTop - dy).coerceIn(height - gridHeight, top)
        if (selecting) selectionMode?.invalidateContentRect()
        invalidate()
    }

    /** 「适配本机屏幕」要的网格：按适配手机时的字号，正好铺满视图。 */
    val fitSize: GridSize
        get() = gridSize(width.toFloat(), height.toFloat(), fitFont(), FIT_MARGIN_DP * resources.displayMetrics.density)

    /** 适配手机时用的字体：默认字号，跟随电脑时捏合缩放过的字号不算。 */
    private fun fitFont(): TerminalFont {
        val rounded = (fontSizes().base * 2).roundToInt() / 2f
        if (abs(font.size - rounded) < 0.01f) return font
        fitFontCache?.let { if (abs(it.size - rounded) < 0.01f) return it }
        return TerminalFont(rounded, symbolTypeface).also { fitFontCache = it }
    }

    private fun reportFitSize() {
        if (width <= 0 || height <= 0 || keyboardMotion != null || zooming) return
        val size = fitSize
        if (size == lastFitSize) return
        lastFitSize = size
        delegate?.fitSizeDidChange(this, size)
    }

    /** 尺寸方式换了：丢掉捏合定的字号，按新的方式重新定字号。 */
    override fun terminalSizeModeDidChange(fitsPhone: Boolean) {
        this.fitsPhone = fitsPhone
        userFontSize = null
        if (!updateFont()) positionGrid()
        reportFitSize()
        invalidate()
    }

    /** 把光标所在的位置挪进可见区域（键盘弹出、打字时）；网格放得下时不用挪。 */
    fun revealCursor() {
        val rect = renderer.cursorRect(screen) ?: return
        val margin = CURSOR_MARGIN_DP * resources.displayMetrics.density
        val gridWidth = gridWidth()
        if (gridWidth > width) {
            if (rect.left - margin < panX) panX = rect.left - margin
            if (rect.right + margin > panX + width) panX = rect.right + margin - width
            panX = panX.coerceIn(0f, gridWidth - width)
        }
        val gridHeight = gridHeight()
        val top = restingTop()
        if (gridHeight + top > height) {
            if (gridTop + rect.bottom + margin > height) gridTop = height - rect.bottom - margin
            if (gridTop + rect.top - margin < top) gridTop = top - rect.top + margin
            gridTop = gridTop.coerceIn(height - gridHeight, top)
        }
        invalidate()
    }

    // 键盘

    /** 键盘要动了：自己开关键盘时在请求之前就先记下，系统的动画回调可能晚到或者不到。 */
    internal fun keyboardWillMove(motion: KeyboardMotion) {
        keyboardMotion = motion
        mainHandler.removeCallbacks(keyboardSettle)
        // 等不到键盘停稳的回调时（没弹成之类）一秒后照常报。
        mainHandler.postDelayed(keyboardSettle, KEYBOARD_SETTLE_MS)
    }

    /** 键盘停稳了：按停稳后的大小摆网格、报尺寸。 */
    internal fun keyboardDidMove() {
        mainHandler.removeCallbacks(keyboardSettle)
        keyboardMotion = null
        positionGrid()
        reportFitSize()
        syncKeyboardVisible()
        invalidate()
    }

    private inner class KeyboardAnimation :
        WindowInsetsAnimation.Callback(WindowInsetsAnimation.Callback.DISPATCH_MODE_CONTINUE_ON_SUBTREE) {
        override fun onPrepare(animation: WindowInsetsAnimation) {
            if (animation.typeMask and WindowInsets.Type.ime() == 0) return
            keyboardWillMove(if (imeShown()) KeyboardMotion.Closing else KeyboardMotion.Opening)
        }

        override fun onProgress(insets: WindowInsets, running: MutableList<WindowInsetsAnimation>): WindowInsets = insets

        override fun onEnd(animation: WindowInsetsAnimation) {
            if (animation.typeMask and WindowInsets.Type.ime() != 0) keyboardDidMove()
        }
    }

    private fun imeShown(): Boolean = rootWindowInsets?.isVisible(WindowInsets.Type.ime()) == true

    private fun syncKeyboardVisible() {
        val visible = isFocused && imeShown()
        if (visible == keyboardVisible) return
        keyboardVisible = visible
        delegate?.keyboardVisible(this, visible)
    }

    /** 唤起软键盘，把光标露出来。 */
    fun showKeyboard() {
        if (!acceptsInput) return
        if (!keyboardVisible) keyboardWillMove(KeyboardMotion.Opening)
        if (!isFocused) requestFocus()
        val input = context.getSystemService(InputMethodManager::class.java)
        if (input?.showSoftInput(this, 0) != true) {
            // 刚拿到焦点时输入法可能还没接上这个视图，下一轮再请求一次。
            post { if (isFocused) input?.showSoftInput(this, 0) }
        }
        revealCursor()
    }

    /** 收起软键盘。焦点留着，接着收硬件键盘的按键。 */
    fun hideKeyboard() {
        if (keyboardVisible) keyboardWillMove(KeyboardMotion.Closing)
        context.getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(windowToken, 0)
    }

    override fun terminalShowKeyboard() {
        showKeyboard()
    }

    override fun onFocusChanged(gainFocus: Boolean, direction: Int, previouslyFocusedRect: Rect?) {
        super.onFocusChanged(gainFocus, direction, previouslyFocusedRect)
        resetBlink()
        syncKeyboardVisible()
        invalidate()
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        viewTreeObserver.addOnGlobalLayoutListener(layoutListener)
        resetBlink()
    }

    override fun onDetachedFromWindow() {
        viewTreeObserver.removeOnGlobalLayoutListener(layoutListener)
        stopMomentum()
        stopEdgeScroll()
        finishSelectionMode()
        magnifier?.dismiss()
        magnifier = null
        mainHandler.removeCallbacks(refreshTask)
        mainHandler.removeCallbacks(heldTask)
        mainHandler.removeCallbacks(imageFrameTask)
        mainHandler.removeCallbacks(blinkTask)
        mainHandler.removeCallbacks(keyboardSettle)
        refreshScheduled = false
        keyboardMotion = null
        super.onDetachedFromWindow()
    }

    // 手势

    // 轻点由 GestureDetector 认出，再经 performClick 处理。
    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
            // 手指一按下就停住惯性。
            stopMomentum()
            drag = Drag.None
            zooming = false
            pinchScale = 1f
            selectionDrag = null
            handleTouch = selecting && grabHandle(event.x, event.y)
        }
        // 拖拖柄（含长按选中后接着拖）的这次触摸不交给捏合和手势识别。
        if (handleTouch) {
            dragHandle(event)
            if (event.actionMasked == MotionEvent.ACTION_UP || event.actionMasked == MotionEvent.ACTION_CANCEL) {
                handleTouch = false
            }
            return true
        }
        // 适配手机时网格按视图排，不能捏合缩放。
        if (!fitsPhone) scaleDetector.onTouchEvent(event)
        gestureDetector.onTouchEvent(event)
        if (event.actionMasked == MotionEvent.ACTION_UP || event.actionMasked == MotionEvent.ACTION_CANCEL) {
            drag = Drag.None
            if (zooming) {
                zooming = false
                reportFitSize()
            }
        }
        return true
    }

    override fun performClick(): Boolean {
        super.performClick()
        handleTap(tapX, tapY)
        return true
    }

    private inner class GestureListener : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent): Boolean = true

        override fun onSingleTapUp(e: MotionEvent): Boolean {
            tapX = e.x
            tapY = e.y
            performClick()
            return true
        }

        override fun onLongPress(e: MotionEvent) {
            if (zooming || drag != Drag.None) return
            if (!beginSelection(e.x, e.y)) showSelectionMenu(pasteOnly = true, x = e.x, y = e.y)
        }

        override fun onScroll(e1: MotionEvent?, e2: MotionEvent, distanceX: Float, distanceY: Float): Boolean {
            if (zooming) return true
            if (drag == Drag.None) {
                val vertical = abs(distanceY) > abs(distanceX)
                // 一个手指上下拖：网格竖直方向放得下时才看回滚历史，放不下时拖着网格走；两个手指上下拖总是看回滚历史。
                val fits = gridHeight() <= height + 1
                drag = if (vertical && (fits || e2.pointerCount >= 2)) Drag.Scrollback else Drag.Pan
                parent?.requestDisallowInterceptTouchEvent(true)
                lastUserScroll = SystemClock.uptimeMillis()
                if (drag == Drag.Scrollback) {
                    wheel.reset()
                    val start = e1 ?: e2
                    scrollAnchorColumn = max(0, ((start.x - gridLeft()) / max(font.cellWidth, 1f)).toInt())
                    scrollAnchorRow = max(0, ((start.y - gridTop) / max(font.cellHeight, 1f)).toInt())
                }
            }
            when (drag) {
                Drag.Scrollback -> {
                    if (selecting) selectionMode?.hide(ViewConfiguration.getDefaultActionModeHideDuration())
                    scrollContent(-distanceY)
                }
                Drag.Pan -> {
                    lastUserScroll = SystemClock.uptimeMillis()
                    panBy(distanceX, distanceY)
                }
                Drag.None -> Unit
            }
            return true
        }

        override fun onFling(e1: MotionEvent?, e2: MotionEvent, velocityX: Float, velocityY: Float): Boolean {
            if (drag == Drag.Scrollback && !zooming) startMomentum(velocityY)
            return true
        }
    }

    /** 捏合改字号：两指的距离变了一成以上才算捏合，免得两指上下拖时字号跟着抖。 */
    private inner class ScaleListener : ScaleGestureDetector.SimpleOnScaleGestureListener() {
        override fun onScaleBegin(detector: ScaleGestureDetector): Boolean {
            pinchStartSize = font.size
            pinchScale = 1f
            return true
        }

        override fun onScale(detector: ScaleGestureDetector): Boolean {
            pinchScale *= detector.scaleFactor
            if (!zooming && drag != Drag.Scrollback && abs(pinchScale - 1f) > PINCH_THRESHOLD) {
                zooming = true
                stopMomentum()
                parent?.requestDisallowInterceptTouchEvent(true)
            }
            if (zooming) {
                lastUserScroll = SystemClock.uptimeMillis()
                setUserFontSize(pinchStartSize * pinchScale)
            }
            return true
        }
    }

    /** 捏合定字号：网格按新字号重新摆。 */
    internal fun setUserFontSize(size: Float) {
        userFontSize = size
        fitFontCache = null
        updateFont()
        invalidate()
    }

    /**
     * 手指（或惯性）往下挪了 [distance] 像素，往下拖看更早的内容：回滚历史跟着手指按像素滚；程序自己管滚动时（全屏的
     * agent 界面、开着备用滚动的分页器）只能按整行，攒够一行给它发一个滚轮。返回假表示回滚历史已经到头、挪不动了。
     */
    private fun scrollContent(distance: Float): Boolean {
        val terminal = terminal ?: return false
        val rowHeight = font.cellHeight
        if (rowHeight <= 0f) return false
        if (!terminal.programScrolls) {
            if (!terminal.scrollSmoothly((distance / rowHeight).toDouble())) return false
            refreshNow()
            return true
        }
        val rows = wheel.rows(distance, rowHeight)
        if (rows != 0) {
            delegate?.didInput(this, TerminalInput.Wheel(lines = -rows, column = scrollAnchorColumn, row = scrollAnchorRow))
        }
        return true
    }

    private fun startMomentum(velocity: Float) {
        stopMomentum()
        if (abs(velocity) <= Momentum.START_DP_PER_SECOND * resources.displayMetrics.density) return
        momentumVelocity = velocity
        momentumLastFrame = 0L
        Choreographer.getInstance().postFrameCallback(momentumFrame)
    }

    private fun stepMomentum(frameTimeNanos: Long) {
        if (momentumVelocity == 0f) return
        val elapsed = if (momentumLastFrame == 0L) {
            1f / 60
        } else {
            min((frameTimeNanos - momentumLastFrame) / 1e9f, Momentum.MAX_STEP_SECONDS)
        }
        momentumLastFrame = frameTimeNanos
        momentumVelocity = Momentum.decayed(momentumVelocity, elapsed)
        val stop = Momentum.STOP_DP_PER_SECOND * resources.displayMetrics.density
        if (abs(momentumVelocity) > stop && scrollContent(momentumVelocity * elapsed)) {
            Choreographer.getInstance().postFrameCallback(momentumFrame)
        } else {
            stopMomentum()
        }
    }

    private fun stopMomentum() {
        if (momentumVelocity != 0f) Choreographer.getInstance().removeFrameCallback(momentumFrame)
        momentumVelocity = 0f
    }

    /** 回到最底下，跟着新输出走。 */
    fun scrollToBottom() {
        stopMomentum()
        terminal?.scrollToBottom()
        refreshNow()
    }

    /**
     * 轻点：程序开着鼠标上报时当成一次点击发给它（全屏 agent 界面里「跳到底部」这类按钮）。只有点在光标那行的上一行
     * 及以下（输入框和它下面的状态栏）才弹键盘，点上面的对话、输出不弹，键盘开着时收起；看不到光标时，程序管鼠标就
     * 只点击，不管就照旧弹键盘。选中文字时只管选区：点在选区里重新弹菜单，点在外面退出选中，都不发点击、不开关键盘。
     */
    private fun handleTap(x: Float, y: Float) {
        if (selecting) {
            if (inSelection(x, y)) showSelectionMenu(pasteOnly = false) else endSelection()
            return
        }
        val gridX = x - gridLeft()
        val gridY = y - gridTop - scrollShift()
        val row = floor(gridY / font.cellHeight).toInt()
        val tracking = terminal?.mouseTracking == true
        if (tracking && gridX >= 0 && gridY >= 0 && gridX < gridWidth() && gridY < gridHeight()) {
            delegate?.didInput(this, TerminalInput.Click(column = (gridX / font.cellWidth).toInt(), row = row))
        }
        val atInput = screen.cursor?.let { row >= it.row - 1 } ?: !tracking
        if (atInput) {
            showKeyboard()
        } else if (keyboardVisible) {
            hideKeyboard()
        }
    }

    // 选区

    /**
     * 长按在 ([x], [y])：选中手指下的词、震一下，之后手指不抬接着拖等于拖终点柄。手指下是空白或不在网格里时返回假，
     * 原来的选区也退出。
     */
    internal fun beginSelection(x: Float, y: Float): Boolean {
        val terminal = terminal
        val cell = gridCellAt(x, y)
        if (terminal == null || cell == null || !terminal.selectWord(cell.column, cell.row)) {
            endSelection()
            return false
        }
        // 落在一串空格上时 VT 也会选中那串空格：当成没取到词。
        if (terminal.selectionText().isNullOrBlank()) {
            endSelection()
            return false
        }
        finishSelectionMode()
        selecting = true
        selectionDrag = SelectionEnd.End
        handleTouch = true
        lastDragCell = null
        dragArmed = false
        pressX = x
        pressY = y
        dragOffsetX = 0f
        dragOffsetY = 0f
        parent?.requestDisallowInterceptTouchEvent(true)
        performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        refreshNow()
        return true
    }

    /** 退出选中：收起拖柄和菜单，清掉 VT 里的选区。 */
    private fun endSelection() {
        leaveSelection()
        val terminal = terminal ?: return
        if (terminal.hasSelection) {
            terminal.clearSelection()
            scheduleRefresh()
        }
    }

    /** 只收起视图这边的选中状态，不碰 VT：换 VT 时原来那份可能已经关了。 */
    private fun leaveSelection() {
        if (selecting) invalidate()
        selecting = false
        selectionDrag = null
        selectionBounds = null
        stopEdgeScroll()
        magnifier?.dismiss()
        finishSelectionMode()
    }

    /** 刷新后对账：视图在选中而 VT 里的选区没了（切到备用屏之类）就退出选中；视图没在选中而 VT 里有选区就清掉。 */
    private fun syncSelection(terminal: VTerminal) {
        if (!selecting) {
            if (terminal.hasSelection) {
                terminal.clearSelection()
                scheduleRefresh()
            }
            return
        }
        val bounds = terminal.selectionBounds
        if (bounds == null) {
            leaveSelection()
            return
        }
        selectionBounds = bounds
        selectionMode?.invalidateContentRect()
    }

    /** 拷贝选中的文字，退出选中。 */
    internal fun copySelection() {
        val text = terminal?.selectionText()
        if (!text.isNullOrEmpty()) writeClipboard(text)
        endSelection()
    }

    /** 全选屏幕连同回滚历史，菜单留着。 */
    private fun selectAllText() {
        val terminal = terminal ?: return
        if (!terminal.selectAll()) return
        selecting = true
        refreshNow()
    }

    /** 视图里 ([x], [y]) 落在网格的哪一格；在网格外时为空。 */
    private fun gridCellAt(x: Float, y: Float): GridCell? {
        if (screen.columns == 0 || screen.rows == 0 || font.cellHeight <= 0f) return null
        val gridX = x - gridLeft()
        val gridY = y - gridTop - scrollShift()
        if (gridX < 0 || gridY < 0 || gridX >= gridWidth() || gridY >= gridHeight()) return null
        return GridCell(
            min((gridX / font.cellWidth).toInt(), screen.columns - 1),
            min((gridY / font.cellHeight).toInt(), screen.rows - 1),
        )
    }

    private fun inSelection(x: Float, y: Float): Boolean {
        val cell = gridCellAt(x, y) ?: return false
        return screen.selected.getOrNull(cell.row)?.contains(cell.column) == true
    }

    /** 拖柄贴着的格边，视图坐标；那一端不在视口里或没在选中时为空。 */
    private fun handleAnchor(end: SelectionEnd): HandleAnchor? {
        if (!selecting) return null
        val bounds = selectionBounds ?: return null
        val cell = (if (end == SelectionEnd.Start) bounds.start else bounds.end) ?: return null
        val width = screen.cells.getOrNull(cell.row)?.getOrNull(cell.column)?.width
        val anchor = SelectionGeometry.handleAnchor(end, cell, width, font.cellWidth, font.cellHeight)
        val top = gridTop + scrollShift()
        return HandleAnchor(anchor.x + gridLeft(), anchor.top + top, anchor.bottom + top)
    }

    private fun handleDrawable(end: SelectionEnd): Drawable? = if (end == SelectionEnd.Start) startHandle else endHandle

    private fun handleWidth(): Float =
        startHandle?.intrinsicWidth?.takeIf { it > 0 }?.toFloat() ?: (HANDLE_FALLBACK_DP * resources.displayMetrics.density)

    private fun handleHeight(): Float =
        startHandle?.intrinsicHeight?.takeIf { it > 0 }?.toFloat() ?: (HANDLE_FALLBACK_DP * resources.displayMetrics.density)

    /** 拖柄图的中心，视图坐标；那一端看不见时为空。 */
    internal fun handleCenter(end: SelectionEnd): PointF? {
        val anchor = handleAnchor(end) ?: return null
        val width = handleWidth()
        return PointF(SelectionGeometry.handleLeft(end, anchor.x, width) + width / 2, anchor.bottom + handleHeight() / 2)
    }

    private fun themeDrawable(attribute: Int): Drawable? {
        val attributes = context.obtainStyledAttributes(intArrayOf(attribute))
        return try {
            attributes.getDrawable(0)
        } finally {
            attributes.recycle()
        }
    }

    /** 拖柄挂在那一端所在行的底下，画在网格的裁剪之外。 */
    private fun drawSelectionHandles(canvas: Canvas) {
        for (end in SelectionEnd.entries) {
            val drawable = handleDrawable(end) ?: continue
            val anchor = handleAnchor(end) ?: continue
            val width = drawable.intrinsicWidth
            val height = drawable.intrinsicHeight
            if (width <= 0 || height <= 0) continue
            val left = SelectionGeometry.handleLeft(end, anchor.x, width.toFloat()).roundToInt()
            val top = anchor.bottom.roundToInt()
            drawable.setBounds(left, top, left + width, top + height)
            drawable.draw(canvas)
        }
    }

    /** 手指按下时按住了拖柄：之后这次触摸拖它。返回是否按住了。 */
    private fun grabHandle(x: Float, y: Float): Boolean {
        val start = handleAnchor(SelectionEnd.Start)
        val end = handleAnchor(SelectionEnd.End)
        val hit = SelectionGeometry.hitHandle(x, y, start, end, handleWidth(), handleHeight(), touchSlop) ?: return false
        val anchor = (if (hit == SelectionEnd.Start) start else end) ?: return false
        selectionDrag = hit
        lastDragCell = null
        dragArmed = true
        dragOffsetX = anchor.x - x
        dragOffsetY = (anchor.top + anchor.bottom) / 2 - y
        parent?.requestDisallowInterceptTouchEvent(true)
        finishSelectionMode()
        return true
    }

    /** 拖拖柄这次触摸里的一个事件：挪动时改选区，抬手时停下自动滚动、收起放大镜、弹菜单。 */
    private fun dragHandle(event: MotionEvent) {
        when (event.actionMasked) {
            MotionEvent.ACTION_MOVE -> {
                if (!dragArmed) {
                    if (hypot(event.x - pressX, event.y - pressY) < touchSlop) return
                    dragArmed = true
                }
                dragSelection(event.x + dragOffsetX, event.y + dragOffsetY)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                selectionDrag = null
                stopEdgeScroll()
                magnifier?.dismiss()
                if (selecting) showSelectionMenu(pasteOnly = false)
            }
        }
    }

    /** 把在拖的那端拖到视图里的 ([x], [y])（已加上偏移）；越出可见区域时开始自动滚动、平移。 */
    private fun dragSelection(x: Float, y: Float) {
        val terminal = terminal ?: return
        val moving = selectionDrag ?: return
        if (screen.columns == 0 || screen.rows == 0 || font.cellHeight <= 0f) return
        dragX = x
        dragY = y
        val cell = SelectionGeometry.handleCell(
            moving, x - gridLeft(), y - gridTop - scrollShift(), font.cellWidth, font.cellHeight, screen.columns,
            screen.rows,
        )
        // 选区一装回 VT 就整屏重读，格子没变时不重装。
        if (cell != lastDragCell) {
            lastDragCell = cell
            selectionDrag = terminal.extendSelection(moving, cell.column, cell.row)
            scheduleRefresh()
        }
        showMagnifier(x, cell.row)
        if (horizontalEdgeRate() != 0f || verticalEdgeRate() != 0f) startEdgeScroll()
    }

    private fun showMagnifier(x: Float, row: Int) {
        if (!isAttachedToWindow) return
        val magnifier = magnifier ?: Magnifier.Builder(this).build().also { magnifier = it }
        val y = gridTop + scrollShift() + (row + 0.5f) * font.cellHeight
        magnifier.show(x.coerceIn(0f, width.toFloat()), y.coerceIn(0f, height.toFloat()))
    }

    /** 网格在视图里看得见的上下沿。 */
    private fun visibleTop(): Float = max(gridTop, 0f)

    private fun visibleBottom(): Float = min(gridTop + gridHeight(), height.toFloat())

    /** 网格比视图宽时，拖到视图左右边上 [EDGE_DP] 以内每秒平移几列。 */
    private fun horizontalEdgeRate(): Float {
        if (gridWidth() <= width) return 0f
        val edge = EDGE_DP * resources.displayMetrics.density
        return SelectionGeometry.panRate(dragX, edge, width - edge, font.cellWidth)
    }

    /** 拖到网格可见区域上下边以外每秒挪几行，往上为正。 */
    private fun verticalEdgeRate(): Float =
        SelectionGeometry.autoscrollRate(dragY, visibleTop(), visibleBottom(), font.cellHeight)

    private fun startEdgeScroll() {
        if (edgeScrolling) return
        edgeScrolling = true
        edgeLastFrame = 0L
        Choreographer.getInstance().postFrameCallback(edgeFrame)
    }

    private fun stopEdgeScroll() {
        if (edgeScrolling) Choreographer.getInstance().removeFrameCallback(edgeFrame)
        edgeScrolling = false
    }

    /**
     * 拖柄停在边外时每帧走一步：网格比视图大、还能往那边挪时先平移网格，竖直方向挪到头了再滚回滚历史，然后把在拖的
     * 那端扩到手指那列、可见区域边上那行。手指回到区域里就停。
     */
    private fun stepEdgeScroll(frameTimeNanos: Long) {
        if (!edgeScrolling) return
        val terminal = terminal
        val moving = selectionDrag
        val across = horizontalEdgeRate()
        val up = verticalEdgeRate()
        if (terminal == null || moving == null || (across == 0f && up == 0f)) {
            stopEdgeScroll()
            return
        }
        val elapsed = if (edgeLastFrame == 0L) {
            1f / 60
        } else {
            min((frameTimeNanos - edgeLastFrame) / 1e9f, Momentum.MAX_STEP_SECONDS)
        }
        edgeLastFrame = frameTimeNanos
        val oldPanX = panX
        val oldTop = gridTop
        panBy(across * font.cellWidth * elapsed, -up * font.cellHeight * elapsed)
        var moved = panX != oldPanX || gridTop != oldTop
        // shortcut: 程序自己管滚动时（VTerminal.programScrolls）不自动滚，选区只能在一屏之内；要支持得给程序发滚轮、
        // 等它重画完再扩选区，等有人要跨屏选全屏程序里的内容时再做。
        if (up != 0f && gridTop == oldTop && !terminal.programScrolls) {
            moved = terminal.scrollSmoothly(up * elapsed.toDouble()) || moved
        }
        if (moved) {
            lastUserScroll = SystemClock.uptimeMillis()
            val top = visibleTop()
            val y = dragY.coerceIn(top, max(top, visibleBottom() - 1f))
            val cell = SelectionGeometry.handleCell(
                moving, dragX - gridLeft(), y - gridTop - scrollShift(), font.cellWidth, font.cellHeight,
                screen.columns, screen.rows,
            )
            lastDragCell = cell
            selectionDrag = terminal.extendSelection(moving, cell.column, cell.row)
            refreshNow()
        }
        Choreographer.getInstance().postFrameCallback(edgeFrame)
    }

    /** 收起菜单，不退出选中。 */
    private fun finishSelectionMode() {
        val mode = selectionMode ?: return
        selectionMode = null
        mode.finish()
    }

    /**
     * 弹浮动菜单。[pasteOnly] 时是长按没取到词，在手指处 ([x], [y]) 只弹「粘贴」，剪贴板里没字或不能打字时不弹；否则是
     * 选中时的「拷贝 / 粘贴 / 全选」，贴着选区看得见的部分，不能打字时没有「粘贴」。
     */
    private fun showSelectionMenu(pasteOnly: Boolean, x: Float = 0f, y: Float = 0f) {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        val canPaste = acceptsInput && clipboard?.hasPrimaryClip() == true
        if (pasteOnly && !canPaste) return
        finishSelectionMode()
        if (pasteOnly) performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        selectionMode = startActionMode(
            object : ActionMode.Callback2() {
                override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
                    if (!pasteOnly) menu.add(Menu.NONE, android.R.id.copy, 0, android.R.string.copy)
                    if (canPaste) menu.add(Menu.NONE, android.R.id.paste, 1, android.R.string.paste)
                    if (!pasteOnly) menu.add(Menu.NONE, android.R.id.selectAll, 2, android.R.string.selectAll)
                    return true
                }

                override fun onPrepareActionMode(mode: ActionMode, menu: Menu): Boolean = false

                override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
                    when (item.itemId) {
                        android.R.id.copy -> copySelection()
                        android.R.id.paste -> paste()
                        android.R.id.selectAll -> {
                            selectAllText()
                            return true
                        }
                    }
                    if (mode === selectionMode) finishSelectionMode()
                    return true
                }

                override fun onDestroyActionMode(mode: ActionMode) {
                    if (mode !== selectionMode) return
                    selectionMode = null
                    if (!pasteOnly) endSelection()
                }

                override fun onGetContentRect(mode: ActionMode, view: View, outRect: Rect) {
                    if (pasteOnly) {
                        outRect.set(x.toInt(), y.toInt(), x.toInt() + 1, y.toInt() + 1)
                    } else if (!selectionRect(outRect)) {
                        outRect.set(width / 2, height / 2, width / 2 + 1, height / 2 + 1)
                    } else {
                        // 拖柄挂在选区下面，算进去：上面放不下、菜单改弹在下面时不盖住拖柄。
                        outRect.bottom += ceil(handleHeight()).toInt()
                    }
                }
            },
            ActionMode.TYPE_FLOATING,
        )
    }

    /** 选区看得见的部分的外接矩形，视图坐标，写进 [out]；看不见选区时返回假。 */
    private fun selectionRect(out: Rect): Boolean {
        var first = -1
        var last = -1
        var left = Int.MAX_VALUE
        var right = -1
        for (row in 0 until screen.rows) {
            val range = screen.selected.getOrNull(row) ?: continue
            if (first < 0) first = row
            last = row
            left = min(left, range.first)
            right = max(right, range.last)
        }
        if (first < 0) return false
        val originX = gridLeft()
        val originY = gridTop + scrollShift()
        out.set(
            max(0, floor(originX + left * font.cellWidth).toInt()),
            max(0, floor(originY + first * font.cellHeight).toInt()),
            min(width, ceil(originX + (right + 1) * font.cellWidth).toInt()),
            min(height, ceil(originY + (last + 1) * font.cellHeight).toInt()),
        )
        return out.left < out.right && out.top < out.bottom
    }

    // 输入

    /** 发一份输入给上层，退出选中；打字时回到最底下、把光标露出来。 */
    private fun emit(input: TerminalInput) {
        endSelection()
        stopMomentum()
        if (lastScrolledBack) {
            terminal?.scrollToBottom()
            scheduleRefresh()
        }
        delegate?.didInput(this, input)
        resetBlink()
        revealCursor()
    }

    /** 打出一段文字：换行按回车键发，粘住 Ctrl 时第一个字按 Ctrl 组合键发。 */
    fun typeText(text: String) {
        val typed = ControlLatch.type(text, controlLatched)
        if (typed.latched != controlLatched) controlLatched = typed.latched
        typed.inputs.forEach(::emit)
    }

    /** 按键栏或硬件键盘按的一个键；粘住的 Ctrl 加在上面。 */
    fun press(key: TerminalKey, modifiers: KeyModifiers = KeyModifiers.none) {
        pressKey(KeyInput(key = key, modifiers = modifiers))
    }

    private fun pressKey(input: KeyInput) {
        val latched = ControlLatch.press(input, controlLatched)
        if (controlLatched) controlLatched = false
        emit(TerminalInput.Key(latched))
    }

    /** 粘贴剪贴板里的文字；没有文字、只有图片时把图片交给 [TerminalViewDelegate.pasteImage]。 */
    fun paste() {
        val clipboard = context.getSystemService(ClipboardManager::class.java) ?: return
        val clip = clipboard.primaryClip ?: return
        if (clip.itemCount == 0) return
        val item = clip.getItemAt(0)
        val uri = item.uri
        if (item.text.isNullOrEmpty() && uri != null && clip.description.hasMimeType("image/*")) {
            if (acceptsInput) delegate?.pasteImage(this, uri)
            return
        }
        paste(item.coerceToText(context).toString())
    }

    /** 把一段文字当作用户粘贴的发出去。 */
    fun paste(text: String) {
        if (text.isEmpty()) return
        emit(TerminalInput.Paste(text))
    }

    // 输入法

    override fun onCheckIsTextEditor(): Boolean = acceptsInput

    /** 键盘不做纠错、自动大写、联想和个性化学习，全屏模式也关掉：打进终端的是命令和密码，不是文章。 */
    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection? {
        if (!acceptsInput) return null
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE or EditorInfo.IME_FLAG_NO_ENTER_ACTION or
            EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
        outAttrs.initialCapsMode = 0
        outAttrs.initialSelStart = document.length
        outAttrs.initialSelEnd = document.length
        return TerminalInputConnection(this)
    }

    /** 输入法组字中的文字；没在组字时为空。 */
    internal val markedText: String
        get() {
            val start = BaseInputConnection.getComposingSpanStart(document)
            val end = BaseInputConnection.getComposingSpanEnd(document)
            if (start < 0 || end < 0) return ""
            return document.substring(min(start, end), max(start, end))
        }

    /** 把文档里组字以外的字（刚上屏的）整段发出去、从文档里拿掉，组字中的留着。 */
    internal fun flushDocument() {
        val start = BaseInputConnection.getComposingSpanStart(document)
        val end = BaseInputConnection.getComposingSpanEnd(document)
        val committed: String
        if (start < 0 || end < 0) {
            committed = document.toString()
            document.clear()
        } else {
            val from = min(start, end)
            val to = max(start, end)
            committed = document.substring(0, from) + document.substring(to)
            document.delete(to, document.length)
            document.delete(0, from)
        }
        markedTextDidChange()
        if (committed.isNotEmpty()) typeText(committed)
    }

    internal fun markedTextDidChange() {
        invalidate()
    }

    /** 丢掉组字中的文字，不发出去：它是给原来那个会话打的。重启输入，输入法才一起丢掉它那份组字。 */
    internal fun discardMarkedText() {
        if (document.isEmpty()) return
        BaseInputConnection.removeComposingSpans(document)
        document.clear()
        context.getSystemService(InputMethodManager::class.java)?.restartInput(this)
        invalidate()
    }

    // 硬件键盘

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        // 输入法正在组字时按键都归输入法。
        if (!acceptsInput || markedText.isNotEmpty()) return super.onKeyDown(keyCode, event)
        val meta = event.metaState
        if (HardwareKeys.isPaste(keyCode, meta)) {
            paste()
            return true
        }
        val input = HardwareKeys.keyInput(keyCode, meta) { event.getUnicodeChar(it) }
        if (input != null) {
            pressKey(input)
            return true
        }
        when (keyCode) {
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> {
                typeText("\r")
                return true
            }
            KeyEvent.KEYCODE_DEL -> {
                press(TerminalKey.Backspace)
                return true
            }
        }
        val unicode = event.getUnicodeChar(meta)
        if (unicode == 0) return super.onKeyDown(keyCode, event)
        if (unicode and KeyCharacterMap.COMBINING_ACCENT != 0) {
            pendingAccent = unicode and KeyCharacterMap.COMBINING_ACCENT_MASK
            return true
        }
        var character = unicode
        if (pendingAccent != 0) {
            val combined = KeyEvent.getDeadChar(pendingAccent, character)
            if (combined != 0) character = combined
            pendingAccent = 0
        }
        typeText(String(Character.toChars(character)))
        return true
    }

    // 无障碍

    /** 读屏时读出屏幕底部几行有字的内容。 */
    override fun onInitializeAccessibilityNodeInfo(info: AccessibilityNodeInfo) {
        super.onInitializeAccessibilityNodeInfo(info)
        val lines = screen.lines.filter { it.isNotEmpty() }
        info.text = lines.joinToString("\n")
        info.stateDescription = lines.takeLast(ACCESSIBLE_LINES).joinToString("\n")
        info.hintText = context.getString(R.string.terminal_hint)
        info.isEditable = acceptsInput
        info.isClickable = acceptsInput
        info.addAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_CLICK)
    }

    /** 读屏时轻点两下：直接打开键盘，不按位置判断。 */
    override fun performAccessibilityAction(action: Int, arguments: android.os.Bundle?): Boolean {
        if (action == AccessibilityNodeInfo.ACTION_CLICK) {
            showKeyboard()
            return true
        }
        return super.performAccessibilityAction(action, arguments)
    }

    /** 默认字号和可读的最小字号（像素）。 */
    internal data class FontSizes(val base: Float, val readable: Float)

    companion object {
        /** 适配手机时网格左右至少各空出的宽度（dp）：字不贴着屏幕的圆角边。 */
        const val FIT_MARGIN_DP = 4f

        /** 捏合最多放大到默认字号的几倍。 */
        private const val MAX_ZOOM = 4f

        /** 两指距离变了这么多（比例）才算捏合。 */
        private const val PINCH_THRESHOLD = 0.1f
        private const val CURSOR_MARGIN_DP = 8f
        private const val BLINK_MS = 600L
        private const val BELL_INTERVAL_MS = 200L
        private const val MIN_REFRESH_MS = 16L
        private const val MAX_REFRESH_MS = 250L
        private const val HELD_RETRY_MS = 50L
        private const val MIN_IMAGE_FRAME_MS = 8L
        private const val KEYBOARD_SETTLE_MS = 1_000L
        private const val FOLLOW_PAUSE_MS = 2_000L
        private const val ACCESSIBLE_LINES = 5

        /** 网格比视图宽时，拖柄拖到视图左右边上这么宽（dp）以内就开始平移网格：手指拖不出屏幕边。 */
        private const val EDGE_DP = 24f

        /** 主题里没有拖柄图时，按这么大（dp）的拖柄认触摸。 */
        private const val HANDLE_FALLBACK_DP = 22f

        /**
         * 默认字号按系统字体大小放大缩小，夹在 11 到 28 dp 之间；可读的最小字号同样按系统字体大小，夹在 9 到 20 dp
         * 之间。用户定了字号（[override]，dp）时默认字号用它，可读的最小字号不超过它。
         */
        internal fun fontSizes(metrics: DisplayMetrics, override: Float?): FontSizes {
            val dp = metrics.density
            val base = override?.let { it * dp }
                ?: TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 13f, metrics).coerceIn(11 * dp, 28 * dp)
            val readable = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 9f, metrics).coerceIn(9 * dp, 20 * dp)
            return FontSizes(base, min(readable, base))
        }

        /**
         * 用终端的默认字号（按 [context] 的系统字体大小，或者用户定的 [fontSize]，dp）铺满 [widthPx] × [heightPx]
         * 像素的网格；新开会话时按它定尺寸。
         */
        fun gridSize(context: Context, widthPx: Int, heightPx: Int, fontSize: Float? = null): GridSize {
            val metrics = context.resources.displayMetrics
            val font = TerminalFont(fontSizes(metrics, fontSize).base, null)
            return gridSize(widthPx.toFloat(), heightPx.toFloat(), font, FIT_MARGIN_DP * metrics.density)
        }

        internal fun gridSize(width: Float, height: Float, font: TerminalFont, margin: Float): GridSize {
            val columns = max(10, ((width - 2 * margin) / font.cellWidth).toInt())
            val rows = max(4, (height / font.cellHeight).toInt())
            return GridSize(
                cols = min(columns, 500).toUShort(),
                rows = min(rows, 300).toUShort(),
                cellWidthPx = font.cellWidth.roundToInt().toUShort(),
                cellHeightPx = font.cellHeight.roundToInt().toUShort(),
            )
        }
    }
}
