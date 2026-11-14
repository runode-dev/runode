#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    extension Color {
        init(_ rgb: Rgb) {
            self.init(.sRGB, red: Double(rgb.r) / 255, green: Double(rgb.g) / 255, blue: Double(rgb.b) / 255)
        }
    }

    /// 终端页以外的界面上的几种底色，从主题（`AppTheme`）的底色和前景色推出来；环境里没设时（预览）用
    /// 系统的。界面上不直接用系统的底色和填充色，一律从环境里的这一份取。
    struct ThemeColors {
        /// 页面，相当于 `systemGroupedBackground`。
        var page: Color
        /// 卡片，相当于 `secondarySystemGroupedBackground`。
        var card: Color
        /// 卡片里的小块（按键、输入框、图标底），相当于 `tertiarySystemFill`。
        var fill: Color
        /// 屏幕预览的底，相当于 `secondarySystemFill`。
        var secondaryFill: Color
        /// 按下时盖在卡片上的一层，相当于 `systemFill`。
        var pressed: Color
        /// 卡片里凸起的一块（按键、选项、标签）。
        var raised: Color
        /// 凹下去的底槽（终端摘录、输入框、分段控件的底）。
        var well: Color
        /// 卡片的边和分隔线。
        var hairline: Color
        /// 次要按钮的边、发送键的底。
        var strongHairline: Color
        /// 首页卡片底下露出来的那截卡边。
        var stackEdge: Color
        /// 下面几种只表示状态，取自 `StatusPalette`。
        var waiting: Color
        var waitingFill: Color
        var onWaitingFill: Color
        var working: Color
        var success: Color
        var failure: Color

        static let system = ThemeColors(
            page: Color(.systemGroupedBackground), card: Color(.secondarySystemGroupedBackground),
            fill: Color(.tertiarySystemFill), secondaryFill: Color(.secondarySystemFill), pressed: Color(.systemFill),
            raised: Color(.tertiarySystemFill), well: Color(.systemBackground), hairline: Color(.separator),
            strongHairline: Color(.opaqueSeparator), stackEdge: Color(.tertiarySystemGroupedBackground),
            waiting: status(\.waiting), waitingFill: status(\.waitingFill), onWaitingFill: status(\.onWaitingFill),
            working: status(\.working), success: status(\.success), failure: status(\.failure))

        /// 跟着系统深浅模式变的状态色。
        private static func status(_ key: KeyPath<StatusPalette, Rgb>) -> Color {
            let dark = UIColor(StatusPalette.dark[keyPath: key])
            let light = UIColor(StatusPalette.light[keyPath: key])
            return Color(UIColor { $0.userInterfaceStyle == .dark ? dark : light })
        }
    }

    extension ThemeColors {
        /// 填充色是前景色加透明度，放在页面和卡片上都看得出来，和系统的填充色一样。
        init(_ theme: AppTheme) {
            let foreground = Color(theme.foreground)
            page = Color(theme.page)
            card = Color(theme.card)
            fill = foreground.opacity(0.10)
            secondaryFill = foreground.opacity(0.14)
            pressed = foreground.opacity(0.18)
            raised = Color(theme.raised)
            well = Color(theme.well)
            hairline = Color(theme.hairline)
            strongHairline = Color(theme.strongHairline)
            stackEdge = Color(theme.stackEdge)
            let status = theme.status
            waiting = Color(status.waiting)
            waitingFill = Color(status.waitingFill)
            onWaitingFill = Color(status.onWaitingFill)
            working = Color(status.working)
            success = Color(status.success)
            failure = Color(status.failure)
        }
    }

    extension UIColor {
        convenience init(_ rgb: Rgb) {
            self.init(red: CGFloat(rgb.r) / 255, green: CGFloat(rgb.g) / 255, blue: CGFloat(rgb.b) / 255, alpha: 1)
        }
    }

    extension EnvironmentValues {
        @Entry var themeColors = ThemeColors.system
        /// 外层套的主题，弹出 sheet 的页面取出来给 sheet 的内容再套一次。没套过主题时为空。
        @Entry var appTheme: AppTheme?
    }

    extension View {
        /// 套用主题：底色和主题本身放进环境，深色主题用深色模式（字、系统控件、键盘跟着变），浅色主题用浅色
        /// 模式；按钮、链接这些强调色用终端的前景色，和桌面一样黑白为主，不用系统的蓝色。`preferredColorScheme`
        /// 传不进 sheet，弹出的页面各自再套一次。
        func appTheme(_ theme: AppTheme) -> some View {
            environment(\.themeColors, ThemeColors(theme))
                .environment(\.appTheme, theme)
                .tint(Color(theme.foreground))
                .preferredColorScheme(theme.isDark ? .dark : .light)
        }

        /// 从环境取来的主题：为空时原样。
        @ViewBuilder func appTheme(_ theme: AppTheme?) -> some View {
            if let theme { appTheme(theme) } else { self }
        }

        /// 实心的主按钮：底是强调色，字用 `systemBackground`，和首页的「配对电脑」一样。系统的
        /// `borderedProminent` 字固定是白的，强调色是主题的前景色，深色主题下就成了白底白字。
        func prominentButtonStyle() -> some View {
            modifier(ProminentButton())
        }
    }

    private struct ProminentButton: ViewModifier {
        @Environment(\.isEnabled) private var isEnabled

        /// 不可用时系统把底换成灰的填充色，字跟着换成次要色，不留 `systemBackground`：深色主题下
        /// 那是灰底上的黑字，看不清。
        func body(content: Content) -> some View {
            content
                .buttonStyle(.borderedProminent)
                .foregroundStyle(isEnabled ? AnyShapeStyle(Color(.systemBackground)) : AnyShapeStyle(.secondary))
        }
    }

    /// 界面上的几档圆角。系统分组列表自带的圆角太大，还会按它裁掉行里的内容，所以列表一律用 `cardList`，
    /// 卡片用 `cardBackground` 自己画。
    enum CornerRadius {
        /// 卡片：列表里的一块、首页的统计格、终端页的横幅。
        static let card: CGFloat = 20
        /// 几行并成一张的列表卡。
        static let group: CGFloat = 18
        /// 底部的大按钮。
        static let action: CGFloat = 15
        /// 卡片里的小块：输入框、预览、图标底、一般的按钮。
        static let inner: CGFloat = 12
        /// 按键。
        static let key: CGFloat = 10
    }

    extension Shape where Self == RoundedRectangle {
        static var card: RoundedRectangle { RoundedRectangle(cornerRadius: CornerRadius.card, style: .continuous) }
        static var group: RoundedRectangle { RoundedRectangle(cornerRadius: CornerRadius.group, style: .continuous) }
        static var action: RoundedRectangle {
            RoundedRectangle(cornerRadius: CornerRadius.action, style: .continuous)
        }
        static var inner: RoundedRectangle { RoundedRectangle(cornerRadius: CornerRadius.inner, style: .continuous) }
        static var key: RoundedRectangle { RoundedRectangle(cornerRadius: CornerRadius.key, style: .continuous) }
    }

    extension View {
        /// 画成一张卡片：四边留 `padding`，`ThemeColors.card` 的底色，`CornerRadius.card` 的圆角，
        /// 描一圈 1pt 的 `ThemeColors.hairline`。
        func cardBackground(padding: CGFloat = 12) -> some View {
            modifier(CardBackground(padding: padding))
        }

        /// 画成按钮的面：至少高 `height`，圆角 `cornerRadius`，底色、字色和字重按 `kind`；宽度和左右的空由
        /// 调用方定。`Menu` 的 label 不一定吃自定义的 `ButtonStyle`，直接套这个，再加 `.buttonStyle(.plain)`。
        func actionSurface(
            _ kind: ActionKind, height: CGFloat = 44, cornerRadius: CGFloat = CornerRadius.inner
        ) -> some View {
            modifier(ActionSurface(kind: kind, height: height, cornerRadius: cornerRadius))
        }

        /// 列表里自己画卡片的一行：左右留出卡片到屏幕边的空，去掉系统给的底色和分隔线。
        func plainListRow() -> some View {
            listRowInsets(EdgeInsets(top: 0, leading: 16, bottom: 0, trailing: 16))
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
        }

        /// 一列卡片的列表：普通样式的 `List`（分组样式会按它的大圆角裁掉行），铺 `ThemeColors.page`，
        /// 卡片之间留空。分组的标题用 `ListSectionHeader` 当作一行放进去，不用 `Section` 的标题，
        /// 免得滚动时钉在顶上；行不设最小高度，标题和说明这种一行小字的行才不会被撑高。
        func cardList() -> some View {
            listStyle(.plain)
                .listRowSpacing(10)
                .environment(\.defaultMinListRowHeight, 0)
                .modifier(PageBackground())
        }

        /// 系统样式的 `Form` 或分组 `List`（开关、选择器要系统的行样式）换上主题的页面和行的底色。
        /// 行的底色要逐个 `Section` 用 `themedRows` 设。
        func themedForm() -> some View {
            modifier(PageBackground())
        }

        /// `themedForm` 里一个 `Section` 的行用卡片的底色。
        func themedRows() -> some View {
            modifier(ThemedRows())
        }

        /// `List` 一行的左滑按钮。`List` 会把左滑按钮在无障碍操作里报两遍，所以 VoiceOver 和切换控制
        /// 开着时不挂左滑，同一组按钮改挂成无障碍操作。
        func trailingSwipeActions<Actions: View>(@ViewBuilder _ actions: () -> Actions) -> some View {
            modifier(TrailingSwipeActions(actions: actions()))
        }
    }

    private struct TrailingSwipeActions<Actions: View>: ViewModifier {
        let actions: Actions
        @Environment(\.accessibilityVoiceOverEnabled) private var voiceOver
        @Environment(\.accessibilitySwitchControlEnabled) private var switchControl

        func body(content: Content) -> some View {
            if voiceOver || switchControl {
                content.accessibilityActions { actions }
            } else {
                content.swipeActions(edge: .trailing) { actions }
            }
        }
    }

    private struct CardBackground: ViewModifier {
        let padding: CGFloat
        @Environment(\.themeColors) private var colors

        func body(content: Content) -> some View {
            content
                .padding(padding)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(colors.card, in: .card)
                .overlay(RoundedRectangle.card.strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
                .contentShape(.contextMenuPreview, .card)
        }
    }

    /// 按钮的几种面。颜色只表示状态，除了 `waiting` 都是中性色。
    enum ActionKind {
        /// 实心，底是前景色、字用页面的底色：一屏里最要紧的那个操作。
        case primary
        /// 卡片的底加一圈 `ThemeColors.strongHairline`。
        case secondary
        /// 凸起的浅块，不描边：卡片里的按键、选项。
        case raised
        /// 琥珀实心：回答在等你的 agent。
        case waiting
    }

    private struct ActionSurface: ViewModifier {
        let kind: ActionKind
        let height: CGFloat
        let cornerRadius: CGFloat
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.themeColors) private var colors

        func body(content: Content) -> some View {
            let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
            content
                .fontWeight(weight)
                .foregroundStyle(foreground)
                .frame(minHeight: height)
                .background(background, in: shape)
                .overlay {
                    if kind == .secondary {
                        shape.strokeBorder(colors.strongHairline, lineWidth: 1).allowsHitTesting(false)
                    }
                }
                .contentShape(shape)
                // 不可用时整块变淡：实心的面换不了灰底，只能这样看出点不了。
                .opacity(isEnabled ? 1 : 0.4)
        }

        private var weight: Font.Weight {
            switch kind {
            case .primary, .waiting: .semibold
            case .secondary, .raised: .medium
            }
        }

        private var foreground: AnyShapeStyle {
            switch kind {
            case .primary: AnyShapeStyle(colors.page)
            case .waiting: AnyShapeStyle(colors.onWaitingFill)
            case .secondary, .raised: AnyShapeStyle(.primary)
            }
        }

        private var background: AnyShapeStyle {
            switch kind {
            case .primary: AnyShapeStyle(Color.primary)
            case .secondary: AnyShapeStyle(colors.card)
            case .raised: AnyShapeStyle(colors.raised)
            case .waiting: AnyShapeStyle(colors.waitingFill)
            }
        }
    }

    /// 按钮套上 `actionSurface` 的面，按下时变淡。
    struct SurfaceButtonStyle: ButtonStyle {
        let kind: ActionKind
        var height: CGFloat = 44
        var cornerRadius: CGFloat = CornerRadius.inner

        init(_ kind: ActionKind, height: CGFloat = 44, cornerRadius: CGFloat = CornerRadius.inner) {
            self.kind = kind
            self.height = height
            self.cornerRadius = cornerRadius
        }

        func makeBody(configuration: Configuration) -> some View {
            configuration.label
                .actionSurface(kind, height: height, cornerRadius: cornerRadius)
                .opacity(configuration.isPressed ? 0.7 : 1)
        }
    }

    /// 滚动视图（`List`、`Form`）去掉系统的底色，铺 `ThemeColors.page`。
    private struct PageBackground: ViewModifier {
        @Environment(\.themeColors) private var colors

        func body(content: Content) -> some View {
            content
                .scrollContentBackground(.hidden)
                .background(colors.page)
        }
    }

    private struct ThemedRows: ViewModifier {
        @Environment(\.themeColors) private var colors

        func body(content: Content) -> some View {
            content.listRowBackground(colors.card)
        }
    }

    /// `cardList` 里一组卡片上面的标题。
    struct ListSectionHeader: View {
        let title: String
        var systemImage: String?
        var tint: Color = .secondary

        var body: some View {
            HStack(spacing: 6) {
                if let systemImage {
                    Image(systemName: systemImage)
                        .accessibilityHidden(true)
                }
                Text(title)
            }
            .font(.footnote.weight(.semibold))
            .foregroundStyle(tint)
            .padding(.leading, 4)
            .padding(.top, 12)
            .accessibilityAddTraits(.isHeader)
            .plainListRow()
        }
    }

    /// `cardList` 里一组卡片下面的说明小字。
    struct ListSectionFooter: View {
        let text: String

        var body: some View {
            Text(text)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 4)
                .padding(.bottom, 8)
                .plainListRow()
        }
    }

    extension View {
        /// 标题放在导航栏左边（下面可以再带一行小字，前面可以带应用的标志），不随内容滚动变大变小，
        /// 也不居中。`navigationTitle` 照样设上，下一页的返回按钮用得到，只是不显示在导航栏中间。
        /// 副标题放不下时按 `subtitleTruncation` 截：默认截掉开头，副标题多是目录，末尾那段最要紧。
        func leadingNavigationTitle(
            _ title: String, subtitle: String? = nil, subtitleTruncation: Text.TruncationMode = .head,
            showsMark: Bool = false
        ) -> some View {
            navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar(removing: .title)
                .toolbar {
                    LeadingTitleItem(
                        title: title, subtitle: subtitle, subtitleTruncation: subtitleTruncation, showsMark: showsMark)
                }
        }
    }

    /// 应用的标志：应用图标里的提示符 `>`、光标和指示灯，不带图标的底，能放在任何底色上。坐标照抄
    /// 应用图标的几个图层。
    struct RunodeMark: View {
        var body: some View {
            Canvas { context, size in
                let scale = min(size.width, size.height) / 824
                func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
                    CGPoint(x: (x - 100) * scale, y: (y - 100) * scale)
                }
                var prompt = Path()
                prompt.move(to: point(310, 374))
                prompt.addLine(to: point(472, 512))
                prompt.addLine(to: point(310, 650))
                context.stroke(
                    prompt, with: .style(.primary),
                    style: StrokeStyle(lineWidth: 70 * scale, lineCap: .round, lineJoin: .round))
                let cursor = CGRect(origin: point(548, 618), size: CGSize(width: 194 * scale, height: 66 * scale))
                context.fill(Path(roundedRect: cursor, cornerRadius: 33 * scale), with: .color(Self.mint))
                let led = CGRect(origin: point(734 - 28, 300 - 28), size: CGSize(width: 56 * scale, height: 56 * scale))
                context.fill(Path(ellipseIn: led), with: .color(Self.green))
            }
            .aspectRatio(1, contentMode: .fit)
            .accessibilityHidden(true)
        }

        private static let mint = Color(red: 0x51 / 255, green: 0xCD / 255, blue: 0xB9 / 255)
        private static let green = Color(red: 0x2C / 255, green: 0xB4 / 255, blue: 0x83 / 255)
    }

    private struct LeadingTitleItem: ToolbarContent {
        let title: String
        let subtitle: String?
        let subtitleTruncation: Text.TruncationMode
        let showsMark: Bool

        /// 375 点宽的手机上，返回按钮和右边两个按钮之外剩下的宽度。
        private static let maxWidth: CGFloat = 180

        var body: some ToolbarContent {
            // iOS 26 起导航栏上的按钮都有一块玻璃底，标题不要。
            if #available(iOS 26, *) {
                item.sharedBackgroundVisibility(.hidden)
            } else {
                item
            }
        }

        private var item: some ToolbarContent {
            ToolbarItem(placement: .topBarLeading) {
                HStack(spacing: 8) {
                    if showsMark {
                        RunodeMark().frame(width: 28, height: 28)
                    }
                    VStack(alignment: .leading, spacing: 0) {
                        Text(title)
                            .font(.headline)
                            .lineLimit(1)
                        if let subtitle {
                            Text(subtitle)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .truncationMode(subtitleTruncation)
                        }
                    }
                }
                .dynamicTypeSize(...DynamicTypeSize.xxLarge)
                // 宽过导航栏的项会被系统整个藏掉，连带挤走右边的按钮，所以限宽，长了截断。
                .frame(idealWidth: Self.maxWidth, maxWidth: Self.maxWidth, alignment: .leading)
                // 不固定尺寸时导航栏按占位的宽度排它，字会被截掉。
                .fixedSize()
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
            }
        }
    }

    /// 整张卡片是一个按钮：按下时卡片变暗一点，和系统列表行的高亮一样。
    struct CardButtonStyle: ButtonStyle {
        var padding: CGFloat = 12
        @Environment(\.themeColors) private var colors

        func makeBody(configuration: Configuration) -> some View {
            configuration.label
                .contentShape(Rectangle())
                .cardBackground(padding: padding)
                .overlay {
                    if configuration.isPressed {
                        RoundedRectangle.card.fill(colors.pressed)
                    }
                }
                .contentShape(.card)
        }
    }

    /// 卡片右边表示能点进去的小箭头。
    struct DisclosureChevron: View {
        var body: some View {
            Image(systemName: "chevron.right")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(.tertiary)
                .accessibilityHidden(true)
        }
    }

    /// 写着 agent 等了、干了多久的视图按 `AgentClock` 重画；空闲或没有时刻时不重画。
    struct AgentClockSchedule: TimelineSchedule {
        let agent: Agent?
        /// `SessionMeta.agentSince`。
        let since: Date?

        init(agent: Agent?, since: Date?) {
            self.agent = agent
            self.since = since
        }

        init(_ session: SessionInfo) {
            self.init(agent: session.meta.agent, since: session.meta.agentSince)
        }

        func entries(from startDate: Date, mode: TimelineScheduleMode) -> AnyIterator<Date> {
            guard let since, agent?.state == .blocked || agent?.state == .working else {
                return AnyIterator([startDate].makeIterator())
            }
            return AgentClock.ticks(since: since, from: startDate)
        }
    }

    /// 屏幕最后几行的预览：等宽小字，凹下去的底槽，每行单独截断（长行折下去会把别的行挤掉）。
    struct ScreenPreview: View {
        /// 字的深浅：一般是次要色；等你回答的卡片上要读的是 agent 的提问，用 `primary`。
        enum Tone {
            case primary, secondary
        }

        let lines: [String]
        var tone: Tone = .secondary
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        @Environment(\.themeColors) private var colors

        var body: some View {
            VStack(alignment: .leading, spacing: 3) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    text(line)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .foregroundStyle(tone == .primary ? AnyShapeStyle(.primary.opacity(0.85)) : AnyShapeStyle(.secondary))
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 9)
            .padding(.horizontal, 12)
            .background(colors.well, in: .inner)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("屏幕预览：\(lines.joined(separator: "，"))")
        }

        /// 预览的字号：等宽小字，跟着动态字体走。
        private var size: CGFloat {
            let traits = UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(dynamicTypeSize))
            return UIFont.preferredFont(forTextStyle: .caption1, compatibleWith: traits).pointSize
        }

        /// 一行预览：私用区的字（提示符里的 Powerline、Nerd Font 图标）那几段用随包的符号字体，其余用
        /// 等宽系统字体。SwiftUI 的 `Font` 不带 Core Text 的后备列表，只能这样分段指定。
        private func text(_ line: String) -> Text {
            let size = size
            var result = AttributedString()
            var run = ""
            var runIsSymbol = false
            func flush() {
                guard !run.isEmpty else { return }
                var piece = AttributedString(run)
                if runIsSymbol, let name = SymbolFont.postScriptName {
                    piece.font = .custom(name, fixedSize: size)
                } else {
                    piece.font = .system(size: size, design: .monospaced)
                }
                result += piece
                run = ""
            }
            for character in line {
                let symbol = character.unicodeScalars.first.map(SymbolFont.isPrivateUse) ?? false
                if symbol != runIsSymbol {
                    flush()
                    runIsSymbol = symbol
                }
                run += TextPresentation.apply(to: String(character))
            }
            flush()
            return Text(result)
        }
    }

    /// 快速回复栏：一排常用按键，加一个单行文本框（发送时粘贴进去再按回车）。首页和会话列表上等回答的
    /// 会话用它。
    struct QuickReplyBar: View {
        @Bindable var model: QuickReplyModel
        /// 屏幕上认出来的提问：有时按键换成它的选项，没有时是 `QuickKey.standard` 那一排。
        var prompt: AgentPrompt?
        var optionLayout: PromptOptionLayout = .rows
        /// 为假时只有按键那一排，不带输入框。
        var showsDraft = true
        /// 栏两边到容器边缘留的空（列表行的左右边距）。按键那一排横着滚时伸进这段空里，
        /// 滚出去的键在边缘淡出，不会被生硬地切掉。
        var edgeInset: CGFloat = 16
        @FocusState private var draftFocused: Bool
        @Environment(\.themeColors) private var colors

        var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                Group {
                    if let prompt {
                        PromptOptions(prompt: prompt, layout: optionLayout) { key in
                            Task { await model.press(key) }
                        }
                    } else {
                        keys
                    }
                    if showsDraft {
                        draftField
                    }
                }
                .disabled(!model.isConnected)
                if let error = model.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(colors.failure)
                }
            }
            .sensoryFeedback(.success, trigger: model.deliveredCount)
            .sensoryFeedback(.error, trigger: model.failedCount)
        }

        private var keys: some View {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(QuickKey.standard) { key in
                        Button {
                            Task { await model.press(key) }
                        } label: {
                            Group {
                                if let symbol = key.symbol {
                                    Image(systemName: symbol)
                                } else {
                                    Text(key.label)
                                }
                            }
                                .font(.callout.monospaced().weight(.medium))
                                .lineLimit(1)
                                .padding(.horizontal, 10)
                                .frame(minWidth: 44)
                        }
                        .buttonStyle(SurfaceButtonStyle(.raised))
                        .accessibilityLabel("发送 \(key.accessibilityLabel)")
                    }
                }
            }
            .contentMargins(.horizontal, edgeInset, for: .scrollContent)
            .padding(.horizontal, -edgeInset)
            .mask {
                HStack(spacing: 0) {
                    LinearGradient(colors: [.clear, .black], startPoint: .leading, endPoint: .trailing)
                        .frame(width: edgeInset)
                    Color.black
                    LinearGradient(colors: [.black, .clear], startPoint: .leading, endPoint: .trailing)
                        .frame(width: edgeInset)
                }
                .padding(.horizontal, -edgeInset)
            }
        }

        /// 输入框，发送按钮在框里的右端：点按的范围比圆底大一圈，多出来的伸进框的右边距里。
        private var draftField: some View {
            HStack(spacing: 4) {
                TextField("或者直接打字回复", text: $model.draft)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.send)
                    .focused($draftFocused)
                    .onSubmit { Task { await model.sendDraft() } }
                Button {
                    Task { await model.sendDraft() }
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(model.canSendDraft ? HierarchicalShapeStyle.primary : .tertiary)
                        .frame(width: 34, height: 34)
                        .background(colors.strongHairline, in: .circle)
                        .frame(width: 44, height: 44)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(!model.canSendDraft)
                .accessibilityLabel("发送回复")
            }
            .padding(.leading, 14)
            .padding(.trailing, 1)
            .frame(minHeight: 46)
            .background(colors.well, in: .inner)
            .overlay(RoundedRectangle.inner.strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
            .contentShape(.inner)
            .onTapGesture { draftFocused = true }
        }
    }

    /// 连接状态的文字：只有等着重连时带倒计时，要按秒刷新；别的状态下文字不随时间变，不必每秒重画。
    struct LinkClock<Content: View>: View {
        let state: LinkState
        @ViewBuilder let content: (Date) -> Content

        var body: some View {
            if case .waiting = state {
                TimelineView(.periodic(from: .now, by: 1)) { context in content(context.date) }
            } else {
                content(.now)
            }
        }
    }

    /// 连接状态：图标、文字（重连时带倒计时），断开时有「重试」。
    struct ConnectionStatusRow: View {
        let state: LinkState
        let onRetry: () -> Void
        @Environment(\.themeColors) private var colors

        var body: some View {
            HStack(spacing: 10) {
                switch state {
                case .connecting, .idle:
                    ProgressView()
                case .waiting:
                    Image(systemName: "wifi.exclamationmark").foregroundStyle(colors.waiting)
                case .failed:
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(colors.failure)
                case .connected:
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(colors.success)
                }
                LinkClock(state: state) { now in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(Presentation.linkStatus(state, now: now))
                            .font(.subheadline.weight(.semibold))
                        if case .failed = state {
                            Text(Presentation.linkState(state))
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        } else if case .waiting(let reason, _, _) = state {
                            Text(reason)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                Spacer(minLength: 8)
                switch state {
                case .waiting, .failed:
                    Button(action: onRetry) {
                        Text("重试")
                            .padding(.horizontal, 16)
                    }
                    .buttonStyle(SurfaceButtonStyle(.raised))
                default:
                    EmptyView()
                }
            }
            .accessibilityElement(children: .contain)
        }
    }

    /// 新开终端时开什么的菜单项：普通终端，和电脑上装着的各家 agent（`agents`），前面是它的 logo。
    /// `action` 收到的为空时是普通终端。
    struct SpawnKindItems: View {
        let agents: [InstalledAgent]
        let action: (InstalledAgent?) -> Void

        var body: some View {
            Button("终端", systemImage: "terminal") { action(nil) }
            ForEach(agents) { agent in
                Button {
                    action(agent)
                } label: {
                    AgentMenuLabel(title: agent.label, kind: agent.kind)
                }
            }
        }
    }

    /// 菜单里一家 agent：名字前面是它的 logo，没收 logo 的用通用的符号。
    struct AgentMenuLabel: View {
        let title: String
        let kind: AgentKind

        var body: some View {
            Label {
                Text(verbatim: title)
            } icon: {
                if let asset = Presentation.agentLogoAsset(kind) {
                    Image(asset, bundle: .module).renderingMode(.template)
                } else {
                    Image(systemName: "sparkles")
                }
            }
        }
    }

    /// 会话卡片上说状态的一行（`Presentation.sessionSubtitle`）：会话里有收了 logo 的 agent 时，agent 的名字换成
    /// 它的 logo 画在最前面；读屏照旧念带名字的整句。`machine` 嵌进等你回答的那句，`trailingMachine` 用「，」
    /// 接在整行末尾，一行放不下时先截掉它。字体、颜色和行数由调用方设。
    struct SessionSubtitleLine: View {
        let session: SessionInfo
        var machine: String?
        var trailingMachine: String?
        let now: Date
        @ScaledMetric(relativeTo: .footnote) private var logoSize: CGFloat = 15

        var body: some View {
            let named = sentence(Presentation.sessionSubtitle(session, machine: machine, now: now))
            if let kind = session.meta.agent?.kind, let asset = Presentation.agentLogoAsset(kind) {
                HStack(spacing: 6) {
                    // 单色的 logo 按资源目录里的设置跟着前景色，比旁边次要色的字亮一档。
                    Image(asset, bundle: .module)
                        .resizable()
                        .scaledToFit()
                        .frame(width: logoSize, height: logoSize)
                        .foregroundStyle(Color.primary)
                        .accessibilityHidden(true)
                    Text(sentence(Presentation.sessionSubtitle(session, machine: machine, now: now, namesAgent: false)))
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(Text(named))
            } else {
                Text(named)
            }
        }

        private func sentence(_ status: String) -> String {
            guard let trailingMachine else { return status }
            return String(localized: "\(status)，\(trailingMachine)")
        }
    }
#endif
