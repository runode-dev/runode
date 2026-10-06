#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 一台 Mac 上的会话，以 agent 为中心：等你回答的放最上面（带快速回复），接着是干活中的，最后是
    /// 其他。每行带屏幕最后几行的预览。点进去开终端，左滑结束，长按有更多操作。
    struct SessionListView: View {
        @Bindable var model: SessionListModel
        let onOpen: (SessionId) -> Void
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize

        var body: some View {
            List {
                if !model.linkState.isConnected {
                    Section {
                        ConnectionStatusRow(state: model.linkState, onRetry: model.reconnect)
                    }
                }
                ForEach(model.sections) { section in
                    Section {
                        ForEach(section.sessions, id: \.id) { session in
                            row(session, group: section.group)
                            if section.group == .waiting {
                                QuickReplyBar(model: model.quickReply(for: session.id))
                                    .padding(.vertical, 4)
                                    .accessibilityElement(children: .contain)
                                    .accessibilityLabel("回复「\(Presentation.sessionTitle(session))」")
                            }
                        }
                    } header: {
                        Label(Presentation.title(for: section.group), systemImage: Presentation.symbol(for: section.group))
                            .foregroundStyle(section.group == .other ? Color.secondary : AgentBadge.tint(for: section.group))
                            .font(.subheadline.weight(.semibold))
                            .textCase(nil)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .animation(.default, value: model.sections.map(\.sessions.count))
            .overlay {
                if model.loaded, model.sessions.isEmpty, model.linkState.isConnected {
                    ContentUnavailableView {
                        Label("这台 Mac 上没有终端", systemImage: "terminal")
                    } description: {
                        Text("新开一个终端，在手机上就能用。")
                    } actions: {
                        Button("新开会话") { Task { await model.spawn() } }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                    }
                } else if !model.loaded, model.linkState.isConnected {
                    ProgressView("正在读取会话…")
                }
            }
            .navigationTitle(model.machine.name)
            .modifier(LinkSubtitle(state: model.linkState))
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button("新开会话", systemImage: "plus") {
                        Task { await model.spawn() }
                    }
                    .disabled(!model.linkState.isConnected || model.isSpawning)
                }
            }
            .refreshable {
                model.refresh()
                model.refreshPreviews()
            }
            .task { await model.keepRefreshing() }
            .onGeometryChange(for: CGSize.self) { $0.size } action: { size in
                model.spawnSize = TerminalView.gridSize(
                    fitting: size, scale: displayScale, contentSize: UIContentSizeCategory(dynamicTypeSize))
            }
            .onChange(of: model.spawnedSession) { _, session in
                guard let session else { return }
                model.spawnedSession = nil
                onOpen(session)
            }
            .confirmationDialog(
                "结束这个终端？", isPresented: $model.isConfirmingKill, titleVisibility: .visible,
                presenting: model.killTarget.flatMap(model.session)
            ) { session in
                Button("结束会话", role: .destructive) { model.kill(session.id) }
            } message: { session in
                Text("「\(Presentation.sessionTitle(session))」里正在跑的程序会收到 SIGHUP 并退出。")
            }
            .alert(
                "出错了", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })
            ) {
                Button("好") {}
            } message: {
                Text(model.errorMessage ?? "")
            }
        }

        private func row(_ session: SessionInfo, group: SessionGroup) -> some View {
            NavigationLink(value: Route.terminal(machine: model.machine.id, session: session.id)) {
                SessionRow(session: session, preview: model.previews[session.id] ?? [], group: group)
            }
            .swipeActions(edge: .trailing) {
                Button("结束", systemImage: "xmark.circle", role: .destructive) {
                    model.killTarget = session.id
                }
            }
            .contextMenu {
                Button("打开", systemImage: "terminal") { onOpen(session.id) }
                if let cwd = session.meta.cwd {
                    Button("复制目录", systemImage: "doc.on.doc") { UIPasteboard.general.string = cwd }
                }
                Section(
                    "\(Presentation.gridSize(session.size)) · \(Presentation.sizeOwner(session.sizeOwner))"
                ) {
                    Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                        model.killTarget = session.id
                    }
                }
            }
        }
    }

    /// 标题下面的连接状态（iOS 26 起有导航栏副标题；更早的系统在列表顶上的状态行里看）。
    private struct LinkSubtitle: ViewModifier {
        let state: LinkState

        func body(content: Content) -> some View {
            if #available(iOS 26, *) {
                content.navigationSubtitle(subtitle)
            } else {
                content
            }
        }

        private var subtitle: String {
            switch state {
            case .connected(_, let address?): "已连接 · \(Presentation.displayAddress(address))"
            case .waiting: "已断开，正在重连"
            default: Presentation.linkStatus(state)
            }
        }
    }

    private struct SessionRow: View {
        let session: SessionInfo
        let preview: [String]
        let group: SessionGroup
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize

        /// 预览的字号：等宽小字，跟着动态字体走。
        private var previewSize: CGFloat {
            let traits = UITraitCollection(preferredContentSizeCategory: UIContentSizeCategory(dynamicTypeSize))
            return UIFont.preferredFont(forTextStyle: .caption1, compatibleWith: traits).pointSize
        }

        /// 一行预览：私用区的字（提示符里的 Powerline、Nerd Font 图标）那几段用随包的符号字体，其余用
        /// 等宽系统字体。SwiftUI 的 `Font` 不带 Core Text 的后备列表，只能这样分段指定。
        private func previewText(_ line: String) -> Text {
            let size = previewSize
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

        var body: some View {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(Presentation.sessionTitle(session))
                        .font(.headline)
                        .lineLimit(2)
                    if session.exited {
                        Text("已退出")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 6)
                            .background(.quaternary, in: Capsule())
                    }
                }
                if session.meta.agent != nil {
                    AgentBadge(agent: session.meta.agent)
                }
                if let directory = Presentation.directory(session.meta.cwd) {
                    Label(directory, systemImage: "folder")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                        .labelStyle(.titleAndIcon)
                }
                if !preview.isEmpty {
                    // 每行单独截断：长行折下去会把别的行挤掉。
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(preview.enumerated()), id: \.offset) { _, line in
                            previewText(line)
                                .lineLimit(1)
                                .truncationMode(.tail)
                        }
                    }
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                        .background(Color(.secondarySystemFill), in: RoundedRectangle(cornerRadius: 8))
                        .accessibilityLabel("屏幕预览：\(preview.joined(separator: "，"))")
                }
            }
            .padding(.vertical, 4)
            .accessibilityElement(children: .combine)
        }
    }
#endif
