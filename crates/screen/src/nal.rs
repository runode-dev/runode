//! `VideoFrame` 里的 HEVC 码流：每个 NAL 单元前是 `LENGTH_SIZE` 字节的大端长度，不是起始码，
//! VideoToolbox 编出来就是这样；Media Foundation 编出的 Annex B 码流（起始码分隔）由 `from_annex_b` 换过来。

/// NAL 单元前长度字段的字节数，建编码会话时 VideoToolbox 默认就是它。
pub(crate) const LENGTH_SIZE: usize = 4;
/// 参数集（VPS、SPS、PPS）和访问单元分隔符的 NAL 类型。
#[cfg(any(windows, test))]
const VPS: u8 = 32;
#[cfg(any(windows, test))]
const PPS: u8 = 34;
#[cfg(any(windows, test))]
const AUD: u8 = 35;

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

/// 有没有随机访问点（IRAP：BLA、IDR、CRA），解码器能从这一帧开始解。
pub(crate) fn is_keyframe(data: &[u8]) -> bool {
    units(data).any(|unit| matches!(kind(unit), Some(16..=21)))
}

fn kind(unit: &[u8]) -> Option<u8> {
    unit.first().map(|header| (header >> 1) & 0x3f)
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
pub(crate) fn from_annex_b(data: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut parameter_sets = Vec::new();
    for unit in annex_b_units(data) {
        match kind(unit) {
            Some(VPS..=PPS) => parameter_sets.push(unit.to_vec()),
            Some(AUD) => {}
            _ => {
                out.extend((unit.len() as u32).to_be_bytes());
                out.extend_from_slice(unit);
            }
        }
    }
    (out, parameter_sets)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(is_keyframe(&[unit(sei, b"x"), unit(idr, b"frame")].concat()));
        assert!(!is_keyframe(&[unit(sei, b"x"), unit(trail, b"frame")].concat()));
    }

    /// 长度字段越界时不读出界，也不把后面的字节当成 NAL。
    #[test]
    fn stops_at_a_truncated_unit() {
        let mut data = unit(1, b"ok");
        data.extend(100u32.to_be_bytes());
        data.push(19 << 1);
        assert_eq!(units(&data).count(), 1);
        assert!(!is_keyframe(&data));
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
        let (data, parameter_sets) = from_annex_b(&stream);
        assert_eq!(parameter_sets, [nal(VPS, b"v"), nal(33, b"s"), nal(PPS, b"p")]);
        assert_eq!(units(&data).collect::<Vec<_>>(), [nal(19, b"idr").as_slice()]);
        assert!(is_keyframe(&data));
        let (data, parameter_sets) = from_annex_b(&[&[0, 0, 1][..], &nal(1, b"p\0")].concat());
        assert!(parameter_sets.is_empty());
        assert!(!is_keyframe(&data));
    }
}
