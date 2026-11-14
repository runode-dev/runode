package dev.runode.mobile.protocol

import kotlinx.serialization.Serializable

/**
 * 一个目录里能跑的命令：自己加的命令（这个项目的和通用的，都在电脑上 `~/.runode/tasks.json` 里）、
 * Makefile 的目标、package.json 的 scripts，回 [ClientMsg.ListTasks]。和宿主的 `TaskSource` 等一一对应。
 * 完整的命令行由宿主拼好，手机在 [project]（为空时是请求的目录）里原样打进 shell。
 */
@Serializable
data class TaskSource(
    val kind: Kind,
    /** 文件的绝对路径。 */
    val file: String,
    /** 自己加的、只用于一个项目的命令（[Kind.Custom]）是哪个项目目录的，也是它们跑的目录；别的种类为空。 */
    val project: String? = null,
    val tasks: List<TaskEntry>,
    /** 太多了，只给了前面一部分。 */
    val truncated: Boolean = false,
) {
    @Serializable(with = TaskSourceKindSerializer::class)
    enum class Kind(val wireName: String) {
        Custom("custom"),
        Global("global"),
        Makefile("makefile"),
        PackageJson("package_json"),

        /** 新的电脑上多出来的种类。 */
        Unknown("unknown"),
    }
}

internal object TaskSourceKindSerializer : WireNameSerializer<TaskSource.Kind>(
    "TaskSource.Kind",
    known = TaskSource.Kind.entries,
    wireName = TaskSource.Kind::wireName,
    unknown = { TaskSource.Kind.Unknown },
)

/** 一条能跑的命令。 */
@Serializable
data class TaskEntry(
    /** 目标名或 script 名。 */
    val name: String,
    /** 在 [TaskSource.project]（为空时是请求的目录）里打进 shell 就能跑的完整命令行。 */
    val command: String,
    /** Makefile 目标那一行 `##` 后面的说明，package.json 里是 script 本身。 */
    val description: String? = null,
)
