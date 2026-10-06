#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import SwiftUI

    /// 一个会话所在仓库的 Git 页：最上面是分支和拉取、推送、同步，接着是提交说明，再往下是已暂存和
    /// 改动两组文件。点文件看 diff，文件右边的按钮（或者左滑）暂存、取消暂存，点分支名切分支。
    struct GitScreen: View {
        @Bindable var model: GitModel
        @State private var diffTarget: GitDiffTarget?
        @State private var showingBranches = false
        @FocusState private var editingMessage: Bool

        var body: some View {
            List {
                if !model.linkState.isConnected {
                    ConnectionStatusRow(state: model.linkState, onRetry: model.reconnect)
                        .cardBackground()
                        .plainListRow()
                }
                if let status = model.status {
                    branchCard(status)
                        .plainListRow()
                    if model.isClean {
                        Label("没有改动，工作区是干净的", systemImage: "checkmark.circle")
                            .foregroundStyle(.secondary)
                            .cardBackground()
                            .plainListRow()
                    } else {
                        commitCard
                            .plainListRow()
                        files(status.staged, staged: true)
                        files(status.unstaged, staged: false)
                    }
                }
            }
            .cardList()
            .overlay { placeholder }
            .leadingNavigationTitle(model.repositoryName ?? "Git", subtitle: subtitle)
            .toolbar {
                ToolbarItem(placement: .primaryAction) { moreMenu }
            }
            .refreshable { await model.refreshAndWait() }
            .navigationDestination(item: $diffTarget) { target in
                GitDiffScreen(model: model, target: target)
            }
            .sheet(isPresented: $showingBranches) {
                GitBranchSheet(model: model)
            }
            .alert(
                "出错了", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })
            ) {
                Button("好") {}
            } message: {
                Text(model.errorMessage ?? "")
            }
        }

        private var subtitle: String? {
            if let action = model.running { return Presentation.gitRunning(action) }
            return Presentation.directory(model.status?.root) ?? "Git"
        }

        @ViewBuilder
        private var placeholder: some View {
            if model.unsupported {
                ContentUnavailableView(
                    "电脑上的 runode 太旧", systemImage: "arrow.down.app",
                    description: Text("升级电脑上的 runode 后就能在手机上管理 Git。"))
            } else if model.loaded, model.status == nil, model.linkState.isConnected {
                ContentUnavailableView(
                    "不在 Git 仓库里", systemImage: "folder.badge.questionmark",
                    description: Text("这个终端当前的目录不在 Git 仓库里。在终端里 cd 进仓库后下拉刷新。"))
            } else if !model.loaded, model.linkState.isConnected {
                ProgressView("正在读取仓库…")
            }
        }

        // MARK: 分支

        private func branchCard(_ status: GitStatus) -> some View {
            VStack(alignment: .leading, spacing: 12) {
                Button {
                    showingBranches = true
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "arrow.triangle.branch")
                            .foregroundStyle(.tint)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(Presentation.gitBranch(status))
                                .font(.headline)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Text(Presentation.gitUpstream(status))
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 8)
                        DisclosureChevron()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityHint("切换分支")
                if let warning = Presentation.gitOperation(status.operation) {
                    Label(warning, systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote)
                        .foregroundStyle(.orange)
                }
                if status.hasRemote {
                    HStack(spacing: 8) {
                        remoteButton("拉取", systemImage: "arrow.down", action: .pull, badge: status.behind) {
                            await model.pull()
                        }
                        remoteButton("推送", systemImage: "arrow.up", action: .push, badge: status.ahead) {
                            await model.push()
                        }
                        remoteButton("同步", systemImage: "arrow.triangle.2.circlepath", action: .sync, badge: 0) {
                            await model.sync()
                        }
                    }
                }
            }
            .cardBackground()
        }

        private func remoteButton(
            _ title: String, systemImage: String, action: GitModel.Action, badge: UInt32,
            perform: @escaping () async -> Void
        ) -> some View {
            Button {
                Task { await perform() }
            } label: {
                HStack(spacing: 4) {
                    if model.running == action {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: systemImage)
                    }
                    Text(badge > 0 ? "\(title) \(badge)" : title)
                }
                .font(.subheadline.weight(.semibold))
                .frame(maxWidth: .infinity, minHeight: 36)
            }
            .buttonStyle(.bordered)
            .disabled(model.running != nil || !model.isReady)
        }

        // MARK: 提交

        private var commitCard: some View {
            VStack(alignment: .leading, spacing: 10) {
                TextField("提交说明", text: $model.commitMessage, axis: .vertical)
                    .lineLimit(1...6)
                    .focused($editingMessage)
                    .padding(10)
                    .background(Color(.tertiarySystemFill), in: .inner)
                Button {
                    editingMessage = false
                    Task { await model.commit() }
                } label: {
                    HStack(spacing: 6) {
                        if model.running == .commit {
                            ProgressView().controlSize(.small)
                        }
                        Text(commitTitle)
                    }
                    .font(.headline)
                    .frame(maxWidth: .infinity, minHeight: 36)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!model.canCommit)
            }
            .cardBackground()
        }

        private var commitTitle: String {
            guard let status = model.status else { return "提交" }
            if model.commitsEverything {
                return "提交全部 \(status.unstaged.count) 个改动"
            }
            return "提交 \(status.staged.count) 个已暂存的文件"
        }

        // MARK: 文件

        @ViewBuilder
        private func files(_ files: [GitFile], staged: Bool) -> some View {
            if !files.isEmpty {
                HStack {
                    ListSectionHeader(title: staged ? "已暂存 \(files.count)" : "改动 \(files.count)")
                    Spacer()
                    Button(staged ? "全部取消暂存" : "全部暂存") {
                        Task {
                            if staged { await model.unstageAll() } else { await model.stageAll() }
                        }
                    }
                    .font(.subheadline)
                    .foregroundStyle(.tint)
                    .buttonStyle(.borderless)
                    .padding(.top, 12)
                    .disabled(model.running != nil || !model.isReady)
                }
                .plainListRow()
                ForEach(files, id: \.path) { file in
                    fileRow(file, staged: staged)
                }
            }
        }

        private func fileRow(_ file: GitFile, staged: Bool) -> some View {
            HStack(spacing: 10) {
                Button {
                    diffTarget = GitDiffTarget(path: file.path, staged: staged)
                } label: {
                    GitFileLabel(file: file)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityHint("查看改动")
                Button {
                    Task {
                        if staged { await model.unstage(file) } else { await model.stage(file) }
                    }
                } label: {
                    Image(systemName: staged ? "minus.circle" : "plus.circle")
                        .font(.title3)
                        .frame(minWidth: 44, minHeight: 44)
                }
                .buttonStyle(.borderless)
                .disabled(model.running != nil || !model.isReady)
                .accessibilityLabel(staged ? "取消暂存" : "暂存")
            }
            .cardBackground()
            .plainListRow()
            .swipeActions(edge: .trailing) {
                if staged {
                    Button("取消暂存", systemImage: "minus.circle") { Task { await model.unstage(file) } }
                        .tint(.orange)
                } else {
                    Button("暂存", systemImage: "plus.circle") { Task { await model.stage(file) } }
                        .tint(.green)
                }
            }
        }

        private var moreMenu: some View {
            Menu {
                Button("刷新", systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                if model.status?.hasRemote == true {
                    Section {
                        Button("获取", systemImage: "arrow.down.circle") { Task { await model.fetch() } }
                        Button("拉取", systemImage: "arrow.down") { Task { await model.pull() } }
                        Button("推送", systemImage: "arrow.up") { Task { await model.push() } }
                        Button("同步", systemImage: "arrow.triangle.2.circlepath") { Task { await model.sync() } }
                    }
                    .disabled(model.running != nil)
                }
                if model.status != nil {
                    Button("切换分支", systemImage: "arrow.triangle.branch") { showingBranches = true }
                }
            } label: {
                Image(systemName: "ellipsis")
            }
            .disabled(!model.isReady)
            .accessibilityLabel("Git 操作")
        }
    }

    /// 一个改动的文件：状态字母、文件名、所在目录、增删的行数。
    struct GitFileLabel: View {
        let file: GitFile

        var body: some View {
            let parts = Presentation.gitPathParts(file.path)
            HStack(spacing: 10) {
                Text(Presentation.gitLetter(file.status))
                    .font(.caption.monospaced().weight(.bold))
                    .foregroundStyle(Self.tint(file.status))
                    .frame(width: 22, height: 22)
                    .background(Self.tint(file.status).opacity(0.14), in: .inner)
                VStack(alignment: .leading, spacing: 2) {
                    Text(parts.name)
                        .font(.body)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let directory = parts.directory {
                        Text(directory)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                }
                Spacer(minLength: 6)
                if file.binary {
                    Text("二进制")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else if file.added + file.removed > 0 {
                    HStack(spacing: 4) {
                        if file.added > 0 { Text("+\(file.added)").foregroundStyle(.green) }
                        if file.removed > 0 { Text("−\(file.removed)").foregroundStyle(.red) }
                    }
                    .font(.caption.monospacedDigit())
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                "\(file.path)，\(Presentation.gitStatusName(file.status))，增加 \(file.added) 行，删除 \(file.removed) 行")
        }

        static func tint(_ status: GitFileStatus) -> Color {
            switch status {
            case .modified: .orange
            case .added, .untracked: .green
            case .deleted, .conflicted: .red
            case .renamed: .blue
            case .unknown: .secondary
            }
        }
    }

    /// 一个文件的 diff：逐行显示，新增的绿底、删掉的红底，长行折行。右上角暂存或取消暂存这个文件。
    struct GitDiffScreen: View {
        let model: GitModel
        let target: GitDiffTarget
        @Environment(\.dismiss) private var dismiss

        var body: some View {
            content
                .navigationTitle(Presentation.gitPathParts(target.path).name)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Button(target.staged ? "取消暂存" : "暂存") {
                            guard let file = model.diff?.file else { return }
                            let staged = target.staged
                            Task {
                                if staged { await model.unstage(file) } else { await model.stage(file) }
                            }
                            dismiss()
                        }
                        .disabled(model.diff == nil || model.running != nil || !model.isReady)
                    }
                }
                .task { await model.openDiff(target) }
                .onDisappear { model.closeDiff() }
        }

        @ViewBuilder
        private var content: some View {
            if let diff = model.diff, model.diffTarget == target {
                if diff.file.binary {
                    ContentUnavailableView("二进制文件", systemImage: "doc", description: Text("二进制文件不显示逐行改动。"))
                } else if diff.hunks.isEmpty {
                    ContentUnavailableView(
                        "没有可显示的内容", systemImage: "doc.text",
                        description: Text(diff.truncated ? "文件太大，没有读它的内容。" : "这个文件没有逐行的改动。"))
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            Text(target.path)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                            ForEach(Array(diff.hunks.enumerated()), id: \.offset) { _, hunk in
                                GitHunkView(hunk: hunk)
                            }
                            if diff.truncated {
                                Text("改动太多，只显示了前面一部分。")
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                                    .padding(12)
                            }
                        }
                    }
                }
            } else if model.isLoadingDiff || model.diffTarget != target {
                ProgressView("正在读取改动…")
            } else {
                ContentUnavailableView(
                    "没有改动了", systemImage: "checkmark.circle",
                    description: Text(target.staged ? "这个文件已经没有暂存的改动。" : "这个文件已经没有未暂存的改动。"))
            }
        }
    }

    /// 一块改动：块头，接着一行一行。
    private struct GitHunkView: View {
        let hunk: GitHunk

        var body: some View {
            VStack(alignment: .leading, spacing: 0) {
                Text(hunk.header)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color(.secondarySystemBackground))
                ForEach(Array(hunk.lines.enumerated()), id: \.offset) { _, line in
                    GitLineView(line: line)
                }
            }
        }
    }

    private struct GitLineView: View {
        let line: GitLine

        var body: some View {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(number)
                    .foregroundStyle(.tertiary)
                    .frame(minWidth: 32, alignment: .trailing)
                Text(marker)
                    .foregroundStyle(tint ?? .secondary)
                Text(line.text.isEmpty ? " " : line.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .font(.caption.monospaced())
            .padding(.horizontal, 8)
            .padding(.vertical, 1)
            .background(tint.map { $0.opacity(0.14) } ?? Color.clear)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(accessibilityKind)：\(line.text)")
        }

        private var number: String {
            (line.new ?? line.old).map(String.init) ?? ""
        }

        private var marker: String {
            switch line.kind {
            case .added: "+"
            case .removed: "−"
            default: " "
            }
        }

        private var tint: Color? {
            switch line.kind {
            case .added: .green
            case .removed: .red
            default: nil
            }
        }

        private var accessibilityKind: String {
            switch line.kind {
            case .added: "新增"
            case .removed: "删除"
            default: "未改"
            }
        }
    }

    /// 切分支：本地分支在前，远端分支在后，当前的打勾。点一个就切过去并关掉。
    struct GitBranchSheet: View {
        let model: GitModel
        @Environment(\.dismiss) private var dismiss

        var body: some View {
            NavigationStack {
                List {
                    section("本地分支", model.branches.filter { !$0.remote })
                    section("远端分支", model.branches.filter(\.remote))
                }
                .overlay {
                    if model.isLoadingBranches, model.branches.isEmpty {
                        ProgressView("正在读取分支…")
                    } else if !model.isLoadingBranches, model.branches.isEmpty {
                        ContentUnavailableView("没有分支", systemImage: "arrow.triangle.branch")
                    }
                }
                .navigationTitle("切换分支")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("关闭") { dismiss() }
                    }
                }
                .refreshable { await model.loadBranches() }
            }
            .task { await model.loadBranches() }
        }

        @ViewBuilder
        private func section(_ title: String, _ branches: [GitBranch]) -> some View {
            if !branches.isEmpty {
                Section(title) {
                    ForEach(branches, id: \.name) { branch in
                        Button {
                            Task { await model.checkout(branch) }
                            dismiss()
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(branch.name)
                                        .font(.body.weight(branch.current ? .semibold : .regular))
                                        .lineLimit(1)
                                        .truncationMode(.middle)
                                    Text([branch.subject, branch.date].filter { !$0.isEmpty }.joined(separator: " · "))
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                                Spacer(minLength: 8)
                                if branch.current {
                                    Image(systemName: "checkmark")
                                        .foregroundStyle(.tint)
                                        .accessibilityLabel("当前分支")
                                }
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(branch.current || model.running != nil)
                    }
                }
            }
        }
    }
#endif
