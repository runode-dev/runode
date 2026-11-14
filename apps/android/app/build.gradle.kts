plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// Firebase 项目的配置（google-services.json）不进仓库：有它时才套 google-services 插件；没有时 Firebase 不初始化，App 照常
// 编译运行，只是不登记推送。
if (file("google-services.json").exists()) {
    apply(plugin = "com.google.gms.google-services")
}

/** 推送中转的基址，编译时用 Gradle 属性 `RUNODE_PUSH_RELAY` 换掉，本地测中转时用。 */
val pushRelay: String = providers.gradleProperty("RUNODE_PUSH_RELAY").getOrElse("https://push.runode.dev")

android {
    namespace = "dev.runode.mobile"
    compileSdk = libs.versions.compile.sdk.get().toInt()

    defaultConfig {
        applicationId = "dev.runode.mobile"
        minSdk = libs.versions.min.sdk.get().toInt()
        targetSdk = libs.versions.target.sdk.get().toInt()
        versionCode = 1
        versionName = "0.1.0"

        buildConfigField("String", "RUNODE_PUSH_RELAY", "\"$pushRelay\"")

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }
}

dependencies {
    implementation(project(":features"))
    implementation(project(":protocol"))
    implementation(project(":connection"))
    implementation(platform(libs.compose.bom))
    implementation(libs.activity.compose)
    implementation(platform(libs.firebase.bom))
    implementation(libs.firebase.messaging)
    implementation(libs.androidx.fragment)
}
