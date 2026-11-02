//! 在 CPU 上处理 BGRA 的画面，给没有能用的显卡时的软件编码器：把转过的屏幕的画面转回用户看到的方向
//! （`rotate_bgra`），缩放并转成 NV12（`from_bgra`，BT.709 的有限范围）。缩小时每个像素取它盖住的那块源像素的
//! 平均，文字不会断笔；放大时取最近的源像素。Windows 的解码器解出的 NV12 也在这里换回 BGRA（`to_bgra`）。

/// 画面在 NV12 里占的位置，宽高比和目标不同时两边留黑边。四个值都是偶数，色度的 2×2 块不会跨过边。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub(crate) left: u32,
    pub(crate) top: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Area {
    /// 把 `source` 等比缩放后居中放进 `target`。`target` 的宽高是不小于 2 的偶数。
    pub(crate) fn letterbox(source: (u32, u32), target: (u32, u32)) -> Self {
        let (sw, sh) = (f64::from(source.0.max(1)), f64::from(source.1.max(1)));
        let (tw, th) = (f64::from(target.0), f64::from(target.1));
        let scale = (tw / sw).min(th / sh);
        let even = |value: f64, limit: u32| ((value.round() as u32) & !1).clamp(2, limit);
        let (width, height) = (even(sw * scale, target.0), even(sh * scale, target.1));
        Self { left: ((target.0 - width) / 2) & !1, top: ((target.1 - height) / 2) & !1, width, height }
    }
}

/// NV12 一帧的字节数：亮度一个平面，色度两两交错、横竖各减半。
pub(crate) fn frame_len((width, height): (u32, u32)) -> usize {
    width as usize * height as usize * 3 / 2
}

/// 把 `source`（`size` 大小的 BGRA，每行 `stride` 字节）缩放进 `out`（`target` 大小的 NV12）的 `area`，
/// `area` 外面填黑。`out` 的长度至少是 `frame_len(target)`。
pub(crate) fn from_bgra(
    source: &[u8],
    stride: usize,
    size: (u32, u32),
    out: &mut [u8],
    target: (u32, u32),
    area: Area,
) {
    let (width, height) = (target.0 as usize, target.1 as usize);
    let (luma, chroma) = out[..frame_len(target)].split_at_mut(width * height);
    luma.fill(16);
    chroma.fill(128);
    let columns = spans(size.0, area.width);
    let rows = spans(size.1, area.height);
    let (left, top) = (area.left as usize, area.top as usize);
    // 一对目标行的平均 RGB，色度按 2×2 取平均。
    let mut pair = [vec![[0u32; 3]; columns.len()], vec![[0u32; 3]; columns.len()]];
    for (index, rows) in rows.chunks(2).enumerate() {
        for (half, span) in rows.iter().enumerate() {
            average_row(source, stride, span, &columns, &mut pair[half]);
            let y = top + index * 2 + half;
            for (x, rgb) in pair[half].iter().enumerate() {
                luma[y * width + left + x] = to_y(*rgb);
            }
        }
        let y = top / 2 + index;
        for x in 0..columns.len() / 2 {
            let mut sum = [0u32; 3];
            for rgb in [pair[0][x * 2], pair[0][x * 2 + 1], pair[1][x * 2], pair[1][x * 2 + 1]] {
                for channel in 0..3 {
                    sum[channel] += rgb[channel];
                }
            }
            let (u, v) = to_uv(sum.map(|value| (value + 2) / 4));
            let at = y * width + left + x * 2;
            chroma[at] = u;
            chroma[at + 1] = v;
        }
    }
}

/// 把 `source`（`size` 大小的 BGRA，每行 `stride` 字节）顺时针转 `quarters` 个直角，写进 `out`：转后的宽高，
/// 行与行紧挨着。`out` 的长度至少是 `size` 的像素数乘 4。
#[cfg(any(windows, test))]
pub(crate) fn rotate_bgra(source: &[u8], stride: usize, size: (u32, u32), quarters: u32, out: &mut [u8]) {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let turned_width = if quarters % 2 == 1 { height } else { width };
    for (index, pixel) in out[..width * height * 4].as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % turned_width, index / turned_width);
        let (from_x, from_y) = match quarters % 4 {
            1 => (y, height - 1 - x),
            2 => (width - 1 - x, height - 1 - y),
            3 => (width - 1 - y, x),
            _ => (x, y),
        };
        let at = from_y * stride + from_x * 4;
        pixel.copy_from_slice(&source[at..at + 4]);
    }
}

