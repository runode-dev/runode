//! HEVC Main 的编码参数：给驱动的序列、图像、片参数，和自己写的 VPS、SPS、PPS、片头。两边按同一套取值填；
//! 驱动的序列参数里没有裁边和色彩信息，所以只在驱动收打包头时用。
//!
//! 只出 IDR 和 P 帧，一帧一个片，一个参考帧（上一帧），片头里各带一个短期参考集；驱动不收 P 片时（预测方向报
//! `BI_NOT_EMPTY`）P 帧按两个参考表都是上一帧的 B 片编。CTB、变换块的大小和 AMP、SAO、时域 MV 预测这些工具按驱动
//! 报的能力挑，驱动不报时用各家驱动都支持的保守取值。SPS 带 BT.709 有限范围的色彩信息，`sps_max_num_reorder_pics`
//! 是 0，解码端拿到一帧就能出一帧。

use std::io;

use super::{
    Frame,
    bits::{BitWriter, Packed, nal_unit},
    ffi::{
        FEATURE_REQUIRED, Id, PICTURE_HEVC_RPS_ST_CURR_BEFORE, PictureHevc, PictureParameterHevc, SequenceHevc,
        SliceParameterHevc, Zeroable as _, pack,
    },
};

/// `slice_pic_order_cnt_lsb` 的位数；POC 每帧加一，到头了绕回。
const LOG2_MAX_POC_LSB: u32 = 8;
/// `init_qp_minus26 + 26`：码率控制自己定每帧的 QP，片头的 `slice_qp_delta` 按它算成 0。
const INIT_QP: u8 = 26;
/// 合并候选的个数，取最多的 5 个。
const MAX_MERGE_CANDIDATES: u32 = 5;
const LOG2_MAX_MV_LENGTH: u32 = 15;
const BT709: u32 = 1;
const VIDEO_FORMAT_UNSPECIFIED: u32 = 5;
const PROFILE_MAIN: u32 = 1;
const SLICE_B: u8 = 0;
const SLICE_P: u8 = 1;
const SLICE_I: u8 = 2;
const NAL_TRAIL_R: u8 = 1;
const NAL_IDR_W_RADL: u8 = 19;
const NAL_VPS: u8 = 32;
const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;
/// `VAEncPictureParameterBufferHEVC::coding_type`。
const CODING_I: u32 = 1;
const CODING_P: u32 = 2;

/// `VAConfigAttribValEncHEVCFeatures` 里各项的位置，每项两位。
mod feature {
    pub(super) const SEPARATE_COLOUR_PLANES: u32 = 0;
    pub(super) const SCALING_LISTS: u32 = 1;
    pub(super) const AMP: u32 = 2;
    pub(super) const SAO: u32 = 3;
    pub(super) const PCM: u32 = 4;
    pub(super) const TEMPORAL_MVP: u32 = 5;
    pub(super) const STRONG_INTRA_SMOOTHING: u32 = 6;
    pub(super) const DEPENDENT_SLICES: u32 = 7;
    pub(super) const SIGN_DATA_HIDING: u32 = 8;
    pub(super) const CONSTRAINED_INTRA_PRED: u32 = 9;
    pub(super) const TRANSFORM_SKIP: u32 = 10;
    pub(super) const CU_QP_DELTA: u32 = 11;
    pub(super) const WEIGHTED_PREDICTION: u32 = 12;
    pub(super) const TRANSQUANT_BYPASS: u32 = 13;
    pub(super) const DEBLOCKING_FILTER_DISABLE: u32 = 14;
}

/// 编码用的块大小，都是以 2 为底的对数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Blocks {
    ctb: u32,
    min_cb: u32,
    min_tb: u32,
    max_tb: u32,
    depth_inter: u32,
    depth_intra: u32,
}

impl Blocks {
    /// 按 `VAConfigAttribValEncHEVCBlockSizes` 取驱动支持的最大 CTB、最小 CB 和完整的变换块范围；驱动不报时取 32×32
    /// 的 CTB、16×16 的最小 CB。不合 HEVC Main 的约束时为空。
    fn new(block_sizes: Option<u32>) -> Option<Self> {
        let blocks = match block_sizes {
            Some(value) => {
                let field = |index: u32| value >> (2 * index) & 3;
                Self {
                    ctb: field(0) + 3,
                    min_cb: field(2) + 3,
                    max_tb: field(3) + 2,
                    min_tb: field(4) + 2,
                    depth_inter: field(5),
                    depth_intra: field(7),
                }
            }
            None => Self { ctb: 5, min_cb: 4, min_tb: 2, max_tb: 5, depth_inter: 3, depth_intra: 3 },
        };
        let valid = (4..=6).contains(&blocks.ctb)
            && blocks.min_cb <= blocks.ctb
            && blocks.min_tb < blocks.min_cb
            && blocks.min_tb <= blocks.max_tb
            && blocks.max_tb <= blocks.ctb.min(5)
            && blocks.depth_inter <= blocks.ctb - blocks.min_tb
            && blocks.depth_intra <= blocks.ctb - blocks.min_tb;
        valid.then_some(blocks)
    }
}

