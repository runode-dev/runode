---
title: Fix failing CI
every: 15
run: "{{script:watch-ci}}"
auto: true
auto_done: true
worktree: true
time_limit: 60
---
The output below lists the workflow runs that failed on the default branch, with the end of their logs. Fix them, without stopping to ask.

1. Fetch the remote and check whether the failure still happens on the latest commit of the default branch: a later commit may have fixed it. If it has, change nothing and finish with one sentence naming the commit that fixed it.
2. Find the root cause. When you need more of a log, read it with `gh run view <run id> --log-failed`.
3. If the failure has an outside cause, such as a flaky network, a broken runner or an expired secret, do not change any code; finish with one sentence that says what the cause is.
4. Otherwise fix it, changing only the files the fix needs, and run the checks related to the failure until they pass.
5. Commit on the current branch, push the branch, and open a pull request with `gh pr create`.
6. Wait for the pull request's checks with `gh pr checks <number> --watch`. If any fail, find out why, fix it, push to the same branch and wait again, until they all pass. The checks that failed on the default branch, which this pull request fixes, must pass. Any other check that also fails the same way on the latest commit of the default branch was not caused by this pull request: leave it, and name it when you finish. If no checks are reported, look again after a minute; if there are still none, the repository runs no checks for pull requests, so go on.
7. Finish by printing the pull request's URL.

Write the commit message and the pull request the way a teammate who knows the code would, not like an AI assistant: plain, specific sentences in the language the project already writes in; no greetings, thanks, praise or offers to help further; no filler such as "it's worth noting" or "overall"; no headings, bold labels or bullet lists where a few sentences will do; no emoji.
