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

    /// 丢弃按路径请宿主办，占着 `running`。
    @Test func discards() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        await model.discard([changed])
        let req = try lastGit(.discard(paths: ["src/a.rs"]))
        #expect(model.running == .discard)
        model.handle(.gitStatus(req: req, id: sessionA, status: status()))
        #expect(model.running == nil && model.isClean)
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

    /// 提交办着的时候改了说明（比如写下一条）：提交成了也不清掉改过的。
    @Test func editsDuringACommitAreKept() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        model.commitMessage = "修好了"
        await model.commit()
        let req = try lastGit(.commit(message: "修好了", stageAll: true))
        model.commitMessage = "下一条"
        model.handle(.gitStatus(req: req, id: sessionA, status: status(head: "5d6e7f8")))
        #expect(model.running == nil)
        #expect(model.commitMessage == "下一条")
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

    /// 读 diff 出错：错误归 diff 页显示，不当成没有改动；重读时清掉。
    @Test func diffFailure() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        await model.openDiff(GitDiffTarget(path: "src/a.rs", staged: false))
        let req = try lastGit(.diff(path: "src/a.rs", staged: false))
        model.handle(.error(req: req, id: sessionA, message: "坏了"))
        #expect(model.diffError != nil)
        #expect(!model.isLoadingDiff)
        #expect(model.diff == nil)
        #expect(model.errorMessage == nil)
        await model.refresh()
        #expect(model.diffError == nil)
        #expect(model.isLoadingDiff)
    }

    /// 旧 diff 请求的错误不落到新打开的 diff 上，新的那个还在读。
    @Test func staleDiffFailureIsIgnored() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        await model.openDiff(GitDiffTarget(path: "src/a.rs", staged: false))
        let first = try lastGit(.diff(path: "src/a.rs", staged: false))
        await model.openDiff(GitDiffTarget(path: "src/a.rs", staged: true))
        model.handle(.error(req: first, id: sessionA, message: "坏了"))
        #expect(model.diffError == nil)
        #expect(model.isLoadingDiff)
    }

    /// 数未暂存段里有冲突的文件，"全部暂存"据此先请用户确认。
    @Test func countsUnstagedConflicts() async throws {
        let conflicted = GitFile(path: "src/b.rs", status: .conflicted)
        let resolved = try await connectedModel(status(staged: [conflicted], unstaged: [changed]))
        #expect(resolved.unstagedConflicts == 0)
        let other = GitFile(path: "src/c.rs", status: .conflicted)
        let model = try await connectedModel(status(unstaged: [changed, conflicted, other]))
        #expect(model.unstagedConflicts == 2)
    }

    /// 断着线打开 diff：不发请求，也不在读；diff 页据 `isReady` 显示没连上，不说没有改动。
    @Test func diffWhileDisconnected() async throws {
        let model = try await connectedModel(status(unstaged: [changed]))
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        await model.openDiff(GitDiffTarget(path: "src/a.rs", staged: false))
        #expect(link.sent.isEmpty)
        #expect(!model.isLoadingDiff)
        #expect(!model.isReady)
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

    /// 取请求编号时连接断了：不记下等不到回话的请求，重新连上后读状态的回话一到就不再转圈。
    @Test func aDisconnectWhileNumberingDoesNotLeaveARefreshPending() async throws {
        let held = HeldRequestLink()
        held.release()
        let model = GitModel(sessionId: sessionA, link: held)
        model.handle(.state(.connected(hostName: "mac", address: nil)))
        model.handle(.ready(generation: 1))
        #expect(await eventually { !held.inner.sent.isEmpty })
        guard case .git(let first, _, .status)? = held.inner.sent.last else {
            Issue.record("\(held.inner.sent)")
            return
        }
        model.handle(.gitStatus(req: first, id: sessionA, status: status()))

        held.hold()
        let refreshing = Task { await model.refresh() }
        #expect(await eventually { held.waiting == 1 })
        model.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        held.release()
        await refreshing.value

        held.inner.clearSent()
        model.handle(.state(.connected(hostName: "mac", address: nil)))
        model.handle(.ready(generation: 2))
        #expect(await eventually { !held.inner.sent.isEmpty })
        guard case .git(let again, _, .status)? = held.inner.sent.last else {
            Issue.record("\(held.inner.sent)")
            return
        }
        model.handle(.gitStatus(req: again, id: sessionA, status: status()))
        #expect(!model.isRefreshing)
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

@Suite struct GitFileTreeTests {
    private func files(_ paths: [String]) -> [GitFile] {
        paths.map { GitFile(path: $0, status: .modified) }
    }

    private func shape(_ items: [GitTreeItem]) -> [String] {
        items.map {
            switch $0 {
            case .directory(_, let name, let depth, let expanded, let files):
                "\(depth) \(name)/ \(files.count)\(expanded ? "" : " +")"
            case .file(let file, let depth): "\(depth) \(file.path)"
            }
        }
    }

    @Test func groupsFilesByDirectory() {
        let tree = Presentation.gitFileTree(
            files(["src/main.rs", "README.md", "src/a/b.rs", "Cargo.toml", "src/A.rs", "docs/x.md"]), collapsed: [])
        // 目录在前、文件在后，各自按名字排，不分大小写；只有一个文件的目录不并。
        #expect(
            shape(tree) == [
                "0 docs/ 1", "1 docs/x.md", "0 src/ 3", "1 a/ 1", "2 src/a/b.rs", "1 src/A.rs", "1 src/main.rs",
                "0 Cargo.toml", "0 README.md",
            ])
    }

    @Test func compactsSingleChildDirectoriesAndCollapses() {
        let changed = files(["crates/desktop/src/a.rs", "crates/desktop/src/b/c.rs", "crates/desktop/src/b/d.rs"])
        #expect(
            shape(Presentation.gitFileTree(changed, collapsed: [])) == [
                "0 crates/desktop/src/ 3", "1 b/ 2", "2 crates/desktop/src/b/c.rs", "2 crates/desktop/src/b/d.rs",
                "1 crates/desktop/src/a.rs",
            ])
        // 收起按并成一行后最深的那个目录认，下面的不排。
        #expect(
            shape(Presentation.gitFileTree(changed, collapsed: ["crates/desktop/src"])) == [
                "0 crates/desktop/src/ 3 +"
            ])
    }
}