/// 开着的编码工具。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tools {
    amp: bool,
    sao: bool,
    temporal_mvp: bool,
    strong_intra_smoothing: bool,
    sign_data_hiding: bool,
    constrained_intra_pred: bool,
    transform_skip: bool,
    cu_qp_delta: bool,
    transquant_bypass: bool,
}

impl Tools {
    /// 按 `VAConfigAttribValEncHEVCFeatures` 挑：AMP、SAO、时域 MV 预测、变换跳过和 CU 级 QP（码率控制要它）
    /// 支持就开，其余的驱动要求时才开；驱动要求而这里写不了的（缩放表、PCM、加权预测这些）为错误。驱动不报时只开
    /// AMP 和 CU 级 QP。
    fn new(features: Option<u32>) -> io::Result<Self> {
        let Some(value) = features else {
            return Ok(Self { amp: true, cu_qp_delta: true, ..Self::default() });
        };
        let level = |index: u32| value >> (2 * index) & 3;
        for (index, name) in [
            (feature::SEPARATE_COLOUR_PLANES, "separate colour planes"),
            (feature::SCALING_LISTS, "scaling lists"),
            (feature::PCM, "PCM"),
            (feature::DEPENDENT_SLICES, "dependent slices"),
            (feature::WEIGHTED_PREDICTION, "weighted prediction"),
            (feature::DEBLOCKING_FILTER_DISABLE, "disabling the deblocking filter"),
        ] {
            if level(index) == FEATURE_REQUIRED {
                return Err(io::Error::new(io::ErrorKind::Unsupported, format!("the HEVC encoder requires {name}")));
            }
        }
        let supported = |index: u32| level(index) != 0;
        let required = |index: u32| level(index) == FEATURE_REQUIRED;
        Ok(Self {
            amp: supported(feature::AMP),
            sao: supported(feature::SAO),
            temporal_mvp: supported(feature::TEMPORAL_MVP),
            strong_intra_smoothing: required(feature::STRONG_INTRA_SMOOTHING),
            sign_data_hiding: required(feature::SIGN_DATA_HIDING),
            constrained_intra_pred: required(feature::CONSTRAINED_INTRA_PRED),
            transform_skip: supported(feature::TRANSFORM_SKIP),
            cu_qp_delta: supported(feature::CU_QP_DELTA),
            transquant_bypass: required(feature::TRANSQUANT_BYPASS),
        })
    }
}

/// 一路 HEVC 码流的参数，建编码器时定下。
pub(super) struct Hevc {
    /// 画面的像素尺寸，编码尺寸多出的部分在 SPS 的一致性窗口里裁掉。
    size: (u32, u32),
    coded: (u32, u32),
    level_idc: u8,
    blocks: Blocks,
    tools: Tools,
    /// 有 `features` 时 CU 级 QP 的深度用到最小 CB；没有时为 0。
    cu_qp_delta_depth: u32,
    /// P 帧按 B 片编，两个参考表都放上一帧。
    p_as_b: bool,
}

