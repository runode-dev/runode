import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// Git 页上一个文件的 diff：哪个文件、看的是暂存段还是未暂存段。
public struct GitDiffTarget: Hashable, Sendable {
    public var path: String
    public var staged: Bool

    public init(path: String, staged: Bool) {
        self.path = path
        self.staged = staged
    }
}

/// 一个会话所在仓库的 Git 页：看分支和改动的文件、看一个文件的 diff、按文件暂存和撤回、提交、
/// 拉取推送、切分支。都经 `ClientMsg.git` 请宿主在会话 shell 当前的目录里办，宿主按到达的先后一件
/// 一件办，改仓库的操作回改完后的状态。
///
/// 改仓库的操作一次只办一件（`running`），视图据此转圈、禁用别的按钮；读状态、读 diff、列分支不占它。
/// 宿主回带着请求编号的 `Error` 算这件没办成，接着重读一次状态。电脑上的 runode 太旧、不认识
/// `Git` 时回的 `Error` 不带编号（`HostMsg.unknownMessage`），据此标出不支持。
@Observable
@MainActor
public final class GitModel {
    /// 改仓库的操作，`running` 用它说正在办哪件。
    public enum Action: Hashable, Sendable {
        case stage, unstage, commit, fetch, pull, push, sync, checkout
    }

    public let sessionId: SessionId
    /// 仓库的状态；`loaded` 为真而它为空时，会话的目录不在 git 仓库里。
    public private(set) var status: GitStatus?
    /// 读到过一次状态。
    public private(set) var loaded = false
    /// 在等状态的回话。
    public private(set) var isRefreshing = false {
        didSet {
            guard !isRefreshing, !refreshWaiters.isEmpty else { return }
            let waiters = refreshWaiters
            refreshWaiters.removeAll()
            for waiter in waiters { waiter.resume() }
        }
    }
    public private(set) var running: Action?
    public private(set) var linkState: LinkState = .idle
    /// 连上了、能发请求（收到过这次连接的 `ready`）。
    public private(set) var isReady = false
    /// 电脑上的 runode 太旧，不认识 `Git`。
    public private(set) var unsupported = false
    /// 提交说明的输入框。
    public var commitMessage = ""
    public var errorMessage: String?
    /// 分支列表，`loadBranches` 读来的。
    public private(set) var branches: [GitBranch] = []
    public private(set) var isLoadingBranches = false
    /// 打开着的 diff 和读到的内容；读到的为空而 `isLoadingDiff` 为假时，这个文件在那一段里已经没有改动了。
    public private(set) var diffTarget: GitDiffTarget?
    public private(set) var diff: GitFileDiff?
    public private(set) var isLoadingDiff = false

    @ObservationIgnored private let link: any HostLink
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var pending: [UInt32: Pending] = [:]
    /// `refreshAndWait` 在等的，状态读完（或者读不了、断线）时放开。
    @ObservationIgnored private var refreshWaiters: [CheckedContinuation<Void, Never>] = []

    private enum Pending {
        case status
        case action(Action)
        case diff(GitDiffTarget)
        case branches
    }

    public init(sessionId: SessionId, link: any HostLink) {
        self.sessionId = sessionId
        self.link = link
    }

    /// 仓库目录的名字，当页面标题。
    public var repositoryName: String? {
        guard let root = status?.root, !root.isEmpty else { return nil }
        return (root as NSString).lastPathComponent
    }

    public var isClean: Bool {
        guard let status else { return true }
        return status.staged.isEmpty && status.unstaged.isEmpty
    }

