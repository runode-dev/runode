//! H.264 的编码参数：给驱动的序列、图像、片参数，和自己写的 SPS、PPS、片头（驱动收打包头时用）。两边按同一套
//! 取值填，驱动自己写头时写出的也是同样的码流。
//!
//! 只出 IDR 和 P 帧，一帧一个片，一个参考帧（上一帧）；POC 用类型 2（由 `frame_num` 推出，片头里不写），所有帧
//! 都当参考帧，参考帧按滑动窗口淘汰。SPS 带 BT.709 有限范围的色彩信息和 `max_num_reorder_frames = 0`，解码端
//! 拿到一帧就能出一帧。

use std::ffi::c_int;

use super::{
    Frame,
    bits::{BitWriter, Packed, nal_unit},
    ffi::{
        self, INVALID_ID, Id, PICTURE_H264_SHORT_TERM_REFERENCE, PictureH264, PictureParameterH264, SequenceH264,
        SliceParameterH264, Zeroable as _, pack,
    },
};

/// `frame_num` 的位数（`log2_max_frame_num_minus4 + 4`）；所有帧都是参考帧，它每帧加一，到头了绕回 0。
const LOG2_MAX_FRAME_NUM: u32 = 8;
/// `pic_init_qp_minus26 + 26`：码率控制自己定每帧的 QP，片头的 `slice_qp_delta` 按它算成 0。
const INIT_QP: u8 = 26;
/// VUI 里的 `log2_max_mv_length_horizontal`、`vertical`：不限制运动矢量。
const LOG2_MAX_MV_LENGTH: u32 = 15;
/// BT.709 在 VUI 里的 `colour_primaries`、`transfer_characteristics`、`matrix_coefficients`。
const BT709: u32 = 1;
/// `video_format` 的「未说明」。
const VIDEO_FORMAT_UNSPECIFIED: u32 = 5;
const SLICE_P: u8 = 0;
const SLICE_I: u8 = 2;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_SLICE: u8 = 1;
const NAL_IDR: u8 = 5;

/// 用哪个档次编：先列的优先。CABAC 只有 Main 和 High 有；High 的 8×8 变换不开（PPS 里不写那几项），各家驱动都编得了。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Profile {
    Main,
    High,
    ConstrainedBaseline,
}

impl Profile {
    pub(super) const PREFERRED: [Self; 3] = [Self::Main, Self::High, Self::ConstrainedBaseline];

    pub(super) fn va(self) -> c_int {
        match self {
            Self::Main => ffi::PROFILE_H264_MAIN,
            Self::High => ffi::PROFILE_H264_HIGH,
            Self::ConstrainedBaseline => ffi::PROFILE_H264_CONSTRAINED_BASELINE,
        }
    }

    fn idc(self) -> u32 {
        match self {
            Self::Main => 77,
            Self::High => 100,
            Self::ConstrainedBaseline => 66,
        }
    }

    fn cabac(self) -> bool {
        self != Self::ConstrainedBaseline
    }
}

/// 一路 H.264 码流的参数，建编码器时定下。
pub(super) struct H264 {
    profile: Profile,
    /// 画面的像素尺寸，编码尺寸多出的部分在 SPS 里裁掉。
    size: (u32, u32),
    /// 宽、高各多少个宏块。
    mbs: (u32, u32),
    level_idc: u8,
}

impl H264 {
    /// `size` 是偶数的画面尺寸，`align` 是驱动要求画面的宽、高对齐到的像素数（2 的幂，不要求时为 1），`fps`、
    /// `bitrate` 只用来挑级别。
    pub(super) fn new(profile: Profile, size: (u32, u32), align: (u32, u32), fps: u32, bitrate: u32) -> Self {
        let mbs = (size.0.next_multiple_of(align.0.max(16)) / 16, size.1.next_multiple_of(align.1.max(16)) / 16);
        Self { profile, size, mbs, level_idc: level(mbs, fps, bitrate) }
    }

    /// 编码的尺寸，按宏块和驱动要求的对齐；画面和重建画面都按它建。
    pub(super) fn coded_size(&self) -> (u32, u32) {
        (self.mbs.0 * 16, self.mbs.1 * 16)
    }

