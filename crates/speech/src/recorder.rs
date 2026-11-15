//! 录系统默认的麦克风。cpal 的流在 macOS 上不能跨线程，所以放在自己的线程里开着，`Recorder`
//! 只拿着叫它停下的通道，可以交给任何线程。没开 `recording` 特性时 `Recorder::start` 报错。
//!
//! macOS 上开着系统的语音处理录（`voice_processing`），由系统消掉本机外放的声音。Windows 和 Linux
//! 上另外回采系统正在外放的声音当参考音轨，停下时用它把外放的声音从麦克风里消掉（`echo`）。语音处理
//! 或回采开不起来时照样录，只是不消回声。

#[cfg(all(feature = "recording", any(test, not(target_os = "macos"))))]
mod echo;
#[cfg(all(feature = "recording", any(windows, target_os = "linux")))]
mod loopback;
#[cfg(all(feature = "recording", target_os = "macos"))]
mod voice_processing;

#[cfg(any(test, feature = "recording"))]
use std::time::Duration;
use std::{
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread::{self, JoinHandle},
    time::Instant,
};

use anyhow::{Context, anyhow};
#[cfg(feature = "recording")]
use cpal::{
    FromSample, SampleFormat, SizedSample, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use sherpa_onnx::LinearResampler;

use crate::SAMPLE_RATE;

/// 一段最多录这么多秒，之后录到的丢掉：忘了停时不一直占着内存，也不交出云端收不下的大段录音。
pub const MAX_RECORDING_SECS: u32 = 120;

/// 一段正在录的音。不要录音时直接 drop：录音线程自己停下、丢掉录到的，不消回声也不重采样。
pub struct Recorder {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<anyhow::Result<Captured>>,
}

impl Recorder {
    /// 开始录音，麦克风开起来了才返回；没有麦克风、没有权限时报错。
    pub fn start() -> anyhow::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let thread = thread::Builder::new().name("speech-recorder".into()).spawn(move || {
            let session = match open() {
                Ok(session) => {
                    let _ = ready.send(Ok(()));
                    session
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    let _ = ready.send(Err(err));
                    return Err(anyhow!(message));
                }
            };
            let _ = stopped.recv();
            Ok(session.finish())
        })?;
        started.recv().context("recorder thread exited")??;
        Ok(Self { stop, thread })
    }

    /// 停下，交出录到的单声道音频，采样率是 `SAMPLE_RATE`。麦克风交来的样本全是 0 时报 `Silent`。
    pub fn stop(self) -> anyhow::Result<Vec<f32>> {
        let _ = self.stop.send(());
        let Captured { mic, reference } = self.thread.join().map_err(|_| anyhow!("recorder thread panicked"))??;
        // 在消回声之前判断：消过回声的录音不再是麦克风原样交来的。
        if !mic.samples.is_empty() && mic.samples.iter().all(|&s| s == 0.) {
            return Err(Silent.into());
        }
        Ok(match reference {
            #[cfg(all(feature = "recording", not(target_os = "macos")))]
            Some(reference) => echo::cancel(mic, reference),
            _ => mic.into_resampled().samples,
        })
    }
}

/// 麦克风交来的样本全是 0：没有录音权限时 macOS 不报错，只给静音；真麦克风再安静也有底噪。
#[derive(Debug)]
pub struct Silent;

impl std::fmt::Display for Silent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the microphone delivered only silence")
    }
}

impl std::error::Error for Silent {}

/// 录音线程停下时交出的：麦克风的音轨，和回采到的外放声音（没有回采时是 `None`）。
struct Captured {
    mic: Track,
    reference: Option<Track>,
}

/// 一条单声道音轨，样本按采集时刻排在时间轴上（`push`）。
#[derive(Default)]
struct Track {
    samples: Vec<f32>,
    rate: u32,
    /// 第一个样本的采集时刻；各条流的 cpal `StreamInstant` 起点各不相同，所以用 `Instant`
    /// 对齐。还没收到样本时是 `None`。
    #[cfg_attr(not(any(test, feature = "recording")), expect(dead_code, reason = "只有录音时读"))]
    start: Option<Instant>,
    /// 中间断了时补 0（`GAP`）。只给回采的音轨开：麦克风一直交数据，给它补只会把采集时刻的抖动
    /// 变成插进录音里的静音。
    #[cfg_attr(not(any(test, feature = "recording")), expect(dead_code, reason = "只有录音时读"))]
    pad_gaps: bool,
}

