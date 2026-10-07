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
                    StatusIcon(state: content.overall, worker: content.leadingWorker, paused: paused)
                        .font(.title2)
                        .padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Group {
                        if let since = runningSince(content, paused: paused) {
                            ElapsedTime(since: since)
                                .foregroundStyle(AgentActivityStyle.color(.working))
                        } else {
                            Text("Runode")
                                .foregroundStyle(.secondary)
                        }
                    }
                    .font(.caption)
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
                StatusIcon(state: content.overall, worker: content.leadingWorker, paused: paused)
            } compactTrailing: {
                // 在干活时是系统自己走的计时（Live Activity 里循环动画不播，只有计时会动），其余时候是个数。
                Group {
                    if let since = runningSince(content, paused: paused) {
                        ElapsedTime(since: since)
                    } else {
                        Text("\(content.headline)")
                    }
                }
                .font(.body.monospacedDigit().weight(.semibold))
                .foregroundStyle(paused ? .gray : AgentActivityStyle.color(content.overall))
                .accessibilityLabel(AgentActivityText.summary(content))
            } minimal: {
                StatusIcon(state: content.overall, worker: content.leadingWorker, paused: paused)
            }
            .keylineTint(paused ? .gray : AgentActivityStyle.color(content.overall))
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
                StatusIcon(state: content.overall, worker: content.leadingWorker, paused: paused)
                    .font(.title3)
                Text(AgentActivityText.summary(content))
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(1)
                Spacer(minLength: 4)
                if let since = runningSince(content, paused: paused) {
                    ElapsedTime(since: since)
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(AgentActivityStyle.color(.working))
                } else {
                    Text("Runode")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
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
            StatusIcon(state: entry.state, worker: entry)
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

/// 整体在干活、连接没停时，从什么时候开始算计时；不该显示计时时为空。
private func runningSince(_ content: AgentActivityContent, paused: Bool) -> Date? {
    guard !paused, content.overall == .working else { return nil }
    return content.workingSince
}

/// 往上走的计时（「1:23」），由系统每秒刷新，不用 App 推更新。计时文字会按最长的样子占宽度，限住它。
private struct ElapsedTime: View {
    let since: Date

    var body: some View {
        Text(timerInterval: since...Date.distantFuture, countsDown: false)
            .multilineTextAlignment(.trailing)
            .lineLimit(1)
            .minimumScaleFactor(0.6)
            .frame(maxWidth: 56, alignment: .trailing)
    }
}

/// 整体或一个会话的状态图标：在干活时是那个 agent 自己转圈里的一帧（`worker` 带着，和 App 里一样），
/// 没带时是青色的齿轮；等回答是醒目的橙色，空闲的是灰色。连接停了或者内容过时了（`paused`）一律是灰色的
/// 暂停，不再让人以为 agent 还在干活。
private struct StatusIcon: View {
    let state: AgentActivityState
    var worker: AgentActivityContent.Entry?
    var paused = false

    var body: some View {
        Group {
            if paused {
                Image(systemName: "pause.circle.fill")
                    .foregroundStyle(.gray)
            } else if state == .working, let spinner = worker?.spinner {
                Text(spinner)
                    .fontWeight(.bold)
                    .foregroundStyle(worker?.spinnerColor.map(Color.init(hex:)) ?? .primary)
            } else {
                Image(systemName: AgentActivityStyle.symbol(state))
                    .foregroundStyle(AgentActivityStyle.color(state))
            }
        }
        .accessibilityLabel(paused ? "连接已暂停" : AgentActivityText.state(state))
    }
}

extension Color {
    /// 0xRRGGBB。
    fileprivate init(hex: UInt32) {
        self.init(
            red: Double(hex >> 16 & 0xFF) / 255, green: Double(hex >> 8 & 0xFF) / 255, blue: Double(hex & 0xFF) / 255)
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
