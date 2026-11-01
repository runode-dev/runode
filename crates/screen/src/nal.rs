//! `VideoFrame` 里的 HEVC 或 H.264 码流：每个 NAL 单元前是 `LENGTH_SIZE` 字节的大端长度，不是起始码，
//! VideoToolbox 编出来就是这样；Media Foundation 编出的 Annex B 码流（起始码分隔）由 `from_annex_b` 换过来。

use runode_types::screen::Codec;

/// NAL 单元前长度字段的字节数，建编码会话时 VideoToolbox 默认就是它。
pub(crate) const LENGTH_SIZE: usize = 4;

/// NAL 单元在编码里算哪一类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    /// 随机访问点，解码器能从这里开始解：HEVC 的 BLA、IDR、CRA，H.264 的 IDR。
    RandomAccess,
    /// 参数集：HEVC 的 VPS、SPS、PPS，H.264 的 SPS、PPS。
    ParameterSet,
    /// 访问单元分隔符。
    Delimiter,
    Other,
}

/// `data` 里依次的 NAL 单元；长度字段对不上时到那里为止。
fn units(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (len, rest) = data.split_first_chunk::<LENGTH_SIZE>()?;
        let len = u32::from_be_bytes(*len) as usize;
        let unit = rest.get(..len)?;
        data = &rest[len..];
        Some(unit)
    })
}

/// 有没有随机访问点，解码器能从这一帧开始解。
pub(crate) fn is_keyframe(codec: Codec, data: &[u8]) -> bool {
    units(data).any(|unit| kind(codec, unit) == Unit::RandomAccess)
}

fn kind(codec: Codec, unit: &[u8]) -> Unit {
    let Some(&header) = unit.first() else { return Unit::Other };
    match codec {
        Codec::Hevc => match (header >> 1) & 0x3f {
            16..=21 => Unit::RandomAccess,
            32..=34 => Unit::ParameterSet,
            35 => Unit::Delimiter,
            _ => Unit::Other,
        },
        Codec::H264 => match header & 0x1f {
            5 => Unit::RandomAccess,
            7 | 8 => Unit::ParameterSet,
            9 => Unit::Delimiter,
            _ => Unit::Other,
        },
    }
}

