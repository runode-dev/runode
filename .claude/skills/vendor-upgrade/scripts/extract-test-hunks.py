#!/usr/bin/env python3
"""从 stdin 的 git diff 里只留下改 Zig 测试的 hunk：hunk 头的上下文是 `test "…"`，或者它新加的第一行是 `test "…"`。

用来拿 fork 上某个修复带的测试、不带修复代码，去跑新上游：
    git -C vendor/ghostty show --format= <提交> | python3 -I extract-test-hunks.py > tests.patch
"""
import re
import sys

out = []
file_header = []
hunk = []
kept_header = False


def flush():
    global hunk, kept_header
    if not hunk:
        return
    header = hunk[0]
    added = [l for l in hunk[1:] if l.startswith("+")]
    is_test = re.search(r'@@ test "', header) or (added and added[0].startswith('+test "'))
    if is_test:
        if not kept_header:
            out.extend(file_header)
            kept_header = True
        out.extend(hunk)
    hunk = []


for line in sys.stdin.read().splitlines(keepends=True):
    if line.startswith("diff --git"):
        flush()
        file_header = [line]
        kept_header = False
    elif line.startswith("@@"):
        flush()
        hunk = [line]
    elif hunk:
        hunk.append(line)
    elif file_header:
        file_header.append(line)
flush()
sys.stdout.write("".join(out))
