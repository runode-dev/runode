package dev.runode.mobile.features.ui.theme

import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import dev.runode.mobile.features.model.UiText

/** 按当前语言解析成文字；语言、配置变了时跟着重新解析。 */
@Composable
@ReadOnlyComposable
fun UiText.resolve(): String {
    LocalConfiguration.current
    return resolve(LocalContext.current.resources)
}

/** 把一句话里用反引号括起来的几段（命令）换成等宽字，其余照原样。 */
fun inlineCode(text: String): AnnotatedString = buildAnnotatedString {
    text.split('`').forEachIndexed { index, part ->
        if (index % 2 == 1) {
            pushStyle(SpanStyle(fontFamily = FontFamily.Monospace))
            append(part)
            pop()
        } else {
            append(part)
        }
    }
}
