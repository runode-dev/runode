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

        static let system = ThemeColors(
            page: Color(.systemGroupedBackground), card: Color(.secondarySystemGroupedBackground),
            fill: Color(.tertiarySystemFill), secondaryFill: Color(.secondarySystemFill), pressed: Color(.systemFill))
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
        }
    }

    extension EnvironmentValues {
        @Entry var themeColors = ThemeColors.system
    }

    extension View {
        /// 套用主题：底色放进环境，深色主题用深色模式（字、系统控件、键盘跟着变），浅色主题用浅色模式；
        /// 按钮、链接这些强调色用终端的前景色，和桌面一样黑白为主，不用系统的蓝色。放在根视图上，弹出的
        /// 页面也跟着。
        func appTheme(_ theme: AppTheme) -> some View {
            environment(\.themeColors, ThemeColors(theme))
                .tint(Color(theme.foreground))
                .preferredColorScheme(theme.isDark ? .dark : .light)
        }

        /// 实心的主按钮：底是强调色，字用 `systemBackground`，和首页的「配对电脑」一样。系统的
        /// `borderedProminent` 字固定是白的，强调色是主题的前景色，深色主题下就成了白底白字。
        func prominentButtonStyle() -> some View {
            buttonStyle(.borderedProminent).foregroundStyle(Color(.systemBackground))
        }
    }

    /// 界面上的两档圆角：卡片（列表里的一块、首页的统计格、终端页的横幅）和卡片里的小块（按键、输入框、
    /// 预览、图标底）。系统分组列表自带的圆角太大，还会按它裁掉行里的内容，所以列表一律用 `cardList`，
    /// 卡片自己画，见 `cardBackground`。
    enum CornerRadius {
        static let card: CGFloat = 14
        static let inner: CGFloat = 8
    }

    extension Shape where Self == RoundedRectangle {
        static var card: RoundedRectangle { RoundedRectangle(cornerRadius: CornerRadius.card, style: .continuous) }
        static var inner: RoundedRectangle { RoundedRectangle(cornerRadius: CornerRadius.inner, style: .continuous) }
    }

    extension View {
        /// 画成一张卡片：四周留边，`ThemeColors.card` 的底色，`CornerRadius.card` 的圆角。
        func cardBackground() -> some View {
            modifier(CardBackground())
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
    }

    private struct CardBackground: ViewModifier {
        @Environment(\.themeColors) private var colors

        func body(content: Content) -> some View {
            content
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(colors.card, in: .card)
                .contentShape(.contextMenuPreview, .card)
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

    /// `cardList` 里一组卡片上面的标题，和卡片里的字对齐。
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
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(tint)
            .padding(.leading, 16)
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
                .padding(.horizontal, 16)
                .padding(.bottom, 8)
                .plainListRow()
        }
    }

    extension View {
        /// 标题放在导航栏左边（下面可以再带一行小字，前面可以带应用的标志），不随内容滚动变大变小，
        /// 也不居中。`navigationTitle` 照样设上，下一页的返回按钮用得到，只是不显示在导航栏中间。
        func leadingNavigationTitle(_ title: String, subtitle: String? = nil, showsMark: Bool = false) -> some View {
            navigationTitle(title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar(removing: .title)
                .toolbar { LeadingTitleItem(title: title, subtitle: subtitle, showsMark: showsMark) }
        }
    }

    /// 应用的标志：应用图标里的提示符 `>`、光标和指示灯，不带图标的底，能放在任何底色上。坐标照抄
    /// 图标的几个图层（824 见方，原点在 100,100）。
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
        let showsMark: Bool

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
                        RunodeMark().frame(width: 22, height: 22)
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
                        }
                    }
                }
                .dynamicTypeSize(...DynamicTypeSize.xxLarge)
                // 不固定尺寸时导航栏按占位的宽度排它，字会被截掉。
                .fixedSize()
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
            }
        }
    }

    /// 整张卡片是一个按钮：按下时卡片变暗一点，和系统列表行的高亮一样。
    struct CardButtonStyle: ButtonStyle {
        @Environment(\.themeColors) private var colors

        func makeBody(configuration: Configuration) -> some View {
            configuration.label
                .contentShape(Rectangle())
                .cardBackground()
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

    /// agent 的状态：图标加文字，不只靠颜色区分。放在已经按状态分了组的地方（分组标题写着「等你回答」）
    /// 时 `showsState` 给假，只写 agent 的名字，图标和颜色照旧。
    struct AgentBadge: View {
        let agent: Agent?
        var showsState = true
        @Environment(\.accessibilityReduceMotion) private var reduceMotion

        var body: some View {
            if let agent, let status = Presentation.agentStatus(agent) {
                let group: SessionGroup =
                    switch status.state {
                    case .blocked: .waiting
                    case .working: .working
                    default: .other
                    }
                let tint = Self.tint(for: group)
                HStack(spacing: 4) {
                    icon(group)
                        .imageScale(.small)
                    Text(showsState ? status.text : agent.kind.displayName)
                        .lineLimit(1)
                }
                .font(.caption.weight(.semibold))
                .foregroundStyle(tint)
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .background(tint.opacity(0.14), in: Capsule())
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(status.text)
            }
        }

        /// 状态图标带着动：干活时齿轮转，空闲时月亮慢慢呼吸，等回答时不动（卡片已经描了橙边）。
        /// 打开了「减弱动态效果」时都不动。
        @ViewBuilder
        private func icon(_ group: SessionGroup) -> some View {
            switch group {
            case .working:
                Image(systemName: Presentation.symbol(for: .working))
                    .symbolEffect(.rotate.byLayer, isActive: !reduceMotion)
            case .other:
                Image(systemName: "moon.zzz.fill")
                    .symbolEffect(.breathe, isActive: !reduceMotion)
            case .waiting:
                Image(systemName: Presentation.symbol(for: .waiting))
            }
        }

        static func tint(for group: SessionGroup) -> Color {
            switch group {
            case .waiting: .orange
            case .working: .primary
            case .other: .secondary
            }
        }
    }

    /// 会话标题前的图标，和桌面卡片样式下标签前的那块一样，边长 `size`：agent 是浅底上的 logo（和桌面
    /// 用的同一套，有品牌色的按原色画，单色的染成前景色，没收 logo 的写名字的头一个字母）；shell、别的
    /// 程序和认不出是哪个的 agent（`other`）是深底上的提示符，像个小终端。
    struct SessionIcon: View {
        let agent: AgentKind?
        let size: CGFloat
        @Environment(\.colorScheme) private var colorScheme
        @Environment(\.themeColors) private var colors

        var body: some View {
            let logoTile = colorScheme == .light ? Color.white : colors.secondaryFill
            Group {
                if let agent, let asset = Presentation.agentLogoAsset(agent) {
                    tile(logoTile) {
                        Image(asset, bundle: .module)
                            .resizable()
                            .scaledToFit()
                            .frame(width: size * 0.66, height: size * 0.66)
                            .foregroundStyle(.primary.opacity(0.85))
                    }
                } else if let agent, agent.label != "other", let initial = agent.displayName.first {
                    tile(logoTile) {
                        Text(String(initial))
                            .font(.system(size: size * 0.55, weight: .bold))
                            .foregroundStyle(.primary.opacity(0.85))
                    }
                } else {
                    // 和桌面上提示符那块一样的颜色，深浅主题下都不变。
                    tile(Color(Rgb(hex: 0x232326))) {
                        Image("prompt", bundle: .module)
                            .resizable()
                            .scaledToFit()
                            .frame(width: size * 0.6, height: size * 0.6)
                            .foregroundStyle(Color(Rgb(hex: 0xEDEDED)))
                    }
                }
            }
            .accessibilityHidden(true)
        }

        private func tile(_ background: Color, @ViewBuilder content: () -> some View) -> some View {
            let shape = RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
            return content()
                .frame(width: size, height: size)
                .background(background, in: shape)
                .overlay(shape.strokeBorder(.primary.opacity(0.15), lineWidth: 1))
        }
    }

    /// 小图标加一行字，图标和字挨得紧（系统的 `Label` 在列表里留的空太大）。
    struct CompactLabel: View {
        let text: String
        let systemImage: String

        var body: some View {
            HStack(alignment: .firstTextBaseline, spacing: 5) {
                Image(systemName: systemImage)
                    .imageScale(.small)
                    .accessibilityHidden(true)
                Text(text)
            }
        }
    }

    /// 屏幕最后几行的预览：等宽小字，浅底圆角，每行单独截断（长行折下去会把别的行挤掉）。
    struct ScreenPreview: View {
        let lines: [String]
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        @Environment(\.themeColors) private var colors

        var body: some View {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    text(line)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(colors.secondaryFill, in: .inner)
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
                run.append(character)
            }
            flush()
            return Text(result)
        }
    }

    /// 快速回复栏：一排常用按键，加一个单行文本框（发送时粘贴进去再按回车）。会话列表上等回答的
    /// 会话、终端页底部都用它。
    struct QuickReplyBar: View {
        @Bindable var model: QuickReplyModel
        /// 栏的标题，比如「Claude Code 在等你回答」；为空时不显示。
        var prompt: String?
        /// 栏两边到容器边缘留的空（列表行、终端页底栏的左右边距）。按键那一排横着滚时伸进这段空里，
        /// 滚出去的键在边缘淡出，不会被生硬地切掉。
        var edgeInset: CGFloat = 16
        @FocusState private var draftFocused: Bool
        @Environment(\.themeColors) private var colors

        var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                if let prompt {
                    Label(prompt, systemImage: "exclamationmark.bubble.fill")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.orange)
                }
                keys
                draftField
                if let error = model.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(.red)
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
                                .font(.callout.monospaced().weight(.semibold))
                                .lineLimit(1)
                                .padding(.horizontal, 10)
                                .frame(minWidth: 44, minHeight: 40)
                        }
                        .buttonStyle(QuickKeyStyle())
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

        /// 输入框，发送按钮在框里的右端。
        private var draftField: some View {
            HStack(spacing: 4) {
                TextField("输入回复，发送时带回车", text: $model.draft)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.send)
                    .focused($draftFocused)
                    .onSubmit { Task { await model.sendDraft() } }
                    .padding(.leading, 14)
                    .frame(minHeight: 40)
                Button {
                    Task { await model.sendDraft() }
                } label: {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.title2)
                        .frame(width: 44, height: 40)
                }
                .buttonStyle(.borderless)
                .disabled(!model.canSendDraft)
                .accessibilityLabel("发送回复")
            }
            .background(colors.fill, in: .inner)
            .contentShape(.inner)
            .onTapGesture { draftFocused = true }
        }
    }

    /// 快速回复的一个键：浅底圆角块，字用强调色，按下时变暗。比 `.bordered` 矮，一排能多放几个。
    private struct QuickKeyStyle: ButtonStyle {
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.themeColors) private var colors

        func makeBody(configuration: Configuration) -> some View {
            configuration.label
                .foregroundStyle(isEnabled ? AnyShapeStyle(.tint) : AnyShapeStyle(.tertiary))
                .background(colors.fill, in: .inner)
                .contentShape(.inner)
                .opacity(configuration.isPressed ? 0.55 : 1)
                .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
        }
    }

    /// 连接状态：图标、文字（重连时带倒计时），断开时有「重试」。
    struct ConnectionStatusRow: View {
        let state: LinkState
        let onRetry: () -> Void

        var body: some View {
            HStack(spacing: 10) {
                switch state {
                case .connecting, .idle:
                    ProgressView()
                case .waiting:
                    Image(systemName: "wifi.exclamationmark").foregroundStyle(.orange)
                case .failed:
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                case .connected:
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                }
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(Presentation.linkStatus(state, now: context.date))
                            .font(.subheadline.weight(.semibold))
                        if case .failed = state {
                            Text(Presentation.linkState(state))
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        } else if case .waiting(let reason, _) = state {
                            Text(reason)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                Spacer(minLength: 8)
                switch state {
                case .waiting, .failed:
                    Button("重试", action: onRetry)
                        .buttonStyle(.bordered)
                        .frame(minHeight: 44)
                default:
                    EmptyView()
                }
            }
            .accessibilityElement(children: .contain)
        }
    }
#endif
