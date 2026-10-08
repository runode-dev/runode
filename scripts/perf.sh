#!/bin/bash
# 量一个 runode 二进制的体积、命令行冷启动、开窗时间、内存的峰值和稳定值、线程、空闲时的 CPU 和唤醒。
#
# 用法：perf.sh [--json] [--rounds N] [--settle SECS] [--samples N] [--idle SECS] BINARY|APP
#
#   --json         输出 JSON，默认输出表格
#   --rounds N     开窗测几轮，默认 5；各项取中位数和最大值
#   --settle SECS  开窗后静置多久再量内存和线程，默认 30，理由见下面的「内存」
#   --samples N    静置后 footprint 每秒取一次样、共取几次，默认 10
#   --idle SECS    量空闲 CPU 和唤醒的秒数，默认 5
#
# 要量用户实际拿到的样子，给 .app 的路径（`scripts/bundle-macos.sh app` 的产物在
# target/release/bundle/Runode.app；`make app` 编的是 local profile，在 target/local/bundle 下，
# 不是发给用户的那种），脚本直接 exec 它 Contents/MacOS 下的可执行文件，HOME 照样换成临时目录。
# 给裸二进制也能量，但未打包运行时 app 会在启动时自己设 Dock 图标（`about::install_icon`），
# 打包后不做，开窗时间和内存会比 .app 多出这一块。
#
# 内存：footprint 的 phys_footprint 里有一块 GPU 驱动替进程占着的临时内存，记在「Owned physical
# footprint (unmapped) (graphics)」这一项（默认窗口约 178 MiB；这一项也含渲染器 GPU 私有的纹理）。
# 只要还在画帧它就一直在，停止绘制约 3 秒后退掉，再画又回来。光标默认不闪，静置时没有东西在重画，
# 它就会退掉；默认还在闪的老版本在前台时每次闪都重画，它一直退不掉，稳定值和空闲 CPU、唤醒都含着
# 这部分，和新版本比时要记得。所以分开报两个数：峰值是内核记的 phys_footprint_peak，进程启动以来
# 的最高值，含这块临时内存；稳定后是静置 --settle 秒后每秒取一次样、共 --samples 次，取
# phys_footprint 最小的那次，同时列出这次的 IOSurface（窗口的 drawable 在这里）、IOAccelerator
# (graphics)（渲染器 CPU 也能访问的纹理和缓冲）和上面那一项。
# 静置期间不要碰窗口，否则稳定值会偏高。
#
# 每轮用一个新的临时目录当 HOME，runode 的配置、窗口存档、提前启动 shell 的尺寸记录和宿主的
# socket 都在它下面的 .runode 里；XDG_CONFIG_HOME 也指进去，免得读到自己的 Ghostty 配置。
# 不碰自己的 runode，跑完删掉。每轮开两次窗口：
#   首次     临时目录是空的，没有尺寸记录，不提前启动 shell
#   第二次   沿用第一次留下的尺寸记录和窗口存档，提前启动 shell；内存、线程、空闲只在这次量
# 窗口会在桌面上弹出来，量完马上结束进程（连同它拉起的 shell 和单独的宿主进程）。
#
# 开窗时间是从 posix_spawn 到 CGWindowListCopyWindowInfo 里出现这个进程的可见窗口，由
# 同目录下的 Swift 小工具量，第一次跑时用 swiftc 编译进 target 目录。二进制带启动计时点
# （`runode::startup`）时，顺带按 RUST_LOG=runode::startup=info 打出的点分解启动时间；不带
# 的（比如老版本）只是没有这一栏。
#
# 命令行冷启动量 `runode help`，它不连宿主、不读配置。装了 hyperfine 时用它，没装时用 perl
# 循环计时。空闲时的 CPU 和唤醒用 `top -l` 每秒取一次样，第一个样本不算（它是从进程启动累计
# 的）；powermetrics 要 sudo，不用。
set -euo pipefail

json=0
rounds=5
settle=30
samples=10
idle=5
binary=
while (($#)); do
    case $1 in
        --json) json=1 ;;
        --rounds) rounds=$2; shift ;;
        --settle) settle=$2; shift ;;
        --samples) samples=$2; shift ;;
        --idle) idle=$2; shift ;;
        -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        -*) echo "perf.sh: 不认识的选项 $1" >&2; exit 2 ;;
        *) binary=$1 ;;
    esac
    shift