/// 一块样本该在的位置比音轨已有的长度多出超过这么久，才当成中间断了、先补上 0：WASAPI 回采的
/// 输出没有声音时不交数据。不到这么久的当成回调时刻的抖动，接着往后排。
#[cfg(any(test, feature = "recording"))]
const GAP: Duration = Duration::from_millis(20);

impl Track {
    #[cfg(any(test, feature = "recording"))]
    fn new(rate: u32, pad_gaps: bool) -> Self {
        Self { samples: Vec::new(), rate, start: None, pad_gaps }
    }

    /// 接上采集时刻是 `at` 的一块样本。开着 `pad_gaps` 时，比该在的位置晚出 `GAP` 以上的先补 0；
    /// 早了不往回挪，接着排在后面。总长不超过 `MAX_RECORDING_SECS`。
    #[cfg(any(test, feature = "recording"))]
    fn push(&mut self, at: Instant, block: impl IntoIterator<Item = f32>) {
        let max = self.rate as usize * MAX_RECORDING_SECS as usize;
        let start = *self.start.get_or_insert(at);
        let rate = f64::from(self.rate);
        let due = (at.saturating_duration_since(start).as_secs_f64() * rate).round() as usize;
        let gap = due.saturating_sub(self.samples.len());
        if self.pad_gaps && gap as f64 > GAP.as_secs_f64() * rate {
            let len = (self.samples.len() + gap).min(max);
            self.samples.resize(len, 0.);
        }
        let room = max.saturating_sub(self.samples.len());
        self.samples.extend(block.into_iter().take(room));
    }

    /// 换到 `SAMPLE_RATE` 上的同一条音轨，起点不变。
    fn into_resampled(self) -> Self {
        if self.rate == SAMPLE_RATE {
            return self;
        }
        Self { samples: resample(&self.samples, self.rate), rate: SAMPLE_RATE, ..self }
    }
}

/// 录音线程和 cpal 回调共用的一条音轨。
type Shared = Arc<Mutex<Track>>;

/// 开着的录音：流 drop 掉才停。
struct Session {
    streams: Streams,
    mic: Shared,
    reference: Option<Shared>,
}

impl Session {
    /// 关掉流，交出录到的音轨。先关流，回调才不再往缓冲里写。
    fn finish(self) -> Captured {
        drop(self.streams);
        let take = |shared: Shared| std::mem::take(&mut *shared.lock().unwrap_or_else(PoisonError::into_inner));
        Captured { mic: take(self.mic), reference: self.reference.map(take) }
    }
}

/// 开着的录音来源，drop 掉就停。
#[cfg(feature = "recording")]
#[expect(dead_code, reason = "只拿着，drop 时停")]
enum Streams {
    Cpal(Vec<cpal::Stream>),
    #[cfg(target_os = "macos")]
    VoiceProcessing(voice_processing::Engine),
}

/// 没有录音时开不出来的流。
#[cfg(not(feature = "recording"))]
enum Streams {}

#[cfg(not(feature = "recording"))]
impl Drop for Streams {
    fn drop(&mut self) {
        match *self {}
    }
}

#[cfg(not(feature = "recording"))]
fn open() -> anyhow::Result<Session> {
    anyhow::bail!("this build of runode cannot record audio")
}

#[cfg(feature = "recording")]
fn open() -> anyhow::Result<Session> {
    #[cfg(target_os = "macos")]
    match voice_processing::open() {
        Ok((engine, mic)) => return Ok(Session { streams: Streams::VoiceProcessing(engine), mic, reference: None }),
        Err(err) => tracing::warn!("开不了系统的语音处理，这段录音不消回声：{err:#}"),
    }
    // 开着 cpal 的 `pulseaudio` 特性时，Linux 上默认的 host 在连得上 PulseAudio 时是它，否则是 ALSA。
    let host = cpal::default_host();
    let device = host.default_input_device().context("no microphone")?;
    let (stream, mic) = capture(&device, device.default_input_config()?, false)?;
    #[cfg(any(windows, target_os = "linux"))]
    let (streams, reference) = match loopback::open(&host) {
        Ok((loopback, reference)) => (vec![stream, loopback], Some(reference)),
        Err(err) => {
            tracing::warn!("回采不到外放的声音，这段录音不消回声：{err:#}");
            (vec![stream], None)
        }
    };
    #[cfg(not(any(windows, target_os = "linux")))]
    let (streams, reference) = (vec![stream], None);
    Ok(Session { streams: Streams::Cpal(streams), mic, reference })
}

