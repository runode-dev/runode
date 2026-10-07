import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

@MainActor
@Suite struct GitModelTests {
    let link = FakeLink()

    private func status(staged: [GitFile] = [], unstaged: [GitFile] = [], head: String = "1a2b3c4") -> GitStatus {
        GitStatus(
            root: "/Users/me/app", branch: "main", head: head, upstream: "origin/main", hasRemote: true,
            staged: staged, unstaged: unstaged)
    }

    private let changed = GitFile(path: "src/a.rs", status: .modified, added: 2, removed: 1)

    /// 连上的模型：已经收到 `ready`，读状态的请求回了 `status`。
    private func connectedModel(_ status: GitStatus?) async throws -> GitModel {
        let model = GitModel(sessionId: sessionA, link: link)
        model.handle(.state(.connected(hostName: "mac", address: nil)))
        model.handle(.ready(generation: 1))
        #expect(await eventually { !link.sent.isEmpty })
        let req = try lastGit(.status)
        model.handle(.gitStatus(req: req, id: sessionA, status: status))
        link.clearSent()
        return model
    }

    /// 最后发出去的一条 `Git` 的编号，要的是 `request`。
    private func lastGit(_ request: GitRequest) throws -> UInt32 {
        guard case .git(let req, let id, let sent)? = link.sent.last else {
            Issue.record("expected a git request, got \(link.sent)")
            throw CancellationError()
        }
        #expect(id == sessionA)
        #expect(sent == request)
        return req
    }

    /// 最后发出去的是不是要 `request` 的 `Git`；不记失败，给 `eventually` 用。
    private func sentLast(_ request: GitRequest) -> Bool {
        if case .git(_, sessionA, request)? = link.sent.last { return true }
        return false
    }

