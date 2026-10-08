import Foundation

/// 一个目录里能跑的项目命令：自己加的命令（这个项目的和通用的，都在电脑上 `~/.runode/tasks.json` 里）、
/// Makefile 的目标、package.json 的 scripts，回 `ListProjectTasks`。和宿主的
/// `TaskSource` 等一一对应。完整的命令行由宿主拼好，手机原样打进 shell。
public struct TaskSource: Hashable, Sendable, Decodable {
    public enum Kind: String, Hashable, Sendable {
        case custom
        case global
        case makefile
        case packageJson = "package_json"
        /// 新的电脑上多出来的种类。
        case unknown
    }

    public var kind: Kind
    /// 文件的绝对路径。
    public var file: String
    public var tasks: [ProjectTask]
    /// 太多了，只给了前面一部分。
    public var truncated: Bool

    public init(kind: Kind, file: String, tasks: [ProjectTask], truncated: Bool = false) {
        self.kind = kind
        self.file = file
        self.tasks = tasks
        self.truncated = truncated
    }

    enum CodingKeys: String, CodingKey { case kind, file, tasks, truncated }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = Kind(rawValue: try c.decode(String.self, forKey: .kind)) ?? .unknown
        file = try c.decode(String.self, forKey: .file)
        tasks = try c.decode([ProjectTask].self, forKey: .tasks)
        truncated = try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false
    }
}

/// 一条能跑的命令。
public struct ProjectTask: Hashable, Sendable, Decodable {
    /// 目标名或 script 名。
    public var name: String
    /// 在请求的目录里打进 shell 就能跑的完整命令行。
    public var command: String
    /// Makefile 目标那一行 `##` 后面的说明，package.json 里是 script 本身。
    public var description: String?

    public init(name: String, command: String, description: String? = nil) {
        self.name = name
        self.command = command
        self.description = description
    }

    enum CodingKeys: String, CodingKey { case name, command, description }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        command = try c.decode(String.self, forKey: .command)
        description = try c.decodeIfPresent(String.self, forKey: .description)
    }
}