done
[[ -n $binary ]] || {
    echo "用法：perf.sh [--json] [--rounds N] [--settle SECS] [--samples N] [--idle SECS] BINARY|APP" >&2
    exit 2
}
((samples >= 1)) || { echo "perf.sh: --samples 至少是 1" >&2; exit 2; }
# 给的是 .app 时换成它里面的可执行文件，名字按 Info.plist 的 CFBundleExecutable。
bundle=
if [[ -d $binary && $binary == *.app ]]; then
    bundle=$(cd "$binary" && pwd)
    exe_name=$(/usr/libexec/PlistBuddy -c "Print :CFBundleExecutable" "$bundle/Contents/Info.plist" 2>/dev/null) ||
        { echo "perf.sh: $bundle/Contents/Info.plist 里没有 CFBundleExecutable" >&2; exit 2; }
    binary=$bundle/Contents/MacOS/$exe_name
fi
[[ -x $binary ]] || { echo "perf.sh: $binary 不是可执行文件" >&2; exit 2; }
binary=$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")

root=$(cd "$(dirname "$0")/.." && pwd)
probe_src="$root/scripts/perf-window.swift"
probe="$root/target/perf/perf-window"
if [[ ! -x $probe || $probe_src -nt $probe ]]; then
    mkdir -p "$(dirname "$probe")"
    swiftc -O "$probe_src" -o "$probe"
fi

say() { ((json)) || echo "$*" >&2; }
sleep_s() { perl -e "select undef, undef, undef, $1"; }

raw=$(mktemp -d "${TMPDIR:-/tmp}/runode-perf.XXXXXX")
home=
pid=