/// 目标的 `count` 个像素各盖住源的哪一段 `[start, end)`，至少一个源像素。
fn spans(source: u32, count: u32) -> Vec<(usize, usize)> {
    let (source, count) = (u64::from(source.max(1)), u64::from(count));
    (0..count)
        .map(|index| {
            let start = (index * source / count) as usize;
            let end = (((index + 1) * source / count) as usize).max(start + 1);
            (start, end)
        })
        .collect()
}

/// 源里 `rows` 那几行、按 `columns` 分好的各块的平均 RGB，写进 `out`。
fn average_row(
    source: &[u8],
    stride: usize,
    &(start, end): &(usize, usize),
    columns: &[(usize, usize)],
    out: &mut [[u32; 3]],
) {
    for (slot, &(left, right)) in out.iter_mut().zip(columns) {
        let mut sum = [0u32; 3];
        for row in start..end {
            let line = &source[row * stride + left * 4..row * stride + right * 4];
            for pixel in line.as_chunks::<4>().0 {
                // BGRA 的字节顺序。
                sum[0] += u32::from(pixel[2]);
                sum[1] += u32::from(pixel[1]);
                sum[2] += u32::from(pixel[0]);
            }
        }
        let count = ((end - start) * (right - left)) as u32;
        *slot = sum.map(|value| (value + count / 2) / count);
    }
}

