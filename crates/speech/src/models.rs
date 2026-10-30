//! 识别模型的清单和下载。模型文件第一次用时下载到调用方给的目录，每个文件按大小和 SHA-256 核对，
//! 核对过才改成正式的名字，所以目录里有这个文件就说明它是完整的。

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use anyhow::{Context, bail};
use runode_types::speech::SpeechModel;
use sha2::{Digest, Sha256};

/// 模型要的一个文件。`dir` 是它在模型目录下的子目录，几个模型共用的文件（VAD）放同一个子目录，只下一次。
pub(crate) struct Asset {
    pub(crate) dir: &'static str,
    pub(crate) name: &'static str,
    /// HuggingFace 上原作者的仓库。
    hf_repo: &'static str,
    size: u64,
    sha256: &'static str,
}

/// 切句用的 Silero VAD。
pub(crate) const VAD: Asset = Asset {
    dir: "silero-vad",
    name: "silero_vad.onnx",
    hf_repo: "csukuangfj/vad",
    size: 1_807_522,
    sha256: "a35ebf52fd3ce5f1469b2a36158dba761bc47b973ea3382b3186ca15b1f5af28",
};

const SENSE_VOICE: &[Asset] = &[
    Asset {
        dir: "sense-voice-int8-2024-07-17",
        name: "model.int8.onnx",
        hf_repo: "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
        size: 239_233_841,
        sha256: "c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51",
    },
    Asset {
        dir: "sense-voice-int8-2024-07-17",
        name: "tokens.txt",
        hf_repo: "csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17",
        size: 315_894,
        sha256: "f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc",
    },
    VAD,
];

const FIRE_RED_ASR2: &[Asset] = &[
    Asset {
        dir: "fire-red-asr2-int8-2026-02-26",
        name: "encoder.int8.onnx",
        hf_repo: "csukuangfj2/sherpa-onnx-fire-red-asr2-zh_en-int8-2026-02-26",
        size: 817_286_833,
        sha256: "54048d66b6e8f3c80ea7ce95efe794587b0fd81d7271651d0decd3803852ae82",
    },
    Asset {
        dir: "fire-red-asr2-int8-2026-02-26",
        name: "decoder.int8.onnx",
        hf_repo: "csukuangfj2/sherpa-onnx-fire-red-asr2-zh_en-int8-2026-02-26",
        size: 417_291_928,
        sha256: "b840ce7196ae4a14d05ae84bbf56082b6b61ccec5610fda907dddbcea37354ff",
    },
    Asset {
        dir: "fire-red-asr2-int8-2026-02-26",
        name: "tokens.txt",
        hf_repo: "csukuangfj2/sherpa-onnx-fire-red-asr2-zh_en-int8-2026-02-26",
        size: 79_172,
        sha256: "1bc613de2112d257e61a349c3e72d1b1a9cf19c33d3ca954197ad2171e5ea07b",
    },
    VAD,
];

/// ModelScope 上 runode 自己的镜像，按 `<dir>/<name>` 放和 HuggingFace 上一样的文件。
const MODELSCOPE: &str = "https://modelscope.cn/models/runode/speech-models/resolve/master";

pub(crate) fn assets(model: SpeechModel) -> &'static [Asset] {
    match model {
        SpeechModel::SenseVoice => SENSE_VOICE,
        SpeechModel::FireRedAsr2 => FIRE_RED_ASR2,
    }
}

/// `model` 全部文件的字节数。
pub fn download_size(model: SpeechModel) -> u64 {
    assets(model).iter().map(|asset| asset.size).sum()
}

/// `model` 的文件都已下载到 `root` 下。
pub fn is_installed(model: SpeechModel, root: &Path) -> bool {
    assets(model).iter().all(|asset| asset.path(root).is_file())
}

/// 删掉 `model` 自己的文件，别的模型共用的留着。
pub fn remove(model: SpeechModel, root: &Path) -> std::io::Result<()> {
    let shared = |dir: &str| SpeechModel::ALL.iter().any(|&m| m != model && assets(m).iter().any(|a| a.dir == dir));
    for asset in assets(model).iter().filter(|asset| !shared(asset.dir)) {
        match fs::remove_dir_all(root.join(asset.dir)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
    }
    Ok(())
}

impl Asset {
    pub(crate) fn path(&self, root: &Path) -> PathBuf {
        root.join(self.dir).join(self.name)
    }

    fn urls(&self, china_first: bool) -> [String; 2] {
        let hf = format!("https://huggingface.co/{}/resolve/main/{}", self.hf_repo, self.name);
        let ms = format!("{MODELSCOPE}/{}/{}", self.dir, self.name);
        if china_first { [ms, hf] } else { [hf, ms] }
    }
}

/// 把 `model` 缺的文件下载到 `root` 下，阻塞到下完、出错或 `cancel` 置位。`china_first` 时先试
/// ModelScope 再试 HuggingFace，否则反过来；一个源失败（连不上、断了、核对不过）就换另一个。
/// 下载期间不时调 `progress(已有的字节, 总字节)`。
pub fn download(
    model: SpeechModel,
    root: &Path,
    china_first: bool,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64, u64),
) -> anyhow::Result<()> {
    let total = download_size(model);
    let mut done = 0;
    for asset in assets(model) {
        let path = asset.path(root);
        if path.is_file() {
            done += asset.size;
            continue;
        }
        fs::create_dir_all(path.parent().unwrap_or(root))?;
        // 几个模型共用的文件（VAD）可能同时在下，各用各的 `.part`。
        let part = path.with_extension(format!("{}.part", model.name()));
        let mut errors = Vec::new();
        for url in asset.urls(china_first) {
            // 上次已经下全（只差核对、改名）的不再请求：续传位置在文件末尾时服务器回 416。
            let complete = fs::metadata(&part).is_ok_and(|m| m.len() >= asset.size);
            let fetched =
                if complete { Ok(()) } else { fetch(&url, &part, cancel, &|got| progress(done + got, total)) };
            match fetched.and_then(|()| verify(asset, &part)) {
                Ok(()) => {
                    fs::rename(&part, &path)?;
                    errors.clear();
                    break;
                }
                Err(_) if cancel.load(Ordering::Relaxed) => bail!("cancelled"),
                Err(err) => {
                    tracing::warn!("下载 {url} 失败：{err:#}");
                    errors.push(format!("{url}: {err:#}"));
                }
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("\n"));
        }
        done += asset.size;
        progress(done, total);
    }
    Ok(())
}

