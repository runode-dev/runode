//! 语音输入在应用里只有一份的东西：识别模型的下载（全局 `Downloads`，设置页和终端都看它），以及按配置
//! 先本机、再云端识别，最后润色（`recognize`，跑在识别子进程 `worker` 里）。录音和把文字打进终端在
//! `terminal_view` 的 `dictation`，手机交来的录音在 `remote`。

pub mod remote;
pub mod worker;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, bail};
use gpui::{App, AppContext as _, BorrowAppContext as _, Global, Task};
use runode_config::Config;
use runode_protocol::speech::SpeechHostMsg;
use runode_speech::{Cloud, DictationCloud, Polish, Recognizer, Recorder, SpeechModel};

/// 下载时多久刷新一次进度。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// 本机录的一段最多等这么久识别：慢机器上 FireRedASR2 识别满长的录音再交给大模型润色，要好几分钟。
const RECOGNIZE_TIMEOUT: Duration = Duration::from_secs(600);

pub enum Download {
    Running { done: u64, total: u64, cancel: Arc<AtomicBool> },
    Failed(String),
}

/// 正在下载和下载失败的模型；下完的不在里面，装没装看文件（`runode_speech::is_installed`）。
#[derive(Default)]
pub struct Downloads(pub HashMap<SpeechModel, Download>);

impl Global for Downloads {}

/// 识别子进程里加载好的识别器。
static RECOGNIZER: Mutex<Option<(SpeechModel, Recognizer)>> = Mutex::new(None);

pub fn models_dir() -> Option<PathBuf> {
    runode_paths::Dirs::from_env().speech_models_dir()
}

pub fn is_installed(model: SpeechModel) -> bool {
    models_dir().is_some_and(|root| runode_speech::is_installed(model, &root))
}

/// 在后台下载 `model`，进度和结果写进 `Downloads`。已经在下时什么也不做。
pub fn start_download(model: SpeechModel, cx: &mut App) {
    let Some(root) = models_dir() else {
        return;
    };
    if matches!(cx.default_global::<Downloads>().0.get(&model), Some(Download::Running { .. })) {
        return;
    }
    let total = runode_speech::download_size(model);
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicU64::new(0));
    let running = Download::Running { done: 0, total, cancel: cancel.clone() };
    cx.update_global::<Downloads, _>(|downloads, _| downloads.0.insert(model, running));
    // 国内连 HuggingFace 常常不通，先试 ModelScope。
    let china_first = rust_i18n::locale().starts_with("zh");
    let result = Arc::new(Mutex::new(None));
    // 要下几分钟，占着一个线程阻塞地等 curl，不放进 GPUI 的后台执行器。
    std::thread::spawn({
        let (done, result) = (done.clone(), result.clone());
        move || {
            let outcome = runode_speech::download(model, &root, china_first, &cancel, &|got, _| {
                done.store(got, Ordering::Relaxed);
            });
            *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
        }
    });
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(PROGRESS_INTERVAL).await;
            let outcome = result.lock().unwrap_or_else(|e| e.into_inner()).take();
            let finished = outcome.is_some();
            cx.update_global::<Downloads, _>(|downloads, _| match outcome {
                None => {
                    if let Some(Download::Running { done: shown, .. }) = downloads.0.get_mut(&model) {
                        *shown = done.load(Ordering::Relaxed);
                    }
                }
                Some(Ok(())) => drop(downloads.0.remove(&model)),
                Some(Err(_)) if cancelled(downloads, model) => drop(downloads.0.remove(&model)),
                Some(Err(err)) => drop(downloads.0.insert(model, Download::Failed(format!("{err:#}")))),
            });
            if finished {
                break;
            }
        }
    })
    .detach();
}

fn cancelled(downloads: &Downloads, model: SpeechModel) -> bool {
    matches!(downloads.0.get(&model), Some(Download::Running { cancel, .. }) if cancel.load(Ordering::Relaxed))
}

pub fn cancel_download(model: SpeechModel, cx: &mut App) {
    if let Some(Download::Running { cancel, .. }) = cx.default_global::<Downloads>().0.get(&model) {
        cancel.store(true, Ordering::Relaxed);
    }
}

/// 删掉下好的 `model`，先结束闲着的识别子进程，免得它还占着模型的内存。
pub fn remove(model: SpeechModel) -> std::io::Result<()> {
    worker::end();
    models_dir().map_or(Ok(()), |root| runode_speech::remove(model, &root))
}

/// 一次识别要用的配置。
pub struct Settings {
    model: SpeechModel,
    words: Vec<String>,
    cloud: Option<Cloud>,
    polish: Option<Polish>,
}

