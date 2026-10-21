//! VideoToolbox 编出的 HEVC 码流：每个 NAL 单元前是 `LENGTH_SIZE` 字节的大端长度，不是起始码。

/// NAL 单元前长度字段的字节数，建编码会话时 VideoToolbox 默认就是它。
pub(crate) const LENGTH_SIZE: usize = 4;

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
    units(data).any(|unit| unit.first().is_some_and(|header| matches!((header >> 1) & 0x3f, 16..=21)))
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
}
