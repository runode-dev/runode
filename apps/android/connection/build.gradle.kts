plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.serialization)
}

/** 测试读宿主那边 `runode_protocol` 的样例（`signature_p256.json`），和 iOS 的测试读同一批文件。 */
val repositoryRoot: File = rootDir.resolve("../..").canonicalFile
val protocolFixtures: File = repositoryRoot.resolve("crates/protocol/tests/fixtures")

android {
    namespace = "dev.runode.mobile.connection"
    compileSdk = libs.versions.compile.sdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.min.sdk.get().toInt()
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        unitTests.all { test ->
            test.systemProperty("runode.repo", repositoryRoot.path)
            // 样例改了测试就要重跑，不能当成没变跳过。
            test.inputs.dir(protocolFixtures).withPathSensitivity(PathSensitivity.RELATIVE)
                .withPropertyName("protocolFixtures")
        }
    }
}

// Gradle 不调 cargo：iroh 连同依赖编一次要几分钟，交给单独的脚本，编出的 .so 放在默认的 jniLibs 下。
val tunnelAbis = listOf("arm64-v8a", "x86_64")
val checkIrohTunnel = tasks.register("checkIrohTunnel") {
    description = "确认 librunode_tunnel.so 已经编好"
    val libraries = tunnelAbis.map { file("src/main/jniLibs/$it/librunode_tunnel.so") }
    doLast {
        val missing = libraries.filterNot { it.isFile }
        if (missing.isNotEmpty()) {
            throw GradleException(
                "找不到 ${missing.joinToString()}。先在仓库根目录跑 make android-tunnel（apps/android/scripts/build-tunnel.sh）",
            )
        }
    }
}

tasks.named("preBuild") {
    dependsOn(checkIrohTunnel)
}

dependencies {
    api(project(":protocol"))
    api(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.coroutines.android)

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test.junit)

    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.kotlin.test.junit)
}
