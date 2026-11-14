package dev.runode.mobile.features.model

import android.content.res.Resources
import androidx.annotation.PluralsRes
import androidx.annotation.StringRes

/**
 * 给用户看的一段文字，界面层按当前语言解析（[resolve]）。视图模型不碰 Android 资源，单元测试比对资源编号和参数。
 * 参数可以是数字、字符串或别的 [UiText]，解析时别的 [UiText] 先解析成文字再填进去。
 */
sealed class UiText {
    /** 原样显示的字：宿主写的原文、名字、路径。 */
    data class Raw(val text: String) : UiText()

    /** strings.xml 里的一条，[args] 按位置填进 `%1$s` 这样的占位。 */
    data class Res(@param:StringRes val id: Int, val args: List<Any> = emptyList()) : UiText()

    /** plurals 里的一条，按 [count] 选单复数，[args] 照 [Res] 填。 */
    data class Plural(@param:PluralsRes val id: Int, val count: Int, val args: List<Any> = emptyList()) : UiText()

    fun resolve(resources: Resources): String = when (this) {
        is Raw -> text
        is Res -> if (args.isEmpty()) resources.getString(id) else resources.getString(id, *resolved(resources, args))
        is Plural -> resources.getQuantityString(id, count, *resolved(resources, args))
    }

    private fun resolved(resources: Resources, args: List<Any>): Array<Any> =
        args.map { if (it is UiText) it.resolve(resources) else it }.toTypedArray()

    companion object {
        fun res(@StringRes id: Int, vararg args: Any): UiText = Res(id, args.toList())

        fun plural(@PluralsRes id: Int, count: Int, vararg args: Any): UiText = Plural(id, count, args.toList())
    }
}
