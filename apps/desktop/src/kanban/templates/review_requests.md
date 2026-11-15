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
3. Post the review with `gh pr review <number> --comment --body-file <file>`, giving the file and line of each finding; when there is nothing to fix, approve it with `--approve` instead.
4. Finish by printing the review.
