---
title: "Issue labeled {{input}}"
input_default: agent
every: 15
run: "{{script:new-issues}} {{input}}"
auto: true
auto_done: true
worktree: true
time_limit: 60
---
Resolve the GitHub issue shown in the output below end to end, without stopping to ask. Its number is at the top of the output.

1. Read it with `gh issue view <number> --comments`, and read the code it is about.
2. Fix it. Add or update tests that cover the fix, and run the project's tests and linters until they pass.
3. Commit with a message that explains the change and ends with `Fixes #<number>`.
4. Push the branch and open a pull request with `gh pr create`, with `Fixes #<number>` in its body.
5. Finish by printing the pull request's URL.