/// Annex B 码流里依次的 NAL 单元：`00 00 01` 起始码之间的字节，去掉结尾补的 0（四字节起始码多出的那个
/// 0 也在里面）。
#[cfg(any(windows, test))]
fn annex_b_units(data: &[u8]) -> Vec<&[u8]> {
    let starts: Vec<usize> =
        data.windows(3).enumerate().filter(|(_, w)| *w == [0, 0, 1]).map(|(at, _)| at + 3).collect();
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = starts.get(index + 1).map_or(data.len(), |next| next - 3);
            let unit = &data[start..end.max(start)];
            let len = unit.iter().rposition(|&byte| byte != 0).map_or(0, |last| last + 1);
            &unit[..len]
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

/// 把 Annex B 的一帧换成 `VideoFrame` 的写法：参数集单拿出来（不带长度字段），访问单元分隔符丢掉，
/// 其余的 NAL 单元前面换成长度字段。
#[cfg(any(windows, test))]
pub(crate) fn from_annex_b(codec: Codec, data: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut parameter_sets = Vec::new();
    for unit in annex_b_units(data) {
        match kind(codec, unit) {
            Unit::ParameterSet => parameter_sets.push(unit.to_vec()),
            Unit::Delimiter => {}
            Unit::RandomAccess | Unit::Other => {
                out.extend((unit.len() as u32).to_be_bytes());
                out.extend_from_slice(unit);
            }
        }
    }
    (out, parameter_sets)
}

/// 把 `VideoFrame` 的一帧换回 Annex B 码流（解码器要的写法）：每个 NAL 单元前换成四字节起始码，关键帧
/// 先放参数集。
#[cfg(any(windows, test))]
pub(crate) fn to_annex_b(data: &[u8], parameter_sets: &[Vec<u8>]) -> Vec<u8> {
    const START: [u8; 4] = [0, 0, 0, 1];
    let mut out = Vec::with_capacity(data.len() + parameter_sets.iter().map(|set| set.len() + 4).sum::<usize>());
    for unit in parameter_sets.iter().map(Vec::as_slice).chain(units(data)) {
        out.extend(START);
        out.extend_from_slice(unit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VPS: u8 = 32;
    const PPS: u8 = 34;
    const AUD: u8 = 35;

    fn unit(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 2) as u32).to_be_bytes().to_vec();
        out.extend([kind << 1, 1]);
        out.extend(body);
        out
    }

    #[test]
    fn keyframe_when_any_unit_is_irap() {
        let idr = 19;
        let trail = 1;
        let sei = 39;
        assert!(is_keyframe(Codec::Hevc, &[unit(sei, b"x"), unit(idr, b"frame")].concat()));
        assert!(!is_keyframe(Codec::Hevc, &[unit(sei, b"x"), unit(trail, b"frame")].concat()));
    }

    /// 长度字段越界时不读出界，也不把后面的字节当成 NAL。
    #[test]
    fn stops_at_a_truncated_unit() {
        let mut data = unit(1, b"ok");
        data.extend(100u32.to_be_bytes());
        data.push(19 << 1);
        assert_eq!(units(&data).count(), 1);
        assert!(!is_keyframe(Codec::Hevc, &data));
    }

    /// 参数集单拿出来、分隔符丢掉，三字节和四字节的起始码都认，换出来的帧认得出是关键帧。
    #[test]
    fn converts_annex_b_frames() {
        let nal = |kind: u8, body: &[u8]| [&[kind << 1, 1][..], body].concat();
        let stream = [
            &[0, 0, 0, 1][..],
            &nal(AUD, &[0x50]),
            &[0, 0, 0, 1],
            &nal(VPS, b"v"),
            &[0, 0, 1],
            &nal(33, b"s"),
            &[0, 0, 1],
            &nal(PPS, b"p"),
            &[0, 0, 0, 1],
            &nal(19, b"idr"),
        ]
        .concat();
        let (data, parameter_sets) = from_annex_b(Codec::Hevc, &stream);
        assert_eq!(parameter_sets, [nal(VPS, b"v"), nal(33, b"s"), nal(PPS, b"p")]);
        assert_eq!(units(&data).collect::<Vec<_>>(), [nal(19, b"idr").as_slice()]);
        assert!(is_keyframe(Codec::Hevc, &data));
        let (data, parameter_sets) = from_annex_b(Codec::Hevc, &[&[0, 0, 1][..], &nal(1, b"p\0")].concat());
        assert!(parameter_sets.is_empty());
        assert!(!is_keyframe(Codec::Hevc, &data));
    }

    /// 换回 Annex B 再换过来是原样。
    #[test]
    fn round_trips_through_annex_b() {
        let nal = |kind: u8, body: &[u8]| [&[kind << 1, 1][..], body].concat();
        let sets = vec![nal(VPS, b"v"), nal(33, b"s"), nal(PPS, b"p")];
        let data = [unit(19, b"idr"), unit(1, b"more")].concat();
        let (back, back_sets) = from_annex_b(Codec::Hevc, &to_annex_b(&data, &sets));
        assert_eq!((back, back_sets), (data, sets));
    }

    /// H.264 的 NAL 类型在头一个字节的低 5 位：SPS、PPS 单拿出来，分隔符丢掉，IDR 是关键帧。
    #[test]
    fn converts_h264_annex_b_frames() {
        let (aud, sps, pps, idr, slice) = ([0x09, 0xf0], [0x67, 0x64, 0x1f], [0x68, 0xee], [0x65, 0x88], [0x41, 0x9a]);
        let stream = [&[0, 0, 0, 1][..], &aud, &[0, 0, 0, 1], &sps, &[0, 0, 1], &pps, &[0, 0, 1], &idr].concat();
        let (data, parameter_sets) = from_annex_b(Codec::H264, &stream);
        assert_eq!(parameter_sets, [sps.to_vec(), pps.to_vec()]);
        assert_eq!(units(&data).collect::<Vec<_>>(), [idr.as_slice()]);
        assert!(is_keyframe(Codec::H264, &data));
        let (data, _) = from_annex_b(Codec::H264, &[&[0, 0, 1][..], &slice].concat());
        assert!(!is_keyframe(Codec::H264, &data));
    }
}
