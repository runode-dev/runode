plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

/**
 * 测试读 `runode_agent` 的样例（`launchers.json`）和 `runode_protocol` 的推送样例、消息样例（`messages.json`），和 iOS 的
 * 测试读同一批文件。
 */
val repositoryRoot: File = rootDir.resolve("../..").canonicalFile
val agentFixtures: File = repositoryRoot.resolve("crates/agent/tests/fixtures")
val pushFixtures: File = repositoryRoot.resolve("crates/protocol/tests/fixtures/push")
val messageFixture: File = repositoryRoot.resolve("crates/protocol/tests/fixtures/messages.json")

android {
    namespace = "dev.runode.mobile.features"
    compileSdk = libs.versions.compile.sdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.min.sdk.get().toInt()
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    // 假的连接、配对和画终端的一方，JVM 上的测试和设备上的测试（要 VT 的终端页）共用。
    sourceSets {
        getByName("test").kotlin.srcDir("src/sharedTest/kotlin")
        getByName("androidTest").kotlin.srcDir("src/sharedTest/kotlin")
    }

    testOptions {
        unitTests.all { test ->
            test.systemProperty("runode.repo", repositoryRoot.path)
            // 样例改了测试就要重跑，不能当成没变跳过。
            test.inputs.dir(agentFixtures).withPathSensitivity(PathSensitivity.RELATIVE)
                .withPropertyName("agentFixtures")
            test.inputs.dir(pushFixtures).withPathSensitivity(PathSensitivity.RELATIVE)
                .withPropertyName("pushFixtures")
            test.inputs.file(messageFixture).withPathSensitivity(PathSensitivity.RELATIVE)
                .withPropertyName("messageFixture")
        }
    }
}

dependencies {
    implementation(project(":protocol"))
    implementation(project(":connection"))
    implementation(project(":terminal"))
    implementation(project(":speech"))

    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.activity.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.camera.camera2)
    implementation(libs.camera.lifecycle)
    implementation(libs.camera.view)
    implementation(libs.mlkit.barcode.scanning)

    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test.junit)
    testImplementation(libs.kotlinx.coroutines.test)

    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.kotlin.test.junit)
    androidTestImplementation(libs.kotlinx.coroutines.test)
    androidTestImplementation(libs.compose.ui.test.junit4)
    // Compose 的界面测试要一个空的 ComponentActivity 来装界面。
    debugImplementation(libs.compose.ui.test.manifest)
}
