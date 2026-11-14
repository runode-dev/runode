package dev.runode.mobile.terminal

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/** 按键栏上的图标：24 格坐标里的圆头描边，颜色由 `Icon` 的 tint 给。 */
internal object KeyIcons {
    val left = stroke("left", "M15 6l-6 6 6 6", width = 2.4f)
    val down = stroke("down", "M6 9l6 6 6-6", width = 2.4f)
    val up = stroke("up", "M6 15l6-6 6 6", width = 2.4f)
    val right = stroke("right", "M9 6l6 6-6 6", width = 2.4f)
    val enter = stroke("enter", "M20 5v7a3 3 0 0 1-3 3H5M9 11l-4 4 4 4", width = 2.2f)
    val paste = stroke(
        "paste",
        "M9.5 3.5h5v3h-5zM9 5H6.5A1.5 1.5 0 0 0 5 6.5v13A1.5 1.5 0 0 0 6.5 21h11a1.5 1.5 0 0 0 1.5-1.5v-13A1.5 1.5 0 0 0 17.5 5H15",
    )
    val microphone =stroke("microphone", "M9 6a3 3 0 0 1 6 0v5a3 3 0 0 1-6 0zM5 11a7 7 0 0 0 14 0M12 18v3")
    val keyboard = stroke(
        "keyboard",
        "M5.5 6h13a2.5 2.5 0 0 1 2.5 2.5v7a2.5 2.5 0 0 1-2.5 2.5h-13a2.5 2.5 0 0 1-2.5-2.5v-7a2.5 2.5 0 0 1 2.5-2.5z" +
            "M7 10h.01M11 10h.01M15 10h.01M8 14h8",
    )

    private fun stroke(name: String, path: String, width: Float = 2f): ImageVector =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .addPath(
                addPathNodes(path),
                stroke = SolidColor(Color.Black),
                strokeLineWidth = width,
                strokeLineCap = StrokeCap.Round,
                strokeLineJoin = StrokeJoin.Round,
            )
            .build()
}
