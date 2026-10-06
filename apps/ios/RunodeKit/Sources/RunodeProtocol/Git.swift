import Foundation

/// 请宿主在一个会话所在的仓库里办的 git 操作，和宿主 `runode_protocol::git` 的 `GitRequest` 一致，
/// 放在 `ClientMsg.git` 里发。宿主按会话 shell 当前的目录找仓库；改仓库的操作办完回改完后的 `GitStatus`。
public enum GitRequest: Hashable, Sendable, Encodable {
    case status
    /// 一个文件在暂存段（`staged`）或未暂存段的逐行改动，回 `GitDiff`。
    case diff(path: String, staged: Bool)
    case stage(paths: [String])
    /// 暂存了的改名要连同旧路径一起给。
    case unstage(paths: [String])
    case stageAll
    case unstageAll
    /// `stageAll` 时先暂存所有改动再提交。
    case commit(message: String, stageAll: Bool)
    case fetch
    case pull
    case push
    /// 先拉取再推送。
    case sync
    /// 回 `GitBranches`。
    case branches
    case checkout(branch: String, remote: Bool)

    private enum Keys: String, CodingKey {
        case op, path, staged, paths, message, branch, remote
        case stageAll = "stage_all"
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case .status: try c.encode("status", forKey: .op)
        case let .diff(path, staged):
            try c.encode("diff", forKey: .op)
            try c.encode(path, forKey: .path)
            try c.encode(staged, forKey: .staged)
        case let .stage(paths):
            try c.encode("stage", forKey: .op)
            try c.encode(paths, forKey: .paths)
        case let .unstage(paths):
            try c.encode("unstage", forKey: .op)
            try c.encode(paths, forKey: .paths)
        case .stageAll: try c.encode("stage_all", forKey: .op)
        case .unstageAll: try c.encode("unstage_all", forKey: .op)
        case let .commit(message, stageAll):
            try c.encode("commit", forKey: .op)
            try c.encode(message, forKey: .message)
            try c.encode(stageAll, forKey: .stageAll)
        case .fetch: try c.encode("fetch", forKey: .op)
        case .pull: try c.encode("pull", forKey: .op)
        case .push: try c.encode("push", forKey: .op)
        case .sync: try c.encode("sync", forKey: .op)
        case .branches: try c.encode("branches", forKey: .op)
        case let .checkout(branch, remote):
            try c.encode("checkout", forKey: .op)
            try c.encode(branch, forKey: .branch)
            try c.encode(remote, forKey: .remote)
        }
    }
}

/// 仓库此刻的样子。
public struct GitStatus: Hashable, Sendable, Decodable {
    /// 仓库根目录，电脑上的绝对路径。
    public var root: String
    /// 当前分支；分离头指针时为空。
    public var branch: String?
    /// HEAD 的短哈希；还没有提交时为空。
    public var head: String?
    /// 上游分支，如 `origin/main`。
    public var upstream: String?
    public var ahead: UInt32
    public var behind: UInt32
    /// 配了至少一个远端。
    public var hasRemote: Bool
    /// 做到一半的合并之类。
    public var operation: GitOperation?
    /// 已暂存、未暂存（含未跟踪）两段的文件；部分暂存的文件两段里都有。
    public var staged: [GitFile]
    public var unstaged: [GitFile]

    public init(
        root: String, branch: String? = nil, head: String? = nil, upstream: String? = nil, ahead: UInt32 = 0,
        behind: UInt32 = 0, hasRemote: Bool = false, operation: GitOperation? = nil, staged: [GitFile] = [],
        unstaged: [GitFile] = []
    ) {
        self.root = root
        self.branch = branch
        self.head = head
        self.upstream = upstream
        self.ahead = ahead
        self.behind = behind
        self.hasRemote = hasRemote
        self.operation = operation
        self.staged = staged
        self.unstaged = unstaged
    }

    enum CodingKeys: String, CodingKey {
        case root, branch, head, upstream, ahead, behind, operation, staged, unstaged
        case hasRemote = "has_remote"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        root = try c.decode(String.self, forKey: .root)
        branch = try c.decodeIfPresent(String.self, forKey: .branch)
        head = try c.decodeIfPresent(String.self, forKey: .head)
        upstream = try c.decodeIfPresent(String.self, forKey: .upstream)
        ahead = try c.decodeIfPresent(UInt32.self, forKey: .ahead) ?? 0
        behind = try c.decodeIfPresent(UInt32.self, forKey: .behind) ?? 0
        hasRemote = try c.decodeIfPresent(Bool.self, forKey: .hasRemote) ?? false
        operation = try c.decodeIfPresent(GitOperation.self, forKey: .operation)
        staged = try c.decodeIfPresent([GitFile].self, forKey: .staged) ?? []
        unstaged = try c.decodeIfPresent([GitFile].self, forKey: .unstaged) ?? []
    }
}

/// 做到一半、等着继续或放弃的操作。宿主新加的读成 `unknown`。
public enum GitOperation: Hashable, Sendable, Decodable {
    case merge, rebase, cherryPick, revert
    case unknown(String)

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        switch text {
        case "merge": self = .merge
        case "rebase": self = .rebase
        case "cherry_pick": self = .cherryPick
        case "revert": self = .revert
        default: self = .unknown(text)
        }
    }
}

