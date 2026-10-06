//! 设置界面改配置文件：换掉一个键的设置行，别的行原样留着；以及写之前的检查。

use runode_config::{ConfigFile, check_value};

const FILE: &str = "\
## 说明
# font-family = Hack Nerd Font Mono
# font-size = 14
font-size = 16

## 主题
# theme =
background = #000000
";

fn set(text: &str, key: &str, values: &[&str]) -> String {
    let mut file = ConfigFile::parse(text);
    file.set(key, &values.iter().map(|v| v.to_string()).collect::<Vec<_>>());
    file.text()
}

#[test]
fn replaces_the_lines_of_a_key_in_place() {
    let file = ConfigFile::parse(FILE);
    assert_eq!(file.values("font-size"), ["16"]);
    assert!(file.has("background") && !file.has("theme"));
    assert_eq!(set(FILE, "font-size", &["18"]), FILE.replace("font-size = 16", "font-size = 18"));
    // 删掉写的值，注释掉的默认值留着。
    assert_eq!(set(FILE, "font-size", &[]), FILE.replace("font-size = 16\n", ""));
    // 多个值每项一行，放在原来第一行的位置。
    let text = "a = 1\nfont-family = X\nb = 2\nfont-family = Y\n";
    assert_eq!(set(text, "font-family", &["P", "Q"]), "a = 1\nfont-family = P\nfont-family = Q\nb = 2\n");
}

#[test]
fn new_keys_go_below_their_commented_default() {
    assert_eq!(
        set(FILE, "font-family", &["Menlo"]),
        FILE.replace(
            "# font-family = Hack Nerd Font Mono\n",
            "# font-family = Hack Nerd Font Mono\nfont-family = Menlo\n"
        )
    );
    assert_eq!(set(FILE, "theme", &["Dracula"]), FILE.replace("# theme =\n", "# theme =\ntheme = Dracula\n"));
    // 模板里没有的键放在按 `KEYS` 排在前面、文件里写到了的键后面。
    assert_eq!(
        set(FILE, "adjust-cell-height", &["10%"]),
        FILE.replace("font-size = 16\n", "font-size = 16\nadjust-cell-height = 10%\n")
    );
    // 都没有就接在末尾，前面空一行。
    assert_eq!(set("# 我的配置\n", "language", &["en"]), "# 我的配置\n\nlanguage = en\n");
    assert_eq!(set("", "language", &["en"]), "language = en\n");
    // 写空值和两头有空格的值。
    assert_eq!(set("", "agent-done-sound", &[""]), "agent-done-sound =\n");
    assert_eq!(set("", "keybind", &[" x"]), "keybind = \" x\"\n");
    assert_eq!(ConfigFile::parse("keybind = \" x\"\n").values("keybind"), [" x"]);
    // 没写过又要删，什么也不做。
    assert_eq!(set(FILE, "theme", &[]), FILE);
}

#[test]
fn checks_values_like_the_loader() {
    assert!(check_value("font-size", "15").is_ok());
    assert!(check_value("font-size", "big").is_err());
    assert!(check_value("remote-access-port", "70000").is_err());
    assert!(check_value("keybind", "cmd+t=new_tab").is_ok());
    assert!(check_value("keybind", "cmd+t=nope").is_err());
    assert!(check_value("no-such-key", "1").is_err());
}
