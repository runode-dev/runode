//! 自己写的码流头：参数集和片头按 H.264、HEVC 的语法逐位写（定长和指数哥伦布码，`BitWriter`），再包成带起始码、
//! 加好防竞争字节的 NAL 单元（`nal_unit`），作为打包头交给驱动原样插进码流。

/// 交给驱动的一段打包头：Annex B 的字节和有效的位数。片头不一定写满最后一个字节，后面接着的片数据由显卡写。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Packed {
    pub(super) data: Vec<u8>,
    pub(super) bits: u32,
}

impl Packed {
    /// 接上另一段；前面这段要正好写满字节。
    pub(super) fn append(&mut self, other: Packed) {
        debug_assert_eq!(self.bits as usize, self.data.len() * 8);
        self.data.extend(other.data);
        self.bits += other.bits;
    }
}

/// 从高位往低位逐位写的 RBSP。
#[derive(Default)]
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    /// 最后一个字节里已经写了几位，0 表示正好写满。
    used: u32,
}

impl BitWriter {
    pub(super) fn bit(&mut self, bit: bool) {
        if self.used == 0 {
            self.bytes.push(0);
        }
        if bit && let Some(last) = self.bytes.last_mut() {
            *last |= 0x80 >> self.used;
        }
        self.used = (self.used + 1) % 8;
    }

    /// `value` 的低 `count` 位，高位先写。
    pub(super) fn bits(&mut self, value: u32, count: u32) {
        debug_assert!(count <= 32 && (count == 32 || value >> count == 0), "{value} does not fit in {count} bits");
        for shift in (0..count).rev() {
            self.bit(value >> shift & 1 != 0);
        }
    }

    /// 无符号指数哥伦布码 `ue(v)`。
    pub(super) fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let len = code.ilog2();
        self.bits(0, len);
        self.bit(true);
        self.bits((code - (1 << len)) as u32, len);
    }

    /// 有符号指数哥伦布码 `se(v)`。
    pub(super) fn se(&mut self, value: i32) {
        let magnitude = value.unsigned_abs();
        self.ue(if value > 0 { 2 * magnitude - 1 } else { 2 * magnitude });
    }

    /// `rbsp_trailing_bits()`，HEVC 片头末尾的 `byte_alignment()` 也是这样：一个 1，再补 0 到字节边界。
    pub(super) fn trailing_bits(&mut self) {
        self.bit(true);
        while self.used != 0 {
            self.bit(false);
        }
    }

    /// 补 1 到字节边界：H.264 用 CABAC 时片数据前的 `cabac_alignment_one_bit`。
    pub(super) fn align_with_ones(&mut self) {
        while self.used != 0 {
            self.bit(true);
        }
    }

    #[cfg(test)]
    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// 一个 NAL 单元：四字节起始码，`header` 和 `payload` 里该加防竞争字节的地方加上 `0x03`。`payload` 最后一个字节
/// 没写满时，没写的位不算进 `Packed::bits`。
pub(super) fn nal_unit(header: &[u8], payload: &BitWriter) -> Packed {
    let mut data = Vec::with_capacity(4 + header.len() + payload.bytes.len() * 9 / 8 + 1);
    data.extend([0, 0, 0, 1]);
    let mut zeros = 0;
    for &byte in header.iter().chain(&payload.bytes) {
        if zeros >= 2 && byte <= 3 {
            data.push(3);
            zeros = 0;
        }
        data.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    let unused = if payload.used == 0 { 0 } else { 8 - payload.used };
    let bits = data.len() as u32 * 8 - unused;
    Packed { data, bits }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(write: impl FnOnce(&mut BitWriter)) -> String {
        let mut writer = BitWriter::default();
        write(&mut writer);
        let used = writer.used;
        let bytes = writer.into_bytes();
        let mut text: String = bytes.iter().map(|byte| format!("{byte:08b}")).collect();
        if used != 0 {
            text.truncate(text.len() - (8 - used as usize));
        }
        text
    }

    #[test]
    fn writes_exp_golomb_codes() {
        assert_eq!(written(|w| w.ue(0)), "1");
        assert_eq!(written(|w| w.ue(1)), "010");
        assert_eq!(written(|w| w.ue(2)), "011");
        assert_eq!(written(|w| w.ue(7)), "0001000");
        assert_eq!(written(|w| w.se(1)), "010");
        assert_eq!(written(|w| w.se(-1)), "011");
        assert_eq!(written(|w| w.se(2)), "00100");
        assert_eq!(written(|w| w.se(-2)), "00101");
        assert_eq!(written(|w| w.se(0)), "1");
        let max = written(|w| w.ue(u32::MAX));
        assert_eq!(max.len(), 65);
        assert_eq!(&max[..33], "000000000000000000000000000000001");
    }

    #[test]
    fn writes_fixed_bits_and_alignment() {
        assert_eq!(written(|w| w.bits(0b101, 3)), "101");
        assert_eq!(written(|w| w.bits(0xffff, 16)), "1111111111111111");
        assert_eq!(
            written(|w| {
                w.bits(0b10, 2);
                w.trailing_bits();
            }),
            "10100000"
        );
        assert_eq!(
            written(|w| {
                w.bit(false);
                w.align_with_ones();
            }),
            "01111111"
        );
        // 已经在字节边界上时不补。
        assert_eq!(
            written(|w| {
                w.bits(0, 8);
                w.align_with_ones();
            }),
            "00000000"
        );
    }

    /// 两个 0 字节后跟着不大于 3 的字节时插 `0x03`；没写满的最后一个字节不算进位数。
    #[test]
    fn adds_emulation_prevention() {
        let mut payload = BitWriter::default();
        for byte in [0, 0, 1, 0, 0, 0, 0, 4, 0, 0] {
            payload.bits(byte, 8);
        }
        payload.bits(0b1, 1);
        let packed = nal_unit(&[0x67], &payload);
        assert_eq!(packed.data, [0, 0, 0, 1, 0x67, 0, 0, 3, 1, 0, 0, 3, 0, 0, 4, 0, 0, 0x80]);
        assert_eq!(packed.bits, 17 * 8 + 1);
        let mut joined = nal_unit(&[0x68], &BitWriter::default());
        joined.append(packed);
        assert_eq!(joined.bits, 5 * 8 + 17 * 8 + 1);
    }
}
