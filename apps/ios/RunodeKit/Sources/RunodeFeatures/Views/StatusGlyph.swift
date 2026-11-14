#if os(iOS)
    import RunodeProtocol
    import SwiftUI

    /// 状态标记画的是哪一种。
    enum StatusMark: Hashable {
        /// 琥珀色的实心方块光标：agent 在等你回答。
        case waiting
        /// 依次点亮的六点盲文格：agent 在干活。
        case working
        /// 提示符 `>_`：普通的 shell，或者空闲的 agent。
        case idle
        /// 绿色的对勾：做完了。
        case done

        init(_ status: SessionStatus) {
            switch status {
            case .waiting: self = .waiting
            case .working: self = .working
            case .agentIdle, .running, .shell, .exited: self = .idle
            }
        }

        init(_ state: AgentState) {
            switch state {
            case .blocked: self = .waiting
            case .working: self = .working
            case .idle, .unknown: self = .idle
            }
        }
    }

    /// 状态标记，边长 `size`（调用方用 `@ScaledMetric` 给，跟着动态字体缩放），在 24 格的坐标里画。只有
    /// 图形，对读屏隐藏，状态由旁边的文字说。盲文格按绝对时间走，几处同时在干活的标记步调一致；打开了
    /// 「减少动态效果」时停在前三个点亮。
    struct StatusGlyph: View {
        let mark: StatusMark
        let size: CGFloat
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @Environment(\.themeColors) private var colors

        init(_ mark: StatusMark, size: CGFloat) {
            self.mark = mark
            self.size = size
        }

        /// 盲文格每走一步的秒数。
        private static let step: TimeInterval = 0.15
        /// 盲文格六个点的圆心，按点亮的先后。
        private static let dots: [CGPoint] = [
            CGPoint(x: 7.5, y: 4), CGPoint(x: 16.5, y: 4), CGPoint(x: 16.5, y: 12),
            CGPoint(x: 16.5, y: 20), CGPoint(x: 7.5, y: 20), CGPoint(x: 7.5, y: 12),
        ]
        private static let dotRadius: CGFloat = 2.5
        private static let dimOpacity: Double = 0.22

        var body: some View {
            Group {
                if mark == .working && !reduceMotion {
                    TimelineView(.periodic(from: .now, by: Self.step)) { context in
                        canvas(tick: Int(context.date.timeIntervalSinceReferenceDate / Self.step))
                    }
                } else {
                    // 第 2 步时亮的正好是前三个点。
                    canvas(tick: 2)
                }
            }
            .frame(width: size, height: size)
            .accessibilityHidden(true)
        }

        /// 第 `index` 个点在第 `tick` 步亮不亮：每个点比前一个晚一步亮起，亮三步。
        private static func isLit(_ index: Int, tick: Int) -> Bool {
            ((tick - index) % dots.count + dots.count) % dots.count < 3
        }

        private func canvas(tick: Int) -> some View {
            let mark = mark
            let colors = colors
            return Canvas { context, size in
                let unit = size.width / 24
                context.scaleBy(x: unit, y: unit)
                switch mark {
                case .waiting:
                    let cursor = Path(roundedRect: CGRect(x: 5.5, y: 1, width: 13, height: 22), cornerRadius: 1.5)
                    context.fill(cursor, with: .color(colors.waiting))
                case .working:
                    for (index, center) in Self.dots.enumerated() {
                        var dot = context
                        dot.opacity = Self.isLit(index, tick: tick) ? 1 : Self.dimOpacity
                        let bounds = CGRect(x: center.x, y: center.y, width: 0, height: 0)
                            .insetBy(dx: -Self.dotRadius, dy: -Self.dotRadius)
                        dot.fill(Path(ellipseIn: bounds), with: .color(colors.working))
                    }
                case .idle:
                    var prompt = Path()
                    prompt.move(to: CGPoint(x: 5, y: 17))
                    prompt.addLine(to: CGPoint(x: 11, y: 12))
                    prompt.addLine(to: CGPoint(x: 5, y: 7))
                    prompt.move(to: CGPoint(x: 13, y: 18))
                    prompt.addLine(to: CGPoint(x: 20, y: 18))
                    context.stroke(
                        prompt, with: .style(.secondary),
                        style: StrokeStyle(lineWidth: 2.2, lineCap: .round, lineJoin: .round))
                case .done:
                    var check = Path()
                    check.move(to: CGPoint(x: 5, y: 12))
                    check.addLine(to: CGPoint(x: 10, y: 17))
                    check.addLine(to: CGPoint(x: 19, y: 7))
                    context.stroke(
                        check, with: .color(colors.success),
                        style: StrokeStyle(lineWidth: 2.8, lineCap: .round, lineJoin: .round))
                }
            }
        }
    }

    /// 首页大标题后面闪的光标，表示有 agent 在等你回答。一个周期里前一段亮、后一段暗下去；打开了
    /// 「减少动态效果」时常亮。对读屏隐藏。
    struct BlinkingCursor: View {
        @ScaledMetric(relativeTo: .title) private var width: CGFloat = 13
        @ScaledMetric(relativeTo: .title) private var height: CGFloat = 26
        @Environment(\.accessibilityReduceMotion) private var reduceMotion
        @Environment(\.themeColors) private var colors

        private static let dimOpacity: Double = 0.3

        var body: some View {
            Group {
                if reduceMotion {
                    cursor(lit: true)
                } else {
                    TimelineView(BlinkSchedule()) { context in
                        cursor(lit: Self.isLit(at: context.date))
                    }
                }
            }
            .accessibilityHidden(true)
        }

        private func cursor(lit: Bool) -> some View {
            RoundedRectangle(cornerRadius: 2, style: .continuous)
                .fill(colors.waiting)
                .frame(width: width, height: height)
                .opacity(lit ? 1 : Self.dimOpacity)
        }

        /// `date` 落在周期的亮段里没有。时间线给的时刻正好在亮暗交界上，往后挪一点再算，浮点误差才不会
        /// 把它算进前一段。
        private static func isLit(at date: Date) -> Bool {
            let period = BlinkSchedule.period
            let time = date.timeIntervalSinceReferenceDate + 0.005
            let phase = time - (time / period).rounded(.down) * period
            return phase < BlinkSchedule.litDuration
        }
    }

    /// `BlinkingCursor` 亮起和暗下的时刻，按绝对时间对齐到周期，只在这两处重画。
    private struct BlinkSchedule: TimelineSchedule {
        /// 一亮一暗的周期，秒。
        static let period: TimeInterval = 1.2
        /// 一个周期里亮着的那段，秒。
        static let litDuration: TimeInterval = 0.72

        func entries(from startDate: Date, mode: Mode) -> some Sequence<Date> {
            let period = Self.period
            let litDuration = Self.litDuration
            let start: TimeInterval = (startDate.timeIntervalSinceReferenceDate / period).rounded(.down) * period
            return (0...).lazy.map { (index: Int) -> Date in
                let cycle: TimeInterval = start + Double(index / 2) * period
                let offset: TimeInterval = index.isMultiple(of: 2) ? 0 : litDuration
                return Date(timeIntervalSinceReferenceDate: cycle + offset)
            }
        }
    }
#endif
