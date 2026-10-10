# Typical workflows

Run tests next to you and look at the failures:

```sh
runode send right 'cargo test 2>&1 | tail -50' --enter --wait --timeout 900
runode read right --command
```

Start a dev server once and check on it later:

```sh
id=$(runode open --down --cwd ~/src/app)
runode send "$id" 'npm run dev' --enter
runode wait "$id" --for text 'ready|error' --timeout 120
runode read "$id" --lines 30
```

Ask another agent to do something and collect the answer. Start it in a
terminal of your own rather than typing into an agent the user is talking to:

```sh
id=$(runode open --right -- codex)
runode wait "$id" --for idle --timeout 60
runode send "$id" 'Review the diff in src/parser.rs and list bugs' --enter --wait --timeout 1800
runode read "$id" --lines 80
```
