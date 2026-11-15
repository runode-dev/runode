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

Write the commit message and your replies the way a teammate who knows the code would, not like an AI assistant: plain, specific sentences in the language the project already writes in; no greetings, thanks, praise or offers to help further; no filler such as "it's worth noting" or "overall"; no headings, bold labels or bullet lists where a few sentences will do; no emoji.