/// 用系统的 curl 把 `url` 下到 `part`，接着上次没下完的部分下。curl 跟环境变量里的代理；macOS、
/// Windows 10 起和常见的 Linux 发行版都自带它，不必为下载另带 HTTP 和 TLS 库。Windows 上的 curl 用
/// Schannel，查不到证书吊销状态（代理或网络拦了吊销列表）时整个握手失败，这时不查吊销重下一次：
/// 下完的文件照样按 SHA-256 核对。
fn fetch(url: &str, part: &Path, cancel: &AtomicBool, progress: &dyn Fn(u64)) -> anyhow::Result<()> {
    match fetch_once(url, part, cancel, progress, false) {
        Err(err) if cfg!(windows) && revocation_unchecked(&format!("{err:#}")) => {
            tracing::info!("retrying {url} without the certificate revocation check: {err:#}");
            fetch_once(url, part, cancel, progress, true)
        }
        result => result,
    }
}

/// Schannel 查不到吊销状态的报错：`CRYPT_E_NO_REVOCATION_CHECK`、`CRYPT_E_REVOCATION_OFFLINE`。
fn revocation_unchecked(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["0x80092012", "0x80092013"].iter().any(|code| error.contains(code))
}

fn fetch_once(
    url: &str,
    part: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64),
    no_revoke: bool,
) -> anyhow::Result<()> {
    let mut command = crate::http::curl();
    command
        .args(["--fail", "--location", "--silent", "--show-error", "--continue-at", "-"])
        // 连不上的源（比如国内连 HuggingFace）早点放弃、换下一个；连上了但一直没数据也算失败。
        .args(["--connect-timeout", "10", "--speed-limit", "1024", "--speed-time", "30"])
        .arg("--output")
        .arg(part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if no_revoke {
        command.arg("--ssl-no-revoke");
    }
    let mut child = command.spawn().context("cannot run curl")?;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("cancelled");
        }
        progress(fs::metadata(part).map_or(0, |m| m.len()));
        thread::sleep(Duration::from_millis(250));
    };
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        bail!("curl {status}: {}", stderr.trim());
    }
    Ok(())
}

/// 核对下完的文件，不对就删掉。
fn verify(asset: &Asset, part: &Path) -> anyhow::Result<()> {
    let result = check(asset, part);
    if result.is_err() {
        let _ = fs::remove_file(part);
    }
    result
}

fn check(asset: &Asset, part: &Path) -> anyhow::Result<()> {
    let size = fs::metadata(part)?.len();
    if size != asset.size {
        bail!("size {size}, expected {}", asset.size);
    }
    let mut file = fs::File::open(part)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        match file.read(&mut buf)? {
            0 => break,
            n => hasher.update(&buf[..n]),
        }
    }
    let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if digest != asset.sha256 {
        bail!("SHA-256 {digest}, expected {}", asset.sha256);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schannel_revocation_failures_are_recognized() {
        let err = "curl exit code: 35: schannel: next InitializeSecurityContext failed: Unknown error \
                   (0x80092012) - The revocation function was unable to check revocation for the certificate.";
        assert!(revocation_unchecked(err));
        assert!(revocation_unchecked("schannel: ... (0x80092013) - The revocation server was offline"));
        assert!(!revocation_unchecked("curl exit status: 22: The requested URL returned error: 404"));
    }

    #[test]
    fn remove_keeps_shared_vad() {
        let root = std::env::temp_dir().join(format!("runode-speech-remove-{}", std::process::id()));
        for asset in assets(SpeechModel::SenseVoice).iter().chain(assets(SpeechModel::FireRedAsr2)) {
            fs::create_dir_all(root.join(asset.dir)).unwrap();
            fs::write(asset.path(&root), b"").unwrap();
        }
        remove(SpeechModel::FireRedAsr2, &root).unwrap();
        assert!(is_installed(SpeechModel::SenseVoice, &root));
        assert!(!is_installed(SpeechModel::FireRedAsr2, &root));
        fs::remove_dir_all(&root).unwrap();
    }
}