/// 用 `config` 开 `device` 的输入流，几个声道混成单声道写进返回的音轨，音轨按 `pad_gaps` 补不补
/// 中间断掉的部分；开好就开始录。
#[cfg(feature = "recording")]
fn capture(
    device: &cpal::Device,
    config: SupportedStreamConfig,
    pad_gaps: bool,
) -> anyhow::Result<(cpal::Stream, Shared)> {
    let track = Shared::new(Mutex::new(Track::new(config.sample_rate(), pad_gaps)));
    let channels = usize::from(config.channels());
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(device, config.into(), channels, &track)?,
        SampleFormat::I16 => build::<i16>(device, config.into(), channels, &track)?,
        SampleFormat::I32 => build::<i32>(device, config.into(), channels, &track)?,
        SampleFormat::U16 => build::<u16>(device, config.into(), channels, &track)?,
        format => anyhow::bail!("unsupported sample format {format}"),
    };
    stream.play()?;
    Ok((stream, track))
}

#[cfg(feature = "recording")]
fn build<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    track: &Shared,
) -> anyhow::Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    let track = track.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            // 这块样本的采集时刻：现在减去从采集到回调过了多久。
            let now = Instant::now();
            let timestamp = info.timestamp();
            let at = now.checked_sub(timestamp.callback.duration_since(timestamp.capture)).unwrap_or(now);
            let mut track = track.lock().unwrap_or_else(PoisonError::into_inner);
            // 几个声道取平均，混成单声道。
            track.push(
                at,
                data.chunks(channels)
                    .map(|frame| frame.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32),
            );
        },
        |err| tracing::warn!("录音出错：{err}"),
        None,
    )?;
    Ok(stream)
}

/// 把 `rate` 上的 `samples` 重采样到 `SAMPLE_RATE`。
fn resample(samples: &[f32], rate: u32) -> Vec<f32> {
    let Some(resampler) = i32::try_from(rate).ok().and_then(|rate| LinearResampler::create(rate, SAMPLE_RATE as i32))
    else {
        return Vec::new();
    };
    resampler.resample(samples, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 1000;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn blocks_on_time_are_appended_without_padding() {
        let t0 = Instant::now();
        let mut track = Track::new(RATE, true);
        track.push(t0, [1.; 10]);
        track.push(t0 + ms(10), [1.; 10]);
        track.push(t0 + ms(20), [1.; 10]);
        assert_eq!(track.start, Some(t0));
        assert_eq!(track.samples, vec![1.; 30]);
    }

    #[test]
    fn jitter_within_gap_is_not_padded() {
        let t0 = Instant::now();
        let mut track = Track::new(RATE, true);
        track.push(t0, [1.; 10]);
        // 晚到 15 ms、早到 5 ms 都当成抖动，接着往后排。
        track.push(t0 + ms(25), [1.; 10]);
        track.push(t0 + ms(15), [1.; 10]);
        assert_eq!(track.samples, vec![1.; 30]);
    }

    #[test]
    fn gap_beyond_threshold_is_padded_with_silence() {
        let t0 = Instant::now();
        let mut track = Track::new(RATE, true);
        track.push(t0, [1.; 10]);
        track.push(t0 + ms(110), [1.; 10]);
        assert_eq!(track.samples.len(), 120);
        assert!(track.samples[..10].iter().all(|&s| s == 1.));
        assert!(track.samples[10..110].iter().all(|&s| s == 0.));
        assert!(track.samples[110..].iter().all(|&s| s == 1.));
    }

    #[test]
    fn gap_is_not_padded_without_pad_gaps() {
        let t0 = Instant::now();
        let mut track = Track::new(RATE, false);
        track.push(t0, [1.; 10]);
        track.push(t0 + ms(110), [1.; 10]);
        assert_eq!(track.samples, vec![1.; 20]);
    }

    #[test]
    fn block_from_before_start_is_appended() {
        let t0 = Instant::now() + ms(1000);
        let mut track = Track::new(RATE, true);
        track.push(t0, [1.; 10]);
        track.push(t0 - ms(100), [2.; 10]);
        assert_eq!(track.start, Some(t0));
        assert_eq!(track.samples.len(), 20);
        assert!(track.samples[10..].iter().all(|&s| s == 2.));
    }

    #[test]
    fn track_stops_growing_at_the_limit() {
        let t0 = Instant::now();
        let max = (RATE * MAX_RECORDING_SECS) as usize;
        let mut track = Track::new(RATE, true);
        track.push(t0, [1.; 10]);
        track.push(t0 + Duration::from_secs(u64::from(MAX_RECORDING_SECS) + 10), [1.; 10]);
        assert_eq!(track.samples.len(), max);
        track.push(t0 + Duration::from_secs(u64::from(MAX_RECORDING_SECS) + 11), [1.; 10]);
        assert_eq!(track.samples.len(), max);
    }
}
