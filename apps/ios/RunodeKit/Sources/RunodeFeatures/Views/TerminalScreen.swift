#if os(iOS)
    import PhotosUI
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI

    /// 终端页：整页铺终端的背景色，标题下面一行 agent 和连接的状态。断线时终端上方叠一条带「重试」的
    /// 横幅；agent 等回答时底部出现快速回复栏；软键盘没弹出时底部常驻一条按键栏，弹出后由键盘上方的
    /// 辅助栏接手。电脑上这个终端所在的标签分了屏时，导航栏下面一排标签切换各个分屏。按键栏上的麦克风键
    /// 语音输入，录音、识别时终端底部正中有提示；图片键挑照片、视频发给终端（剪贴板上只有图片时「粘贴」发那
    /// 张），压视频、发送时底部也有提示。
    struct TerminalScreen: View {
        @Bindable var model: TerminalModel
        /// 设置里的字号和响铃震动。
        let preferences: AppPreferences
        /// 打开这个会话所在仓库的 Git 页。
        var onOpenGit: () -> Void = {}
        /// 切到同一个标签里的另一个分屏。
        var onSelectPane: (SessionId) -> Void = { _ in }
        /// 显示的分屏结束了，带着它所在标签里的分屏：切回别的分屏。
        var onPaneEnded: ([SessionId]) -> Void = { _ in }
        /// 这台电脑的会话列表，「⋯」菜单里的命令从它取、经它跑。
        var sessions: SessionListModel?
        /// 语音输入；为空时麦克风键不起作用。
        var dictation: DictationModel?
        /// 往终端里发图片、视频；为空时图片键不起作用。
        var mediaSend: MediaSendModel?
        /// 这个终端所在的电脑，交给电脑识别、发图片和视频时连它。
        var machine: MachineRecord?
        /// 打开 app 的设置页，语音输入缺模型时从弹框过去下载。
        var onOpenSettings: () -> Void = {}
        @Environment(\.openURL) private var openURL
        /// 外层套的 app 主题，导航栏、分屏标签和底栏按它上色。
        @Environment(\.appTheme) private var appTheme
        /// 断线横幅占的高度，终端视图据此在顶上让出地方，横幅不挡内容。
        @State private var bannerHeight: CGFloat = 0
        /// 嵌着的终端视图，底部的按键栏按了发给它。
        @State private var terminalView: TerminalView?
        /// 连了一会儿还没连上：这时才出「正在连接…」横幅，切分屏、回到前台时一闪而过的连接不出。
        @State private var connectingLong = false
        /// 最后一次知道的、这个会话所在标签里的分屏。会话结束后电脑关掉它的分屏，布局里就找不到它了，
        /// 所以先记着；换到别的标签的会话后，不含它的旧值不算数。
        @State private var tabMates: [SessionId] = []
        /// 挑图片、视频的界面开着。
        @State private var pickingMedia = false
        @State private var pickedMedia: [PhotosPickerItem] = []
        /// 挑好的发到哪里，按下图片键时定下：挑的时候切了分屏也发给原来的会话。
        @State private var mediaTarget: MediaTarget?
        @ScaledMetric(relativeTo: .headline) private var titleGlyphSize: CGFloat = 15

        private var background: Color { Color(model.background) }
        /// 叠在终端上的横幅、提示、按钮用的深浅模式，跟着终端的背景；它们的强调色是终端的前景色。
        private var scheme: ColorScheme { model.background.isDark ? .dark : .light }
        /// 导航栏、分屏标签和底栏的主题；外层没套主题时按终端的配色。
        private var chromeTheme: AppTheme { appTheme ?? model.chromeTheme }
        private var chromeScheme: ColorScheme { chromeTheme.isDark ? .dark : .light }

        var body: some View {
            TerminalViewRepresentable(
                model: model, preferences: preferences, topObstruction: bannerMessage == nil ? 0 : bannerHeight,
                dictating: dictation?.phase == .recording, keyBarTheme: chromeTheme,
                onMake: { view in terminalView = view },
                onDictation: { press in
                    dictation?.press(press, machine: machine, source: preferences.dictationSource) { [model] text in
                        // 文字发给按下麦克风时的会话；识别完之前切了分屏的话，终端视图已经接到别的会话上。
                        if let terminalView,
                            (terminalView.delegate as? TerminalViewRepresentable.Coordinator)?.model === model
                        {
                            terminalView.pasteText(text)
                        } else {
                            model.send(.paste(text))
                        }
                    }
                },
                onRequestMedia: {
                    guard let target = currentMediaTarget() else { return }
                    mediaTarget = target
                    pickingMedia = true
                },
                onPasteImage: { data in
                    guard let target = currentMediaTarget() else { return }
                    Task { await send([.image(data)], to: target) }
                })
                .photosPicker(
                    isPresented: $pickingMedia, selection: $pickedMedia, maxSelectionCount: 5,
                    selectionBehavior: .ordered, matching: .any(of: [.images, .videos])
                )
                .onChange(of: pickedMedia) { _, items in
                    guard !items.isEmpty else { return }
                    pickedMedia = []
                    guard let target = mediaTarget else { return }
                    mediaTarget = nil
                    Task { await send(items, to: target) }
                }
                .overlay(alignment: .top) { banner }
                // 印章的出现要由外层驱动；动画只挂在这层 ZStack 上，不让终端跟着按键栏收起播改尺寸的动画。
                .overlay { ZStack { endedStamp }.animation(.spring(duration: 0.3), value: model.isEnded) }
                .overlay(alignment: .bottom) {
                    VStack(spacing: 6) {
                        mediaBadge
                        dictationBadge
                    }
                }
                .overlay(alignment: .bottomTrailing) {
                    if model.scrolledBack {
                        Button("回到最新", systemImage: "arrow.down.to.line") { model.scrollToBottom() }
                            .prominentButtonStyle()
                            .controlSize(.large)
                            .tint(Color(model.foreground))
                            .environment(\.colorScheme, scheme)
                            .padding()
                            .transition(.opacity)
                    }
                }
                .safeAreaInset(edge: .top, spacing: 0) { paneTabs }
                .safeAreaInset(edge: .bottom, spacing: 0) { bottomBars }
                // 终端底色不铺进键盘让出的区域：软键盘浮在页面上，它圆角外露出的是那里，要和按键栏一样是页面底色。
                .background(background.ignoresSafeArea(.container))
                .background(Color(chromeTheme.page).ignoresSafeArea())
                .animation(.easeOut(duration: 0.2), value: model.isAwaitingAnswer)
                .navigationTitle(model.title)
                .navigationBarTitleDisplayMode(.inline)
                .toolbarBackground(Color(chromeTheme.page), for: .navigationBar)
                .toolbarBackground(.visible, for: .navigationBar)
                .toolbarColorScheme(chromeScheme, for: .navigationBar)
                .toolbar {
                    TitleItem { titleView }
                    ToolbarItem(placement: .primaryAction) { moreMenu }
                }
                // 之后电脑在布局变了时推 `LayoutChanged`，列表收到就重新要。
                .task { sessions?.refreshLayout() }
                .task(id: model.phase == .connecting) {
                    connectingLong = false
                    guard model.phase == .connecting else { return }
                    try? await Task.sleep(for: .milliseconds(600))
                    if !Task.isCancelled { connectingLong = true }
                }
                .task(id: session?.meta.cwd) {
                    if let cwd = session?.meta.cwd { await sessions?.loadTasks(in: cwd) }
                }
                .onChange(of: paneIds, initial: true) { _, ids in
                    if !ids.isEmpty { tabMates = ids }
                }
                .onChange(of: model.isEnded) { _, ended in
                    if ended, tabMates.contains(model.sessionId) { onPaneEnded(tabMates) }
                }
                .sensoryFeedback(.start, trigger: dictation?.phase == .recording) { _, recording in recording }
                .onDisappear { dictation?.cancel() }
                .alert(
                    "语音输入没有成功",
                    isPresented: Binding(
                        get: { dictation?.errorMessage != nil }, set: { if !$0 { dictation?.errorMessage = nil } })
                ) {
                    switch dictation?.remedy {
                    case .downloadModel:
                        Button("去下载", action: onOpenSettings)
                        Button("取消", role: .cancel) {}
                    case .microphonePermission:
                        Button("打开系统设置") {
                            if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                        }
                        Button("取消", role: .cancel) {}
                    case nil:
                        Button("好") {}
                    }
                } message: {
                    Text(dictation?.errorMessage ?? "")
                }
                .alert(
                    "发送失败",
                    isPresented: Binding(
                        get: { mediaSend?.errorMessage != nil }, set: { if !$0 { mediaSend?.errorMessage = nil } })
                ) {
                    Button("好") {}
                } message: {
                    Text(mediaSend?.errorMessage ?? "")
                }
                .environment(\.themeColors, chrome)
        }

        private var paneIds: [SessionId] { tabPanes.map(\.id) }

        private var tabPanes: [SplitPane] { sessions?.panes(sharingTabWith: model.sessionId) ?? [] }

        /// 电脑上同一个标签里分了屏时的分屏标签；没分屏时不出现。
        @ViewBuilder
        private var paneTabs: some View {
            if !tabPanes.isEmpty {
                PaneTabBar(panes: tabPanes, current: model.sessionId, onSelect: onSelectPane)
                    .background(chrome.page)
                    .overlay(alignment: .bottom) { chrome.hairline.frame(height: 1) }
                    .environment(\.colorScheme, chromeScheme)
            }
        }

        /// 导航栏、分屏标签和底栏的配色，整页放进环境；这一页自己画底栏时直接取。
        private var chrome: ThemeColors { ThemeColors(chromeTheme) }

        /// 分屏时标题是工作区的名字，当前会话由下面的分屏标签标出；没分屏时是会话名，前面带它的状态。
        private var titleView: some View {
            LinkClock(state: model.linkState) { now in
                VStack(spacing: 0) {
                    Group {
                        if tabPanes.isEmpty {
                            HStack(spacing: 6) {
                                StatusGlyph(model.agent.map { StatusMark($0.state) } ?? .idle, size: titleGlyphSize)
                                Text(model.title)
                            }
                        } else {
                            Text(workspaceTitle)
                        }
                    }
                    .font(.headline)
                    .lineLimit(1)
                    Text(
                        Presentation.terminalSubtitle(
                            machine: machine?.name, link: model.linkState, phase: model.phase, now: now)
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                }
                .multilineTextAlignment(.center)
                .dynamicTypeSize(...DynamicTypeSize.xxLarge)
                .environment(\.colorScheme, chromeScheme)
                .accessibilityElement(children: .combine)
                .accessibilityAddTraits(.isHeader)
            }
        }

        private var workspaceTitle: String {
            sessions?.sections.first { $0.sessions.contains { $0.id == model.sessionId } }
                .map(Presentation.sectionTitle) ?? model.title
        }

        /// 终端底部正中的提示：正在听、正在识别。
        @ViewBuilder
        private var dictationBadge: some View {
            if let phase = dictation?.phase, phase != .idle {
                HStack(spacing: 6) {
                    if phase == .recording {
                        Circle().fill(.red).frame(width: 8, height: 8)
                    } else {
                        ProgressView().controlSize(.mini)
                    }
                    Text(phase == .recording ? "正在听…" : "正在识别…")
                        .font(.footnote)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.regularMaterial, in: .capsule)
                .environment(\.colorScheme, scheme)
                .padding(.bottom, 8)
                .allowsHitTesting(false)
                .accessibilityElement(children: .combine)
                .transition(.opacity)
            }
        }

        /// 终端底部正中的提示：正在压、正在发一批里的第几项，发视频时带上发出去的百分比。
        @ViewBuilder
        private var mediaBadge: some View {
            if let text = mediaProgressText {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text(text)
                        .font(.footnote)
                        .monospacedDigit()
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.regularMaterial, in: .capsule)
                .environment(\.colorScheme, scheme)
                .padding(.bottom, 8)
                .allowsHitTesting(false)
                .accessibilityElement(children: .combine)
                .transition(.opacity)
            }
        }

        private var mediaProgressText: String? {
            switch mediaSend?.phase {
            case .preparing(let index, let count)?:
                String(localized: "正在压缩视频（\(index)/\(count)）…")
            case .sending(let index, let count, let fraction?)?:
                String(localized: "正在发送（\(index)/\(count)）… \(fraction.formatted(.percent.precision(.fractionLength(0))))")
            case .sending(let index, let count, nil)?:
                String(localized: "正在发送（\(index)/\(count)）…")
            case .idle?, nil:
                nil
            }
        }

        /// 现在要发到的地方：这个会话。发不了时为空，`mediaSend` 已经设好了要弹出来说的错。
        private func currentMediaTarget() -> MediaTarget? {
            guard let mediaSend, let machine, let link = sessions?.link, mediaSend.checkAvailable(link: link) else {
                return nil
            }
            return MediaTarget(model: mediaSend, machine: machine, link: link, sessionId: model.sessionId)
        }

        /// 读出挑好的照片和视频再按挑的先后发：视频拷成自己的文件，图片读出内容。读不出的跳过，发完再报。
        private func send(_ items: [PhotosPickerItem], to target: MediaTarget) async {
            var media: [MediaItem] = []
            var unreadable = 0
            for item in items {
                if item.supportedContentTypes.contains(where: { $0.conforms(to: .movie) }) {
                    if let movie = try? await item.loadTransferable(type: PickedMovie.self) {
                        media.append(.video(movie.url))
                    } else {
                        unreadable += 1
                    }
                } else if let data = try? await item.loadTransferable(type: Data.self) {
                    media.append(.image(data))
                } else {
                    unreadable += 1
                }
            }
            await send(media, to: target)
            if unreadable > 0, mediaSend?.errorMessage == nil {
                mediaSend?.errorMessage = String(localized: "有 \(unreadable) 项读不出来，没有发送")
            }
        }

        private func send(_ media: [MediaItem], to target: MediaTarget) async {
            await target.model.send(media, machine: target.machine, link: target.link, sessionId: target.sessionId)
        }

        /// 会话列表里这个终端的信息（目录、前台程序），菜单里的命令据此列出、判断能不能跑。
        private var session: SessionInfo? { sessions?.session(model.sessionId) }

        /// 「⋯」菜单里的终端尺寸子菜单，图标表示现在适配的是手机还是电脑。
        private var sizeMenu: some View {
            Menu("终端尺寸", systemImage: model.fitsPhone ? "iphone" : "laptopcomputer") {
                Picker(
                    "终端尺寸",
                    selection: Binding(get: { model.sizePreference }, set: { model.setSizePreference($0) })
                ) {
                    Label("自动", systemImage: "wand.and.stars").tag(SizePreference.automatic)
                    Label("适配手机", systemImage: "iphone").tag(SizePreference.fitPhone)
                    Label("跟随电脑", systemImage: "laptopcomputer").tag(SizePreference.followMachine)
                }
                .pickerStyle(.inline)
                Section {
                    Text(Presentation.sizeOwnership(model.sizeOwnership))
                    if let size = model.gridSize {
                        Text("网格 \(Presentation.gridSize(size))")
                    }
                }
            }
            // 会话结束了，尺寸改不了了。
            .disabled(model.isEnded)
        }

        private var moreMenu: some View {
            Menu {
                if !model.isEnded {
                    Button("键盘", systemImage: "keyboard") { model.showKeyboard() }
                }
                Button("回到最新", systemImage: "arrow.down.to.line") { model.scrollToBottom() }
                sizeMenu
                // 会话结束了宿主那边就没有它了，Git 请求只会回「no session」。
                if !model.isEnded {
                    Button("Git", systemImage: "arrow.triangle.branch") { onOpenGit() }
                }
                if let sessions, let session, !model.isEnded {
                    TasksSection(model: sessions, session: session) { model.scrollToBottom() }
                }
                if !model.isEnded {
                    Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                        model.isConfirmingKill = true
                    }
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .accessibilityLabel("终端选项")
            // 挂在菜单按钮上，确认框的气泡才指着它。
            .confirmationDialog("结束这个终端？", isPresented: $model.isConfirmingKill, titleVisibility: .visible) {
                Button("结束会话", role: .destructive) { model.kill() }
            } message: {
                Text("它在电脑上也会关掉，里面正在跑的程序跟着结束。")
            }
        }

        @ViewBuilder
        private var banner: some View {
            if let message = bannerMessage {
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    HStack(spacing: 10) {
                        Image(systemName: bannerSymbol)
                            .foregroundStyle(bannerTint)
                        Text(message(context.date))
                            .font(.subheadline)
                            .fixedSize(horizontal: false, vertical: true)
                        Spacer(minLength: 8)
                        if canRetry {
                            Button("重试") { model.reconnect() }
                                .buttonStyle(.bordered)
                                .frame(minHeight: 44)
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 6)
                    .frame(minHeight: 44)
                    .background(.regularMaterial, in: .card)
                    .tint(Color(model.foreground))
                    .environment(\.colorScheme, scheme)
                    .padding(.horizontal, 12)
                    .padding(.top, 8)
                    .accessibilityElement(children: .contain)
                }
                .onGeometryChange(for: CGFloat.self) { $0.size.height + 8 } action: { bannerHeight = $0 }
                .transition(.opacity)
            }
        }

        /// 会话结束了时斜盖在终端上的印章：大字「已结束」，下面一行小字说怎么结束的。点不到它，终端照样能滚动、
        /// 选字。
        @ViewBuilder
        private var endedStamp: some View {
            if let detail = endedDetail {
                VStack(spacing: 4) {
                    Text("已结束")
                        .font(.largeTitle.weight(.heavy))
                    Text(detail)
                        .font(.footnote.weight(.semibold))
                        .multilineTextAlignment(.center)
                        .lineLimit(2)
                }
                .foregroundStyle(.red.opacity(0.75))
                .padding(.horizontal, 20)
                .padding(.vertical, 12)
                .overlay { RoundedRectangle(cornerRadius: 10).strokeBorder(.red.opacity(0.75), lineWidth: 3) }
                .frame(maxWidth: 280)
                .rotationEffect(.degrees(-12))
                .allowsHitTesting(false)
                .accessibilityElement(children: .combine)
                .transition(.scale(scale: 1.4).combined(with: .opacity))
            }
        }

        /// 印章下面的小字；会话没结束时为空。
        private var endedDetail: String? {
            switch model.phase {
            case .exited(let status?): String(localized: "shell 已退出（退出码 \(status)）")
            case .exited(nil): String(localized: "shell 已退出")
            case .gone(let message): message
            default: nil
            }
        }

        /// 横幅上的文字；不用横幅时为空。
        private var bannerMessage: ((Date) -> String)? {
            switch model.phase {
            case .disconnected:
                let state = model.linkState
                return { now in
                    if case .failed(let failure) = state { return failure.errorDescription ?? String(localized: "连接失败") }
                    return Presentation.linkStatus(state, now: now)
                }
            case .connecting where connectingLong:
                return { _ in String(localized: "正在连接…") }
            default:
                return nil
            }
        }

        private var canRetry: Bool {
            if case .disconnected = model.phase { return true }
            return false
        }

        private var bannerSymbol: String {
            switch model.phase {
            case .disconnected: "wifi.exclamationmark"
            case .connecting: "arrow.triangle.2.circlepath"
            default: "info.circle"
            }
        }

        private var bannerTint: Color {
            if case .disconnected = model.phase { return .orange }
            return .secondary
        }

        /// 底部的回答行和按键栏；两样都没有时整条不占地方。
        @ViewBuilder
        private var bottomBars: some View {
            // 软键盘弹出时键盘上方有一样的辅助栏，这条就收起来。不加动画：和键盘让出的地方在同一次布局里
            // 换好，终端只改一次大小；渐隐的话动画的每一帧都让宿主改一次尺寸。
            let restingBar = model.keyboardVisible || model.isEnded ? nil : terminalView
            if model.isAwaitingAnswer || restingBar != nil {
                VStack(spacing: 8) {
                    if model.isAwaitingAnswer {
                        AnswerRow(
                            model: model.quickReply, prompt: sessions?.prompts[model.sessionId],
                            agentName: model.agent?.kind.displayName)
                            .padding(.horizontal, 10)
                            .transition(.move(edge: .bottom).combined(with: .opacity))
                    }
                    // 按键栏铺满宽度，左右的空由它自己留，拖动时键在边缘淡出。
                    if let restingBar {
                        RestingKeyBar(terminalView: restingBar)
                            .frame(height: 52)
                    }
                }
                .padding(.top, 8)
                .padding(.bottom, restingBar == nil ? 8 : 0)
                .background(chrome.page)
                .overlay(alignment: .top) { chrome.hairline.frame(height: 1) }
                .environment(\.colorScheme, chromeScheme)
            }
        }
    }

    /// 终端页底部回答 agent 的那一行：琥珀标记加「等你回答」，右边几个选项编号键。一行放不下时字在上、
    /// 键在下，键在一行里也放不下时横着滚。只有键，不带输入框：终端本身就能打字。
    private struct AnswerRow: View {
        @Bindable var model: QuickReplyModel
        /// 屏幕上认出来的提问：有时一个选项一个键，默认项是琥珀色；没有时是 `QuickKey.options`。
        let prompt: AgentPrompt?
        /// 在等回答的 agent 的名字，读屏时念「回复「名字」」。
        let agentName: String?
        @Environment(\.themeColors) private var colors
        @ScaledMetric(relativeTo: .footnote) private var glyphSize: CGFloat = 15

        var body: some View {
            VStack(alignment: .leading, spacing: 6) {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 4) {
                        label
                        Spacer(minLength: 8)
                        keys
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        label
                        keys
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        label
                        ScrollView(.horizontal, showsIndicators: false) { keys }
                    }
                }
                .padding(.leading, 4)
                if let error = model.errorMessage {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(colors.failure)
                }
            }
            .sensoryFeedback(.success, trigger: model.deliveredCount)
            .sensoryFeedback(.error, trigger: model.failedCount)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("回复「\(agentName ?? "Agent")」")
        }

        private var label: some View {
            HStack(spacing: 4) {
                StatusGlyph(.waiting, size: glyphSize)
                    .padding(.trailing, 3)
                Text("等你回答")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(colors.waiting)
            }
        }

        private var keys: some View {
            HStack(spacing: 4) {
                if let prompt {
                    ForEach(prompt.options) { option in
                        key(
                            option.key, kind: prompt.isDefault(option) ? .waiting : .secondary,
                            checked: option.checkbox?.checked
                        )
                        .checkboxAccessibility(option)
                        .accessibilityAddTraits(prompt.isDefault(option) ? .isSelected : [])
                    }
                } else {
                    ForEach(QuickKey.options) { key($0, kind: .secondary) }
                }
            }
            .disabled(!model.isConnected)
        }

        /// `checked` 不为空时在编号后画复选框。
        private func key(_ key: QuickKey, kind: ActionKind, checked: Bool? = nil) -> some View {
            Button {
                Task { await model.press(key) }
            } label: {
                HStack(spacing: 4) {
                    Text(verbatim: key.label)
                    if let checked {
                        OptionCheckbox(checked: checked)
                    }
                }
                .font(.subheadline.monospaced().weight(.bold))
                .frame(minWidth: 56)
            }
            .buttonStyle(SurfaceButtonStyle(kind, cornerRadius: CornerRadius.key))
            .accessibilityLabel("发送 \(key.accessibilityLabel)")
        }
    }

    /// 终端页顶上的一排分屏标签，一个分屏一个：状态标记加会话名。点了切过去；当前的那个滚到中间。
    private struct PaneTabBar: View {
        let panes: [SplitPane]
        let current: SessionId
        let onSelect: (SessionId) -> Void
        @Environment(\.themeColors) private var colors
        @ScaledMetric(relativeTo: .subheadline) private var glyphSize: CGFloat = 15

        var body: some View {
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(panes) { pane in
                            tab(pane).id(pane.id)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                    .padding(.bottom, 10)
                }
                .onAppear { proxy.scrollTo(current, anchor: .center) }
                // 切分屏时页面不重建，只换了 `current`。
                .onChange(of: current) { _, id in withAnimation { proxy.scrollTo(id, anchor: .center) } }
            }
        }

        private func tab(_ pane: SplitPane) -> some View {
            let selected = pane.id == current
            let title = Presentation.sessionTitle(pane.session)
            let meta = pane.session.meta
            let shape = RoundedRectangle(cornerRadius: 13, style: .continuous)
            return TimelineView(AgentClockSchedule(agent: meta.agent, since: meta.agentSince)) { context in
                let status = Presentation.agentStatus(meta.agent, since: meta.agentSince, now: context.date)
                Button {
                    guard !selected else { return }
                    UISelectionFeedbackGenerator().selectionChanged()
                    onSelect(pane.id)
                } label: {
                    HStack(spacing: 8) {
                        StatusGlyph(status.map { StatusMark($0.state) } ?? .idle, size: glyphSize)
                        Text(title)
                            .lineLimit(1)
                    }
                    .font(.subheadline.weight(selected ? .semibold : .regular))
                    .foregroundStyle(selected ? .primary : .secondary)
                    .opacity(pane.session.exited ? 0.5 : 1)
                    .padding(.horizontal, 14)
                    .frame(maxWidth: 220, minHeight: 44)
                    .background(selected ? colors.raised : .clear, in: shape)
                    .contentShape(shape)
                }
                .buttonStyle(.plain)
                .accessibilityElement(children: .combine)
                .accessibilityLabel(title)
                .accessibilityValue(status?.text ?? "")
                .accessibilityAddTraits(selected ? .isSelected : [])
                .accessibilityHint(selected ? "" : String(localized: "切到这个分屏"))
            }
        }
    }

    /// 图片、视频经谁发到哪台电脑的哪个会话。
    private struct MediaTarget {
        var model: MediaSendModel
        var machine: MachineRecord
        var link: any HostLink
        var sessionId: SessionId
    }

    /// 照片库里挑的一段视频，拷进了自己的临时目录：系统交来的文件在导入的闭包返回后就删了。
    private struct PickedMovie: Transferable {
        let url: URL

        static var transferRepresentation: some TransferRepresentation {
            FileRepresentation(importedContentType: .movie) { received in
                let copy = FileManager.default.temporaryDirectory.appending(
                    path: "\(UUID().uuidString).\(received.file.pathExtension)")
                try FileManager.default.copyItem(at: received.file, to: copy)
                return PickedMovie(url: copy)
            }
        }
    }

    /// 软键盘收着时底部的按键栏（`TerminalView.makeRestingKeyBar`）。
    private struct RestingKeyBar: UIViewRepresentable {
        let terminalView: TerminalView

        func makeUIView(context: Context) -> UIView {
            terminalView.makeRestingKeyBar()
        }

        func updateUIView(_ view: UIView, context: Context) {}
    }

    /// 把 UIKit 的 `TerminalView` 嵌进 SwiftUI，用户的输入转给视图模型。
    struct TerminalViewRepresentable: UIViewRepresentable {
        let model: TerminalModel
        let preferences: AppPreferences
        /// 叠在终端顶上的横幅的高度。
        var topObstruction: CGFloat = 0
        /// 正在录音，麦克风键显示成按下去的样子。
        var dictating = false
        /// 按键栏和软键盘照它配色、定深浅。
        var keyBarTheme: AppTheme
        /// 建好了终端视图，页面的按键栏要用它。
        var onMake: (TerminalView) -> Void = { _ in }
        var onDictation: (DictationPress) -> Void = { _ in }
        var onRequestMedia: () -> Void = {}
        var onPasteImage: (Data) -> Void = { _ in }

        func makeCoordinator() -> Coordinator {
            Coordinator(model: model)
        }

        func makeUIView(context: Context) -> TerminalView {
            let view = TerminalView(frame: .zero)
            view.delegate = context.coordinator
            model.attachDisplay(view)
            // 正在更新视图时不能改页面的状态，下一轮再交出去。
            Task { @MainActor in onMake(view) }
            return view
        }

        func updateUIView(_ view: TerminalView, context: Context) {
            // 切分屏、从终端页开新终端时这一页接着用，终端视图也不换，改接到新会话上：换一个视图的话
            // 键盘焦点跟着旧视图走掉，软键盘先收起再弹出来。
            if context.coordinator.model !== model {
                context.coordinator.model.detachDisplay(view)
                context.coordinator.model = model
                model.attachDisplay(view)
                // 正在更新视图时不能改模型的状态，下一轮再告诉它。
                Task { @MainActor in
                    model.setKeyboardVisible(view.isFirstResponder)
                    if view.bounds.width > 0 { model.updateFitSize(view.fitSize) }
                }
            }
            view.topObstruction = topObstruction
            view.fontSizeOverride = preferences.fontSize.map { CGFloat($0) }
            view.bellHaptics = preferences.bellHaptics
            view.acceptsInput = !model.isEnded
            view.dictating = dictating
            view.keyBarPalette = KeyBarPalette(theme: keyBarTheme)
            view.keyboardIsDark = keyBarTheme.isDark
            context.coordinator.onDictation = onDictation
            context.coordinator.onRequestMedia = onRequestMedia
            context.coordinator.onPasteImage = onPasteImage
        }

        @MainActor
        final class Coordinator: TerminalViewDelegate {
            var model: TerminalModel
            var onDictation: (DictationPress) -> Void = { _ in }
            var onRequestMedia: () -> Void = {}
            var onPasteImage: (Data) -> Void = { _ in }

            init(model: TerminalModel) {
                self.model = model
            }

            func terminalView(_ view: TerminalView, didInput input: TerminalInput) {
                model.send(input)
            }

            func terminalView(_ view: TerminalView, fitSizeDidChange size: GridSize) {
                model.updateFitSize(size)
            }

            func terminalView(_ view: TerminalView, didScrollBack scrolledBack: Bool) {
                model.setScrolledBack(scrolledBack)
            }

            func terminalView(_ view: TerminalView, keyboardVisible visible: Bool) {
                model.setKeyboardVisible(visible)
            }

            func terminalView(_ view: TerminalView, dictation press: DictationPress) {
                onDictation(press)
            }

            func terminalViewRequestsMedia(_ view: TerminalView) {
                onRequestMedia()
            }

            func terminalView(_ view: TerminalView, didPasteImage data: Data) {
                onPasteImage(data)
            }
        }
    }

    extension KeyBarPalette {
        /// 底部按键栏照主题配色：键是卡片的底加细线、前景色的字，按下去时反过来。
        init(theme: AppTheme) {
            self.init(
                key: UIColor(theme.card), border: UIColor(theme.hairline), label: UIColor(theme.foreground),
                selectedKey: UIColor(theme.foreground), selectedLabel: UIColor(theme.page),
                background: UIColor(theme.page))
        }
    }

    /// 导航栏正中的标题。
    private struct TitleItem<Content: View>: ToolbarContent {
        @ViewBuilder let content: () -> Content

        var body: some ToolbarContent {
            // iOS 26 起导航栏上的项都有一块玻璃底，标题不要。
            if #available(iOS 26, *) {
                item.sharedBackgroundVisibility(.hidden)
            } else {
                item
            }
        }

        private var item: some ToolbarContent {
            ToolbarItem(placement: .principal) { content() }
        }
    }
#endif
