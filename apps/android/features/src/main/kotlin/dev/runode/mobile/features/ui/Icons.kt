package dev.runode.mobile.features.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/** 界面上的图标：24 格坐标里的圆头描边（少数实心），颜色由 `Icon` 的 tint 给。 */
internal object AppIcons {
    val back = stroke("back", "M15 6l-6 6 6 6", width = 2.4f)
    val chevronRight = stroke("chevronRight", "M9 6l6 6-6 6", width = 2.4f)
    val check = stroke("check", "M5 12l5 5 9-10", width = 2.4f)
    val close = stroke("close", "M6 6l12 12M18 6L6 18", width = 2.4f)
    val plus = stroke("plus", "M12 5v14M5 12h14", width = 2.2f)
    val settings = stroke("settings", "M4 7h2m6 0h8M4 17h8m6 0h2" + circle(9f, 7f, 2.5f) + circle(15f, 17f, 2.5f))
    val scan = stroke(
        "scan",
        "M4 8V5a1 1 0 0 1 1-1h3M16 4h3a1 1 0 0 1 1 1v3M20 16v3a1 1 0 0 1-1 1h-3M8 20H5a1 1 0 0 1-1-1v-3M8 12h8",
    )
    val laptop = stroke("laptop", "M5 6a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v9H5zM3 18.5h18")
    val display = stroke("display", rect(3f, 5f, 18f, 12f, 2f) + "M9 21h6M12 17v4")
    val folder = stroke("folder", "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z")
    val moon = stroke("moon", "M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z")
    val keyboard = stroke("keyboard", rect(3f, 6f, 18f, 12f, 2.5f) + "M7 10h.01M11 10h.01M15 10h.01M8 14h8")
    val qrcode = stroke(
        "qrcode",
        rect(4f, 4f, 6f, 6f, 1f) + rect(14f, 4f, 6f, 6f, 1f) + rect(4f, 14f, 6f, 6f, 1f) + "M14 14h2v2M20 14v2M14 20h2M18 18h2v2",
    )
    val camera = stroke(
        "camera",
        "M4 8h3l2-3h6l2 3h3a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V9a1 1 0 0 1 1-1z" + circle(12f, 13.5f, 3.5f),
    )
    val bell = stroke("bell", "M6 9a6 6 0 0 1 12 0c0 6 2.5 7 2.5 7h-17S6 15 6 9zM10 20a2 2 0 0 0 4 0")
    val arrowUp = stroke("arrowUp", "M12 19V5M6 11l6-6 6 6", width = 2.2f)
    val jumpToLatest = stroke("jumpToLatest", "M12 4v11M6 10l6 6 6-6M5 20h14", width = 2.2f)
    val warning = stroke("warning", "M12 4l9 16H3zM12 10v4M12 17h.01", width = 2.2f)
    val offline = stroke("offline", "M2 9a15 15 0 0 1 20 0M5.5 12.5a10 10 0 0 1 13 0M9 16a5 5 0 0 1 6 0M12 19.5h.01M3 3l18 18", width = 2.2f)
    val more = fill("more", circle(5f, 12f, 1.8f) + circle(12f, 12f, 1.8f) + circle(19f, 12f, 1.8f))
    val branch = stroke("branch", "M6 3v12" + circle(18f, 6f, 3f) + circle(6f, 18f, 3f) + "M18 9a9 9 0 0 1 -9 9")
    val arrowDown = stroke("arrowDown", "M12 5v14M6 13l6 6 6-6", width = 2.2f)
    val arrowLeft = stroke("arrowLeft", "M19 12H5M11 6l-6 6 6 6", width = 2.2f)
    val arrowRight = stroke("arrowRight", "M5 12h14M13 6l6 6-6 6", width = 2.2f)
    val sync = stroke(
        "sync",
        "M3 12a9 9 0 0 1 15.7 -6L21 8M21 3v5h-5M21 12a9 9 0 0 1 -15.7 6L3 16M8 16H3v5",
    )
    val refresh = stroke("refresh", "M21 12a9 9 0 1 1 -2.6 -6.4L21 8M21 3v5h-5")
    val list = stroke("list", "M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01")
    val tree = stroke("tree", "M21 12h-8M21 6H8M21 18h-8M3 6v4a2 2 0 0 0 2 2h3M3 10v6a2 2 0 0 0 2 2h3")
    val plusCircle = stroke("plusCircle", circle(12f, 12f, 9f) + "M12 8v8M8 12h8")
    val minusCircle = stroke("minusCircle", circle(12f, 12f, 9f) + "M8 12h8")
    val discard = stroke("discard", "M9 14L4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11")
    val copy = stroke("copy", rect(9f, 9f, 12f, 12f, 2f) + "M5 15H4a1 1 0 0 1 -1 -1V4a1 1 0 0 1 1 -1h10a1 1 0 0 1 1 1v1")
    val eye = stroke("eye", "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" + circle(12f, 12f, 3f))
    val eyeOff = stroke("eyeOff", "M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" + circle(12f, 12f, 3f) + "M3 3l18 18")
    val folderPlus = stroke(
        "folderPlus",
        "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2H5a2 2 0 0 1 -2-2zM12 10v6M9 13h6",
    )
    val parentFolder = stroke("parentFolder", "M14 9L9 4 4 9M20 20h-7a4 4 0 0 1 -4-4V4")
    val lock = stroke("lock", rect(5f, 11f, 14f, 10f, 2f) + "M8 11V7a4 4 0 0 1 8 0v4")
    val lockOpen = stroke("lockOpen", rect(5f, 11f, 14f, 10f, 2f) + "M8 11V7a4 4 0 0 1 7.9 -1")
    val touch = stroke("touch", circle(12f, 7f, 3f) + "M12 10v11M8 16l4 5 4-5" + "M6.5 4.5a7 7 0 0 0 0 5M17.5 4.5a7 7 0 0 1 0 5")
    val pointer = stroke("pointer", "M5 3l14 7-6.2 1.8L10 18z")
    val displays = stroke("displays", rect(2f, 4f, 14f, 10f, 2f) + "M18 8h2a2 2 0 0 1 2 2v7a2 2 0 0 1 -2 2H10a2 2 0 0 1 -2-2v-1M13 22h6")
    val keyboardHide = stroke("keyboardHide", rect(3f, 3f, 18f, 12f, 2.5f) + "M7 7h.01M11 7h.01M15 7h.01M8 11h8M9 19l3 3 3-3")
    val download = stroke("download", "M12 3v12M7 10l5 5 5-5M5 21h14")
    val sparkles = stroke(
        "sparkles",
        "M10 4l1.8 4.7L16.5 10.5l-4.7 1.8L10 17l-1.8-4.7L3.5 10.5l4.7-1.8zM18 15l.9 2.1L21 18l-2.1.9L18 21l-.9-2.1L15 18l2.1-.9z",
    )
    val star = stroke("star", "M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z")
    val globe = stroke("globe", circle(12f, 12f, 9f) + "M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18")
    val hammer = stroke("hammer", "M14 4l6 6-3 3-6-6zM11 7l-8 8 3 3 8-8")
    val box = stroke("box", "M3 7l9-4 9 4v10l-9 4-9-4zM3 7l9 4 9-4M12 11v10")
    val terminal = stroke("terminal", "M4 17l6-6-6-6M12 19h8")
    val document = stroke("document", "M14 3H6a1 1 0 0 0 -1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1 -1V8zM14 3v5h5")

    private fun rect(x: Float, y: Float, w: Float, h: Float, r: Float): String =
        "M${x + r} ${y}h${w - 2 * r}a$r $r 0 0 1 $r ${r}v${h - 2 * r}a$r $r 0 0 1 -$r ${r}h-${w - 2 * r}" +
            "a$r $r 0 0 1 -$r -${r}v-${h - 2 * r}a$r $r 0 0 1 $r -${r}z"

    private fun circle(cx: Float, cy: Float, r: Float): String =
        "M${cx - r} ${cy}a$r $r 0 1 0 ${2 * r} 0a$r $r 0 1 0 -${2 * r} 0z"

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

    private fun fill(name: String, path: String): ImageVector =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
            .addPath(addPathNodes(path), fill = SolidColor(Color.Black))
            .build()
}
