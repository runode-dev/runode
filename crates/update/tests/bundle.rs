use runode_update::bundle::{expected_sum, sha256};

#[test]
fn finds_the_sum_of_a_file_in_sha256sums() {
    let sums = "AB12  Runode-0.2.0-linux-x86_64.tar.gz\ncd34 *Runode-0.2.0-macos-arm64.dmg\nef56  latest.json\n";
    assert_eq!(expected_sum(sums, "Runode-0.2.0-linux-x86_64.tar.gz").as_deref(), Some("ab12"));
    assert_eq!(expected_sum(sums, "Runode-0.2.0-macos-arm64.dmg").as_deref(), Some("cd34"));
    assert_eq!(expected_sum(sums, "Runode-0.2.0-linux-aarch64.tar.gz"), None);
    assert_eq!(expected_sum(sums, "json"), None);
}

#[test]
fn hashes_a_file() {
    let dir = std::env::temp_dir().join(format!("runode-bundle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("abc");
    std::fs::write(&file, b"abc").unwrap();
    assert_eq!(sha256(&file).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    std::fs::remove_dir_all(&dir).unwrap();
}
