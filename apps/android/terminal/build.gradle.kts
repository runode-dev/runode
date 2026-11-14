plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.compose)
}

/** zig 编出的 libghostty-vt，`make android-vt` 生成，按 zig 的目标三元组分目录。 */
val ghosttyVtDir: File = rootDir.resolve("build/ghostty-vt")
val ghosttyVtTriples = listOf("aarch64-linux-android", "x86_64-linux-android")

android {
    namespace = "dev.runode.mobile.terminal"
    compileSdk = libs.versions.compile.sdk.get().toInt()
    ndkVersion = libs.versions.ndk.get()

    defaultConfig {
        minSdk = libs.versions.min.sdk.get().toInt()
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
        externalNativeBuild {
            cmake {
                arguments += "-DGHOSTTY_VT_DIR=${ghosttyVtDir.absolutePath}"
            }
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = libs.versions.cmake.get()
        }
    }

    // VTerminal 在 debug 构建里断言只在主线程上用。
    buildFeatures {
        buildConfig = true
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// Gradle 不调 zig：编 libghostty-vt 要几分钟，交给单独的脚本。
val checkGhosttyVt = tasks.register("checkGhosttyVt") {
    description = "确认 libghostty-vt 已经编好"
    val libraries = ghosttyVtTriples.map { ghosttyVtDir.resolve("$it/lib/libghostty-vt.a") }
    doLast {
        val missing = libraries.filterNot { it.isFile }
        if (missing.isNotEmpty()) {
            throw GradleException(
                "找不到 ${missing.joinToString()}。先在仓库根目录跑 make android-vt（apps/android/scripts/build-ghostty-vt.sh）",
            )
        }
    }
}

tasks.named("preBuild") {
    dependsOn(checkGhosttyVt)
}

dependencies {
    api(project(":protocol"))

    // 对外的 Composable（Terminal、KeyBar）的签名里有 Modifier、Color，用的一方也要有 compose-ui。
    api(platform(libs.compose.bom))
    api(libs.compose.ui)
    implementation(libs.compose.material3)

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test.junit)

    androidTestImplementation(libs.junit)
    androidTestImplementation(libs.kotlin.test.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
}
