//! 服务端证书：第一次开远程访问时生成的自签证书（ECDSA P-256），私钥和证书以 DER 存在
//! `remote_access_dir` 里（0600），之后一直用它。手机配对时记下它的指纹（叶子证书 DER 的
//! SHA-256），之后只认这个指纹，所以证书不能随便换：换了就要重新配对。

use std::{io, path::Path};

use rcgen::{CertificateParams, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use runode_paths::Dirs;
use runode_protocol::remote::FINGERPRINT_LEN;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::files::{no_home, read_optional, write_private};

/// 证书和私钥。
pub(crate) struct Identity {
    pub(crate) cert: CertificateDer<'static>,
    pub(crate) key: PrivateKeyDer<'static>,
    pub(crate) fingerprint: [u8; FINGERPRINT_LEN],
}

/// 读存着的证书和私钥，两个都还没有时生成一份存下。只有一个在时（上次写到一半）也重新生成：
/// 配对过的手机认的指纹随之作废，但半份证书本来就用不了。读到的内容坏了时报错，不悄悄换掉。
///
/// 调用方要拿着 `remote_access_lock_file` 的锁，免得两个进程同时生成。
pub(crate) fn load_or_create(dirs: &Dirs) -> io::Result<Identity> {
    let cert_path = dirs.remote_access_cert_file().ok_or_else(no_home)?;
    let key_path = dirs.remote_access_key_file().ok_or_else(no_home)?;
    let (cert, key) = match (read_optional(&cert_path)?, read_optional(&key_path)?) {
        (Some(cert), Some(key)) => (cert, key),
        _ => {
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(io::Error::other)?;
            let mut params = CertificateParams::new(vec!["runode".to_owned()]).map_err(io::Error::other)?;
            params.distinguished_name.push(DnType::CommonName, "runode remote access");
            let cert = params.self_signed(&key).map_err(io::Error::other)?;
            let (cert, key) = (cert.der().to_vec(), key.serialize_der());
            store(&cert_path, &key_path, &cert, &key, write_private)?;
            tracing::info!("generated the remote access certificate {}", cert_path.display());
            (cert, key)
        }
    };
    Ok(Identity {
        fingerprint: fingerprint(&cert),
        cert: CertificateDer::from(cert),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
    })
}

/// 把新生成的证书和私钥存下。先删掉留着的旧证书（上次只写成了证书），再写私钥、最后写证书：
/// 中途失败或崩溃时最多只剩私钥，下次重新生成，不会留下一对互相对不上的证书和私钥、之后一直用不了。
fn store(
    cert_path: &Path,
    key_path: &Path,
    cert: &[u8],
    key: &[u8],
    mut write: impl FnMut(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    match std::fs::remove_file(cert_path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    write(key_path, key)?;
    write(cert_path, cert)
}

/// 证书的指纹：DER 的 SHA-256。
pub(crate) fn fingerprint(cert_der: &[u8]) -> [u8; FINGERPRINT_LEN] {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert_der);
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(digest.as_ref());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有旧证书、补写私钥后写证书失败：旧证书已经删了，下次重新生成一对，不拿旧证书配新私钥。
    #[test]
    fn a_failed_certificate_write_does_not_leave_a_mismatched_pair() {
        let root = std::env::temp_dir().join(format!("rra-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
        dirs.create_remote_access_dir().unwrap();
        let cert_path = dirs.remote_access_cert_file().unwrap();
        let key_path = dirs.remote_access_key_file().unwrap();
        let stale = b"stale certificate".to_vec();
        write_private(&cert_path, &stale).unwrap();

        let failed = store(&cert_path, &key_path, b"new certificate", b"new key", |path, bytes| {
            if path == cert_path { Err(io::Error::other("disk full")) } else { write_private(path, bytes) }
        });

        assert!(failed.is_err());
        assert!(read_optional(&cert_path).unwrap().is_none());
        let identity = load_or_create(&dirs).unwrap();
        assert_ne!(identity.cert.as_ref(), stale.as_slice());
        assert_eq!(read_optional(&cert_path).unwrap().as_deref(), Some(identity.cert.as_ref()));
        assert_ne!(read_optional(&key_path).unwrap().as_deref(), Some(b"new key".as_slice()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