    pub(super) fn sequence(&self, intra_period: u32, bitrate: u32) -> SequenceH264 {
        let (crop_right, crop_bottom) = self.crop();
        SequenceH264 {
            seq_parameter_set_id: 0,
            level_idc: self.level_idc,
            intra_period,
            intra_idr_period: intra_period,
            ip_period: 1,
            bits_per_second: bitrate,
            max_num_ref_frames: 1,
            picture_width_in_mbs: self.mbs.0 as u16,
            picture_height_in_mbs: self.mbs.1 as u16,
            seq_fields: pack(&[
                (1, 2), // chroma_format_idc：4:2:0
                (1, 1), // frame_mbs_only_flag
                (0, 1),
                (0, 1),
                (1, 1), // direct_8x8_inference_flag
                (LOG2_MAX_FRAME_NUM - 4, 4),
                (2, 2), // pic_order_cnt_type
                (0, 4),
                (0, 1),
            ]),
            frame_cropping_flag: u8::from(crop_right != 0 || crop_bottom != 0),
            frame_crop_right_offset: crop_right,
            frame_crop_bottom_offset: crop_bottom,
            vui_parameters_present_flag: 1,
            vui_fields: pack(&[
                (0, 1),
                (0, 1),
                (1, 1), // bitstream_restriction_flag
                (LOG2_MAX_MV_LENGTH, 5),
                (LOG2_MAX_MV_LENGTH, 5),
                (0, 1),
                (0, 1),
                (1, 1), // motion_vectors_over_pic_boundaries_flag
            ]),
            ..SequenceH264::zeroed()
        }
    }

    pub(super) fn picture(&self, frame: &Frame, coded: Id) -> PictureParameterH264 {
        let mut reference_frames = [PictureH264::invalid(); 16];
        if let Some(reference) = frame.reference {
            reference_frames[0] = picture(reference, frame.index - 1, PICTURE_H264_SHORT_TERM_REFERENCE);
        }
        PictureParameterH264 {
            curr_pic: picture(frame.recon, frame.index, 0),
            reference_frames,
            coded_buf: coded,
            frame_num: frame_num(frame.index) as u16,
            pic_init_qp: INIT_QP,
            pic_fields: pack(&[
                (u32::from(frame.is_idr()), 1),
                (1, 2), // reference_pic_flag
                (u32::from(self.profile.cabac()), 1),
            ]),
            ..PictureParameterH264::zeroed()
        }
    }

    pub(super) fn slice(&self, frame: &Frame) -> SliceParameterH264 {
        let mut ref_pic_list0 = [PictureH264::invalid(); 32];
        if let Some(reference) = frame.reference {
            ref_pic_list0[0] = picture(reference, frame.index - 1, PICTURE_H264_SHORT_TERM_REFERENCE);
        }
        SliceParameterH264 {
            macroblock_address: 0,
            num_macroblocks: self.mbs.0 * self.mbs.1,
            macroblock_info: INVALID_ID,
            slice_type: if frame.is_idr() { SLICE_I } else { SLICE_P },
            idr_pic_id: idr_pic_id(frame),
            direct_spatial_mv_pred_flag: 1,
            ref_pic_list0,
            ref_pic_list1: [PictureH264::invalid(); 32],
            ..SliceParameterH264::zeroed()
        }
    }

    /// SPS 和 PPS，接在一起当一个序列打包头。
    pub(super) fn parameter_sets(&self) -> Packed {
        let mut packed = self.sps();
        packed.append(self.pps());
        packed
    }

