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
5. Wait for the pull request's checks with `gh pr checks <number> --watch`. If any fail, find out why, fix it, push to the same branch and wait again, until they all pass. A check that also fails the same way on the latest commit of the default branch was not caused by this pull request: leave it, and name it when you finish. If no checks are reported, look again after a minute; if there are still none, the repository runs no checks for pull requests, so go on.
6. Finish by printing the pull request's URL.

Write the commit message and the pull request the way a teammate who knows the code would, not like an AI assistant: plain, specific sentences in the language the project already writes in; no greetings, thanks, praise or offers to help further; no filler such as "it's worth noting" or "overall"; no headings, bold labels or bullet lists where a few sentences will do; no emoji.
