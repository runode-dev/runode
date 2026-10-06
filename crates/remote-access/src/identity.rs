//! 服务端证书：第一次开远程访问时生成的自签证书（ECDSA P-256），私钥和证书以 DER 存在
//! `remote_access_dir` 里（0600），之后一直用它。手机配对时记下它的指纹（叶子证书 DER 的
//! SHA-256），之后只认这个指纹，所以证书不能随便换：换了就要重新配对。

use std::io;

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
            // 先写私钥：只有私钥、没有证书时下次重新生成，不会配出一份对不上的。
            write_private(&key_path, &key)?;
            write_private(&cert_path, &cert)?;
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

/// 证书的指纹：DER 的 SHA-256。
pub(crate) fn fingerprint(cert_der: &[u8]) -> [u8; FINGERPRINT_LEN] {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert_der);
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(digest.as_ref());
    out
}
