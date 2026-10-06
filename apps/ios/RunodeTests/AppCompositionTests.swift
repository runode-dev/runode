import Testing

@testable import Runode

/// App 入口里的组装代码。各模块自己的测试在 RunodeKit 包里，scheme 一起跑。
@MainActor
@Suite struct AppCompositionTests {
    @Test func buildIdentifierCarriesVersionAndBuild() {
        #expect(
            AppComposition.buildIdentifier(info: ["CFBundleShortVersionString": "0.1.0", "CFBundleVersion": "7"])
                == "ios-0.1.0+7")
        #expect(AppComposition.buildIdentifier(info: [:]) == "ios-0+0")
    }

    @Test func liveDependenciesComposeWithoutTouchingTheNetwork() {
        let dependencies = AppComposition.dependencies()
        #expect(!dependencies.deviceName.isEmpty)
    }
}