impl Hevc {
    /// `size` 是偶数的画面尺寸，`align` 是驱动要求画面的宽、高对齐到的像素数（2 的幂，不要求时为 1），`fps`、
    /// `bitrate` 只用来挑级别。`features`、`block_sizes` 是驱动报的
    /// `VAConfigAttribEncHEVCFeatures`、`VAConfigAttribEncHEVCBlockSizes`，不报时为空。驱动的能力凑不出合规的 HEVC
    /// Main 时错误是 `io::ErrorKind::Unsupported`。
    pub(super) fn new(
        size: (u32, u32),
        align: (u32, u32),
        fps: u32,
        bitrate: u32,
        features: Option<u32>,
        block_sizes: Option<u32>,
        p_as_b: bool,
    ) -> io::Result<Self> {
        let blocks = Blocks::new(block_sizes).ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, format!("unusable HEVC block sizes {block_sizes:#x?}"))
        })?;
        let tools = Tools::new(features)?;
        let min_cb = 1 << blocks.min_cb;
        let coded = (size.0.next_multiple_of(align.0.max(min_cb)), size.1.next_multiple_of(align.1.max(min_cb)));
        let cu_qp_delta_depth = if tools.cu_qp_delta && features.is_some() { blocks.ctb - blocks.min_cb } else { 0 };
        Ok(Self { size, coded, level_idc: level(coded, fps, bitrate), blocks, tools, cu_qp_delta_depth, p_as_b })
    }

    /// 编码的尺寸，按最小 CB 和驱动要求的对齐；画面和重建画面都按它建。
    pub(super) fn coded_size(&self) -> (u32, u32) {
        self.coded
    }

    pub(super) fn sequence(&self, intra_period: u32, bitrate: u32) -> SequenceHevc {
        let (blocks, tools) = (self.blocks, self.tools);
        SequenceHevc {
            general_profile_idc: PROFILE_MAIN as u8,
            general_level_idc: self.level_idc,
            general_tier_flag: 0,
            intra_period,
            intra_idr_period: intra_period,
            ip_period: 1,
            bits_per_second: bitrate,
            pic_width_in_luma_samples: self.coded.0 as u16,
            pic_height_in_luma_samples: self.coded.1 as u16,
            seq_fields: pack(&[
                (1, 2), // chroma_format_idc：4:2:0
                (0, 1),
                (0, 3),
                (0, 3),
                (0, 1),
                (u32::from(tools.strong_intra_smoothing), 1),
                (u32::from(tools.amp), 1),
                (u32::from(tools.sao), 1),
                (0, 1),
                (0, 1),
                (u32::from(tools.temporal_mvp), 1),
            ]),
            log2_min_luma_coding_block_size_minus3: (blocks.min_cb - 3) as u8,
            log2_diff_max_min_luma_coding_block_size: (blocks.ctb - blocks.min_cb) as u8,
            log2_min_transform_block_size_minus2: (blocks.min_tb - 2) as u8,
            log2_diff_max_min_transform_block_size: (blocks.max_tb - blocks.min_tb) as u8,
            max_transform_hierarchy_depth_inter: blocks.depth_inter as u8,
            max_transform_hierarchy_depth_intra: blocks.depth_intra as u8,
            ..SequenceHevc::zeroed()
        }
    }

    pub(super) fn picture(&self, frame: &Frame, coded: Id) -> PictureParameterHevc {
        let tools = self.tools;
        let idr = frame.is_idr();
        let mut reference_frames = [PictureHevc::invalid(); 15];
        if let Some(reference) = frame.reference {
            reference_frames[0] = picture(reference, frame.index - 1, PICTURE_HEVC_RPS_ST_CURR_BEFORE);
        }
        PictureParameterHevc {
            decoded_curr_pic: picture(frame.recon, frame.index, 0),
            reference_frames,
            coded_buf: coded,
            collocated_ref_pic_index: if tools.temporal_mvp { 0 } else { 0xff },
            pic_init_qp: INIT_QP,
            diff_cu_qp_delta_depth: self.cu_qp_delta_depth as u8,
            nal_unit_type: if idr { NAL_IDR_W_RADL } else { NAL_TRAIL_R },
            pic_fields: pack(&[
                (u32::from(idr), 1),
                (if idr { CODING_I } else { CODING_P }, 3),
                (1, 1), // reference_pic_flag
                (0, 1),
                (u32::from(tools.sign_data_hiding), 1),
                (u32::from(tools.constrained_intra_pred), 1),
                (u32::from(tools.transform_skip), 1),
                (u32::from(tools.cu_qp_delta), 1),
                (0, 1),
                (0, 1),
                (u32::from(tools.transquant_bypass), 1),
            ]),
            ..PictureParameterHevc::zeroed()
        }
    }

    pub(super) fn slice(&self, frame: &Frame) -> SliceParameterHevc {
        let tools = self.tools;
        let ctb = 1 << self.blocks.ctb;
        let mut ref_pic_list0 = [PictureHevc::invalid(); 15];
        let mut ref_pic_list1 = [PictureHevc::invalid(); 15];
        if let Some(reference) = frame.reference {
            ref_pic_list0[0] = picture(reference, frame.index - 1, PICTURE_HEVC_RPS_ST_CURR_BEFORE);
            if self.p_as_b {
                ref_pic_list1[0] = ref_pic_list0[0];
            }
        }
        let inter = !frame.is_idr();
        SliceParameterHevc {
            slice_segment_address: 0,
            num_ctu_in_slice: self.coded.0.div_ceil(ctb) * self.coded.1.div_ceil(ctb),
            slice_type: self.slice_type(frame),
            ref_pic_list0,
            ref_pic_list1,
            max_num_merge_cand: MAX_MERGE_CANDIDATES as u8,
            slice_fields: pack(&[
                (1, 1), // last_slice_of_pic_flag
                (0, 1),
                (0, 2),
                (u32::from(inter && tools.temporal_mvp), 1),
                (u32::from(tools.sao), 1),
                (u32::from(tools.sao), 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 2),
                (0, 1),
                (u32::from(inter), 1), // collocated_from_l0_flag：没写进片头时推定为 1
            ]),
            ..SliceParameterHevc::zeroed()
        }
    }

    fn slice_type(&self, frame: &Frame) -> u8 {
        match (frame.is_idr(), self.p_as_b) {
            (true, _) => SLICE_I,
            (false, false) => SLICE_P,
            (false, true) => SLICE_B,
        }
    }

    /// VPS、SPS 和 PPS，接在一起当一个序列打包头。
    pub(super) fn parameter_sets(&self) -> Packed {
        let mut packed = self.vps();
        packed.append(self.sps());
        packed.append(self.pps());
        packed
    }

    fn vps(&self) -> Packed {
        let mut w = BitWriter::default();
        w.bits(0, 4); // vps_video_parameter_set_id
        w.bit(true); // vps_base_layer_internal_flag
        w.bit(true); // vps_base_layer_available_flag
        w.bits(0, 6); // vps_max_layers_minus1
        w.bits(0, 3); // vps_max_sub_layers_minus1
        w.bit(true); // vps_temporal_id_nesting_flag
        w.bits(0xffff, 16);
        self.profile_tier_level(&mut w);
        w.bit(false); // vps_sub_layer_ordering_info_present_flag
        dpb_size(&mut w);
        w.bits(0, 6); // vps_max_layer_id
        w.ue(0); // vps_num_layer_sets_minus1
        w.bit(false); // vps_timing_info_present_flag
        w.bit(false); // vps_extension_flag
        w.trailing_bits();
        nal_unit(&nal_header(NAL_VPS), &w)
    }

    fn sps(&self) -> Packed {
        let (blocks, tools) = (self.blocks, self.tools);
        let mut w = BitWriter::default();
        w.bits(0, 4); // sps_video_parameter_set_id
        w.bits(0, 3); // sps_max_sub_layers_minus1
        w.bit(true); // sps_temporal_id_nesting_flag
        self.profile_tier_level(&mut w);
        w.ue(0); // sps_seq_parameter_set_id
        w.ue(1); // chroma_format_idc
        w.ue(self.coded.0);
        w.ue(self.coded.1);
        // 一致性窗口按色度采样计，4:2:0 是两个像素。
        let (right, bottom) = ((self.coded.0 - self.size.0) / 2, (self.coded.1 - self.size.1) / 2);
        w.bit(right != 0 || bottom != 0);
        if right != 0 || bottom != 0 {
            w.ue(0);
            w.ue(right);
            w.ue(0);
            w.ue(bottom);
        }
        w.ue(0); // bit_depth_luma_minus8
        w.ue(0); // bit_depth_chroma_minus8
        w.ue(LOG2_MAX_POC_LSB - 4);
        w.bit(false); // sps_sub_layer_ordering_info_present_flag
        dpb_size(&mut w);
        w.ue(blocks.min_cb - 3);
        w.ue(blocks.ctb - blocks.min_cb);
        w.ue(blocks.min_tb - 2);
        w.ue(blocks.max_tb - blocks.min_tb);
        w.ue(blocks.depth_inter);
        w.ue(blocks.depth_intra);
        w.bit(false); // scaling_list_enabled_flag
        w.bit(tools.amp);
        w.bit(tools.sao);
        w.bit(false); // pcm_enabled_flag
        w.ue(0); // num_short_term_ref_pic_sets：每个片头自带
        w.bit(false); // long_term_ref_pics_present_flag
        w.bit(tools.temporal_mvp);
        w.bit(tools.strong_intra_smoothing);
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
        w.bit(false); // neutral_chroma_indication_flag
        w.bit(false); // field_seq_flag
        w.bit(false); // frame_field_info_present_flag
        w.bit(false); // default_display_window_flag
        w.bit(false); // vui_timing_info_present_flag：只在画面变了时出帧，没有固定帧率
        w.bit(true); // bitstream_restriction_flag
        w.bit(false); // tiles_fixed_structure_flag
        w.bit(true); // motion_vectors_over_pic_boundaries_flag
        w.bit(true); // restricted_ref_pic_lists_flag
        w.ue(0); // min_spatial_segmentation_idc
        w.ue(0); // max_bytes_per_pic_denom
        w.ue(0); // max_bits_per_min_cu_denom
        w.ue(LOG2_MAX_MV_LENGTH);
        w.ue(LOG2_MAX_MV_LENGTH);
        w.bit(false); // sps_extension_present_flag
        w.trailing_bits();
        nal_unit(&nal_header(NAL_SPS), &w)
    }

    fn pps(&self) -> Packed {
        let tools = self.tools;
        let mut w = BitWriter::default();
        w.ue(0); // pps_pic_parameter_set_id
        w.ue(0); // pps_seq_parameter_set_id
        w.bit(false); // dependent_slice_segments_enabled_flag
        w.bit(false); // output_flag_present_flag
        w.bits(0, 3); // num_extra_slice_header_bits
        w.bit(tools.sign_data_hiding);
        w.bit(false); // cabac_init_present_flag
        w.ue(0); // num_ref_idx_l0_default_active_minus1
        w.ue(0); // num_ref_idx_l1_default_active_minus1
        w.se(i32::from(INIT_QP) - 26); // init_qp_minus26
        w.bit(tools.constrained_intra_pred);
        w.bit(tools.transform_skip);
        w.bit(tools.cu_qp_delta);
        if tools.cu_qp_delta {
            w.ue(self.cu_qp_delta_depth);
        }
        w.se(0); // pps_cb_qp_offset
        w.se(0); // pps_cr_qp_offset
        w.bit(false); // pps_slice_chroma_qp_offsets_present_flag
        w.bit(false); // weighted_pred_flag
        w.bit(false); // weighted_bipred_flag
        w.bit(tools.transquant_bypass);
        w.bit(false); // tiles_enabled_flag
        w.bit(false); // entropy_coding_sync_enabled_flag
        w.bit(false); // pps_loop_filter_across_slices_enabled_flag
        w.bit(false); // deblocking_filter_control_present_flag
        w.bit(false); // pps_scaling_list_data_present_flag
        w.bit(false); // lists_modification_present_flag
        w.ue(0); // log2_parallel_merge_level_minus2
        w.bit(false); // slice_segment_header_extension_present_flag
        w.bit(false); // pps_extension_present_flag
        w.trailing_bits();
        nal_unit(&nal_header(NAL_PPS), &w)
    }

    /// 片头，连同末尾补齐字节的 `byte_alignment()`。
    pub(super) fn slice_header(&self, frame: &Frame) -> Packed {
        let tools = self.tools;
        let idr = frame.is_idr();
        let slice_type = self.slice_type(frame);
        let mut w = BitWriter::default();
        w.bit(true); // first_slice_segment_in_pic_flag
        if idr {
            w.bit(false); // no_output_of_prior_pics_flag
        }
        w.ue(0); // slice_pic_parameter_set_id
        w.ue(u32::from(slice_type));
        if !idr {
            w.bits(frame.index % (1 << LOG2_MAX_POC_LSB), LOG2_MAX_POC_LSB);
            w.bit(false); // short_term_ref_pic_set_sps_flag
            // st_ref_pic_set(0)：前面一帧，POC 差 1，当前帧要用。
            w.ue(1); // num_negative_pics
            w.ue(0); // num_positive_pics
            w.ue(0); // delta_poc_s0_minus1
            w.bit(true); // used_by_curr_pic_s0_flag
            if tools.temporal_mvp {
                w.bit(true); // slice_temporal_mvp_enabled_flag
            }
        }
        if tools.sao {
            w.bit(true); // slice_sao_luma_flag
            w.bit(true); // slice_sao_chroma_flag
        }
        if !idr {
            w.bit(false); // num_ref_idx_active_override_flag
            if slice_type == SLICE_B {
                w.bit(false); // mvd_l1_zero_flag
            }
            if tools.temporal_mvp && slice_type == SLICE_B {
                w.bit(true); // collocated_from_l0_flag
            }
            w.ue(5 - MAX_MERGE_CANDIDATES); // five_minus_max_num_merge_cand
        }
        w.se(0); // slice_qp_delta
        w.trailing_bits(); // byte_alignment()
        let header = if idr { NAL_IDR_W_RADL } else { NAL_TRAIL_R };
        nal_unit(&nal_header(header), &w)
    }

    /// `profile_tier_level(1, 0)`：Main 档次、Main 层级，只有逐行的帧。
    fn profile_tier_level(&self, w: &mut BitWriter) {
        w.bits(0, 2); // general_profile_space
        w.bit(false); // general_tier_flag
        w.bits(PROFILE_MAIN, 5);
        // general_profile_compatibility_flag[j]：Main 的码流也合 Main 10。
        for j in 0..32 {
            w.bit(j == 1 || j == 2);
        }
        w.bit(true); // general_progressive_source_flag
        w.bit(false); // general_interlaced_source_flag
        w.bit(true); // general_non_packed_constraint_flag
        w.bit(true); // general_frame_only_constraint_flag
        w.bits(0, 32); // general_reserved_zero_43bits
        w.bits(0, 11);
        w.bit(false); // general_inbld_flag
        w.bits(u32::from(self.level_idc), 8);
    }
}

