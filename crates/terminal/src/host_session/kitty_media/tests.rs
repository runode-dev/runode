use std::time::Duration;

use super::*;

fn control(keys: &str) -> Option<Control> {
    parse_control(keys.as_bytes())
}

fn base64(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_base64(data, &mut out);
    out
}

#[test]
fn control_keys_parse_when_rewritable() {
    let parsed = control("a=T,t=f,f=100,S=10,O=3,q=2,i=7,m=0").unwrap();
    assert_eq!(parsed.medium, Medium::File);
    assert_eq!((parsed.offset, parsed.size), (3, 10));
    assert_eq!(parsed.kept, b"a=T,f=100,q=2,i=7");
    assert_eq!(parsed.quiet.as_deref(), Some(&b"2"[..]));
    assert!(!parsed.query);

    let parsed = control("t=t").unwrap();
    assert_eq!(parsed.medium, Medium::TemporaryFile);
    assert_eq!((parsed.offset, parsed.size), (0, 0));
    assert!(parsed.kept.is_empty());

    assert!(control("a=q,t=f,i=31,s=1,v=1,f=24").unwrap().query);
    for action in ["t", "T", "f"] {
        assert!(control(&format!("a={action},t=f")).is_some(), "{action}");
    }
}

#[test]
fn control_keys_not_rewritten() {
    for keys in [
        "",
        "f=100",
        "t=d",
        "t=s",
        "t=ff",
        "t=f,t=f",
        "t=f,a=T,a=T",
        "tt=f",
        "t=f,ab=1",
        "t=f,m=1",
        "t=f,m=00",
        "a=p,t=f",
        "a=d,t=f",
        "t=f,",
        "t=f,,a=T",
        "t=",
        "t",
        "=f",
        "t=f,S=x",
        "t=f,O=-1",
        "t=f,S=4294967296",
        "t=f,i= 1",
        "t=f,i=1;",
        "1=2,t=f",
    ] {
        assert_eq!(control(keys), None, "{keys:?}");
    }
}

#[test]
fn a_single_chunk_drops_the_file_keys() {
    let parsed = control("a=T,t=f,f=100,S=3,O=1,q=2,i=7").unwrap();
    assert_eq!(rewrite(&parsed, b"abc"), b"Ga=T,f=100,q=2,i=7;YWJj");
    // 正好一块也不写 `m`。
    let data = vec![7; KITTY_CHUNK_BYTES / 4 * 3];
    let mut expected = b"Ga=T,f=100,q=2,i=7;".to_vec();
    expected.extend_from_slice(&base64(&data));
    assert_eq!(rewrite(&parsed, &data), expected);
    assert_eq!(rewrite(&control("t=t").unwrap(), b""), b"G;");
}

#[test]
fn several_chunks_carry_m_and_q() {
    let data: Vec<u8> = (0..KITTY_CHUNK_BYTES / 4 * 3 * 2 + 3).map(|n| n as u8).collect();
    let encoded = base64(&data);
    let (a, b, c) = (
        &encoded[..KITTY_CHUNK_BYTES],
        &encoded[KITTY_CHUNK_BYTES..2 * KITTY_CHUNK_BYTES],
        &encoded[2 * KITTY_CHUNK_BYTES..],
    );
    assert_eq!(c.len(), 4);

    let expected = [&b"Ga=T,f=100,q=2,m=1;"[..], a, b"\x1b\\\x1b_Gm=1,q=2;", b, b"\x1b\\\x1b_Gm=0,q=2;", c].concat();
    assert_eq!(rewrite(&control("a=T,t=f,f=100,q=2,S=9").unwrap(), &data), expected);

    let expected = [&b"Gm=1;"[..], a, b"\x1b\\\x1b_Gm=1;", b, b"\x1b\\\x1b_Gm=0;", c].concat();
    assert_eq!(rewrite(&control("t=t").unwrap(), &data), expected);

    let expected = [&b"Ga=q,i=31;"[..], &encoded].concat();
    assert_eq!(rewrite(&control("a=q,t=f,i=31").unwrap(), &data), expected);
}