/// 全范围的 RGB 换成有限范围的亮度（BT.709，系数乘了 256）。
fn to_y([r, g, b]: [u32; 3]) -> u8 {
    (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8
}

fn to_uv([r, g, b]: [u32; 3]) -> (u8, u8) {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    let u = ((-25 * r - 87 * g + 112 * b + 128) >> 8) + 128;
    let v = ((112 * r - 102 * g - 10 * b + 128) >> 8) + 128;
    (u.clamp(16, 240) as u8, v.clamp(16, 240) as u8)
}

/// NV12（Y 平面 `rows` 行、每行 `pitch` 字节，后面紧跟交错的 UV 平面）左上角 `shown` 那块换成 BGRA，按
/// BT.709 有限范围。数据不够长时报错。
#[cfg(any(windows, test))]
pub(crate) fn to_bgra(nv12: &[u8], pitch: usize, rows: usize, shown: (u32, u32)) -> std::io::Result<Vec<u8>> {
    let (width, height) = (shown.0 as usize, shown.1 as usize);
    let uv_start = pitch * rows;
    if width > pitch || height > rows || nv12.len() < uv_start + pitch * height.div_ceil(2) {
        return Err(std::io::Error::other("a decoded frame of the wrong size"));
    }
    let mut out = vec![0u8; width * height * 4];
    for (y, row) in out.chunks_exact_mut(width * 4).enumerate() {
        let luma = &nv12[y * pitch..][..width];
        let chroma = &nv12[uv_start + (y / 2) * pitch..][..width.next_multiple_of(2).min(pitch)];
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let c = 298 * (i32::from(luma[x]) - 16);
            let u = i32::from(chroma[x & !1]) - 128;
            let v = i32::from(chroma[(x & !1) + 1]) - 128;
            let clamp = |value: i32| ((value + 128) >> 8).clamp(0, 255) as u8;
            pixel[0] = clamp(c + 541 * u);
            pixel[1] = clamp(c - 55 * u - 136 * v);
            pixel[2] = clamp(c + 459 * v);
            pixel[3] = 255;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid((width, height): (u32, u32), [b, g, r]: [u8; 3]) -> Vec<u8> {
        [b, g, r, 255].repeat(width as usize * height as usize)
    }

    fn convert(source: &[u8], size: (u32, u32), target: (u32, u32)) -> Vec<u8> {
        let mut out = vec![0; frame_len(target)];
        from_bgra(source, size.0 as usize * 4, size, &mut out, target, Area::letterbox(size, target));
        out
    }

    #[test]
    fn white_and_black_hit_the_limited_range() {
        let white = convert(&solid((8, 4), [255; 3]), (8, 4), (8, 4));
        assert!(white[..32].iter().all(|&y| y == 235));
        assert!(white[32..].iter().all(|&c| c == 128));
        let black = convert(&solid((8, 4), [0; 3]), (8, 4), (8, 4));
        assert!(black[..32].iter().all(|&y| y == 16));
        assert!(black[32..].iter().all(|&c| c == 128));
    }

    #[test]
    fn red_matches_bt709() {
        let red = convert(&solid((4, 4), [0, 0, 255]), (4, 4), (4, 4));
        // BT.709 有限范围的纯红：Y 63、Cb 102、Cr 240。
        assert!(red[0].abs_diff(63) <= 1, "Y {}", red[0]);
        assert!(red[16].abs_diff(102) <= 1, "Cb {}", red[16]);
        assert!(red[17].abs_diff(240) <= 1, "Cr {}", red[17]);
    }

    #[test]
    fn shrinking_averages_the_covered_pixels() {
        // 一黑一白相间的竖条缩成一半宽，每个像素是灰的。
        let size = (8, 2);
        let mut source = Vec::new();
        for _ in 0..2 {
            for x in 0..8 {
                source.extend_from_slice(&if x % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
            }
        }
        let out = convert(&source, size, (4, 2));
        assert!(out[..8].iter().all(|&y| y.abs_diff(126) <= 1), "{:?}", &out[..8]);
    }

    #[test]
    fn rotation_turns_clockwise() {
        // 3×2 的画面，每个像素的第一个字节是它的序号，每行多 4 字节的填充。
        let (size, stride) = ((3, 2), 16);
        let mut source = vec![0; stride * 2];
        for index in 0..6 {
            source[index / 3 * stride + index % 3 * 4] = index as u8;
        }
        let turned = |quarters| {
            let mut out = vec![0; 24];
            rotate_bgra(&source, stride, size, quarters, &mut out);
            out.chunks(4).map(|pixel| pixel[0]).collect::<Vec<_>>()
        };
        // 0 1 2      3 0      5 4 3      2 5
        // 3 4 5  ->  4 1  ->  2 1 0  ->  1 4
        //            5 2                 0 3
        assert_eq!(turned(0), [0, 1, 2, 3, 4, 5]);
        assert_eq!(turned(1), [3, 0, 4, 1, 5, 2]);
        assert_eq!(turned(2), [5, 4, 3, 2, 1, 0]);
        assert_eq!(turned(3), [2, 5, 1, 4, 0, 3]);
    }

    #[test]
    fn letterbox_pads_with_black_on_even_bounds() {
        let area = Area::letterbox((1920, 1080), (1280, 1024));
        assert_eq!(area, Area { left: 0, top: 152, width: 1280, height: 720 });
        let area = Area::letterbox((1000, 1000), (1280, 720));
        assert_eq!((area.left % 2, area.top % 2, area.width % 2, area.height % 2), (0, 0, 0, 0));
        assert_eq!(area.height, 720);

        let out = convert(&solid((4, 4), [255; 3]), (4, 4), (8, 4));
        let row: Vec<u8> = out[..8].to_vec();
        assert_eq!(row, [16, 16, 235, 235, 235, 235, 16, 16]);
    }

    /// 灰的、纯红的像素换出来对得上；画面比解码器给的整幅小时只换左上角那块。
    #[test]
    fn converts_to_bgra() {
        // 2x2 的画面放在每行 4 字节、4 行的缓冲里：亮度 16..235 映到 0..255，色度 128 是灰。
        let (pitch, rows) = (4, 4);
        let mut nv12 = vec![0u8; pitch * rows * 3 / 2];
        nv12[..pitch * rows].fill(235);
        nv12[pitch * rows..].fill(128);
        let bgra = to_bgra(&nv12, pitch, rows, (2, 2)).unwrap();
        assert_eq!(bgra.len(), 2 * 2 * 4);
        assert!(bgra.chunks(4).all(|pixel| pixel == [255, 255, 255, 255]));
        // BT.709 有限范围的纯红：Y 63，U 102，V 240。
        nv12[..pitch * rows].fill(63);
        for pair in nv12[pitch * rows..].chunks_mut(2) {
            pair.copy_from_slice(&[102, 240]);
        }
        let bgra = to_bgra(&nv12, pitch, rows, (2, 2)).unwrap();
        let [b, g, r, _] = bgra[..4] else { unreachable!() };
        assert!(r > 250 && g < 5 && b < 5, "{b} {g} {r}");
        assert!(to_bgra(&nv12[..10], pitch, rows, (2, 2)).is_err());
    }
}
