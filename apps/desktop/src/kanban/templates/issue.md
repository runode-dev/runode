---
title: Issue {{input}}
auto: true
worktree: true
---
Resolve GitHub issue {{input}} end to end, without stopping to ask.

1. Read it with `gh issue view {{input}} --comments`, and read the code it is about.
2. Fix it. Add or update tests that cover the fix, and run the project's tests and linters until they pass.
3. Commit with a message that explains the change and ends with `Fixes #<issue number>`.
4. Push the branch and open a pull request with `gh pr create`, linking the issue in its body.
5. Finish by printing the pull request's URL.

Write the commit message and the pull request the way a teammate who knows the code would, not like an AI assistant: plain, specific sentences in the language the project already writes in; no greetings, thanks, praise or offers to help further; no filler such as "it's worth noting" or "overall"; no headings, bold labels or bullet lists where a few sentences will do; no emoji.
