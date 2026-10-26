//! Windows 上核对 Authenticode 签名：用 PowerShell 的 `Get-AuthenticodeSignature` 读出签名状态、
//! 签名者证书的指纹和文件的产品版本（`SCRIPT`），再按输出判断（`check`）。

/// 读 `RUNODE_FILE` 这个文件的签名，依次输出状态、签名者证书的指纹、产品版本，各占一行。
#[cfg(windows)]
pub(crate) const SCRIPT: &str = "$f = $env:RUNODE_FILE; $s = Get-AuthenticodeSignature -LiteralPath $f; \
     $s.Status; $s.SignerCertificate.Thumbprint; (Get-Item -LiteralPath $f).VersionInfo.ProductVersion";

/// 一个文件的签名者证书指纹和产品版本。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Signed {
    pub thumbprint: String,
    pub version: String,
}

/// 解读 `SCRIPT` 的输出。发布者用自签名证书，系统不信任它，状态是 `UnknownError` 或 `NotTrusted`
/// 也算签名完好；没签名、文件被改过（`HashMismatch`）和别的状态都不行。
pub(crate) fn check(output: &str) -> Result<Signed, String> {
    let mut lines = output.lines().map(str::trim);
    let (status, thumbprint, version) =
        (lines.next().unwrap_or(""), lines.next().unwrap_or(""), lines.next().unwrap_or(""));
    if !matches!(status, "Valid" | "UnknownError" | "NotTrusted") {
        return Err(format!("signature status is {status:?}"));
    }
    if thumbprint.is_empty() {
        return Err("the file has no signer certificate".into());
    }
    Ok(Signed { thumbprint: thumbprint.to_ascii_uppercase(), version: version.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untrusted_but_intact_signature_is_accepted() {
        let signed = check("UnknownError\r\nab12\r\n0.2.0.0\r\n").unwrap();
        assert_eq!(signed, Signed { thumbprint: "AB12".into(), version: "0.2.0.0".into() });
        assert!(check("Valid\nAB12\n0.2.0\n").is_ok());
    }

    #[test]
    fn unsigned_and_tampered_files_are_rejected() {
        assert!(check("NotSigned\n\n0.2.0\n").is_err());
        assert!(check("HashMismatch\nAB12\n0.2.0\n").is_err());
        assert!(check("Valid\n\n0.2.0\n").is_err());
        assert!(check("").is_err());
    }
}
