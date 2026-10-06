import Foundation
import Testing

@testable import RunodeProtocol

/// 读写 git 的消息和宿主（Rust）序列化出来的一样，样例见测试资源里的 `git_*`。
@Suite struct GitMessageTests {
    let id = SessionId("0123456789abcdef0011223344556677")!

    @Test(arguments: [
        "git_status_request", "git_diff_request", "git_unstage_request", "git_commit_request", "git_checkout_request",
    ])
    func requestsMatchTheHost(_ name: String) throws {
        let request: GitRequest =
            switch name {
            case "git_status_request": .status
            case "git_diff_request": .diff(path: "src/a.rs", staged: true)
            case "git_unstage_request": .unstage(paths: ["b.rs", "a.rs"])
            case "git_commit_request": .commit(message: "修好了", stageAll: true)
            case "git_checkout_request": .checkout(branch: "origin/feat", remote: true)
            default: throw CancellationError()
            }
        let req: UInt32 =
            switch name {
            case "git_status_request": 1
            case "git_diff_request": 2
            case "git_unstage_request": 3
            case "git_commit_request": 4
            default: 5
            }
        let encoded = try JSONEncoder().encode(ClientMsg.git(req: req, id: id, request: request))
        #expect(try sameJSON(encoded, RustSamples.data(name)))
    }

    /// 没有参数的操作只写 `op`，和宿主的 `GitRequest` 一样。
    @Test func bareOperations() throws {
        let cases: [(GitRequest, String)] = [
            (.stageAll, "stage_all"), (.unstageAll, "unstage_all"), (.fetch, "fetch"), (.pull, "pull"),
            (.push, "push"), (.sync, "sync"), (.branches, "branches"),
        ]
        for (request, op) in cases {
            let object = try JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: String]
            #expect(object == ["op": op])
        }
    }

    private func decode(_ name: String) throws -> HostMsg {
        try JSONDecoder().decode(HostMsg.self, from: RustSamples.data(name))
    }

    @Test func status() throws {
        guard case .gitStatus(1, let answered, let status?) = try decode("git_status") else {
            Issue.record("not a git status")
            return
        }
        #expect(answered == id)
        #expect(status.root == "/Users/me/app")
        #expect(status.branch == "main" && status.head == "1a2b3c4" && status.upstream == "origin/main")
        #expect(status.ahead == 2 && status.behind == 1 && status.hasRemote)
        #expect(status.operation == .cherryPick)
        #expect(
            status.staged == [
                GitFile(path: "src/new.rs", oldPath: "src/old.rs", status: .renamed, added: 3, removed: 1)
            ])
        #expect(status.unstaged == [GitFile(path: "notes.txt", status: .untracked, added: 1)])
        #expect(try decode("git_status_none") == .gitStatus(req: 2, id: id, status: nil))
    }

    @Test func diff() throws {
        guard case .gitDiff(3, _, let diff?) = try decode("git_diff") else {
            Issue.record("not a git diff")
            return
        }
        #expect(diff.file.path == "a.txt" && diff.file.status == .modified)
        #expect(!diff.truncated)
        #expect(diff.hunks.first?.header == "@@ -1,2 +1,2 @@ fn main")
        #expect(
            diff.hunks.first?.lines == [
                GitLine(kind: .context, old: 1, new: 1, text: "keep"),
                GitLine(kind: .removed, old: 2, text: "old"),
                GitLine(kind: .added, new: 2, text: "new"),
            ])
    }

    @Test func branches() throws {
        #expect(
            try decode("git_branches")
                == .gitBranches(
                    req: 4, id: id,
                    branches: [
                        GitBranch(
                            name: "main", current: true, upstream: "origin/main", subject: "init", date: "2 days ago"),
                        GitBranch(name: "origin/feat", remote: true, subject: "wip", date: "1 hour ago"),
                    ]))
    }

    /// 宿主新加的状态、操作、行的种类读成 `unknown`，不让整条消息读不了。
    @Test func newerValuesAreTolerated() throws {
        let json = Data(
            #"{"path":"x","status":"typechange","old_path":null,"added":0,"removed":0,"binary":false,"gitlink":false}"#
                .utf8)
        #expect(try JSONDecoder().decode(GitFile.self, from: json).status == .unknown("typechange"))
        #expect(try JSONDecoder().decode(GitOperation.self, from: Data(#""bisect""#.utf8)) == .unknown("bisect"))
        #expect(try JSONDecoder().decode(GitLineKind.self, from: Data(#""moved""#.utf8)) == .unknown("moved"))
    }
}
