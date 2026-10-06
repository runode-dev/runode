// 性能测量脚本用的小工具：拉起 runode，量从 exec 到它在屏幕上出现第一个可见窗口的时间。
//
// 用法：perf-window --log FILE [--timeout SECS] -- EXE [ARGS...]
//
// 子进程继承环境变量，标准输入接 /dev/null，标准输出和标准错误都写进 FILE。窗口出现后在
// 标准输出打一行 `pid=PID window_ms=MS` 就退出，子进程留着由调用方收拾；超时打
// `pid=PID window_ms=timeout`，子进程先退出了打 `pid=PID window_ms=exited`，退出码都是 1。
//
// 窗口按 CGWindowListCopyWindowInfo 认：属于这个进程、在普通窗口层（layer 0）、不透明度
// 大于 0、在屏幕上，宽高都不小于 100 点。只读窗口的属主、层和边框，不要屏幕录制权限。

import CoreGraphics
import Darwin
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(("perf-window: " + message + "\n").data(using: .utf8)!)
    exit(2)
}

var log: String?
var timeout = 20.0
var rest = Array(CommandLine.arguments.dropFirst())
while let first = rest.first, first != "--" {
    rest.removeFirst()
    switch first {
    case "--log":
        guard !rest.isEmpty else { fail("--log needs a path") }
        log = rest.removeFirst()
    case "--timeout":
        guard let value = rest.first.flatMap(Double.init) else { fail("--timeout needs seconds") }
        rest.removeFirst()
        timeout = value
    default:
        fail("unknown option \(first)")
    }
}
guard rest.first == "--", rest.count >= 2, let log else {
    fail("usage: perf-window --log FILE [--timeout SECS] -- EXE [ARGS...]")
}
let argv = Array(rest.dropFirst())

var actions: posix_spawn_file_actions_t?
posix_spawn_file_actions_init(&actions)
posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
posix_spawn_file_actions_addopen(&actions, 1, log, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
posix_spawn_file_actions_adddup2(&actions, 1, 2)

let cArgs = argv.map { strdup($0) } + [nil]
defer { cArgs.forEach { free($0) } }

func now() -> UInt64 { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }

var pid: pid_t = 0
let start = now()
let spawned = posix_spawn(&pid, argv[0], &actions, nil, cArgs, environ)
guard spawned == 0 else { fail("posix_spawn \(argv[0]): \(String(cString: strerror(spawned)))") }

func visibleWindow(of pid: pid_t) -> Bool {
    let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] else {
        return false
    }
    return windows.contains { window in
        guard (window[kCGWindowOwnerPID as String] as? Int).map(pid_t.init) == pid,
              window[kCGWindowLayer as String] as? Int == 0,
              (window[kCGWindowAlpha as String] as? Double ?? 0) > 0,
              let bounds = window[kCGWindowBounds as String] as? [String: Double]
        else { return false }
        return (bounds["Width"] ?? 0) >= 100 && (bounds["Height"] ?? 0) >= 100
    }
}

let deadline = start + UInt64(timeout * 1e9)
while true {
    if visibleWindow(of: pid) {
        let ms = Double(now() - start) / 1e6
        print("pid=\(pid) window_ms=\(String(format: "%.1f", ms))")
        exit(0)
    }
    var status: Int32 = 0
    if waitpid(pid, &status, WNOHANG) == pid {
        print("pid=\(pid) window_ms=exited")
        exit(1)
    }
    if now() >= deadline {
        print("pid=\(pid) window_ms=timeout")
        exit(1)
    }
    usleep(2_000)
}
