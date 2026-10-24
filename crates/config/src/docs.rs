//! 配置模板里的英文说明：每个配置项能填什么值，以及模板开头和各段的提示。模板只有英文，
//! 所以不经翻译，直接写在这里；动作的说明在 `ActionSpec::doc`。

pub(crate) const HEADER: &str = "Runode configuration file. Syntax and key names are the same as Ghostty's.\n\
     Runode reads the Ghostty configuration first, then this file; when both set a key, this file wins.\n\
     Lines starting with ## are explanations; lines starting with a single # are commented-out defaults.\n\
     Remove that # and change the value to apply it; the file reloads automatically when saved.\n\
     The accepted values are described above each setting. An empty value (e.g. `background =`) restores the default.\n\
     Colors can be #RRGGBB, #RGB (the # is optional), an X11 color name (e.g. white, dark slate gray),\n\
     or rgb:RR/GG/BB.";
pub(crate) const ACTIONS: &str = "Available actions; ones with a parameter are written ACTION:PARAM:";
pub(crate) const THEMES: &str = "Available themes:";

/// 配置项 `key` 的说明。`agent-notifications-exclude` 的 `%{agents}` 是 agent 的短名，由模板填上。
pub(crate) fn config_doc(key: &str) -> &'static str {
    match key {
        "language" => {
            "Interface language: a language tag with a translation, such as en, zh-Hans or zh-Hant; the Language\n\
     setting in the settings page lists them all. Empty follows the system, and uses English when the\n\
     language has no translation."
        }
        "font-family" => {
            "Font family: the name of an installed font. Can be repeated; the first one available is used."
        }
        "font-size" => "Font size: a positive number, in points.",
        "adjust-cell-height" => {
            "Cell height adjustment: pixels (e.g. 2, -1) or a percentage (e.g. 10%); empty means no adjustment."
        }
        "window-padding-x" => "Left and right padding: one number for both sides, or left,right, in points.",
        "window-padding-y" => "Top and bottom padding: one number for both sides, or top,bottom, in points.",
        "window-style" => {
            "Window style: cards (default) or classic. With cards, each split and side panel is a rounded card with a title bar on a slightly darker frame, and tabs are pill-shaped. With classic, the terminal fills the window and splits are divided by thin lines."
        }
        "ui-font-size" => {
            "Base font size of the interface, in points: 11 to 20, 12 by default. All text scales with it except the terminal and the body of the preview pane."
        }
        "status-bar" => {
            "Show the status bar at the bottom of the window: true (default) or false. Toggle it from the View menu or by right-clicking the status bar."
        }
        "status-bar-hidden" => {
            "Parts of the status bar at the bottom of the window to hide: sleep (prevent sleep), resources (memory and terminals), ports; separated by commas, can be repeated, empty shows them all. Right-click the status bar to pick them."
        }
        "file-tree-preview-click" => {
            "How to open a file from the file tree in the preview pane: single (default) or double. With single, a click opens a temporary tab that the next opened file replaces, and a double-click keeps it open. With double, a double-click opens a tab that stays."
        }
        "preview-font-size" => "Preview pane font size: a positive number, in points.",
        "dictation-model" => {
            "Speech recognition model for dictation: sense-voice (default) or fire-red-asr2. Both run offline on this computer and are downloaded the first time from the settings page. sense-voice (about 240 MB) is fast and knows Chinese, English, Japanese, Korean and Cantonese; fire-red-asr2 (about 1.2 GB) is the most accurate for Chinese and English but slower and writes no punctuation."
        }
        "dictation-words" => {
            "Words dictation should get right, such as project names and technical terms; separated by commas, can be repeated, empty clears them. The offline models do not take them; they are passed to the cloud recognizer and to the polishing model."
        }
        "dictation-cloud" => {
            "Cloud recognizer used when the offline model is not downloaded or fails: none (default), openai or doubao. openai is any service with an OpenAI-compatible /audio/transcriptions endpoint; doubao is Volcengine's Doubao flash recognition. The recording is uploaded to that service."
        }
        "dictation-openai-url" => {
            "Base URL of the OpenAI-compatible API for cloud recognition, https://api.openai.com/v1 by default; /audio/transcriptions is appended."
        }
        "dictation-openai-key" => "API key for dictation-openai-url.",
        "dictation-openai-model" => "Transcription model at dictation-openai-url, gpt-4o-transcribe by default.",
        "dictation-doubao-app-id" => {
            "App ID of a Doubao speech app from the old Volcengine console, with its access token in dictation-doubao-key. Leave it empty when dictation-doubao-key is an API key from the new console."
        }
        "dictation-doubao-key" => "API key (new console) or access token (old console) for Doubao recognition.",
        "dictation-polish-url" => {
            "Base URL of an OpenAI-compatible API (/chat/completions is appended) whose model cleans up recognized text: fixes misheard words, punctuation and casing. Empty (default) turns polishing off. When polishing fails, the text is typed as recognized."
        }
        "dictation-polish-key" => "API key for dictation-polish-url.",
        "dictation-polish-model" => "Chat model at dictation-polish-url that polishes the text.",
        "task-placement" => {
            "Where the title bar's task menu runs a task: right (default), down or tab. right and down split the current terminal in the current tab and put the new one to its right or below; tab opens a new tab to the right of the current one."
        }
        "theme" => {
            "Theme: a theme name listed at the end of this file, or the absolute path of a theme file. It can also\n\
     follow the system appearance, e.g. light:Catppuccin Latte,dark:Catppuccin Mocha. The theme applies first,\n\
     and colors set below override it. Theme files in the runode/themes and ghostty/themes directories\n\
     can be used by file name too."
        }
        "background" => "Background color: a color.",
        "foreground" => "Foreground (text) color: a color.",
        "cursor-color" => {
            "Cursor color: a color, or cell-foreground / cell-background (follow the cell under the cursor);\n\
     empty uses the foreground color."
        }
        "cursor-text" => "Text color under a block cursor: same as above; empty uses the background color.",
        "selection-background" => {
            "Selection background: a color, or cell-foreground / cell-background; empty uses a blue that suits\n\
     the background."
        }
        "selection-foreground" => {
            "Selection text color: same as above; when empty, uses the cell background if a selection background\n\
     is set, and keeps the original text color otherwise."
        }
        "search-background" => "Search match background: a color, or cell-foreground / cell-background.",
        "search-foreground" => "Search match text color: same as above.",
        "search-selected-background" => "Background of the current search match: same as above.",
        "search-selected-foreground" => "Text color of the current search match: same as above.",
        "palette" => {
            "Palette: N=COLOR with N from 0 to 255, can be repeated; 0-7 are the normal colors, 8-15 the bright ones."
        }
        "minimum-contrast" => {
            "Minimum contrast between text and its background: 1 to 21, 3 by default. When a program's fixed colors are hard to read on the current background (say, a program with a dark color scheme under a light theme), text is drawn darker or lighter until it reaches this contrast, keeping its hue; box drawing, block elements and other symbols that form shapes are left alone. 1 turns it off."
        }
        "cursor-style" => {
            "Cursor shape: block, bar, underline, block_hollow. Shown while a command runs and in programs that leave the cursor alone; at the prompt the shell sets a bar unless shell-integration-features says no-cursor."
        }
        "cursor-style-blink" => {
            "Cursor blinking: true, false (the default, the cursor stays solid); running programs can still turn blinking on or off."
        }
        "cursor-style-blink-timeout" => {
            "When the cursor blinks, stop blinking and keep it solid after this many seconds without typing, terminal output or the window regaining focus; any of those starts it blinking again. 0 keeps it blinking."
        }
        "macos-option-as-alt" => {
            "Treat Option as Alt on macOS: true (both sides), false, left (left only), right (right only). Unset, it follows the keyboard layout: both sides on U.S. and ABC layouts and under Apple's Chinese input methods, whose Option characters are rarely typed, and off on other layouts."
        }
        "scrollback-limit" => {
            "How much scrollback each terminal keeps, in bytes: a whole number; anything below 2 MiB (2097152) counts as 2 MiB. At most 10,000 lines are kept as well; whichever limit is reached first applies. Changes apply to open terminals too."
        }
        "shell-integration" => {
            "Shell integration: detect (by the shell's program name), none (off), bash, zsh, fish, elvish, nushell. Naming a shell only matters when the program name is not recognized (say a symlink called sh); a program recognized as another shell gets its own integration."
        }
        "shell-integration-features" => {
            "What else shell integration does, written as in Ghostty: cursor (the default) turns the cursor into a bar at the prompt, a block in zsh's vi command mode, and back to cursor-style while a command runs; no-cursor leaves the cursor alone. The bar blinks if cursor-style-blink says so. Ghostty's sudo, title, ssh-env, ssh-terminfo and path are accepted but do nothing. Only applies to shells started afterwards."
        }
        "clipboard-write" => {
            "What happens when a program in the terminal writes the system clipboard with OSC 52 (vim, tmux and remote shells over ssh copy this way): allow writes it, deny drops it. ask is not supported yet and counts as deny."
        }
        "clipboard-read" => {
            "What happens when a program in the terminal asks for the clipboard with OSC 52: ask shows a dialog naming the terminal and the program first, allow hands over the text without asking, deny answers with an empty clipboard. The clipboard may hold passwords, so only allow programs you trust."
        }
        "vt-window-resize-allowed" => {
            "Let a program in the terminal resize the window with the xterm sequence CSI 8 ; rows ; columns t: true, false. A zero or missing size keeps that dimension, sizes below 40 columns by 10 rows are raised to that, and the window never grows past the screen. The request is ignored unless the terminal is showing, has its tab to itself (no splits), and the window is not full screen. Off by default, since any program, including one on a remote machine over ssh, could change the size of your window."
        }
        "terminal-host" => {
            "Keep terminal sessions running in the background after Runode quits: true, false. With true, quitting Runode leaves the sessions running and the next launch picks them up; with false they end when Runode quits. Takes effect right away. Switching it off while sessions are still running in the background brings them back once more; quitting then ends them."
        }
        "auto-update" => {
            "Check for a new version of Runode when it starts and every few hours after that: true, false. With true, a new version is downloaded in the background and installed when you quit Runode; with false, Runode only checks when you choose Check for Updates in the Runode menu. Only a Runode.app signed by its publisher updates itself."
        }
        "remote-access" => {
            "Let your phone reach this Mac's terminals over the local network or Tailscale: true, false. Pair a phone with `runode remote pair`, list paired phones with `runode remote devices`, and remove one with `runode remote revoke`. Takes effect within a few seconds, without restarting Runode."
        }
        "remote-access-port" => "The TCP port remote access listens on, on all network interfaces: 1 to 65535.",
        "remote-access-name" => {
            "The name paired phones show for this computer. When empty, the computer name from System Settings › General › Sharing. Takes effect within a few seconds; connected phones drop briefly and reconnect."
        }
        "remote-access-push" => {
            "When an agent waits for your answer, send a notification to paired phones, even when the Runode app on the phone is closed: true, false. The phone registers for this when it connects; the official app's pushes go through the push relay, an app you build yourself needs the apns-* keys below."
        }
        "remote-access-push-text" => {
            "Include the question and choices from the bottom of the screen in the notification: true, false. With false only the title and the agent are sent."
        }
        "remote-access-push-delay" => {
            "Seconds an agent has to keep waiting before the phone is notified, so quick answers at the computer don't buzz the phone: a whole number, 0 notifies right away."
        }
        "apns-key-file" => {
            "Push straight to Apple instead of through the relay, for an app you sign yourself: the .p8 key file downloaded from your Apple developer account; ~/ for the home directory. Only used together with apns-key-id, apns-team-id and apns-bundle-id."
        }
        "apns-key-id" => "The ID of that key (10 characters, also in the file name AuthKey_<ID>.p8).",
        "apns-team-id" => "Your Apple developer team ID (10 characters).",
        "apns-bundle-id" => {
            "The bundle ID of your build of the phone app; registrations from this app are pushed with your key."
        }
        "agent-notifications" => {
            "Post a system notification when an agent is waiting for your answer or has finished in a split you are not looking at: true, false. Notifications only work in the packaged Runode.app."
        }
        "agent-notifications-exclude" => {
            "Agents that get no notification and no sound (their marks still show): short names separated by commas,\n\
     can be repeated; empty clears the list. Names: %{agents}."
        }
        "agent-done-sound" => {
            "Sound when an agent finishes in a split you are not looking at: a macOS system sound name (Basso, Blow, Bottle, Frog, Funk, Glass, Hero, Morse, Ping, Pop, Purr, Sosumi, Submarine, Tink), or none."
        }
        "agent-blocked-sound" => {
            "Sound when an agent waits for your answer in a split you are not looking at: same as above."
        }
        "agent-split-fold" => {
            "Fold the split an agent opens with `runode open --right` or `--down` from its own terminal, so it stays out of your way: true (default), false. `--fold` and `--no-fold` on the command line win. Only works with the cards window style."
        }
        "config-file" => {
            "Include another configuration file: a path, relative to this file's directory, ~/ for the home\n\
     directory; prefix it with ? if the file may not exist. Can be repeated."
        }
        "keybind" => {
            "Key bindings: TRIGGER=ACTION, can be repeated; the last line for a trigger wins.\n\
     A trigger joins modifiers and a key with +; the modifiers are cmd, ctrl, alt, shift. Separate a key\n\
     sequence with >, e.g. ctrl+a>n. A key is the character itself or a name such as left, right, up,\n\
     down, page_up, page_down, home, end, enter, escape, tab, backspace, delete, space, equal, plus,\n\
     minus, f1 to f24. The key digit stands for 1 through 9 and binds all nine at once; goto_tab and\n\
     goto_workspace without a parameter then go to the pressed number, 9 to the last one, e.g.\n\
     cmd+digit=goto_tab.\n\
     TRIGGER=unbind removes a binding; keybind = clear removes every earlier binding, including the\n\
     defaults below.\n\
     Only Runode's own configuration file is read for keybind; keybind in the Ghostty configuration has\n\
     no effect.\n\
     Default bindings:"
        }
        _ => "",
    }
}
