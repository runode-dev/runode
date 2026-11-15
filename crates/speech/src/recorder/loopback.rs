//! 回采系统正在外放的声音，当消回声的参考音轨。Windows 上把默认输出设备当输入设备开，cpal 自动
//! 进 WASAPI 的 loopback；Linux 上录 PulseAudio 默认 sink 的 monitor source。

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait};

use super::{Shared, capture};

/// 开始回采 `host` 默认输出设备放出来的声音，交出开着的流和它写的音轨。
#[cfg(windows)]
pub(super) fn open(host: &cpal::Host) -> anyhow::Result<(cpal::Stream, Shared)> {
    let output = host.default_output_device().context("no output device")?;
    let config = output.default_output_config()?;
    capture(&output, config, true)
}

/// 开始回采 `host` 默认 sink 放出来的声音，交出开着的流和它写的音轨。`host` 不是 PulseAudio
/// （连不上它、退回了 ALSA）时报错；默认 source 本身就是这个 monitor（没有麦克风时）也报错：麦克风和
/// 参考是同一路声音，消回声会把要录的整个消掉。
#[cfg(target_os = "linux")]
pub(super) fn open(host: &cpal::Host) -> anyhow::Result<(cpal::Stream, Shared)> {
    anyhow::ensure!(host.id() == cpal::HostId::PulseAudio, "PulseAudio is not available");
    let sink = host.default_output_device().context("no output device")?.id()?;
    // PulseAudio 和 pipewire-pulse 给每个 sink 配一个 monitor source，名字是 sink 名加 `.monitor`。
    let monitor = cpal::DeviceId::new(cpal::HostId::PulseAudio, format!("{}.monitor", sink.id()));
    let source = host.default_input_device().and_then(|device| device.id().ok());
    anyhow::ensure!(source.as_ref() != Some(&monitor), "the default source is the monitor of the default sink");
    let device = host
        .input_devices()?
        .find(|device| device.id().is_ok_and(|id| id == monitor))
        .with_context(|| format!("no monitor source for {}", sink.id()))?;
    let config = device.default_input_config()?;
    capture(&device, config, true)
}
