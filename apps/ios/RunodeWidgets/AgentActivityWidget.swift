import ActivityKit
import RunodeActivity
import SwiftUI
import WidgetKit

/// 灵动岛和锁屏上「agent 在等你回答」的提醒，一个会话一张：电脑经推送起、更新和收起，App 不送内容。
/// 只用 SF Symbols 和颜色，不引用 App 里各家 agent 的 logo（那些资源在 RunodeFeatures 里，扩展不链接它）。
/// 点一下按深链接打开那个会话的终端页。电脑太久没推新内容（`isStale`）时变灰，不再当成还在等。
struct AgentActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AgentActivityAttributes.self) { context in
            LockScreenView(attributes: context.attributes, state: context.state, stale: context.isStale)
                .widgetURL(context.attributes.link?.url)
        } dynamicIsland: { context in
            let state = context.state
            let stale = context.isStale
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    WaitingIcon(stale: stale)
                        .font(.title2)
                        .padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.center) {
                    Heading(attributes: context.attributes, state: state, stale: stale)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    ScreenLines(lines: state.lines)
                        .padding(.horizontal, 4)
                }
            } compactLeading: {
                WaitingIcon(stale: stale)
            } compactTrailing: {
                // 紧凑态右边很窄，只放 agent 名字的头一个词（「Claude」）。
                Text(AgentActivityText.shortAgent(state.agent))
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                    .frame(maxWidth: 56)
                    .foregroundStyle(stale ? .gray : .orange)
                    .accessibilityLabel(AgentActivityText.headline(state.agent))
            } minimal: {
                WaitingIcon(stale: stale)
            }
            .keylineTint(stale ? .gray : .orange)
            .widgetURL(context.attributes.link?.url)
        }
    }
}

/// 锁屏上的样子：和灵动岛展开后一样，图标在左边。
private struct LockScreenView: View {
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState
    let stale: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                WaitingIcon(stale: stale)
                    .font(.title2)
                Heading(attributes: attributes, state: state, stale: stale)
            }
            ScreenLines(lines: state.lines)
        }
        .padding(14)
    }
}

/// agent 名字（「Claude Code 在等你回答」）和「标题 · 电脑名」。
private struct Heading: View {
    let attributes: AgentActivityAttributes
    let state: AgentActivityAttributes.ContentState
    let stale: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(AgentActivityText.headline(state.agent))
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(stale ? .secondary : .primary)
            Text("\(state.title) · \(attributes.machineName)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .lineLimit(1)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// 屏幕底部的问题和选项：等宽小字，每行一行，放不下就截断。
private struct ScreenLines: View {
    let lines: [String]

    var body: some View {
        if !lines.isEmpty {
            VStack(alignment: .leading, spacing: 1) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .font(.caption2.monospaced())
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// 等回答的图标：醒目的橙色；过时了是灰色。
private struct WaitingIcon: View {
    let stale: Bool

    var body: some View {
        Image(systemName: "exclamationmark.bubble.fill")
            .foregroundStyle(stale ? .gray : .orange)
            .accessibilityLabel(stale ? "提醒已过时" : "等你回答")
    }
}

private enum AgentActivityText {
    /// 「Claude Code 在等你回答」；电脑没报 agent 时只说「在等你回答」。
    static func headline(_ agent: String) -> String {
        agent.isEmpty ? String(localized: "在等你回答") : String(localized: "\(agent) 在等你回答")
    }

    /// agent 名字的头一个词；没有时是「等回答」。
    static func shortAgent(_ agent: String) -> String {
        agent.split(separator: " ").first.map(String.init) ?? String(localized: "等回答")
    }
}