# 结束 $1 和它所有的子孙进程，以及在 $home 里拿着宿主锁的进程（单独跑的宿主不是它的子进程）。
stop_app() {
    local target=$1 all=() queue=("$1") next
    while ((${#queue[@]})); do
        next=${queue[0]}
        queue=("${queue[@]:1}")
        all+=("$next")
        # shellcheck disable=SC2207
        queue+=($(pgrep -P "$next" || true))
    done
    if [[ -n $home ]]; then
        # shellcheck disable=SC2207
        all+=($(lsof -t "$home"/.runode/run/*.lock 2>/dev/null || true))
    fi
    kill -TERM "${all[@]}" 2>/dev/null || true
    for _ in $(seq 20); do
        kill -0 "$target" 2>/dev/null || break
        sleep_s 0.1
    done
    kill -KILL "${all[@]}" 2>/dev/null || true
}

cleanup() {
    [[ -n $pid ]] && stop_app "$pid"
    [[ -n $home ]] && rm -rf "$home"
    rm -rf "$raw"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# 开一次窗口：在 $home 里拉起二进制、日志写进 $1，等到窗口出现，设好 $pid 和开窗毫秒数 $window_ms。
launch() {
    local log=$1 out
    out=$(HOME=$home XDG_CONFIG_HOME=$home/.config RUST_LOG=runode::startup=info \
        "$probe" --log "$log" --timeout 20 -- "$binary") || true
    pid=$(sed -n 's/^pid=\([0-9]*\).*/\1/p' <<<"$out")
    window_ms=$(sed -n 's/.*window_ms=\(.*\)$/\1/p' <<<"$out")
    if [[ -z $pid || ! $window_ms =~ ^[0-9.]+$ ]]; then
        echo "perf.sh: 没等到窗口（${out}），日志：" >&2
        cat "$log" >&2
        exit 1
    fi
}

# 等启动计时点打到 first_content（最多 $1 秒）；不带计时点的二进制等不到，到时间就算。
wait_content() {
    local log=$1 limit=$2 i
    for ((i = 0; i < limit * 10; i++)); do
        grep -q 'first_content' "$log" 2>/dev/null && return
        sleep_s 0.1
    done
}

# 从日志里取启动计时点，写成「节点<TAB>毫秒」。
startup_points() {
    sed $'s/\x1b\\[[0-9;]*m//g' "$1" | sed -n 's/.*runode::startup: node="\([a-z_]*\)" ms=\([0-9.]*\).*/\1\t\2/p'
}

say "二进制：$binary"
[[ -n $bundle ]] || say "（裸二进制：启动时会设 Dock 图标，比打包后的 .app 多一点开窗时间和内存）"
size=$(stat -f %z "$binary")

# 新写出的二进制第一次 exec 时系统要先检查它（实测几百毫秒），不算进下面的结果。
"$binary" help >/dev/null

say "量命令行冷启动……"
cli_runs=$((rounds < 4 ? 20 : rounds * 4))
if command -v hyperfine >/dev/null; then
    hyperfine -N --warmup 3 --runs "$cli_runs" --export-json "$raw/cli.json" --output null "$binary help" >/dev/null 2>&1
    cli_tool=hyperfine
else
    perl -MTime::HiRes=time -e '
        my ($bin, $runs) = @ARGV;
        for (1 .. 3) { system("$bin help >/dev/null") }
        my @t;
        for (1 .. $runs) { my $s = time; system("$bin help >/dev/null"); push @t, time - $s }
        print join("\n", @t), "\n";
    ' "$binary" "$cli_runs" >"$raw/cli.txt"
    cli_tool=perl
fi

for ((round = 1; round <= rounds; round++)); do
    say "第 $round/$rounds 轮……"
    home=$(mktemp -d "${TMPDIR:-/tmp}/runode-perf-home.XXXXXX")
    mkdir -p "$home/.runode"
    socket="$home/.runode/run/host.sock"
    if ((round == 1 && ${#socket} >= 104)); then
        echo "perf.sh: 临时目录太长，宿主的 socket 路径超过 104 字节，app 会不开 socket；可设短一点的 TMPDIR" >&2
    fi

    # 首次启动：等提前启动 shell 的尺寸记录写好再结束，第二次启动才会用上。
    launch "$raw/$round.first.log"
    echo "$window_ms" >"$raw/$round.first.window"
    wait_content "$raw/$round.first.log" 5
    for _ in $(seq 50); do
        [[ -f $home/.runode/cache/first-terminal-size ]] && break
        sleep_s 0.1
    done
    stop_app "$pid"
    pid=

    # 第二次启动：量开窗时间，静置后量内存和线程，再量空闲。
    launch "$raw/$round.second.log"
    echo "$window_ms" >"$raw/$round.second.window"
    wait_content "$raw/$round.second.log" 5
    sleep_s "$settle"
    echo "$(($(ps -o rss= -p "$pid") * 1024))" >"$raw/$round.rss"
    echo "$(($(ps -M -p "$pid" | wc -l) - 1))" >"$raw/$round.threads"
    for ((sample = 1; sample <= samples; sample++)); do
        ((sample == 1)) || sleep_s 1
        footprint -p "$pid" -j "$raw/$round.footprint.$sample.json" >/dev/null 2>&1 || true
    done
    top -l $((idle + 1)) -s 1 -pid "$pid" -stats pid,cpu,idlew,time | awk -v pid="$pid" '$1 == pid' >"$raw/$round.top"
    stop_app "$pid"
    pid=

    startup_points "$raw/$round.first.log" >"$raw/$round.first.startup"
    startup_points "$raw/$round.second.log" >"$raw/$round.second.startup"
    rm -rf "$home"
    home=
done

# 汇总成中位数和最大值。
python3 -I - "$raw" "$rounds" "$json" "$binary" "$size" "$cli_tool" "$idle" "$settle" "$samples" <<'PY'
import json, os, statistics, subprocess, sys

raw, rounds, as_json, binary, size, cli_tool, idle, settle, samples = sys.argv[1:]
rounds, as_json, size, idle, samples = int(rounds), as_json == "1", int(size), int(idle), int(samples)

def read(name):
    with open(os.path.join(raw, name)) as f:
        return f.read()

def stat(values):
    values = [v for v in values if v is not None]
    if not values:
        return None
    return {"median": round(statistics.median(values), 2), "max": round(max(values), 2), "n": len(values)}

if cli_tool == "hyperfine":
    times = json.loads(read("cli.json"))["results"][0]["times"]
else:
    times = [float(t) for t in read("cli.txt").split()]
cli = stat([t * 1e3 for t in times])

def number(name):
    text = read(name).strip()
    return float(text) if text else None

# footprint 分项里看的几项：窗口的 drawable、渲染器 CPU 也能访问的纹理和缓冲、GPU 驱动的临时内存
# 和 GPU 私有的纹理。
CATEGORIES = {
    "iosurface": "IOSurface",
    "ioaccelerator_graphics": "IOAccelerator (graphics)",
    "graphics_unmapped": "Owned physical footprint (unmapped) (graphics)",
}

def footprint_stats(r):
    # 返回（峰值，稳定后那次的 phys_footprint 和分项）；一次样都没取到时是 None。
    taken = []
    for n in range(1, samples + 1):
        try:
            process = json.loads(read(f"{r}.footprint.{n}.json"))["processes"][0]
        except (OSError, ValueError, KeyError, IndexError):
            continue
        taken.append(process)
    if not taken:
        return None, None
    peak = max(p["auxiliary"]["phys_footprint_peak"] for p in taken)
    low = min(taken, key=lambda p: p["auxiliary"]["phys_footprint"])
    stable = {"phys_footprint": low["auxiliary"]["phys_footprint"]}
    for key, name in CATEGORIES.items():
        stable[key] = low["categories"].get(name, {}).get("dirty", 0)
    return peak, stable

def idle_stats(r):
    # 每行：pid %CPU IDLEW TIME。第一个样本是从进程启动累计的，不算；IDLEW 是累计的唤醒次数，
    # 取头尾之差除以秒数。
    rows = [line.split() for line in read(f"{r}.top").splitlines()]
    rows = rows[1:]
    if len(rows) < 2:
        return None, None
    cpu = statistics.mean(float(row[1]) for row in rows)
    wakeups = lambda row: int(row[2].rstrip("+-"))
    per_second = (wakeups(rows[-1]) - wakeups(rows[0])) / (len(rows) - 1)
    return cpu, per_second

def points(r, which):
    out = {}
    for line in read(f"{r}.{which}.startup").splitlines():
        node, ms = line.split("\t")
        out.setdefault(node, float(ms))
    return out

result = {
    "binary": binary,
    "machine": subprocess.run(["sysctl", "-n", "hw.model"], capture_output=True, text=True).stdout.strip(),
    "macos": subprocess.run(["sw_vers", "-productVersion"], capture_output=True, text=True).stdout.strip(),
    "rounds": rounds,
    "size_bytes": size,
    "cli_help_ms": cli,
    "cli_tool": cli_tool,
    "window_first_ms": stat([number(f"{r}.first.window") for r in range(1, rounds + 1)]),
    "window_second_ms": stat([number(f"{r}.second.window") for r in range(1, rounds + 1)]),
    "settle_s": float(settle),
    "footprint_samples": samples,
    "rss_bytes": stat([number(f"{r}.rss") for r in range(1, rounds + 1)]),
    "threads": stat([number(f"{r}.threads") for r in range(1, rounds + 1)]),
}
footprints = [footprint_stats(r) for r in range(1, rounds + 1)]
result["footprint_peak_bytes"] = stat([peak for peak, _ in footprints])
result["footprint_stable_bytes"] = {
    key: stat([stable[key] if stable else None for _, stable in footprints])
    for key in ["phys_footprint", *CATEGORIES]
}
idles = [idle_stats(r) for r in range(1, rounds + 1)]
result["idle_cpu_percent"] = stat([c for c, _ in idles])
result["idle_wakeups_per_s"] = stat([w for _, w in idles])
for which in ("first", "second"):
    per_round = [points(r, which) for r in range(1, rounds + 1)]
    nodes = []
    for p in per_round:
        nodes += [n for n in p if n not in nodes]
    result[f"startup_{which}_ms"] = {n: stat([p.get(n) for p in per_round]) for n in nodes}

if as_json:
    json.dump(result, sys.stdout, indent=2, ensure_ascii=False)
    print()
    sys.exit()

def fmt(s, unit="", scale=1, digits=1):
    if s is None:
        return "—", "—"
    return tuple(f"{s[k] / scale:.{digits}f}{unit}" for k in ("median", "max"))

stable = result["footprint_stable_bytes"]
rows = [
    ("体积", (f"{size:,} B ({size / 2**20:.2f} MiB)", "")),
    (f"命令行 help 冷启动（{cli_tool}，{cli['n']} 次）", fmt(cli, " ms", digits=2)),
    ("开窗：首次（无尺寸记录）", fmt(result["window_first_ms"], " ms")),
    ("开窗：第二次（提前启动 shell）", fmt(result["window_second_ms"], " ms")),
    (f"RSS（第二次，静置 {settle} 秒后）", fmt(result["rss_bytes"], " MiB", 2**20)),
    ("线程数（同上）", fmt(result["threads"], "", digits=0)),
    ("phys_footprint 峰值（phys_footprint_peak）", fmt(result["footprint_peak_bytes"], " MiB", 2**20)),
    (f"phys_footprint 稳定后（静置后 {samples} 次取样的最小值）", fmt(stable["phys_footprint"], " MiB", 2**20)),
    ("　其中 IOSurface", fmt(stable["iosurface"], " MiB", 2**20)),
    ("　其中 IOAccelerator (graphics)", fmt(stable["ioaccelerator_graphics"], " MiB", 2**20)),
    ("　其中 Owned physical footprint (unmapped) (graphics)", fmt(stable["graphics_unmapped"], " MiB", 2**20)),
    (f"空闲 CPU（{idle} 秒）", fmt(result["idle_cpu_percent"], " %", digits=2)),
    (f"空闲唤醒（{idle} 秒）", fmt(result["idle_wakeups_per_s"], " 次/秒", digits=2)),
]
for which, title in (("first", "首次"), ("second", "第二次")):
    for node, s in result[f"startup_{which}_ms"].items():
        rows.append((f"启动点（{title}）{node}", fmt(s, " ms")))

print(f"{binary}（{result['machine']}，macOS {result['macos']}，{rounds} 轮）")
print()
print("| 项目 | 中位数 | 最大值 |")
print("| --- | ---: | ---: |")
for name, (median, maximum) in rows:
    print(f"| {name} | {median} | {maximum} |")
PY
