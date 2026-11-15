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

/**
 * 推送中转的基址。release 默认用正式的中转；debug 默认不带中转，FCM 的 token 原文直接交给配了 Firebase 服务账号的电脑。
 * Gradle 属性 `RUNODE_PUSH_RELAY` 给了时所有构建类型都用它，空串表示不带中转，本地测中转时用
 * `-PRUNODE_PUSH_RELAY=https://…`；非空时必须是带主机的 https 地址。
 */
val pushRelayProperty: String? = providers.gradleProperty("RUNODE_PUSH_RELAY").orNull?.trim()

fun pushRelay(default: String): String = (pushRelayProperty ?: default).also { relay ->
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
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        getByName("debug") {
            buildConfigField("String", "RUNODE_PUSH_RELAY", "\"${pushRelay("")}\"")
        }
        getByName("release") {
            buildConfigField("String", "RUNODE_PUSH_RELAY", "\"${pushRelay("https://push.runode.dev")}\"")
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
