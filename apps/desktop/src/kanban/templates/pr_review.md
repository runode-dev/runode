---
title: Review PR {{input}}
auto: true
---
Review pull request {{input}}, without stopping to ask.

1. Read it with `gh pr view {{input}} --comments` and `gh pr diff {{input}}`, and read the code around what it changes.
2. Look for bugs, missing tests, security problems and anything that goes against the project's conventions. Do not change any files.
3. Post the review with `gh pr review {{input}} --comment --body-file <file>`, giving the file and line of each finding; when there is nothing to fix, approve it with `--approve` instead.
4. Finish by printing the review.
