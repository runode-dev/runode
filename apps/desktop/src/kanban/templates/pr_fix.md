---
title: PR {{input}}
auto: true
worktree: true
---
Get pull request {{input}} ready to merge, without stopping to ask.

1. Check it out with `gh pr checkout {{input}}`.
2. Read the review comments (`gh pr view {{input}} --comments`, and `gh api repos/{owner}/{repo}/pulls/<number>/comments` for the ones on lines) and the failing checks (`gh pr checks {{input}}`, then `gh run view <run id> --log-failed`).
3. Address every review comment and fix every failing check. Run the tests and linters until they pass.
4. Commit, push to the pull request's branch, and reply to each review comment you addressed.
5. Finish by summing up what you changed.
