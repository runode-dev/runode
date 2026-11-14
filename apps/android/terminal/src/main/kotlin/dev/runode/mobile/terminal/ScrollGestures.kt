package dev.runode.mobile.terminal

import kotlin.math.pow

/**
 * 程序自己管滚动时（[VTerminal.programScrolls]）只能按整行滚：手指挪的像素攒着，攒够一行出一行，零头留到下一次。
 */
internal class WheelAccumulator {
    private var remainder = 0f

    fun reset() {
        remainder = 0f
    }

    /** 手指又往下挪了 [distance] 像素（往上为负），行高 [rowHeight]，返回这次凑够的整行数，往下为正。 */
    fun rows(distance: Float, rowHeight: Float): Int {
        if (rowHeight <= 0f) return 0
        remainder += distance
        val rows = (remainder / rowHeight).toInt()
        remainder -= rows * rowHeight
        return rows
    }
}

/** 松手后接着滚的惯性：每毫秒速度乘一次 [DECELERATION_PER_MS]。 */
internal object Momentum {
    /** iOS 滚动视图默认的减速率，手感和系统的列表接近。 */
    const val DECELERATION_PER_MS = 0.998

    /** 松手时快过这个速度（dp 每秒）才接着滚。 */
    const val START_DP_PER_SECOND = 200f

    /** 慢到这个速度（dp 每秒）以下就停。 */
    const val STOP_DP_PER_SECOND = 30f

    /** 一帧最多按这么久算，掉帧时不一下滚出一大截。 */
    const val MAX_STEP_SECONDS = 0.05f

    /** 过了 [elapsedSeconds] 秒后的速度。 */
    fun decayed(velocity: Float, elapsedSeconds: Float): Float =
        (velocity * DECELERATION_PER_MS.pow(elapsedSeconds * 1000.0)).toFloat()
}