    fn sps(&self) -> Packed {
        let profile = self.profile;
        let mut w = BitWriter::default();
        w.bits(profile.idc(), 8);
        w.bit(profile == Profile::ConstrainedBaseline); // constraint_set0_flag
        w.bit(profile != Profile::High); // constraint_set1_flag
        w.bit(false);
        w.bit(false);
        w.bit(profile != Profile::ConstrainedBaseline); // constraint_set4_flag：只有帧，没有场
        w.bit(profile != Profile::ConstrainedBaseline); // constraint_set5_flag：没有 B 片
        w.bits(0, 2);
        w.bits(u32::from(self.level_idc), 8);
        w.ue(0); // seq_parameter_set_id
        if profile == Profile::High {
            w.ue(1); // chroma_format_idc
            w.ue(0); // bit_depth_luma_minus8
            w.ue(0); // bit_depth_chroma_minus8
            w.bit(false); // qpprime_y_zero_transform_bypass_flag
            w.bit(false); // seq_scaling_matrix_present_flag
        }
        w.ue(LOG2_MAX_FRAME_NUM - 4);
        w.ue(2); // pic_order_cnt_type
        w.ue(1); // max_num_ref_frames
        w.bit(false); // gaps_in_frame_num_value_allowed_flag
        w.ue(self.mbs.0 - 1);
        w.ue(self.mbs.1 - 1);
        w.bit(true); // frame_mbs_only_flag
        w.bit(true); // direct_8x8_inference_flag
        let (crop_right, crop_bottom) = self.crop();
        w.bit(crop_right != 0 || crop_bottom != 0);
        if crop_right != 0 || crop_bottom != 0 {
            w.ue(0);
            w.ue(crop_right);
            w.ue(0);
            w.ue(crop_bottom);
        }
        w.bit(true); // vui_parameters_present_flag
        w.bit(false); // aspect_ratio_info_present_flag
        w.bit(false); // overscan_info_present_flag
        w.bit(true); // video_signal_type_present_flag
        w.bits(VIDEO_FORMAT_UNSPECIFIED, 3);
        w.bit(false); // video_full_range_flag
        w.bit(true); // colour_description_present_flag
        w.bits(BT709, 8);
        w.bits(BT709, 8);
        w.bits(BT709, 8);
        w.bit(false); // chroma_loc_info_present_flag
        w.bit(false); // timing_info_present_flag：只在画面变了时出帧，没有固定帧率
        w.bit(false); // nal_hrd_parameters_present_flag
        w.bit(false); // vcl_hrd_parameters_present_flag
        w.bit(false); // pic_struct_present_flag
        w.bit(true); // bitstream_restriction_flag
        w.bit(true); // motion_vectors_over_pic_boundaries_flag
        w.ue(0); // max_bytes_per_pic_denom
        w.ue(0); // max_bits_per_mb_denom
        w.ue(LOG2_MAX_MV_LENGTH);
        w.ue(LOG2_MAX_MV_LENGTH);
        w.ue(0); // max_num_reorder_frames
        w.ue(1); // max_dec_frame_buffering
        w.trailing_bits();
        nal_unit(&[nal_header(3, NAL_SPS)], &w)
    }

    fn pps(&self) -> Packed {
        let mut w = BitWriter::default();
        w.ue(0); // pic_parameter_set_id
        w.ue(0); // seq_parameter_set_id
        w.bit(self.profile.cabac()); // entropy_coding_mode_flag
        w.bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.ue(0); // num_slice_groups_minus1
        w.ue(0); // num_ref_idx_l0_default_active_minus1
        w.ue(0); // num_ref_idx_l1_default_active_minus1
        w.bit(false); // weighted_pred_flag
        w.bits(0, 2); // weighted_bipred_idc
        w.se(i32::from(INIT_QP) - 26); // pic_init_qp_minus26
        w.se(0); // pic_init_qs_minus26
        w.se(0); // chroma_qp_index_offset
        w.bit(false); // deblocking_filter_control_present_flag
        w.bit(false); // constrained_intra_pred_flag
        w.bit(false); // redundant_pic_cnt_present_flag
        w.trailing_bits();
        nal_unit(&[nal_header(3, NAL_PPS)], &w)
    }

    /// 片头，到片数据之前为止；用 CABAC 时连同补齐字节的 `cabac_alignment_one_bit`。
    pub(super) fn slice_header(&self, frame: &Frame) -> Packed {
        let idr = frame.is_idr();
        let mut w = BitWriter::default();
        w.ue(0); // first_mb_in_slice
        w.ue(u32::from(if idr { SLICE_I } else { SLICE_P }));
        w.ue(0); // pic_parameter_set_id
        w.bits(frame_num(frame.index), LOG2_MAX_FRAME_NUM);
        if idr {
            w.ue(u32::from(idr_pic_id(frame)));
        } else {
            w.bit(false); // num_ref_idx_active_override_flag
            w.bit(false); // ref_pic_list_modification_flag_l0
        }
        // dec_ref_pic_marking()
        if idr {
            w.bit(false); // no_output_of_prior_pics_flag
            w.bit(false); // long_term_reference_flag
        } else {
            w.bit(false); // adaptive_ref_pic_marking_mode_flag：滑动窗口
        }
        if self.profile.cabac() && !idr {
            w.ue(0); // cabac_init_idc
        }
        w.se(0); // slice_qp_delta
        if self.profile.cabac() {
            w.align_with_ones();
        }
        let header = if idr { nal_header(3, NAL_IDR) } else { nal_header(2, NAL_SLICE) };
        nal_unit(&[header], &w)
    }

    /// 右边、下边裁掉多少，单位是两个像素（4:2:0 的 `CropUnitX`、`CropUnitY`）。
    fn crop(&self) -> (u32, u32) {
        let (width, height) = self.coded_size();
        ((width - self.size.0) / 2, (height - self.size.1) / 2)
    }
}

