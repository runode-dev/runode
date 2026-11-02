use std::path::Path;

use runode_paths::home_relative;

#[test]
fn paths_under_home_start_with_a_tilde() {
    let home = Path::new("/home/me");
    assert_eq!(home_relative(Path::new("/home/me"), home, '/').as_deref(), Some("~"));
    assert_eq!(home_relative(Path::new("/home/me/src/a"), home, '/').as_deref(), Some("~/src/a"));
    assert_eq!(home_relative(Path::new("/home/me/src"), home, '\\').as_deref(), Some("~\\src"));
    assert_eq!(home_relative(Path::new("/home/meow"), home, '/'), None);
    assert_eq!(home_relative(Path::new("/etc"), home, '/'), None);
}
