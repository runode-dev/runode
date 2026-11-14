package dev.runode.mobile

import android.content.Context
import dev.runode.mobile.features.model.AppModel

/** 演示模式只在调试构建里有；正式构建不认演示参数，照常连真的电脑。 */
internal object DemoComposition {
    const val EXTRA: String = "demo"

    @Suppress("UNUSED_PARAMETER")
    fun make(context: Context, words: String): AppModel? = null

    @Suppress("UNUSED_PARAMETER")
    fun open(app: AppModel, words: String) {}
}