/// 有改动的一个文件。
public struct GitFile: Hashable, Sendable, Decodable {
    /// 相对仓库根；删掉的文件是原来的路径。
    public var path: String
    /// 改名前的路径。
    public var oldPath: String?
    public var status: GitFileStatus
    public var added: UInt32
    public var removed: UInt32
    public var binary: Bool
    /// 子模块那样记着一个提交号的条目。
    public var gitlink: Bool

    public init(
        path: String, oldPath: String? = nil, status: GitFileStatus, added: UInt32 = 0, removed: UInt32 = 0,
        binary: Bool = false, gitlink: Bool = false
    ) {
        self.path = path
        self.oldPath = oldPath
        self.status = status
        self.added = added
        self.removed = removed
        self.binary = binary
        self.gitlink = gitlink
    }

    enum CodingKeys: String, CodingKey {
        case path, status, added, removed, binary, gitlink
        case oldPath = "old_path"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        oldPath = try c.decodeIfPresent(String.self, forKey: .oldPath)
        status = try c.decode(GitFileStatus.self, forKey: .status)
        added = try c.decodeIfPresent(UInt32.self, forKey: .added) ?? 0
        removed = try c.decodeIfPresent(UInt32.self, forKey: .removed) ?? 0
        binary = try c.decodeIfPresent(Bool.self, forKey: .binary) ?? false
        gitlink = try c.decodeIfPresent(Bool.self, forKey: .gitlink) ?? false
    }
}

/// 文件的状态。宿主新加的读成 `unknown`。
public enum GitFileStatus: Hashable, Sendable, Decodable {
    case modified, added, deleted, renamed, untracked, conflicted
    case unknown(String)

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        switch text {
        case "modified": self = .modified
        case "added": self = .added
        case "deleted": self = .deleted
        case "renamed": self = .renamed
        case "untracked": self = .untracked
        case "conflicted": self = .conflicted
        default: self = .unknown(text)
        }
    }
}

/// 一个文件在一段里的逐行改动。
public struct GitFileDiff: Hashable, Sendable, Decodable {
    public var file: GitFile
    public var hunks: [GitHunk]
    /// 改动太多，或者是没读内容的未跟踪文件，`hunks` 不全。
    public var truncated: Bool

    public init(file: GitFile, hunks: [GitHunk], truncated: Bool = false) {
        self.file = file
        self.hunks = hunks
        self.truncated = truncated
    }

    enum CodingKeys: String, CodingKey { case file, hunks, truncated }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        file = try c.decode(GitFile.self, forKey: .file)
        hunks = try c.decode([GitHunk].self, forKey: .hunks)
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

public struct GitHunk: Hashable, Sendable, Decodable {
    /// `@@ -a,b +c,d @@` 以及后面的函数名之类。
    public var header: String
    public var lines: [GitLine]

    public init(header: String, lines: [GitLine]) {
        self.header = header
        self.lines = lines
    }
}

public struct GitLine: Hashable, Sendable, Decodable {
    public var kind: GitLineKind
    /// 在旧文件和新文件里的行号；新增的行没有旧行号，删掉的行没有新行号。
    public var old: UInt32?
    public var new: UInt32?
    /// 去掉开头的 `+`、`-` 或空格，制表符换成了空格。
    public var text: String

    public init(kind: GitLineKind, old: UInt32? = nil, new: UInt32? = nil, text: String) {
        self.kind = kind
        self.old = old
        self.new = new
        self.text = text
    }
}

/// 一行改动的种类。宿主新加的读成 `unknown`，当作上下文行显示。
public enum GitLineKind: Hashable, Sendable, Decodable {
    case context, added, removed
    case unknown(String)

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        switch text {
        case "context": self = .context
        case "added": self = .added
        case "removed": self = .removed
        default: self = .unknown(text)
        }
    }
}

/// 分支列表里的一项。
public struct GitBranch: Hashable, Sendable, Decodable {
    /// 本地分支是 `main` 这样的名字，远端分支带上远端名，如 `origin/main`。
    public var name: String
    public var remote: Bool
    public var current: Bool
    public var upstream: String?
    /// 最近一次提交的标题和相对时间（如 `2 days ago`）。
    public var subject: String
    public var date: String

    public init(
        name: String, remote: Bool = false, current: Bool = false, upstream: String? = nil, subject: String = "",
        date: String = ""
    ) {
        self.name = name
        self.remote = remote
        self.current = current
        self.upstream = upstream
        self.subject = subject
        self.date = date
    }

    enum CodingKeys: String, CodingKey { case name, remote, current, upstream, subject, date }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        remote = try c.decode(Bool.self, forKey: .remote)
        current = try c.decodeIfPresent(Bool.self, forKey: .current) ?? false
        upstream = try c.decodeIfPresent(String.self, forKey: .upstream)
        subject = try c.decodeIfPresent(String.self, forKey: .subject) ?? ""
        date = try c.decodeIfPresent(String.self, forKey: .date) ?? ""
    }
}