/// 成对改动的行里改了哪些字，和电脑上 `changed_words` 的测试对得上。
@Suite struct GitWordChangesTests {
    private func changed(_ old: String, _ new: String) -> ([String], [String]) {
        let (a, b) = Presentation.gitChangedWords(old, new)
        return (a.map { String(old[$0]) }, b.map { String(new[$0]) })
    }

    @Test func marksOnlyTheChangedWords() {
        #expect(changed("let total = count + 1;", "let total = count * 2;") == (["+ 1"], ["* 2"]))
        #expect(changed("foo(bar)", "foo(bar, baz)") == ([], [", baz"]))
    }

    @Test func chineseIsComparedCharByChar() {
        #expect(changed("改动现在只有一处：实现。", "改动现在有两处：实现，再关掉。") == (["只", "一"], ["两", "，再关掉"]))
    }

    @Test func comparesBytesNotCanonicalForms() {
        // é 写成一个字和写成 e 加尖音符，看着一样，字节不同，要标出来。
        let (a, b) = Presentation.gitChangedWords("let title = \"\u{E9}\";", "let title = \"e\u{301}\";")
        #expect(a.count == 1 && b.count == 1)
    }

    @Test func rewrittenLinesAreNotMarked() {
        #expect(changed("fn alpha() {}", "let beta = gamma;") == ([], []))
    }

    @Test func pairsRemovedWithAddedLines() {
        let hunk = GitHunk(
            header: "@@ -1,2 +1,2 @@",
            lines: [
                GitLine(kind: .removed, old: 1, text: "a longer new line"),
                GitLine(kind: .added, new: 1, text: "a much longer new line"),
                GitLine(kind: .context, old: 2, new: 2, text: "b"),
                GitLine(kind: .added, new: 3, text: "c"),
            ])
        let words = Presentation.gitWordChanges(hunk)
        #expect(words.map(\.count) == [0, 1, 0, 0])
        #expect(String(hunk.lines[1].text[words[1][0]]) == "much ")
    }
}
