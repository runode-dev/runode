import Foundation
import Testing

@testable import RunodeFeatures

/// Claude Code 2.1 在 runode 里处理一个滚轮事件的那段（base 为 1、开着加速、不是 wheelFlood、不走 decay），
/// 照它的 JS 逐行抄的，含 `ClaudeWheelPacer` 要躲开的 wheelMode。返回这个事件滚几行。
private struct ClaudeReference {
    var time = -10_000.0
    var mult = 1.0
    var dir = 0
    var pendingFlip = false
    var wheelMode = false
    var enteredWheelMode = false
    var burstCount = 0

    mutating func wheel(_ k: Int, at m: Double) -> Int {
        if wheelMode, m - time > 1500 { wheelMode = false; burstCount = 0; mult = 1 }
        if pendingFlip {
            pendingFlip = false
            if k != dir || m - time > 200 { dir = k; time = m; mult = 1; return 1 }
            wheelMode = true
            enteredWheelMode = true
        }
        let pe = m - time
        if k != dir, dir != 0 { pendingFlip = true; time = m; return 0 }
        dir = k
        time = m
        if wheelMode {
            if pe < 5 {
                burstCount += 1
                if burstCount >= 5 { wheelMode = false; burstCount = 0; mult = 1 } else { return 1 }
            } else {
                burstCount = 0
            }
        }
        if wheelMode {
            let decay = pow(0.5, pe / 150)
            mult = min(15, 1 + (mult - 1) * decay + 15 * decay, mult + 3)
            return max(1, Int(mult))
        }
        mult = pe > 40 ? 1 : min(6, mult + 0.3)
        return max(1, Int(mult))
    }
}

@Suite struct ClaudeWheelPacerTests {
    /// 拖动（每 8 毫秒报一次）：慢慢往上、快速往上、来回抖、再往下甩。Claude Code 最后滚的总行数和手指要的
    /// 一样，中途也从不进 wheelMode。
    @Test func claudeScrollsWhatTheFingerAskedFor() {
        let start = ContinuousClock.now
        var pacer = ClaudeWheelPacer()
        var claude = ClaudeReference()
        var asked = 0
        var scrolled = 0
        var retryAt: Int?
        for ms in 0..<3000 {
            var due = retryAt.map { ms >= $0 } ?? false
            if ms % 8 == 0, ms < 2000 {
                let step = ms / 8
                let lines =
                    switch ms {
                    case ..<400: step % 4 == 0 ? -1 : 0
                    case ..<900: -3
                    case ..<1200: step % 3 == 0 ? (step % 2 == 0 ? 1 : -1) : 0
                    default: 2
                    }
                pacer.add(lines)
                asked += lines
                due = true
            }
            guard due else { continue }
            let (events, retry) = pacer.take(at: start + .milliseconds(ms))
            for _ in 0..<abs(events) {
                scrolled += events.signum() * claude.wheel(events.signum(), at: Double(ms))
            }
            retryAt = retry.map { ms + Int(($0 / .milliseconds(1)).rounded(.up)) }
        }
        #expect(retryAt == nil)
        #expect(scrolled == asked)
        #expect(!claude.enteredWheelMode)
    }
}
