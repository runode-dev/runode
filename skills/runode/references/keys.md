# Control keys and pasting

```sh
runode send right --key ctrl-c                 # interrupt the running program
runode send right --key 'down*3' --key enter   # press down three times, then Enter
runode send right --key esc                    # leave insert mode in vim...
runode send right ':wq' --enter                # ...then save and quit
runode send right --paste "$(cat snippet.py)"  # paste instead of typing
```

A key is a letter, digit or one of `` - = [ ] \ ; ' , . / ` ``, or a name:
`esc tab enter backspace delete insert space up down left right home end pageup
pagedown f1`..`f12`. Put `ctrl-`, `alt-` or `shift-` in front, stacked if need
be (`shift-tab`, `ctrl-alt-x`); `'down*3'` repeats (quote it: the shell would
expand the `*`).
They are encoded for whatever the program has switched on (application cursor
keys, the kitty keyboard protocol), as if the user pressed them.

One `send` types the text first, then presses the keys, then Enter; for another
order, use several `send` commands. Text is typed literally; `--paste` sends it
as a paste, which multi-line input and editors handle better. `-` instead of
TEXT reads it from stdin. A last word `Enter` presses Enter like `--enter` (the
tmux habit); put `--` before text that has to end with the word Enter.
