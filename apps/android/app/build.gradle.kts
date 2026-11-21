import java.net.URI

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// Firebase 项目的配置（google-services.json）不进仓库：有它时才套 google-services 插件；没有时 Firebase 不初始化，App 照常
// 编译运行，只是不登记推送。
if (file("google-services.json").exists()) {
    apply(plugin = "com.google.gms.google-services")
}

/** 构建参数：先取 Gradle 属性（`-P名字=值`），没给时取同名的环境变量。 */
fun setting(name: String): String? =
    providers.gradleProperty(name).orElse(providers.environmentVariable(name)).orNull?.trim()

/**
 * 推送中转的基址（`RUNODE_PUSH_RELAY`），非空时必须是带主机的 https 地址。
 * 没给或给空串时不带中转，FCM 的 token 原文直接交给配了 Firebase 服务账号的电脑。
 */
val pushRelay: String = (setting("RUNODE_PUSH_RELAY") ?: "").also { relay ->
    require(
        relay.isEmpty() ||
            runCatching { URI(relay) }.getOrNull()?.let { it.scheme == "https" && !it.host.isNullOrEmpty() } == true,
    ) {
        "RUNODE_PUSH_RELAY 要是带主机的 https 地址或空串：$relay"
    }
}

android {
    namespace = "dev.runode.mobile"
    compileSdk = libs.versions.compile.sdk.get().toInt()

    defaultConfig {
        applicationId = "dev.runode.mobile"
        minSdk = libs.versions.min.sdk.get().toInt()
        targetSdk = libs.versions.target.sdk.get().toInt()
        versionCode = setting("RUNODE_VERSION_CODE")?.toInt() ?: 1
        versionName = "0.1.0"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    // 给了 RUNODE_KEYSTORE 时 release 包用这把上传密钥签名，没给时编出来的 release 包不签名。
    val keystore = setting("RUNODE_KEYSTORE")
    if (keystore != null) {
        signingConfigs.create("upload") {
            storeFile = file(keystore)
            storePassword = setting("RUNODE_KEYSTORE_PASSWORD")
            keyAlias = setting("RUNODE_KEY_ALIAS")
            keyPassword = setting("RUNODE_KEY_PASSWORD")
        }
    }

    buildTypes {
        all {
            buildConfigField("String", "RUNODE_PUSH_RELAY", "\"$pushRelay\"")
        }
        getByName("release") {
            if (keystore != null) signingConfig = signingConfigs.getByName("upload")
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