impl Settings {
    pub fn new(config: &Config) -> Self {
        let text = |value: &Option<String>| value.clone().unwrap_or_default();
        let cloud = match config.dictation_cloud {
            DictationCloud::None => None,
            DictationCloud::OpenAi => Some(Cloud::OpenAi {
                url: config.dictation_openai_url.clone(),
                key: text(&config.dictation_openai_key),
                model: config.dictation_openai_model.clone(),
            }),
            DictationCloud::Doubao => Some(Cloud::Doubao {
                app_id: text(&config.dictation_doubao_app_id),
                key: text(&config.dictation_doubao_key),
            }),
        };
        let polish = config.dictation_polish_url.as_ref().map(|url| Polish {
            url: url.clone(),
            key: text(&config.dictation_polish_key),
            model: text(&config.dictation_polish_model),
        });
        Self { model: config.dictation_model, words: config.dictation_words.clone(), cloud, polish }
    }

    pub fn model(&self) -> SpeechModel {
        self.model
    }

    /// 本机的模型下好了，或者配了云端识别。
    pub fn available(&self) -> bool {
        self.cloud.is_some() || is_installed(self.model)
    }
}

/// 停下录音，交给识别子进程识别。识别连同云端和润色可能要等几分钟，占着一个自己的线程，不放进 GPUI
/// 的后台执行器。
pub fn transcribe(recorder: Recorder, cx: &mut App) -> Task<anyhow::Result<String>> {
    let (done, result) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("speech-transcribe".into())
        .spawn(move || drop(done.send(recorder.stop().and_then(|samples| recognize_in_worker(&samples)))));
    cx.background_spawn(async move {
        spawned?;
        result.await.context("the transcription thread ended")?
    })
}

const _: () = assert!(runode_speech::MAX_RECORDING_SECS <= runode_protocol::speech::MAX_AUDIO_SECS);

fn recognize_in_worker(samples: &[f32]) -> anyhow::Result<String> {
    // 重采样可能比 `runode_speech::MAX_RECORDING_SECS` 多出几个样本，超出子进程收的上限。
    let pcm: Vec<u8> = samples
        .iter()
        .take(worker::MAX_AUDIO_BYTES / 2)
        .flat_map(|s| ((s.clamp(-1., 1.) * 32767.) as i16).to_le_bytes())
        .collect();
    match worker::recognize(&pcm, RECOGNIZE_TIMEOUT)? {
        SpeechHostMsg::Text { text } => Ok(text),
        SpeechHostMsg::Failed { reason } => bail!(reason),
        other => bail!("unexpected reply from the speech process: {other:?}"),
    }
}

/// 识别 `samples`（`runode_speech::SAMPLE_RATE` 的单声道），阻塞到识别完：本机的模型下好了就在本机识别，
/// 没下或者识别出错时交给配置的云端；配了润色再交给大模型润色，润色出错时用原文。
pub fn recognize(settings: &Settings, samples: &[f32]) -> anyhow::Result<String> {
    let local = if is_installed(settings.model) {
        recognize_locally(settings.model, samples)
    } else {
        Err(anyhow::anyhow!("the speech model {} is not downloaded on this computer", settings.model.name()))
    };
    let text = match (local, &settings.cloud) {
        (Ok(text), _) => text,
        (Err(err), Some(cloud)) => {
            tracing::info!("dictation goes to the cloud: {err:#}");
            runode_speech::transcribe_cloud(cloud, samples, &settings.words)?
        }
        (Err(err), None) => bail!(err),
    };
    let Some(polish) = settings.polish.as_ref().filter(|_| !text.is_empty()) else {
        return Ok(text);
    };
    Ok(runode_speech::polish(polish, &text, &settings.words).unwrap_or_else(|err| {
        tracing::warn!("dictation polish failed: {err:#}");
        text
    }))
}

/// 用 `model` 在本机识别，识别器没加载或换了模型时先加载。
fn recognize_locally(model: SpeechModel, samples: &[f32]) -> anyhow::Result<String> {
    let mut slot = RECOGNIZER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_none_or(|(loaded, _)| *loaded != model) {
        *slot = None;
        let root = models_dir().context("no home directory")?;
        *slot = Some((model, Recognizer::load(model, &root)?));
    }
    let (_, recognizer) = slot.as_mut().expect("loaded above");
    Ok(recognizer.transcribe(samples))
}

/// 给人看的大小，按 1000 进位，和系统的「访达」一致。
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes.div_ceil(1_000_000))
    }
}
