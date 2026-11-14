plugins {
    alias(libs.plugins.android.library)
}

/** 官方 AAR 里取出的 sherpa-onnx：Kotlin API 的 classes.jar 和按 ABI 分目录的 JNI 库，`make android-speech` 生成。 */
val sherpaOnnxDir: File = rootDir.resolve("build/sherpa-onnx")
val sherpaOnnxAbis = listOf("arm64-v8a", "x86_64")

android {
    namespace = "dev.runode.mobile.speech"
    compileSdk = libs.versions.compile.sdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.min.sdk.get().toInt()
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets {
        getByName("main").jniLibs.directories.add(sherpaOnnxDir.resolve("jni").path)
    }
}

// Gradle 不下载 sherpa-onnx：要按写死的哈希核对官方的 release，交给单独的脚本。
val checkSherpaOnnx = tasks.register("checkSherpaOnnx") {
    description = "确认 sherpa-onnx 已经取好"
    val files = listOf(sherpaOnnxDir.resolve("classes.jar")) +
        sherpaOnnxAbis.map { sherpaOnnxDir.resolve("jni/$it/libsherpa-onnx-jni.so") }
    doLast {
        val missing = files.filterNot { it.isFile }
        if (missing.isNotEmpty()) {
            throw GradleException(
                "找不到 ${missing.joinToString()}。先在仓库根目录跑 make android-speech（apps/android/scripts/fetch-sherpa-onnx.sh）",
            )
        }
    }
}

tasks.named("preBuild") {
    dependsOn(checkSherpaOnnx)
}

dependencies {
    implementation(files(sherpaOnnxDir.resolve("classes.jar")))
    implementation(libs.kotlinx.coroutines.android)

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test.junit)
    testImplementation(libs.kotlinx.coroutines.test)

    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.kotlin.test.junit)
}
