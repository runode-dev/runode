//! 语音输入在应用里只有一份的东西：识别模型的下载（全局 `Downloads`，设置页和终端都看它）、加载好的
//! 识别器，以及按配置先本机、再云端识别，最后润色（`recognize`）。录音和把文字打进终端在 `terminal_view`
//! 的 `dictation`，手机交来的录音在 `remote`。

pub mod remote;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use gpui::{App, AppContext as _, BorrowAppContext as _, Global, Task};
use runode_config::Config;
use runode_speech::{Cloud, DictationCloud, Polish, Recognizer, Recorder, SpeechModel};

/// 下载时多久刷新一次进度。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
/// 识别器这么久没用就卸掉：FireRedASR2 加载后占一个多 GB 内存，用一次就一直占着不划算。
const IDLE_UNLOAD: Duration = Duration::from_secs(180);

pub enum Download {
    Running { done: u64, total: u64, cancel: Arc<AtomicBool> },
    Failed(String),
}

/// 正在下载和下载失败的模型；下完的不在里面，装没装看文件（`runode_speech::is_installed`）。
#[derive(Default)]
pub struct Downloads(pub HashMap<SpeechModel, Download>);

impl Global for Downloads {}

static RECOGNIZER: Mutex<Slot> = Mutex::new(Slot { loaded: None, watching: false });

struct Slot {
    /// 加载好的识别器和上次用它的时刻。
    loaded: Option<(SpeechModel, Recognizer, Instant)>,
    /// 有一个线程在等识别器闲下来卸掉它。
    watching: bool,
}

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

/// 删掉下好的 `model`，加载着的话先卸掉。
pub fn remove(model: SpeechModel) -> std::io::Result<()> {
    let mut slot = RECOGNIZER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.loaded.as_ref().is_some_and(|(loaded, ..)| *loaded == model) {
        slot.loaded = None;
    }
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

/// 停下录音，在后台识别录到的音。识别连同云端和润色可能要等几分钟，占着一个自己的线程，不放进 GPUI
/// 的后台执行器。
pub fn transcribe(settings: Settings, recorder: Recorder, cx: &mut App) -> Task<anyhow::Result<String>> {
    let (done, result) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("speech-transcribe".into())
        .spawn(move || drop(done.send(recorder.stop().and_then(|samples| recognize(&settings, &samples)))));
    cx.background_spawn(async move {
        spawned?;
        result.await.context("the transcription thread ended")?
    })
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

/// 用 `model` 在本机识别。识别器没加载或换了模型时先加载，闲了 `IDLE_UNLOAD` 之后卸掉。
fn recognize_locally(model: SpeechModel, samples: &[f32]) -> anyhow::Result<String> {
    let mut slot = RECOGNIZER.lock().unwrap_or_else(|e| e.into_inner());
    if slot.loaded.as_ref().is_none_or(|(loaded, ..)| *loaded != model) {
        slot.loaded = None;
        let root = models_dir().context("no home directory")?;
        slot.loaded = Some((model, Recognizer::load(model, &root)?, Instant::now()));
    }
    let (_, recognizer, used) = slot.loaded.as_mut().expect("loaded above");
    let text = recognizer.transcribe(samples);
    *used = Instant::now();
    if !slot.watching {
        slot.watching = std::thread::Builder::new().name("speech-unload".into()).spawn(unload_when_idle).is_ok();
    }
    Ok(text)
}

fn unload_when_idle() {
    loop {
        std::thread::sleep(IDLE_UNLOAD);
        let mut slot = RECOGNIZER.lock().unwrap_or_else(|e| e.into_inner());
        if slot.loaded.as_ref().is_some_and(|(.., used)| used.elapsed() < IDLE_UNLOAD) {
            continue;
        }
        slot.loaded = None;
        slot.watching = false;
        return;
    }
}

/// 给人看的大小，按 1000 进位，和系统的「访达」一致。
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes.div_ceil(1_000_000))
    }
}
