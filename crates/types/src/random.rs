//! 系统随机数源里取的随机字节：会话标识、口令和别人不该猜到的名字都用它，不拿 `RandomState` 的哈希凑。

/// `N` 个随机字节。
#[cfg(unix)]
pub fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    use std::io::Read as _;

    let mut bytes = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// `N` 个随机字节，从 `ProcessPrng` 取（标准库的随机数也取自它，不会失败）。
#[cfg(windows)]
pub fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    #[link(name = "bcryptprimitives", kind = "raw-dylib")]
    unsafe extern "system" {
        fn ProcessPrng(data: *mut u8, len: usize) -> i32;
    }
    let mut bytes = [0u8; N];
    // SAFETY: 指针和长度来自这里的局部数组。
    unsafe { ProcessPrng(bytes.as_mut_ptr(), bytes.len()) };
    Ok(bytes)
}

/// `N` 个随机字节写成 `2 * N` 个小写十六进制数字。
pub fn random_hex<const N: usize>() -> std::io::Result<String> {
    Ok(random_bytes::<N>()?.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_differ_and_hex_has_two_digits_a_byte() {
        assert_ne!(random_bytes::<16>().unwrap(), random_bytes::<16>().unwrap());
        let hex = random_hex::<8>().unwrap();
        assert!(hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }
}
