import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    api(libs.kotlinx.serialization.json)
    testImplementation(libs.junit)
    testImplementation(libs.kotlin.test.junit)
}

/** 测试读宿主那边 `runode_protocol` 的样例，和 iOS 的测试读同一批文件。 */
val repositoryRoot: File = rootDir.resolve("../..").canonicalFile
val protocolFixtures: File = repositoryRoot.resolve("crates/protocol/tests/fixtures")

tasks.withType<Test>().configureEach {
    systemProperty("runode.repo", repositoryRoot.path)
    // 样例改了测试就要重跑，不能当成没变跳过。
    inputs.dir(protocolFixtures).withPathSensitivity(PathSensitivity.RELATIVE).withPropertyName("protocolFixtures")
}
