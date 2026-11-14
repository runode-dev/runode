plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.jvm) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.kotlin.serialization) apply false
    alias(libs.plugins.google.services) apply false
}

/** 每个模块能依赖哪些模块：只能依赖它下层的。加模块时在这里登记，不登记的模块 [checkLayers] 报错。 */
val allowedProjectDependencies: Map<String, Set<String>> = mapOf(
    ":protocol" to emptySet(),
    ":connection" to setOf(":protocol"),
    ":terminal" to setOf(":protocol"),
    ":speech" to emptySet(),
    ":features" to setOf(":protocol", ":connection", ":terminal", ":speech"),
    ":app" to setOf(":features", ":protocol", ":connection"),
)

val checkLayers = tasks.register("checkLayers") {
    group = "verification"
    description = "核对各模块之间的 project 依赖符合 allowedProjectDependencies"
    val allowed = allowedProjectDependencies
    // 声明的依赖在配置阶段读好，执行时只用这份数据，配置缓存才存得下。
    val declared: Map<String, Set<String>> = subprojects.associate { module ->
        module.path to module.configurations
            .flatMap { configuration -> configuration.dependencies.withType<ProjectDependency>().map { it.path } }
            .filter { it != module.path }
            .toSet()
    }
    doLast {
        val problems = declared.flatMap { (module, dependencies) ->
            val ok = allowed[module] ?: return@flatMap listOf("$module 没有登记在 allowedProjectDependencies 里")
            (dependencies - ok).sorted().map { "$module 不能依赖 $it" }
        }
        if (problems.isNotEmpty()) throw GradleException(problems.joinToString("\n"))
    }
}

/** 把繁体中文的 strings.xml 复制成台湾、香港、澳门三个地区目录各一份。 */
abstract class TraditionalChineseRegions : DefaultTask() {
    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val source: RegularFileProperty

    @get:OutputDirectory
    abstract val output: DirectoryProperty

    @TaskAction
    fun copy() {
        val root = output.get().asFile
        root.deleteRecursively()
        for (region in listOf("TW", "HK", "MO")) {
            source.get().asFile.copyTo(root.resolve("values-zh-r$region/strings.xml"))
        }
    }
}

/**
 * Android 模块的译文：缺了、多了（`MissingTranslation`、`ExtraTranslation`）lint 报错。繁体中文只写在 `values-b+zh+Hant`
 * 一份，`zh-rTW`、`zh-rHK`、`zh-rMO` 三个地区目录在构建时由它复制出来，不在仓库里另存几份。
 */
subprojects {
    listOf("com.android.application", "com.android.library").forEach { id ->
        pluginManager.withPlugin(id) {
            val android = extensions.getByName("android") as com.android.build.api.dsl.CommonExtension
            android.lint.error += setOf("MissingTranslation", "ExtraTranslation")
            val traditional = file("src/main/res/values-b+zh+Hant/strings.xml")
            if (!traditional.isFile) return@withPlugin
            val regions = tasks.register<TraditionalChineseRegions>("traditionalChineseRegions") { source.set(traditional) }
            val components = extensions.getByName("androidComponents") as com.android.build.api.variant.AndroidComponentsExtension<*, *, *>
            components.onVariants { variant ->
                variant.sources.res?.addGeneratedSourceDirectory(regions, TraditionalChineseRegions::output)
            }
        }
    }
}

// 根项目不套 base 插件：它的 clean 会删掉根目录的 build，那里放着 zig 编出的 libghostty-vt。
tasks.register("check") {
    group = "verification"
    dependsOn(checkLayers)
}
