import Testing

@testable import Runode

/// App 入口里的组装代码。各模块自己的测试在 RunodeKit 包里，scheme 一起跑。
@MainActor
@Suite struct AppCompositionTests {
    @Test func buildIdentifierIsFixed() {
        #expect(AppComposition.buildIdentifier == "dev.runode.mobile")
    }

    @Test func liveDependenciesComposeWithoutTouchingTheNetwork() {
        let dependencies = AppComposition.dependencies()
        #expect(!dependencies.deviceName.isEmpty)
    }
}