fn nal_header(ref_idc: u8, kind: u8) -> u8 {
    ref_idc << 5 | kind
}

fn frame_num(index: u32) -> u32 {
    index % (1 << LOG2_MAX_FRAME_NUM)
}

/// 连着的两个 IDR 不能用同一个 `idr_pic_id`。
fn idr_pic_id(frame: &Frame) -> u16 {
    frame.idr as u16
}

/// 离 IDR 第 `index` 帧的参考画面。POC 类型 2 下参考帧的 POC 是 `2 × index`。
fn picture(surface: Id, index: u32, flags: u32) -> PictureH264 {
    let order = index.wrapping_mul(2) as i32;
    PictureH264 {
        picture_id: surface,
        frame_idx: frame_num(index),
        flags,
        top_field_order_cnt: order,
        bottom_field_order_cnt: order,
        ..PictureH264::zeroed()
    }
}

/// 装得下这个尺寸、帧率和码率的最低级别（附录 A 的表 A-1），从 3.0 起挑：更低的级别没有解码器不支持。都装不下时
/// 用最高的 6.2。
fn level(mbs: (u32, u32), fps: u32, bitrate: u32) -> u8 {
    /// `level_idc`、`MaxMBPS`、`MaxFS`、`MaxBR`（1000 bit/s，按 NAL 层乘 1.2）。
    const LEVELS: [(u8, u64, u64, u64); 11] = [
        (30, 40_500, 1_620, 10_000),
        (31, 108_000, 3_600, 14_000),
        (32, 216_000, 5_120, 20_000),
        (40, 245_760, 8_192, 20_000),
        (41, 245_760, 8_192, 50_000),
        (42, 522_240, 8_704, 50_000),
        (50, 589_824, 22_080, 135_000),
        (51, 983_040, 36_864, 240_000),
        (52, 2_073_600, 36_864, 240_000),
        (60, 4_177_920, 139_264, 240_000),
        (61, 8_355_840, 139_264, 480_000),
    ];
    let frame = u64::from(mbs.0) * u64::from(mbs.1);
    let side = u64::from(mbs.0.max(mbs.1));
    LEVELS
        .iter()
        .find(|&&(_, max_rate, max_frame, max_bitrate)| {
            frame <= max_frame
                && side * side <= 8 * max_frame
                && frame * u64::from(fps) <= max_rate
                && u64::from(bitrate) <= max_bitrate * 1200
        })
        .map_or(62, |&(idc, ..)| idc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(index: u32) -> Frame {
        Frame { index, idr: 3, recon: 10 + index % 2, reference: (index > 0).then_some(10 + (index + 1) % 2) }
    }

    #[test]
    fn picks_the_lowest_level_that_fits() {
        assert_eq!(level((80, 45), 30, 5_000_000), 31);
        assert_eq!(level((120, 68), 30, 8_000_000), 40);
        assert_eq!(level((120, 68), 60, 8_000_000), 42);
        assert_eq!(level((120, 68), 30, 30_000_000), 41);
        assert_eq!(level((240, 135), 60, 30_000_000), 52);
        assert_eq!(level((480, 270), 60, 30_000_000), 61);
        assert_eq!(level((480, 270), 120, 30_000_000), 62);
        // 很宽的屏幕按边长限制往上挑。
        assert_eq!(level((400, 20), 30, 5_000_000), 50);
    }

    /// 位域的期望值是 GCC 给同样的字段赋值后读出的整数。
    #[test]
    fn fills_va_bit_fields() {
        let h264 = H264::new(Profile::Main, (1920, 1080), (1, 1), 60, 10_000_000);
        let sequence = h264.sequence(65_535, 10_000_000);
        assert_eq!(sequence.seq_fields, 2341);
        assert_eq!(sequence.vui_fields, 36732);
        assert_eq!((sequence.picture_width_in_mbs, sequence.picture_height_in_mbs), (120, 68));
        assert_eq!(
            (sequence.frame_cropping_flag, sequence.frame_crop_right_offset, sequence.frame_crop_bottom_offset),
            (1, 0, 4)
        );
        assert_eq!(h264.picture(&frame(0), 7).pic_fields, 11);
    }

    /// 驱动要求的对齐比宏块大时按它补，多出的部分在 SPS 里裁掉。
    #[test]
    fn aligns_to_what_the_driver_asks() {
        let h264 = H264::new(Profile::Main, (1366, 768), (64, 16), 30, 5_000_000);
        assert_eq!(h264.coded_size(), (1408, 768));
        assert_eq!(h264.crop(), (21, 0));
        assert_eq!(H264::new(Profile::Main, (1366, 768), (8, 8), 30, 5_000_000).coded_size(), (1376, 768));
    }

    #[test]
    fn references_the_previous_frame() {
        let h264 = H264::new(Profile::Main, (1280, 720), (1, 1), 30, 5_000_000);
        let idr = h264.picture(&frame(0), 7);
        assert_eq!((idr.curr_pic.picture_id, idr.curr_pic.top_field_order_cnt, idr.frame_num), (10, 0, 0));
        assert_eq!(idr.reference_frames[0].picture_id, INVALID_ID);
        let p = h264.picture(&frame(257), 7);
        assert_eq!((p.curr_pic.picture_id, p.curr_pic.top_field_order_cnt, p.frame_num), (11, 514, 1));
        assert_eq!((p.reference_frames[0].picture_id, p.reference_frames[0].frame_idx), (10, 0));
        assert_eq!(p.reference_frames[0].flags, PICTURE_H264_SHORT_TERM_REFERENCE);
        assert_eq!(p.reference_frames[1].flags, ffi::PICTURE_H264_INVALID);
        let slice = h264.slice(&frame(257));
        assert_eq!((slice.slice_type, slice.num_macroblocks, slice.ref_pic_list0[0].picture_id), (SLICE_P, 3600, 10));
        assert_eq!(h264.slice(&frame(0)).slice_type, SLICE_I);
    }

    /// Main 和 Constrained Baseline 的两组字节核对过：接上 x264 照同样设置编出的片数据，FFmpeg 解出的画面和 x264
    /// 原来的码流逐帧相同。High 的由 FFmpeg 的 `trace_headers` 逐个字段核对过。
    #[test]
    fn writes_parameter_sets() {
        let main = H264::new(Profile::Main, (1920, 1080), (1, 1), 60, 10_000_000);
        #[rustfmt::skip]
        assert_eq!(main.parameter_sets(), Packed {
            data: vec![
                0, 0, 0, 1, 0x67, 77, 76, 42, 149, 160, 30, 0, 137, 249, 102, 160, 32, 32, 32, 240, 128, 66, 160,
                0, 0, 0, 1, 0x68, 238, 56, 128,
            ],
            bits: 248,
        });
        let baseline = H264::new(Profile::ConstrainedBaseline, (1280, 720), (1, 1), 30, 5_000_000);
        #[rustfmt::skip]
        assert_eq!(baseline.parameter_sets(), Packed {
            data: vec![
                0, 0, 0, 1, 0x67, 66, 192, 31, 149, 160, 20, 1, 110, 154, 128, 128, 128, 131, 194, 1, 10, 128,
                0, 0, 0, 1, 0x68, 206, 56, 128,
            ],
            bits: 240,
        });
        let high = H264::new(Profile::High, (1366, 768), (1, 1), 30, 5_000_000);
        #[rustfmt::skip]
        assert_eq!(high.parameter_sets(), Packed {
            data: vec![
                0, 0, 0, 1, 0x67, 100, 12, 32, 172, 43, 64, 43, 3, 15, 55, 53, 1, 1, 1, 7, 132, 2, 21,
                0, 0, 0, 1, 0x68, 238, 56, 128,
            ],
            bits: 248,
        });
    }

    /// 同上，和参数集一起核对过；CAVLC 的片头不补齐字节，有效位数不是 8 的倍数。
    #[test]
    fn writes_slice_headers() {
        let main = H264::new(Profile::Main, (1920, 1080), (1, 1), 60, 10_000_000);
        assert_eq!(main.slice_header(&frame(0)), Packed { data: vec![0, 0, 0, 1, 0x65, 184, 1, 15], bits: 64 });
        assert_eq!(main.slice_header(&frame(1)), Packed { data: vec![0, 0, 0, 1, 0x41, 224, 35], bits: 56 });
        // frame_num 绕回了，和第 1 帧一样。
        assert_eq!(main.slice_header(&frame(257)), main.slice_header(&frame(1)));
        let baseline = H264::new(Profile::ConstrainedBaseline, (1280, 720), (1, 1), 30, 5_000_000);
        assert_eq!(baseline.slice_header(&frame(0)), Packed { data: vec![0, 0, 0, 1, 0x65, 184, 1, 8], bits: 61 });
        assert_eq!(baseline.slice_header(&frame(1)), Packed { data: vec![0, 0, 0, 1, 0x41, 224, 34], bits: 55 });
    }
}
