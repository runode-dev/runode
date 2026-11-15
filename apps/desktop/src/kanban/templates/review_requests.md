---
title: Review requested PR
every: 10
run: "{{script:review-requests}}"
auto: true
auto_done: true
time_limit: 30
---
Review the pull request shown in the output below, without stopping to ask. Its number is at the top of the output.

1. Read it with `gh pr view <number> --comments` and `gh pr diff <number>`, and read the code around what it changes.
2. Look for bugs, missing tests, security problems and anything that goes against the project's conventions. Do not change any files.
3. Post the review with `gh pr review <number> --comment --body-file <file>`, giving the file and line of each finding, or saying that you found nothing to fix. Do not approve or request changes: a person decides that.
4. Finish by printing the review.

Write the review the way a teammate who knows the code would, not like an AI assistant: plain, specific sentences in the language the project already writes in; no greetings, thanks, praise or offers to help further; no filler such as "it's worth noting" or "overall"; no headings, bold labels or bullet lists where a few sentences will do; no emoji.
