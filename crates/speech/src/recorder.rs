//! 录系统默认的麦克风。cpal 的流在 macOS 上不能跨线程，所以放在自己的线程里开着，`Recorder`
//! 只拿着叫它停下的通道，可以交给任何线程。没开 `recording` 特性时 `Recorder::start` 报错。

use std::{
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use anyhow::{Context, anyhow};
#[cfg(feature = "recording")]
use cpal::{
    FromSample, SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use sherpa_onnx::LinearResampler;

use crate::SAMPLE_RATE;

/// 一段最多录这么多秒，之后录到的丢掉：忘了停时不一直占着内存，也不交出云端收不下的大段录音。
pub const MAX_RECORDING_SECS: u32 = 120;

/// 一段正在录的音。
pub struct Recorder {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<anyhow::Result<Vec<f32>>>,
}

impl Recorder {
    /// 开始录音，麦克风开起来了才返回；没有麦克风、没有权限时报错。
    pub fn start() -> anyhow::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let thread = thread::Builder::new().name("speech-recorder".into()).spawn(move || {
            let (stream, samples, rate) = match open() {
                Ok(opened) => {
                    let _ = ready.send(Ok(()));
                    opened
                }
                Err(err) => {
                    let message = format!("{err:#}");
                    let _ = ready.send(Err(err));
                    return Err(anyhow!(message));
                }
            };
            let _ = stopped.recv();
            drop(stream);
            let samples = std::mem::take(&mut *samples.lock().unwrap_or_else(|e| e.into_inner()));
            Ok(resample(&samples, rate))
        })?;
        started.recv().context("recorder thread exited")??;
        Ok(Self { stop, thread })
    }

    /// 停下，交出录到的单声道音频，采样率是 `SAMPLE_RATE`。录到的样本全是 0 时报 `Silent`。
    pub fn stop(self) -> anyhow::Result<Vec<f32>> {
        let _ = self.stop.send(());
        let samples = self.thread.join().map_err(|_| anyhow!("recorder thread panicked"))??;
        if !samples.is_empty() && samples.iter().all(|&s| s == 0.) {
            return Err(Silent.into());
        }
        Ok(samples)
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

type Samples = Arc<Mutex<Vec<f32>>>;

/// 没有录音时开不出来的流。
#[cfg(not(feature = "recording"))]
enum NoStream {}

#[cfg(not(feature = "recording"))]
impl Drop for NoStream {
    fn drop(&mut self) {
        match *self {}
    }
}

#[cfg(not(feature = "recording"))]
fn open() -> anyhow::Result<(NoStream, Samples, u32)> {
    anyhow::bail!("this build of runode cannot record audio")
}

#[cfg(feature = "recording")]
fn open() -> anyhow::Result<(cpal::Stream, Samples, u32)> {
    let device = cpal::default_host().default_input_device().context("no microphone")?;
    let config = device.default_input_config()?;
    let samples = Samples::default();
    let channels = usize::from(config.channels());
    let rate = config.sample_rate();
    let max = rate as usize * MAX_RECORDING_SECS as usize;
    let stream = match config.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config.into(), channels, max, &samples)?,
        SampleFormat::I16 => build::<i16>(&device, config.into(), channels, max, &samples)?,
        SampleFormat::I32 => build::<i32>(&device, config.into(), channels, max, &samples)?,
        SampleFormat::U16 => build::<u16>(&device, config.into(), channels, max, &samples)?,
        format => anyhow::bail!("unsupported sample format {format}"),
    };
    stream.play()?;
    Ok((stream, samples, rate))
}

#[cfg(feature = "recording")]
fn build<T: SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    max: usize,
    samples: &Samples,
) -> anyhow::Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    let samples = samples.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &_| {
            let mut samples = samples.lock().unwrap_or_else(|e| e.into_inner());
            // 几个声道取平均，混成单声道。
            let room = max.saturating_sub(samples.len());
            samples.extend(
                data.chunks(channels)
                    .take(room)
                    .map(|frame| frame.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32),
            );
        },
        |err| tracing::warn!("录音出错：{err}"),
        None,
    )?;
    Ok(stream)
}

fn resample(samples: &[f32], rate: u32) -> Vec<f32> {
    if rate == SAMPLE_RATE {
        return samples.to_vec();
    }
    let Some(resampler) = i32::try_from(rate).ok().and_then(|rate| LinearResampler::create(rate, SAMPLE_RATE as i32))
    else {
        return Vec::new();
    };
    resampler.resample(samples, true)
}
