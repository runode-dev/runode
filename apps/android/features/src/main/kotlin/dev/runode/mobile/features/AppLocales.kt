package dev.runode.mobile.features

import android.app.LocaleManager
import android.content.Context
import android.content.res.Configuration
import android.content.res.Resources
import android.os.Build
import android.os.LocaleList
import dev.runode.mobile.features.model.AppLanguage
import java.util.Locale

/**
 * 把设置里的 [AppLanguage] 用到界面上。Android 13 起交给系统的分 App 语言（`LocaleManager`），系统设置里也能改，
 * 改了系统会重建 Activity；更早的系统没有分 App 语言，由 Activity 在 `attachBaseContext` 里经 [wrap] 换语言，
 * 改设置后重建 Activity 生效。
 */
object AppLocales {
    /** 设置里换了语言后马上生效；Android 13 以前靠重建 [context] 所在的 Activity，先把设置存好再调。 */
    fun apply(context: Context, language: AppLanguage) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.getSystemService(LocaleManager::class.java).applicationLocales = localeList(language)
        } else {
            context.findActivity()?.recreate()
        }
    }

    /** 按 [language] 取资源的 Context；Android 13 起系统已经换好了，跟随系统时也用不着换，原样交回。 */
    fun wrap(base: Context, language: AppLanguage): Context {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) return base
        val tag = language.tag
        if (tag == null) {
            Locale.setDefault(Resources.getSystem().configuration.locales[0])
            return base
        }
        val configuration = Configuration(base.resources.configuration)
        configuration.setLocales(LocaleList.forLanguageTags(tag))
        Locale.setDefault(Locale.forLanguageTag(tag))
        return base.createConfigurationContext(configuration)
    }

    /** 系统给本 App 定的语言（用户可能在系统设置里改过）；Android 13 以前没有这一项，为空。 */
    fun systemChoice(context: Context): AppLanguage? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return null
        val locales = context.getSystemService(LocaleManager::class.java).applicationLocales
        return AppLanguage.fromLanguageTags(locales.toLanguageTags())
    }

    /**
     * 启动时对齐设置里的 [stored] 和系统的分 App 语言（[AppLanguage.reconcile]），交回设置该改成的语言，不用改时为空；
     * Android 13 以前没有分 App 语言，什么都不做。[synced] 是头一次对齐做过没有，对齐了就调 [markSynced]。
     */
    fun sync(context: Context, stored: AppLanguage, synced: Boolean, markSynced: () -> Unit): AppLanguage? {
        val system = systemChoice(context) ?: return null
        if (!synced) markSynced()
        val language = AppLanguage.reconcile(system, stored, synced)
        if (language == null) {
            apply(context, stored)
            return null
        }
        return language.takeIf { it != stored }
    }

    private fun localeList(language: AppLanguage): LocaleList =
        language.tag?.let(LocaleList::forLanguageTags) ?: LocaleList.getEmptyLocaleList()
}