    /// 能提交：有说明、有改动、没别的操作在办。没有暂存的改动时提交全部改动，见 `commit`。
    public var canCommit: Bool {
        running == nil && isReady && !isClean
            && !commitMessage.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// 提交时会先暂存全部改动：暂存段是空的。
    public var commitsEverything: Bool {
        status?.staged.isEmpty ?? true
    }

    /// 开始看这个仓库：订阅连接的事件，连上了就读状态。连接本身的开、停归会话列表管。
    public func open() {
        guard task == nil else { return }
        let link = self.link
        task = Task { [weak self] in
            let events = await link.events()
            for await event in events {
                guard let self else { return }
                self.handle(event)
            }
        }
    }

    /// 不看了：停止订阅。还在宿主那边办的操作照样办完，回话没人收。
    public func close() {
        task?.cancel()
        task = nil
        pending.removeAll()
        running = nil
        isRefreshing = false
    }

    // MARK: 用户操作

    /// 断线时立刻重连。
    public func reconnect() {
        let link = self.link
        Task { await link.reconnectNow() }
    }

    /// 重读状态；打开着 diff 时也重读它。
    public func refresh() async {
        guard isReady, !unsupported else { return }
        isRefreshing = true
        await request(.status, as: .status)
        if let diffTarget { await requestDiff(diffTarget) }
    }

    /// 重读状态，等到回话、读不了或者断线再返回，给下拉刷新用。
    public func refreshAndWait() async {
        await refresh()
        guard isRefreshing else { return }
        await withCheckedContinuation { refreshWaiters.append($0) }
    }

    public func stage(_ file: GitFile) async {
        await perform(.stage, .stage(paths: [file.path]))
    }

    /// 撤回暂存；改名连同旧路径一起撤回，不然只撤回新路径那一半。
    public func unstage(_ file: GitFile) async {
        await perform(.unstage, .unstage(paths: [file.path] + (file.oldPath.map { [$0] } ?? [])))
    }

    public func stageAll() async {
        await perform(.stage, .stageAll)
    }

    public func unstageAll() async {
        await perform(.unstage, .unstageAll)
    }

    /// 提交暂存的改动；暂存段是空的时先暂存全部改动。提交成了才清空说明。
    public func commit() async {
        guard canCommit else { return }
        let message = commitMessage.trimmingCharacters(in: .whitespacesAndNewlines)
        await perform(.commit, .commit(message: message, stageAll: commitsEverything))
    }

    public func fetch() async { await perform(.fetch, .fetch) }
    public func pull() async { await perform(.pull, .pull) }
    public func push() async { await perform(.push, .push) }
    public func sync() async { await perform(.sync, .sync) }

    public func loadBranches() async {
        guard isReady, !unsupported else { return }
        isLoadingBranches = true
        await request(.branches, as: .branches)
    }

    public func checkout(_ branch: GitBranch) async {
        guard !branch.current else { return }
        await perform(.checkout, .checkout(branch: branch.name, remote: branch.remote))
    }

    /// 打开一个文件的 diff。
    public func openDiff(_ target: GitDiffTarget) async {
        diffTarget = target
        diff = nil
        await requestDiff(target)
    }

    public func closeDiff() {
        diffTarget = nil
        diff = nil
        isLoadingDiff = false
    }

    // MARK: 收发

    private func perform(_ action: Action, _ request: GitRequest) async {
        guard running == nil, isReady, !unsupported else { return }
        running = action
        errorMessage = nil
        await self.request(request, as: .action(action))
    }

    private func requestDiff(_ target: GitDiffTarget) async {
        guard isReady, !unsupported else { return }
        isLoadingDiff = true
        await request(.diff(path: target.path, staged: target.staged), as: .diff(target))
    }

    private func request(_ request: GitRequest, as kind: Pending) async {
        let req = await link.nextRequestId()
        pending[req] = kind
        link.send(.git(req: req, id: sessionId, request: request))
    }

    func handle(_ event: HostEvent) {
        switch event {
        case .state(let state):
            linkState = state
            if !state.isConnected { connectionLost() }
        case .ready:
            isReady = true
            Task { await refresh() }
        case .message(let message):
            handle(message)
        case .frame:
            break
        }
    }

    /// 看是不是回给这里的请求的；是的话处理掉，返回 true。
    @discardableResult
    func handle(_ message: HostMsg) -> Bool {
        switch message {
        case .gitStatus(let req, _, let status):
            guard let kind = pending.removeValue(forKey: req) else { return false }
            if case .action(let action) = kind {
                running = nil
                if action == .commit { commitMessage = "" }
                if action == .checkout { branches = [] }
                // 改了仓库：打开着的 diff 可能变了。
                if let diffTarget { Task { await requestDiff(diffTarget) } }
            } else {
                isRefreshing = pending.values.contains { if case .status = $0 { true } else { false } }
            }
            self.status = status
            loaded = true
        case .gitDiff(let req, _, let diff):
            guard case .diff(let target)? = pending.removeValue(forKey: req) else { return false }
            guard target == diffTarget else { return true }
            self.diff = diff
            isLoadingDiff = false
        case .gitBranches(let req, _, let branches):
            guard case .branches? = pending.removeValue(forKey: req) else { return false }
            self.branches = branches
            isLoadingBranches = false
        case .error(let req?, _, let message):
            guard let kind = pending.removeValue(forKey: req) else { return false }
            failed(kind, message)
        case .error(nil, _, HostMsg.unknownMessage) where !pending.isEmpty:
            // 电脑上的 runode 太旧，不认识 `Git`，回的 `Error` 不带编号。
            pending.removeAll()
            unsupported = true
            loaded = true
            running = nil
            isRefreshing = false
            isLoadingDiff = false
            isLoadingBranches = false
        default:
            return false
        }
        return true
    }

    private func failed(_ kind: Pending, _ message: String) {
        switch kind {
        case .status:
            isRefreshing = false
            loaded = true
            errorMessage = "读不了仓库的状态：\(message)"
        case .action(let action):
            running = nil
            errorMessage = "\(Presentation.gitAction(action))没成功：\(message)"
            // 失败的操作可能做了一半（拉取合出了冲突）：重读状态。
            Task { await refresh() }
        case .diff:
            isLoadingDiff = false
            errorMessage = "读不了这个文件的改动：\(message)"
        case .branches:
            isLoadingBranches = false
            errorMessage = "读不了分支：\(message)"
        }
    }

    /// 连接断了：还没回话的请求不会再有回音。重新连上后 `ready` 时重读。
    private func connectionLost() {
        isReady = false
        pending.removeAll()
        if running != nil {
            running = nil
            errorMessage = "连接断了，操作可能没办完，重新连上后看看状态"
        }
        isRefreshing = false
        isLoadingDiff = false
        isLoadingBranches = false
    }
}