/// `max_dec_pic_buffering_minus1`、`max_num_reorder_pics`、`max_latency_increase_plus1`：解码时存一帧参考加当前帧，
/// 不重排。
fn dpb_size(w: &mut BitWriter) {
    w.ue(1);
    w.ue(0);
    w.ue(0);
}

fn nal_header(kind: u8) -> [u8; 2] {
    // nuh_layer_id 是 0，nuh_temporal_id_plus1 是 1。
    [kind << 1, 1]
}

fn picture(surface: Id, index: u32, flags: u32) -> PictureHevc {
    PictureHevc { picture_id: surface, pic_order_cnt: index as i32, flags, ..PictureHevc::zeroed() }
}

/// 装得下这个尺寸、帧率和码率的最低级别（附录 A 的表 A.8、A.9，Main 层级），从 3 起挑：更低的级别没有解码器
/// 不支持。都装不下时用最高的 6.2。
fn level(coded: (u32, u32), fps: u32, bitrate: u32) -> u8 {
    /// `general_level_idc`、`MaxLumaPs`、`MaxLumaSr`、`MaxBR`（1000 bit/s，按 NAL 层乘 1.1）。
    const LEVELS: [(u8, u64, u64, u64); 9] = [
        (90, 552_960, 16_588_800, 6_000),
        (93, 983_040, 33_177_600, 10_000),
        (120, 2_228_224, 66_846_720, 12_000),
        (123, 2_228_224, 133_693_440, 20_000),
        (150, 8_912_896, 267_386_880, 25_000),
        (153, 8_912_896, 534_773_760, 40_000),
        (156, 8_912_896, 1_069_547_520, 60_000),
        (180, 35_651_584, 1_069_547_520, 60_000),
        (183, 35_651_584, 2_139_095_040, 120_000),
    ];
    let samples = u64::from(coded.0) * u64::from(coded.1);
    let side = u64::from(coded.0.max(coded.1));
    LEVELS
        .iter()
        .find(|&&(_, max_samples, max_rate, max_bitrate)| {
            samples <= max_samples
                && side * side <= 8 * max_samples
                && samples * u64::from(fps) <= max_rate
                && u64::from(bitrate) <= max_bitrate * 1100
        })
        .map_or(186, |&(idc, ..)| idc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(index: u32) -> Frame {
        Frame { index, idr: 0, recon: 20 + index % 2, reference: (index > 0).then_some(20 + (index + 1) % 2) }
    }

    /// 期望值是 GCC 给 `VAConfigAttribValEncHEVCFeatures`、`VAConfigAttribValEncHEVCBlockSizes` 设同样的字段后读出的
    /// 整数：AMP、时域 MV 预测、变换跳过支持，SAO、CU 级 QP 要求，关掉去块滤波要求；CTB 64 到 16，最小 CB 8，变换块
    /// 4 到 32，层级深度 0 到 2。
    const FEATURES: u32 = 546_309_264;
    const BLOCK_SIZES: u32 = 35_015;
    /// 同上：AMP、SAO、时域 MV 预测、变换跳过、CU 级 QP 支持，强帧内平滑、符号位隐藏、变换量化旁路要求。
    const ALL_TOOLS: u32 = 139_600_976;
    /// `ALL_TOOLS` 去掉 CU 级 QP。
    const RICH: u32 = ALL_TOOLS & !(3 << 22);
    /// 只支持 AMP。
    const AMP: u32 = 1 << 4;
    /// CTB 32、最小 CB 16、变换块 4 到 32、层级深度 2。
    const PLAIN_BLOCKS: u32 = 2 | 1 << 4 | 3 << 6 | 2 << 10 | 2 << 14;
    /// CTB 64、最小 CB 8、变换块 4 到 32、层级深度 1。
    const RICH_BLOCKS: u32 = 3 | 3 << 6 | 1 << 10 | 1 << 14;

    #[test]
    fn reads_driver_capabilities() {
        let blocks = Blocks::new(Some(BLOCK_SIZES)).unwrap();
        assert_eq!(blocks, Blocks { ctb: 6, min_cb: 3, min_tb: 2, max_tb: 5, depth_inter: 2, depth_intra: 2 });
        assert!(Blocks::new(None).is_some());
        // CTB 8×8 不合 Main。
        assert!(Blocks::new(Some(0)).is_none());
        let err = Tools::new(Some(FEATURES)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        let tools = Tools::new(Some(FEATURES & !(3 << 28))).unwrap();
        assert_eq!(
            tools,
            Tools {
                amp: true,
                sao: true,
                temporal_mvp: true,
                transform_skip: true,
                cu_qp_delta: true,
                ..Tools::default()
            }
        );
        assert_eq!(Tools::new(None).unwrap(), Tools { amp: true, cu_qp_delta: true, ..Tools::default() });
    }

    #[test]
    fn picks_the_lowest_level_that_fits() {
        assert_eq!(level((1280, 720), 30, 5_000_000), 93);
        assert_eq!(level((1920, 1080), 30, 8_000_000), 120);
        assert_eq!(level((1920, 1080), 60, 8_000_000), 123);
        assert_eq!(level((3840, 2160), 60, 20_000_000), 153);
        assert_eq!(level((7680, 4320), 60, 20_000_000), 183);
        assert_eq!(level((7680, 4320), 120, 20_000_000), 186);
    }

    /// 位域的期望值是 GCC 给同样的字段赋值后读出的整数。
    #[test]
    fn fills_va_bit_fields() {
        let hevc = Hevc::new((1920, 1080), (1, 1), 60, 10_000_000, Some(ALL_TOOLS), Some(BLOCK_SIZES), false).unwrap();
        assert_eq!(hevc.cu_qp_delta_depth, 3);
        assert_eq!(hevc.sequence(65_535, 10_000_000).seq_fields, 39937);
        assert_eq!(hevc.picture(&frame(1), 7).pic_fields, 4948);
        assert_eq!(hevc.slice(&frame(1)).slice_fields, 8305);
    }

    #[test]
    fn references_the_previous_frame() {
        let hevc = Hevc::new((1920, 1080), (1, 1), 60, 10_000_000, None, None, false).unwrap();
        assert_eq!(hevc.coded_size(), (1920, 1088));
        let sequence = hevc.sequence(65_535, 10_000_000);
        assert_eq!(
            (sequence.log2_min_luma_coding_block_size_minus3, sequence.log2_diff_max_min_luma_coding_block_size),
            (1, 1)
        );
        let idr = hevc.picture(&frame(0), 7);
        assert_eq!((idr.decoded_curr_pic.picture_id, idr.nal_unit_type, idr.collocated_ref_pic_index), (20, 19, 0xff));
        let p = hevc.picture(&frame(300), 7);
        assert_eq!((p.decoded_curr_pic.pic_order_cnt, p.reference_frames[0].pic_order_cnt), (300, 299));
        assert_eq!(
            (p.reference_frames[0].picture_id, p.reference_frames[0].flags),
            (21, PICTURE_HEVC_RPS_ST_CURR_BEFORE)
        );
        let slice = hevc.slice(&frame(300));
        assert_eq!((slice.slice_type, slice.num_ctu_in_slice), (SLICE_P, 60 * 34));
        assert_eq!(slice.ref_pic_list1[0].picture_id, super::super::ffi::INVALID_ID);
        let aligned = Hevc::new((1366, 768), (64, 16), 30, 5_000_000, None, Some(BLOCK_SIZES), false).unwrap();
        assert_eq!(aligned.coded_size(), (1408, 768));
        let gpb = Hevc::new((1920, 1080), (1, 1), 60, 10_000_000, None, None, true).unwrap();
        let slice = gpb.slice(&frame(300));
        assert_eq!((slice.slice_type, slice.ref_pic_list1[0].picture_id), (SLICE_B, 21));
    }

    /// 前两组字节核对过：接上 x265 照同样的块大小和工具编出的片数据，FFmpeg 解出的画面和 x265 原来的码流逐帧相同。
    /// P 帧按 B 片编、开着 CU 级 QP 的那组由 FFmpeg 的 `trace_headers` 逐个字段核对过。
    #[test]
    fn writes_parameter_sets() {
        let plain = Hevc::new((1280, 720), (1, 1), 30, 5_000_000, Some(AMP), Some(PLAIN_BLOCKS), false).unwrap();
        #[rustfmt::skip]
        assert_eq!(plain.parameter_sets(), Packed {
            data: vec![
                0, 0, 0, 1, 64, 1, 12, 1, 255, 255, 1, 96, 0, 0, 3, 0, 176, 0, 0, 3, 0, 0, 3, 0, 93, 44, 9,
                0, 0, 0, 1, 66, 1, 1, 1, 96, 0, 0, 3, 0, 176, 0, 0, 3, 0, 0, 3, 0, 93, 160, 2, 128, 128, 45, 22, 82, 210,
                145, 180, 137, 168, 8, 8, 8, 23, 194, 1, 4,
                0, 0, 0, 1, 68, 1, 192, 113, 128, 18,
            ],
            bits: 624,
        });
        let rich = Hevc::new((1280, 720), (1, 1), 30, 5_000_000, Some(RICH), Some(RICH_BLOCKS), false).unwrap();
        #[rustfmt::skip]
        assert_eq!(rich.parameter_sets(), Packed {
            data: vec![
                0, 0, 0, 1, 64, 1, 12, 1, 255, 255, 1, 96, 0, 0, 3, 0, 176, 0, 0, 3, 0, 0, 3, 0, 93, 44, 9,
                0, 0, 0, 1, 66, 1, 1, 1, 96, 0, 0, 3, 0, 176, 0, 0, 3, 0, 0, 3, 0, 93, 160, 2, 128, 128, 45, 22, 82, 228,
                145, 38, 185, 168, 8, 8, 8, 23, 194, 1, 4,
                0, 0, 0, 1, 68, 1, 193, 117, 136, 18,
            ],
            bits: 624,
        });
        let gpb = Hevc::new((1366, 768), (1, 1), 30, 5_000_000, Some(ALL_TOOLS), Some(BLOCK_SIZES), true).unwrap();
        #[rustfmt::skip]
        assert_eq!(gpb.parameter_sets(), Packed {
            data: vec![
                0, 0, 0, 1, 64, 1, 12, 1, 255, 255, 1, 96, 0, 0, 3, 0, 176, 0, 0, 3, 0, 0, 3, 0, 120, 44, 9,
                0, 0, 0, 1, 66, 1, 1, 1, 96, 0, 0, 3, 0, 176, 0, 0, 3, 0, 0, 3, 0, 120, 160, 2, 172, 128, 48, 29, 121,
                75, 146, 70, 218, 230, 160, 32, 32, 32, 95, 8, 4, 16,
                0, 0, 0, 1, 68, 1, 193, 118, 76, 64, 144,
            ],
            bits: 640,
        });
    }

    /// 同上，和参数集一起核对过。
    #[test]
    fn writes_slice_headers() {
        let plain = Hevc::new((1280, 720), (1, 1), 30, 5_000_000, Some(AMP), Some(PLAIN_BLOCKS), false).unwrap();
        assert_eq!(plain.slice_header(&frame(0)), Packed { data: vec![0, 0, 0, 1, 38, 1, 175], bits: 56 });
        assert_eq!(plain.slice_header(&frame(1)), Packed { data: vec![0, 0, 0, 1, 2, 1, 208, 9, 119], bits: 72 });
        // POC 的低位绕回了：300 写成 44。
        assert_eq!(plain.slice_header(&frame(300)), Packed { data: vec![0, 0, 0, 1, 2, 1, 209, 97, 119], bits: 72 });
        let rich = Hevc::new((1280, 720), (1, 1), 30, 5_000_000, Some(RICH), Some(RICH_BLOCKS), false).unwrap();
        assert_eq!(rich.slice_header(&frame(0)), Packed { data: vec![0, 0, 0, 1, 38, 1, 175, 192], bits: 64 });
        assert_eq!(rich.slice_header(&frame(1)), Packed { data: vec![0, 0, 0, 1, 2, 1, 208, 9, 126, 224], bits: 80 });
        let gpb = Hevc::new((1366, 768), (1, 1), 30, 5_000_000, Some(ALL_TOOLS), Some(BLOCK_SIZES), true).unwrap();
        assert_eq!(gpb.slice_header(&frame(1)), Packed { data: vec![0, 0, 0, 1, 2, 1, 224, 37, 249, 224], bits: 80 });
    }
}