/// 照宿主那份 VT 的用法，把 APC 引导之后的 `chunks` 依次交给 `MediumRewriter`，拼出喂给 VT 的字节。
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn feed(chunks: &[&[u8]]) -> Vec<u8> {
    let now = Instant::now();
    let mut rewriter = MediumRewriter::default();
    rewriter.start();
    let mut fed = Vec::new();
    for chunk in chunks {
        let mut rest = *chunk;
        while rewriter.holding() {
            match rewriter.step(rest, now) {
                Step::Held => {
                    rest = &[];
                    break;
                }
                Step::Pass(used) => {
                    fed.extend_from_slice(&rest[..used]);
                    rest = &rest[used..];
                }
                Step::Replace(bytes, used) => {
                    fed.extend_from_slice(&bytes);
                    rest = &rest[used..];
                }
            }
        }
        fed.extend_from_slice(rest);
    }
    fed
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn commands_that_are_not_rewritten_pass_through() {
    for stream in [
        &b"Ga=T,t=f\x1b\\after"[..],
        b"Xa=T,t=f;AAAA\x1b\\",
        b"Ga=T,t=d;AAAA\x1b\\",
        b"Gt=f;L25vbmU=\x1b\\",
        b"Gt=f;not base64\x1b\\",
        b"Gt=f;L25vbm\nU=\x1b\\",
        b"\x1b\\",
        b"\x9c",
    ] {
        assert_eq!(feed(&[stream]), stream, "{:?}", String::from_utf8_lossy(stream));
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn passing_through_starts_as_soon_as_it_is_known() {
    for (content, used) in
        [(&b"Xa=T"[..], 0), (b"Ga=T,t=d;AAAA", 0), (b"Gt=f,m=1;", 0), (b"Gt=f\x01", 5), (b"Gt=f\x7f;", 0)]
    {
        let mut rewriter = MediumRewriter::default();
        rewriter.start();
        assert_eq!(rewriter.step(content, Instant::now()), Step::Pass(used), "{:?}", String::from_utf8_lossy(content));
        assert!(!rewriter.holding());
    }

    // 扣过一部分以后再放行，扣下的要一起交出去。
    let now = Instant::now();
    let mut rewriter = MediumRewriter::default();
    rewriter.start();
    let long = [&b"Gt=f;"[..], &vec![b'A'; HELD_MAX_BYTES]].concat();
    assert_eq!(rewriter.step(&long[..HELD_MAX_BYTES], now), Step::Held);
    assert_eq!(rewriter.step(&long[HELD_MAX_BYTES..], now), Step::Replace(long[..=HELD_MAX_BYTES].to_vec(), 1));
    assert!(!rewriter.holding());
}

#[test]
fn not_holding_uses_nothing() {
    let mut rewriter = MediumRewriter::default();
    assert!(!rewriter.holding());
    assert_eq!(rewriter.step(b"Gt=f;AAAA\x1b\\", Instant::now()), Step::Pass(0));
}

#[test]
fn the_budget_refills_over_time() {
    let start = Instant::now();
    let at = |nanos: u64| start + Duration::from_nanos(nanos);
    let mut budget = Budget::default();
    assert_eq!(budget.left(start), IMAGE_FILE_MAX_BYTES);
    budget.spend(IMAGE_FILE_MAX_BYTES);
    assert_eq!(budget.left(start), 0);
    assert_eq!(budget.left(at(1_000_000_000)), REWRITE_REFILL_BYTES_PER_SEC);
    assert_eq!(budget.left(at(60_000_000_000)), IMAGE_FILE_MAX_BYTES);

    // 每次只隔一纳秒、不满一个字节地问，攒够了照样补上。
    budget.spend(IMAGE_FILE_MAX_BYTES);
    let base = 60_000_000_000;
    for nanos in 1..=1_000 {
        budget.left(at(base + nanos));
    }
    let left = budget.left(at(base + 1_000));
    assert!(left > 0 && left <= REWRITE_REFILL_BYTES_PER_SEC * 1_000 / 1_000_000_000, "{left}");
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod files {
    use std::{
        fs, io,
        os::{fd::AsRawFd, unix::ffi::OsStrExt},
        path::{Path, PathBuf},
    };

    use super::{super::read, *};

    /// 最多能读 `IMAGE_FILE_MAX_BYTES` 个字节的 `read::read_file`。
    fn read_file(medium: Medium, path: &[u8], offset: u64, size: u64) -> io::Result<Vec<u8>> {
        read::read_file(medium, path, offset, size, IMAGE_FILE_MAX_BYTES)
    }

    /// 测试自己的目录，在系统的临时目录里。
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("runode-kitty-media-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bytes(path: &Path) -> &[u8] {
        path.as_os_str().as_bytes()
    }

    fn apc(keys: &str, path: &Path, end: &[u8]) -> Vec<u8> {
        [format!("G{keys};").as_bytes(), &base64(bytes(path)), end].concat()
    }

    #[test]
    fn a_file_command_becomes_direct_however_it_is_cut() {
        let scratch = Scratch::new("cuts");
        let path = scratch.file("image.png", b"abc");
        let stream = [apc("a=T,t=f,f=100", &path, b"\x1b\\"), b"after".to_vec()].concat();
        let expected = b"Ga=T,f=100;YWJj\x1b\\after";
        assert_eq!(feed(&[&stream]), expected);
        for cut in 0..=stream.len() {
            assert_eq!(feed(&[&stream[..cut], &stream[cut..]]), expected, "cut at {cut}");
        }
        let bytewise: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(feed(&bytewise), expected);

        let stream = apc("t=f,q=1", &path, b"\x9c");
        assert_eq!(feed(&[&stream]), b"Gq=1;YWJj\x9c");
    }

    #[test]
    fn a_large_file_is_sent_in_chunks() {
        let scratch = Scratch::new("large");
        let data: Vec<u8> = (0..10_000u32).map(|n| n as u8).collect();
        let path = scratch.file("image.rgba", &data);
        let stream = apc("a=T,t=f,f=32,s=50,v=50,q=2", &path, b"\x1b\\");
        let expected = [rewrite(&control("a=T,t=f,f=32,s=50,v=50,q=2").unwrap(), &data), b"\x1b\\".to_vec()].concat();
        assert_eq!(feed(&[&stream]), expected);
    }

    #[test]
    fn cancelling_midway_passes_through() {
        let scratch = Scratch::new("cancel");
        let path = scratch.file("image.png", b"abc");
        for end in [&b"\x18"[..], b"\x1a", b"\x90"] {
            let whole = apc("t=f", &path, end);
            let stream = [&whole[..whole.len() - 3], end, b"rest"].concat();
            assert_eq!(feed(&[&stream]), stream);
        }
    }

    #[test]
    fn a_missing_file_passes_through() {
        let scratch = Scratch::new("missing");
        let stream = apc("t=f", &scratch.0.join("missing.png"), b"\x1b\\");
        assert_eq!(feed(&[&stream]), stream);
    }

    #[test]
    fn paths_must_be_absolute() {
        assert!(read_file(Medium::File, b"Cargo.toml", 0, 0).is_err());
        assert!(read_file(Medium::File, b"", 0, 0).is_err());
        assert!(read_file(Medium::File, b"/tmp/\0x", 0, 0).is_err());
    }

    #[test]
    fn offset_and_size_pick_the_bytes() {
        let scratch = Scratch::new("range");
        let path = scratch.file("image.png", b"0123456789");
        let read = |offset, size| read_file(Medium::File, bytes(&path), offset, size).ok();
        assert_eq!(read(0, 0).as_deref(), Some(&b"0123456789"[..]));
        assert_eq!(read(2, 3).as_deref(), Some(&b"234"[..]));
        assert_eq!(read(7, 0).as_deref(), Some(&b"789"[..]));
        assert_eq!(read(8, 5), None);
        assert!(path.exists());

        // 要读的超过给的上限就不读。
        let read = |offset, size, max| read::read_file(Medium::File, bytes(&path), offset, size, max);
        assert_eq!(read(0, 0, 9).unwrap_err().kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(read(0, 10, 9).unwrap_err().kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(read(1, 0, 9).ok().as_deref(), Some(&b"123456789"[..]));
        assert_eq!(read(0, 0, 10).ok().as_deref(), Some(&b"0123456789"[..]));
    }

    /// 经 `/dev/fd`、`/proc/self/fd` 打开的是宿主自己的描述符，按请求的路径拒掉，怎么写都一样；
    /// 同一个文件按它自己的路径照样读。
    #[test]
    fn descriptors_of_the_host_are_refused() {
        let scratch = Scratch::new("descriptors");
        let path = scratch.file("image.png", b"abc");
        let file = fs::File::open(&path).unwrap();
        let fd = file.as_raw_fd();
        let mut paths = vec![
            format!("/dev/fd/{fd}"),
            format!("//dev/fd/{fd}"),
            format!("/./dev/fd/{fd}"),
            format!("/tmp/../dev/fd/{fd}"),
        ];
        if cfg!(target_os = "macos") {
            paths.extend([format!("/DEV/fd/{fd}"), format!("/Dev/fd/{fd}")]);
        }
        if cfg!(target_os = "linux") {
            paths.push(format!("/proc/self/fd/{fd}"));
        }
        for refused in paths {
            assert!(read_file(Medium::File, refused.as_bytes(), 0, 0).is_err(), "{refused}");
        }
        assert_eq!(read_file(Medium::File, bytes(&path), 0, 0).ok().as_deref(), Some(&b"abc"[..]));
    }

    /// 一次喂入里几条命令同一时刻改写，读进来的总量不超过预算，超出的原样放行；过一阵预算补回来。
    #[test]
    fn rewriting_is_held_to_a_budget() {
        let scratch = Scratch::new("budget");
        let size = IMAGE_FILE_MAX_BYTES / 8 * 3;
        let path = scratch.0.join("sparse.rgba");
        fs::File::create(&path).unwrap().set_len(size).unwrap();
        let small = scratch.file("small.png", b"abc");

        let start = Instant::now();
        let mut rewriter = MediumRewriter::default();
        let mut rewritten = |path: &Path, now: Instant| {
            let content = apc("a=t,t=f,f=100,q=2", path, b"");
            rewriter.start();
            match rewriter.step(&[&content[..], b"\x1b\\"].concat(), now) {
                Step::Replace(_, used) | Step::Pass(used) if used != content.len() => panic!("used {used}"),
                Step::Replace(..) => true,
                Step::Pass(_) => false,
                Step::Held => panic!("held"),
            }
        };
        assert!(rewritten(&path, start));
        assert!(rewritten(&path, start));
        assert!(!rewritten(&path, start), "over the budget");
        // 剩下的预算还够小文件。
        assert!(rewritten(&small, start));
        assert!(!rewritten(&path, start + Duration::from_millis(100)));
        assert!(rewritten(&path, start + Duration::from_secs(1)));
    }

    #[test]
    fn only_non_empty_regular_files_are_read() {
        let scratch = Scratch::new("kinds");
        let empty = scratch.file("empty.png", b"");
        assert!(read_file(Medium::File, bytes(&empty), 0, 0).is_err());
        assert!(read_file(Medium::File, bytes(&scratch.0), 0, 0).is_err());
        assert!(read_file(Medium::File, b"/dev/zero", 0, 4).is_err());

        // 没有写端的 FIFO 立刻失败，不等。
        let fifo = scratch.0.join("fifo");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(status.success());
        assert!(read_file(Medium::File, bytes(&fifo), 0, 0).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_files_are_refused() {
        assert!(read_file(Medium::File, b"/proc/self/environ", 0, 0).is_err());
    }

    #[test]
    fn files_over_the_limit_are_refused() {
        let scratch = Scratch::new("limit");
        let path = scratch.0.join("sparse.png");
        let file = fs::File::create(&path).unwrap();
        file.set_len(IMAGE_FILE_MAX_BYTES + 1).unwrap();
        assert!(read_file(Medium::File, bytes(&path), 0, 0).is_err());
        assert!(read_file(Medium::File, bytes(&path), 0, IMAGE_FILE_MAX_BYTES + 1).is_err());
        // 读的那一段没超就行。
        assert_eq!(
            read_file(Medium::File, bytes(&path), 1, 0).map(|data| data.len()).ok(),
            Some(IMAGE_FILE_MAX_BYTES as usize)
        );
        assert_eq!(read_file(Medium::File, bytes(&path), 0, 4).ok(), Some(vec![0; 4]));
    }

    #[test]
    fn a_temporary_file_is_read_and_deleted() {
        let scratch = Scratch::new("temporary");
        let path = scratch.file("tty-graphics-protocol-1.png", b"abc");
        let stream = apc("a=T,t=t,f=100", &path, b"\x1b\\");
        assert_eq!(feed(&[&stream]), b"Ga=T,f=100;YWJj\x1b\\");
        assert!(!path.exists());

        // 读不成也删掉。
        let path = scratch.file("tty-graphics-protocol-2.png", b"abc");
        assert!(read_file(Medium::TemporaryFile, bytes(&path), 0, 9).is_err());
        assert!(!path.exists());

        // 超过能读的上限（比如改写预算不够）也算读不成，照样删掉。
        let path = scratch.file("tty-graphics-protocol-3.png", b"abc");
        assert!(read::read_file(Medium::TemporaryFile, bytes(&path), 0, 0, 2).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn a_temporary_file_must_look_like_one() {
        let scratch = Scratch::new("misnamed");
        let path = scratch.file("image.png", b"abc");
        assert!(read_file(Medium::TemporaryFile, bytes(&path), 0, 0).is_err());
        assert!(path.exists());

        // 测试程序所在的目录不是临时目录，除非整个构建目录就放在临时目录里。
        let dir = std::env::current_exe().unwrap().parent().unwrap().canonicalize().unwrap();
        if super::super::read::in_temp_dir(&dir) {
            return;
        }
        let path = dir.join(format!("tty-graphics-protocol-{}.png", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let result = read_file(Medium::TemporaryFile, bytes(&path), 0, 0);
        let kept = path.exists();
        let _ = fs::remove_file(&path);
        assert!(result.is_err());
        assert!(kept);
    }
}
