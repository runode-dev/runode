import SwiftUI
import WidgetKit

/// 小组件扩展的入口。现在只有灵动岛和锁屏上那个 agent 状态的 Live Activity，没有桌面小组件。
@main
struct RunodeWidgetsBundle: WidgetBundle {
    var body: some Widget {
        AgentActivityWidget()
    }
}
