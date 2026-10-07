import ActivityKit
import RunodeActivity
import SwiftUI
import WidgetKit

/// 灵动岛和锁屏上的 agent 状态：内容由 App 的 `AgentActivityModel` 送来。只用 SF Symbols 和颜色，不引用
/// App 里各家 agent 的 logo（那些资源在 RunodeFeatures 里，扩展不链接它）。点一下打开 App，系统默认就是
/// 这样，不带深链接。
struct AgentActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivityAttributes.self) { context in
            LockScreenView(content: context.state, paused: context.state.paused || context.isStale)
        } dynamicIsland: { context in
            let content = context.state
            let paused = content.paused || context.isStale
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    StatusIcon(state: content.overall)
                        .font(.title2)
                        .padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Text("Runode")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .padding(.trailing, 4)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(AgentActivityText.summary(content))
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    VStack(alignment: .leading, spacing: 4) {
                        // 灵动岛展开后高度有限，暂停时少列一条，给提示留地方。
                        ForEach(content.entries.prefix(paused ? 3 : 4)) { entry in
                            EntryRow(entry: entry)
                        }
                        if paused {
                            PausedLine()
                        }
                    }
                    .padding(.horizontal, 4)
                }
            } compactLeading: {
                StatusIcon(state: content.overall)
            } compactTrailing: {
                Text("\(content.headline)")
                    .font(.body.monospacedDigit().weight(.semibold))
                    .foregroundStyle(AgentActivityStyle.color(content.overall))
                    .opacity(paused ? 0.6 : 1)
                    .accessibilityLabel(AgentActivityText.summary(content))
            } minimal: {
                StatusIcon(state: content.overall)
            }
            .keylineTint(AgentActivityStyle.color(content.overall))
        }
    }
}

/// 锁屏上的样子：和灵动岛展开后差不多，多一行标题。
private struct LockScreenView: View {
    let content: AgentActivityContent
    let paused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                StatusIcon(state: content.overall)
                    .font(.title3)
                Text(AgentActivityText.summary(content))
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Spacer(minLength: 4)
                Text("Runode")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            ForEach(content.entries) { entry in
                EntryRow(entry: entry)
            }
            if paused {
                PausedLine()
            }
        }
        .padding(14)
    }
}

/// 一个会话：状态图标、会话标题，右边是 agent 和电脑。
private struct EntryRow: View {
    let entry: AgentActivityContent.Entry

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: AgentActivityStyle.symbol(entry.state))
                .foregroundStyle(AgentActivityStyle.color(entry.state))
                .frame(width: 16)
            Text(entry.title)
                .lineLimit(1)
            Spacer(minLength: 4)
            Text("\(entry.agent) · \(entry.machine)")
                .lineLimit(1)
                .foregroundStyle(.secondary)
        }
        .font(.caption)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(entry.title)，\(entry.agent) \(AgentActivityText.state(entry.state))，在 \(entry.machine) 上")
    }
}

/// 连接停了时的那行提示。
private struct PausedLine: View {
    var body: some View {
        Label("连接已暂停，打开 Runode 刷新", systemImage: "arrow.clockwise")
            .font(.caption2)
            .foregroundStyle(.secondary)
    }
}

/// 整体或一个会话的状态图标：等回答是醒目的橙色，在干活的转起来，空闲的是灰色。
private struct StatusIcon: View {
    let state: AgentActivityState

    var body: some View {
        Image(systemName: AgentActivityStyle.symbol(state))
            .foregroundStyle(AgentActivityStyle.color(state))
            .symbolEffect(.rotate, options: .repeating, isActive: state == .working)
            .accessibilityLabel(AgentActivityText.state(state))
    }
}

private enum AgentActivityStyle {
    static func symbol(_ state: AgentActivityState) -> String {
        switch state {
        case .blocked: "exclamationmark.bubble.fill"
        case .working: "gearshape.2.fill"
        case .idle: "checkmark.circle.fill"
        }
    }

    static func color(_ state: AgentActivityState) -> Color {
        switch state {
        case .blocked: .orange
        case .working: .cyan
        case .idle: .gray
        }
    }
}

private enum AgentActivityText {
    static func state(_ state: AgentActivityState) -> String {
        switch state {
        case .blocked: "等你回答"
        case .working: "干活中"
        case .idle: "空闲"
        }
    }

    /// 「2 个等你回答 · 1 个在干活」：只写不是零的。
    static func summary(_ content: AgentActivityContent) -> String {
        var parts: [String] = []
        if content.blocked > 0 { parts.append("\(content.blocked) 个等你回答") }
        if content.working > 0 { parts.append("\(content.working) 个在干活") }
        if content.idle > 0 { parts.append("\(content.idle) 个空闲") }
        return parts.isEmpty ? "没有 agent" : parts.joined(separator: " · ")
    }
}