    @Test func readsTheStatusOnceConnected() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        #expect(model.loaded)
        #expect(model.status?.unstaged == [changed])
        #expect(model.repositoryName == "app")
        #expect(!model.isRefreshing)
    }

    @Test func notARepository() async throws {
        let model = try await connectedModel(nil)
        #expect(model.loaded)
        #expect(model.status == nil)
    }

    /// 改仓库的操作一次一件：办着的时候别的操作不发，回了改完的状态才放开。
    @Test func oneActionAtATime() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        await model.stage(changed)
        let req = try lastGit(.stage(paths: ["src/a.rs"]))
        #expect(model.running == .stage)
        await model.push()
        #expect(link.sent.count == 1)
        model.handle(.gitStatus(req: req, id: sessionA, status: status(staged: [changed])))
        #expect(model.running == nil)
        #expect(model.status?.staged == [changed])
        #expect(model.status?.unstaged == [])
    }

    /// 撤回暂存的改名连同旧路径一起撤回。
    @Test func unstagingARenameIncludesTheOldPath() async throws {
        let renamed = GitFile(path: "new.rs", oldPath: "old.rs", status: .renamed)
        let model = try await connectedModel(status(staged: [renamed]))
        await model.unstage(renamed)
        _ = try lastGit(.unstage(paths: ["new.rs", "old.rs"]))
    }

    /// 没有暂存的改动时提交全部；提交成了才清空说明，没成时留着、报错并重读状态。
    @Test func commits() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        #expect(!model.canCommit)
        model.commitMessage = "  修好了 \n"
        #expect(model.canCommit && model.commitsEverything)
        await model.commit()
        let failed = try lastGit(.commit(message: "修好了", stageAll: true))
        model.handle(.error(req: failed, id: nil, message: "hook declined"))
        #expect(model.running == nil)
        #expect(model.errorMessage?.contains("hook declined") == true)
        #expect(model.commitMessage == "  修好了 \n")
        #expect(await eventually { sentLast(.status) })

        link.clearSent()
        model.errorMessage = nil
        await model.commit()
        let req = try lastGit(.commit(message: "修好了", stageAll: true))
        model.handle(.gitStatus(req: req, id: sessionA, status: status(head: "5d6e7f8")))
        #expect(model.commitMessage == "")
        #expect(model.isClean)
        #expect(!model.canCommit)
    }

    /// 有暂存的改动时只提交暂存的。
    @Test func commitsOnlyStagedChanges() async throws {
        let model = try await connectedModel(status(staged: [changed], unstaged: [changed]))
        model.commitMessage = "只提交暂存的"
        #expect(!model.commitsEverything)
        await model.commit()
        _ = try lastGit(.commit(message: "只提交暂存的", stageAll: false))
    }

    /// 打开 diff 读那一段的改动；回给已经关掉的 diff 的丢掉。
    @Test func diffs() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        let target = GitDiffTarget(path: "src/a.rs", staged: false)
        await model.openDiff(target)
        let first = try lastGit(.diff(path: "src/a.rs", staged: false))
        #expect(model.isLoadingDiff)
        let other = GitDiffTarget(path: "src/a.rs", staged: true)
        await model.openDiff(other)
        let second = try lastGit(.diff(path: "src/a.rs", staged: true))
        let diff = GitFileDiff(file: changed, hunks: [GitHunk(header: "@@", lines: [GitLine(kind: .added, new: 1, text: "x")])])
        model.handle(.gitDiff(req: first, id: sessionA, diff: diff))
        #expect(model.diff == nil)
        model.handle(.gitDiff(req: second, id: sessionA, diff: nil))
        #expect(model.diff == nil)
        #expect(!model.isLoadingDiff)
        #expect(model.diffTarget == other)
    }

    @Test func branchesAndCheckout() async throws {
        let model = try await connectedModel(status())
        await model.loadBranches()
        let req = try lastGit(.branches)
        let main = GitBranch(name: "main", current: true)
        let feat = GitBranch(name: "origin/feat", remote: true)
        model.handle(.gitBranches(req: req, id: sessionA, branches: [main, feat]))
        #expect(model.branches == [main, feat])
        #expect(!model.isLoadingBranches)

        link.clearSent()
        await model.checkout(main)
        #expect(link.sent.isEmpty)
        await model.checkout(feat)
        let checkout = try lastGit(.checkout(branch: "origin/feat", remote: true))
        #expect(model.running == .checkout)
        model.handle(.gitStatus(req: checkout, id: sessionA, status: status()))
        #expect(model.running == nil)
    }

    /// 电脑上的 runode 太旧，回不带编号的「不认识」：标出不支持，之后不再发。
    @Test func olderComputersAreReported() async throws {
        let model = GitModel(sessionId: sessionA, link: link)
        model.handle(.state(.connected(hostName: "mac", address: nil)))
        model.handle(.ready(generation: 1))
        #expect(await eventually { !link.sent.isEmpty })
        model.handle(.error(req: nil, id: nil, message: HostMsg.unknownMessage))
        #expect(model.unsupported)
        #expect(model.loaded)
        link.clearSent()
        await model.refresh()
        #expect(link.sent.isEmpty)
    }

    /// 断线：在办的操作不会再有回话，放开并提醒；重新连上后重读。
    @Test func connectionLoss() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        await model.pull()
        #expect(model.running == .pull)
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        #expect(model.running == nil)
        #expect(!model.isReady)
        #expect(model.errorMessage != nil)
        link.clearSent()
        await model.refresh()
        #expect(link.sent.isEmpty)
    }

    /// 下拉刷新等到状态回话才返回。
    @Test func refreshWaitsForTheAnswer() async throws {
        let model = try await connectedModel(status())
        let refreshing = Task { await model.refreshAndWait() }
        // `isRefreshing` 先变，请求要等拿到编号才发出去：等请求发出去再取它的编号。
        #expect(await eventually { sentLast(.status) })
        #expect(model.isRefreshing)
        let req = try lastGit(.status)
        model.handle(.gitStatus(req: req, id: sessionA, status: status(unstaged: [changed])))
        await refreshing.value
        #expect(model.status?.unstaged == [changed])
    }

    /// 别的请求的回话和错误不归它管。
    @Test func ignoresOtherReplies() async throws {
        let model = try await connectedModel(status())
        #expect(!model.handle(.gitStatus(req: 999, id: sessionA, status: nil)))
        #expect(!model.handle(.error(req: 999, id: nil, message: "x")))
        #expect(model.errorMessage == nil)
        #expect(model.status != nil)
    }
}
