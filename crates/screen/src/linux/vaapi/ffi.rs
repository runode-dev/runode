//! libva 的 C 接口：`libva.so.2` 和 `libva-drm.so.2` 在运行时 dlopen（`Lib`），没装 libva 的系统上可执行文件照样
//! 跑。用到的结构体和常量照 libva 1.x 的头文件（`va.h`、`va_enc_h264.h`、`va_enc_hevc.h`）手写，布局由本模块的
//! 测试按 C 编译器给的 `sizeof`、`offsetof` 核对。C 的位域写成一个 `u32`，按 `pack` 从低位往高位排（x86-64 和
//! AArch64 上 GCC 的排法）。

use std::{
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    io,
    sync::OnceLock,
};

pub(super) type VaDisplay = *mut c_void;
pub(super) type Status = c_int;
/// `VAGenericID`：配置、上下文、画面、缓冲、图像的编号。
pub(super) type Id = c_uint;

pub(super) const SUCCESS: Status = 0;
pub(super) const INVALID_ID: Id = 0xffff_ffff;
pub(super) const ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;

pub(super) const PROFILE_H264_MAIN: c_int = 6;
pub(super) const PROFILE_H264_HIGH: c_int = 7;
pub(super) const PROFILE_H264_CONSTRAINED_BASELINE: c_int = 13;
pub(super) const PROFILE_HEVC_MAIN: c_int = 17;

pub(super) const ENTRYPOINT_ENC_SLICE: c_int = 6;
pub(super) const ENTRYPOINT_ENC_SLICE_LP: c_int = 8;

pub(super) const ATTRIB_RT_FORMAT: u32 = 0;
pub(super) const ATTRIB_RATE_CONTROL: u32 = 5;
pub(super) const ATTRIB_ENC_PACKED_HEADERS: u32 = 10;
pub(super) const ATTRIB_ENC_MAX_REF_FRAMES: u32 = 13;
pub(super) const ATTRIB_MAX_PICTURE_WIDTH: u32 = 18;
pub(super) const ATTRIB_MAX_PICTURE_HEIGHT: u32 = 19;
pub(super) const ATTRIB_PREDICTION_DIRECTION: u32 = 39;
pub(super) const ATTRIB_ENC_HEVC_FEATURES: u32 = 50;
pub(super) const ATTRIB_ENC_HEVC_BLOCK_SIZES: u32 = 51;

pub(super) const RT_FORMAT_YUV420: u32 = 0x1;
pub(super) const RC_CBR: u32 = 0x2;
pub(super) const RC_VBR: u32 = 0x4;
pub(super) const PACKED_HEADER_SEQUENCE: u32 = 0x1;
pub(super) const PACKED_HEADER_SLICE: u32 = 0x4;
pub(super) const PREDICTION_DIRECTION_BI_NOT_EMPTY: u32 = 0x4;

/// `VAConfigAttribValEncHEVCFeatures` 各项的取值。
pub(super) const FEATURE_REQUIRED: u32 = 2;

pub(super) const FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");
pub(super) const LSB_FIRST: u32 = 1;
pub(super) const PROGRESSIVE: c_int = 0x1;
pub(super) const SURFACE_ATTRIB_PIXEL_FORMAT: u32 = 1;
/// `VASurfaceAttribAlignmentSize`：值的低 4 位是宽、再 4 位是高要对齐到的像素数的对数。
pub(super) const SURFACE_ATTRIB_ALIGNMENT_SIZE: u32 = 10;
pub(super) const SURFACE_ATTRIB_SETTABLE: u32 = 0x2;
pub(super) const GENERIC_VALUE_INTEGER: u32 = 1;

pub(super) const BUFFER_ENC_CODED: c_int = 21;
pub(super) const BUFFER_ENC_SEQUENCE_PARAMETER: c_int = 22;
pub(super) const BUFFER_ENC_PICTURE_PARAMETER: c_int = 23;
pub(super) const BUFFER_ENC_SLICE_PARAMETER: c_int = 24;
pub(super) const BUFFER_ENC_PACKED_HEADER_PARAMETER: c_int = 25;
pub(super) const BUFFER_ENC_PACKED_HEADER_DATA: c_int = 26;
pub(super) const BUFFER_ENC_MISC_PARAMETER: c_int = 27;

pub(super) const MISC_FRAME_RATE: u32 = 0;
pub(super) const MISC_RATE_CONTROL: u32 = 1;
pub(super) const MISC_HRD: u32 = 5;

pub(super) const PACKED_HEADER_TYPE_SEQUENCE: u32 = 1;
pub(super) const PACKED_HEADER_TYPE_SLICE: u32 = 3;

pub(super) const CODED_BUF_STATUS_SLICE_OVERFLOW: u32 = 0x200;

pub(super) const PICTURE_H264_INVALID: u32 = 0x1;
pub(super) const PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x8;
pub(super) const PICTURE_HEVC_INVALID: u32 = 0x1;
pub(super) const PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = 0x10;

/// 全零是合法取值的 C 结构体。
///
/// # Safety
///
/// 实现的类型只含整数、整数数组和裸指针。
pub(super) unsafe trait Zeroable: Sized {
    fn zeroed() -> Self {
        // SAFETY: 由 trait 的约定，全零的位模式是这个类型的合法值。
        unsafe { std::mem::zeroed() }
    }
}

/// 把 C 位域的各字段（值、位宽）从低位往高位拼成一个 `u32`。
pub(super) fn pack(fields: &[(u32, u32)]) -> u32 {
    let mut value = 0;
    let mut shift = 0;
    for &(field, width) in fields {
        debug_assert!(width == 32 || field < 1 << width, "{field} does not fit in {width} bits");
        value |= (field & (u32::MAX >> (32 - width))) << shift;
        shift += width;
    }
    debug_assert!(shift <= 32);
    value
}

/// `VAConfigAttrib`。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(super) struct ConfigAttrib {
    pub(super) kind: u32,
    pub(super) value: u32,
}

/// `VAGenericValue` 的值，只用到整数。
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) union GenericValueData {
    pub(super) i: i32,
    /// 不用，只为了大小和对齐跟 C 的联合体一样。
    pub(super) p: *mut c_void,
}

/// `VASurfaceAttrib`（内嵌 `VAGenericValue`）。
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SurfaceAttrib {
    pub(super) kind: u32,
    pub(super) flags: u32,
    pub(super) value_type: u32,
    pub(super) value: GenericValueData,
}

/// `VAImageFormat`。
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ImageFormat {
    pub(super) fourcc: u32,
    pub(super) byte_order: u32,
    pub(super) bits_per_pixel: u32,
    pub(super) depth: u32,
    pub(super) red_mask: u32,
    pub(super) green_mask: u32,
    pub(super) blue_mask: u32,
    pub(super) alpha_mask: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAImage`。
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Image {
    pub(super) image_id: Id,
    pub(super) format: ImageFormat,
    pub(super) buf: Id,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) data_size: u32,
    pub(super) num_planes: u32,
    pub(super) pitches: [u32; 3],
    pub(super) offsets: [u32; 3],
    pub(super) num_palette_entries: i32,
    pub(super) entry_bytes: i32,
    pub(super) component_order: [i8; 4],
    pub(super) va_reserved: [u32; 4],
}

/// `VACodedBufferSegment`：编好的码流一段一段串成链表。
#[repr(C)]
pub(super) struct CodedBufferSegment {
    pub(super) size: u32,
    pub(super) bit_offset: u32,
    pub(super) status: u32,
    pub(super) reserved: u32,
    pub(super) buf: *mut c_void,
    pub(super) next: *mut c_void,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncPackedHeaderParameterBuffer`。
#[repr(C)]
pub(super) struct PackedHeaderParameter {
    pub(super) kind: u32,
    pub(super) bit_length: u32,
    pub(super) has_emulation_bytes: u8,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncMiscParameterBuffer` 的头和跟在后面的参数。
#[repr(C)]
pub(super) struct Misc<T> {
    pub(super) kind: u32,
    pub(super) data: T,
}

/// `VAEncMiscParameterRateControl`。
#[repr(C)]
pub(super) struct RateControl {
    pub(super) bits_per_second: u32,
    pub(super) target_percentage: u32,
    pub(super) window_size: u32,
    pub(super) initial_qp: u32,
    pub(super) min_qp: u32,
    pub(super) basic_unit_size: u32,
    /// 位域：`reset`、`disable_frame_skip`、`disable_bit_stuffing` 各一位，后面的不用。
    pub(super) rc_flags: u32,
    pub(super) icq_quality_factor: u32,
    pub(super) max_qp: u32,
    pub(super) quality_factor: u32,
    pub(super) target_frame_size: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncMiscParameterFrameRate`。
#[repr(C)]
pub(super) struct FrameRate {
    pub(super) framerate: u32,
    pub(super) framerate_flags: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncMiscParameterHRD`。
#[repr(C)]
pub(super) struct Hrd {
    pub(super) initial_buffer_fullness: u32,
    pub(super) buffer_size: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAPictureH264`。
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct PictureH264 {
    pub(super) picture_id: Id,
    pub(super) frame_idx: u32,
    pub(super) flags: u32,
    pub(super) top_field_order_cnt: i32,
    pub(super) bottom_field_order_cnt: i32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncSequenceParameterBufferH264`。
#[repr(C)]
pub(super) struct SequenceH264 {
    pub(super) seq_parameter_set_id: u8,
    pub(super) level_idc: u8,
    pub(super) intra_period: u32,
    pub(super) intra_idr_period: u32,
    pub(super) ip_period: u32,
    pub(super) bits_per_second: u32,
    pub(super) max_num_ref_frames: u32,
    pub(super) picture_width_in_mbs: u16,
    pub(super) picture_height_in_mbs: u16,
    /// 位域：`chroma_format_idc:2`、`frame_mbs_only_flag:1`、`mb_adaptive_frame_field_flag:1`、
    /// `seq_scaling_matrix_present_flag:1`、`direct_8x8_inference_flag:1`、`log2_max_frame_num_minus4:4`、
    /// `pic_order_cnt_type:2`、`log2_max_pic_order_cnt_lsb_minus4:4`、`delta_pic_order_always_zero_flag:1`。
    pub(super) seq_fields: u32,
    pub(super) bit_depth_luma_minus8: u8,
    pub(super) bit_depth_chroma_minus8: u8,
    pub(super) num_ref_frames_in_pic_order_cnt_cycle: u8,
    pub(super) offset_for_non_ref_pic: i32,
    pub(super) offset_for_top_to_bottom_field: i32,
    pub(super) offset_for_ref_frame: [i32; 256],
    pub(super) frame_cropping_flag: u8,
    pub(super) frame_crop_left_offset: u32,
    pub(super) frame_crop_right_offset: u32,
    pub(super) frame_crop_top_offset: u32,
    pub(super) frame_crop_bottom_offset: u32,
    pub(super) vui_parameters_present_flag: u8,
    /// 位域：`aspect_ratio_info_present_flag:1`、`timing_info_present_flag:1`、`bitstream_restriction_flag:1`、
    /// `log2_max_mv_length_horizontal:5`、`log2_max_mv_length_vertical:5`、`fixed_frame_rate_flag:1`、
    /// `low_delay_hrd_flag:1`、`motion_vectors_over_pic_boundaries_flag:1`。
    pub(super) vui_fields: u32,
    pub(super) aspect_ratio_idc: u8,
    pub(super) sar_width: u32,
    pub(super) sar_height: u32,
    pub(super) num_units_in_tick: u32,
    pub(super) time_scale: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncPictureParameterBufferH264`。
#[repr(C)]
pub(super) struct PictureParameterH264 {
    pub(super) curr_pic: PictureH264,
    pub(super) reference_frames: [PictureH264; 16],
    pub(super) coded_buf: Id,
    pub(super) pic_parameter_set_id: u8,
    pub(super) seq_parameter_set_id: u8,
    pub(super) last_picture: u8,
    pub(super) frame_num: u16,
    pub(super) pic_init_qp: u8,
    pub(super) num_ref_idx_l0_active_minus1: u8,
    pub(super) num_ref_idx_l1_active_minus1: u8,
    pub(super) chroma_qp_index_offset: i8,
    pub(super) second_chroma_qp_index_offset: i8,
    /// 位域：`idr_pic_flag:1`、`reference_pic_flag:2`、`entropy_coding_mode_flag:1`、`weighted_pred_flag:1`、
    /// `weighted_bipred_idc:2`、`constrained_intra_pred_flag:1`、`transform_8x8_mode_flag:1`、
    /// `deblocking_filter_control_present_flag:1`、`redundant_pic_cnt_present_flag:1`、`pic_order_present_flag:1`、
    /// `pic_scaling_matrix_present_flag:1`。
    pub(super) pic_fields: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncSliceParameterBufferH264`。
#[repr(C)]
pub(super) struct SliceParameterH264 {
    pub(super) macroblock_address: u32,
    pub(super) num_macroblocks: u32,
    pub(super) macroblock_info: Id,
    pub(super) slice_type: u8,
    pub(super) pic_parameter_set_id: u8,
    pub(super) idr_pic_id: u16,
    pub(super) pic_order_cnt_lsb: u16,
    pub(super) delta_pic_order_cnt_bottom: i32,
    pub(super) delta_pic_order_cnt: [i32; 2],
    pub(super) direct_spatial_mv_pred_flag: u8,
    pub(super) num_ref_idx_active_override_flag: u8,
    pub(super) num_ref_idx_l0_active_minus1: u8,
    pub(super) num_ref_idx_l1_active_minus1: u8,
    pub(super) ref_pic_list0: [PictureH264; 32],
    pub(super) ref_pic_list1: [PictureH264; 32],
    pub(super) luma_log2_weight_denom: u8,
    pub(super) chroma_log2_weight_denom: u8,
    pub(super) luma_weight_l0_flag: u8,
    pub(super) luma_weight_l0: [i16; 32],
    pub(super) luma_offset_l0: [i16; 32],
    pub(super) chroma_weight_l0_flag: u8,
    pub(super) chroma_weight_l0: [[i16; 2]; 32],
    pub(super) chroma_offset_l0: [[i16; 2]; 32],
    pub(super) luma_weight_l1_flag: u8,
    pub(super) luma_weight_l1: [i16; 32],
    pub(super) luma_offset_l1: [i16; 32],
    pub(super) chroma_weight_l1_flag: u8,
    pub(super) chroma_weight_l1: [[i16; 2]; 32],
    pub(super) chroma_offset_l1: [[i16; 2]; 32],
    pub(super) cabac_init_idc: u8,
    pub(super) slice_qp_delta: i8,
    pub(super) disable_deblocking_filter_idc: u8,
    pub(super) slice_alpha_c0_offset_div2: i8,
    pub(super) slice_beta_offset_div2: i8,
    pub(super) va_reserved: [u32; 4],
}

/// `VAPictureHEVC`。
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct PictureHevc {
    pub(super) picture_id: Id,
    pub(super) pic_order_cnt: i32,
    pub(super) flags: u32,
    pub(super) va_reserved: [u32; 4],
}

/// `VAEncSequenceParameterBufferHEVC`。
#[repr(C)]
pub(super) struct SequenceHevc {
    pub(super) general_profile_idc: u8,
    pub(super) general_level_idc: u8,
    pub(super) general_tier_flag: u8,
    pub(super) intra_period: u32,
    pub(super) intra_idr_period: u32,
    pub(super) ip_period: u32,
    pub(super) bits_per_second: u32,
    pub(super) pic_width_in_luma_samples: u16,
    pub(super) pic_height_in_luma_samples: u16,
    /// 位域：`chroma_format_idc:2`、`separate_colour_plane_flag:1`、`bit_depth_luma_minus8:3`、
    /// `bit_depth_chroma_minus8:3`、`scaling_list_enabled_flag:1`、`strong_intra_smoothing_enabled_flag:1`、
    /// `amp_enabled_flag:1`、`sample_adaptive_offset_enabled_flag:1`、`pcm_enabled_flag:1`、
    /// `pcm_loop_filter_disabled_flag:1`、`sps_temporal_mvp_enabled_flag:1`、`low_delay_seq:1`、`hierachical_flag:1`。
    pub(super) seq_fields: u32,
    pub(super) log2_min_luma_coding_block_size_minus3: u8,
    pub(super) log2_diff_max_min_luma_coding_block_size: u8,
    pub(super) log2_min_transform_block_size_minus2: u8,
    pub(super) log2_diff_max_min_transform_block_size: u8,
    pub(super) max_transform_hierarchy_depth_inter: u8,
    pub(super) max_transform_hierarchy_depth_intra: u8,
    pub(super) pcm_sample_bit_depth_luma_minus1: u32,
    pub(super) pcm_sample_bit_depth_chroma_minus1: u32,
    pub(super) log2_min_pcm_luma_coding_block_size_minus3: u32,
    pub(super) log2_max_pcm_luma_coding_block_size_minus3: u32,
    pub(super) vui_parameters_present_flag: u8,
    pub(super) vui_fields: u32,
    pub(super) aspect_ratio_idc: u8,
    pub(super) sar_width: u32,
    pub(super) sar_height: u32,
    pub(super) vui_num_units_in_tick: u32,
    pub(super) vui_time_scale: u32,
    pub(super) min_spatial_segmentation_idc: u16,
    pub(super) max_bytes_per_pic_denom: u8,
    pub(super) max_bits_per_min_cu_denom: u8,
    pub(super) scc_fields: u32,
    pub(super) va_reserved: [u32; 7],
}

/// `VAEncPictureParameterBufferHEVC`。
#[repr(C)]
pub(super) struct PictureParameterHevc {
    pub(super) decoded_curr_pic: PictureHevc,
    pub(super) reference_frames: [PictureHevc; 15],
    pub(super) coded_buf: Id,
    pub(super) collocated_ref_pic_index: u8,
    pub(super) last_picture: u8,
    pub(super) pic_init_qp: u8,
    pub(super) diff_cu_qp_delta_depth: u8,
    pub(super) pps_cb_qp_offset: i8,
    pub(super) pps_cr_qp_offset: i8,
    pub(super) num_tile_columns_minus1: u8,
    pub(super) num_tile_rows_minus1: u8,
    pub(super) column_width_minus1: [u8; 19],
    pub(super) row_height_minus1: [u8; 21],
    pub(super) log2_parallel_merge_level_minus2: u8,
    pub(super) ctu_max_bitsize_allowed: u8,
    pub(super) num_ref_idx_l0_default_active_minus1: u8,
    pub(super) num_ref_idx_l1_default_active_minus1: u8,
    pub(super) slice_pic_parameter_set_id: u8,
    pub(super) nal_unit_type: u8,
    /// 位域：`idr_pic_flag:1`、`coding_type:3`、`reference_pic_flag:1`、`dependent_slice_segments_enabled_flag:1`、
    /// `sign_data_hiding_enabled_flag:1`、`constrained_intra_pred_flag:1`、`transform_skip_enabled_flag:1`、
    /// `cu_qp_delta_enabled_flag:1`、`weighted_pred_flag:1`、`weighted_bipred_flag:1`、
    /// `transquant_bypass_enabled_flag:1`、`tiles_enabled_flag:1`、`entropy_coding_sync_enabled_flag:1`、
    /// `loop_filter_across_tiles_enabled_flag:1`、`pps_loop_filter_across_slices_enabled_flag:1`，后面的不用。
    pub(super) pic_fields: u32,
    pub(super) hierarchical_level_plus1: u8,
    pub(super) va_byte_reserved: u8,
    pub(super) scc_fields: u16,
    pub(super) va_reserved: [u32; 15],
}

/// `VAEncSliceParameterBufferHEVC`。
#[repr(C)]
pub(super) struct SliceParameterHevc {
    pub(super) slice_segment_address: u32,
    pub(super) num_ctu_in_slice: u32,
    pub(super) slice_type: u8,
    pub(super) slice_pic_parameter_set_id: u8,
    pub(super) num_ref_idx_l0_active_minus1: u8,
    pub(super) num_ref_idx_l1_active_minus1: u8,
    pub(super) ref_pic_list0: [PictureHevc; 15],
    pub(super) ref_pic_list1: [PictureHevc; 15],
    pub(super) luma_log2_weight_denom: u8,
    pub(super) delta_chroma_log2_weight_denom: i8,
    pub(super) delta_luma_weight_l0: [i8; 15],
    pub(super) luma_offset_l0: [i8; 15],
    pub(super) delta_chroma_weight_l0: [[i8; 2]; 15],
    pub(super) chroma_offset_l0: [[i8; 2]; 15],
    pub(super) delta_luma_weight_l1: [i8; 15],
    pub(super) luma_offset_l1: [i8; 15],
    pub(super) delta_chroma_weight_l1: [[i8; 2]; 15],
    pub(super) chroma_offset_l1: [[i8; 2]; 15],
    pub(super) max_num_merge_cand: u8,
    pub(super) slice_qp_delta: i8,
    pub(super) slice_cb_qp_offset: i8,
    pub(super) slice_cr_qp_offset: i8,
    pub(super) slice_beta_offset_div2: i8,
    pub(super) slice_tc_offset_div2: i8,
    /// 位域：`last_slice_of_pic_flag:1`、`dependent_slice_segment_flag:1`、`colour_plane_id:2`、
    /// `slice_temporal_mvp_enabled_flag:1`、`slice_sao_luma_flag:1`、`slice_sao_chroma_flag:1`、
    /// `num_ref_idx_active_override_flag:1`、`mvd_l1_zero_flag:1`、`cabac_init_flag:1`、
    /// `slice_deblocking_filter_disabled_flag:2`、`slice_loop_filter_across_slices_enabled_flag:1`、
    /// `collocated_from_l0_flag:1`。
    pub(super) slice_fields: u32,
    pub(super) pred_weight_table_bit_offset: u32,
    pub(super) pred_weight_table_bit_length: u32,
    pub(super) va_reserved: [u32; 6],
}

// SAFETY: 以下类型都只含整数、整数数组、裸指针和由它们组成的结构体，全零合法。
unsafe impl Zeroable for Image {}
// SAFETY: 同上。
unsafe impl Zeroable for SurfaceAttrib {}
// SAFETY: 同上。
unsafe impl Zeroable for PackedHeaderParameter {}
// SAFETY: 同上。
unsafe impl Zeroable for RateControl {}
// SAFETY: 同上。
unsafe impl Zeroable for FrameRate {}
// SAFETY: 同上。
unsafe impl Zeroable for Hrd {}
// SAFETY: 同上。
unsafe impl Zeroable for PictureH264 {}
// SAFETY: 同上。
unsafe impl Zeroable for SequenceH264 {}
// SAFETY: 同上。
unsafe impl Zeroable for PictureParameterH264 {}
// SAFETY: 同上。
unsafe impl Zeroable for SliceParameterH264 {}
// SAFETY: 同上。
unsafe impl Zeroable for PictureHevc {}
// SAFETY: 同上。
unsafe impl Zeroable for SequenceHevc {}
// SAFETY: 同上。
unsafe impl Zeroable for PictureParameterHevc {}
// SAFETY: 同上。
unsafe impl Zeroable for SliceParameterHevc {}

impl PictureH264 {
    /// 参考表里空着的一项。
    pub(super) fn invalid() -> Self {
        Self { picture_id: INVALID_ID, flags: PICTURE_H264_INVALID, ..Self::zeroed() }
    }
}

impl PictureHevc {
    /// 参考表里空着的一项。
    pub(super) fn invalid() -> Self {
        Self { picture_id: INVALID_ID, flags: PICTURE_HEVC_INVALID, ..Self::zeroed() }
    }
}

/// `VAMessageCallback`。
type MessageCallback = Option<unsafe extern "C" fn(*mut c_void, *const c_char)>;

/// 用到的 libva 函数，签名照 `va.h` 和 `va_drm.h`。
pub(super) struct Lib {
    pub(super) get_display_drm: unsafe extern "C" fn(c_int) -> VaDisplay,
    pub(super) set_error_callback: unsafe extern "C" fn(VaDisplay, MessageCallback, *mut c_void) -> MessageCallback,
    pub(super) set_info_callback: unsafe extern "C" fn(VaDisplay, MessageCallback, *mut c_void) -> MessageCallback,
    pub(super) initialize: unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> Status,
    pub(super) terminate: unsafe extern "C" fn(VaDisplay) -> Status,
    pub(super) query_vendor_string: unsafe extern "C" fn(VaDisplay) -> *const c_char,
    pub(super) error_str: unsafe extern "C" fn(Status) -> *const c_char,
    pub(super) max_num_profiles: unsafe extern "C" fn(VaDisplay) -> c_int,
    pub(super) max_num_entrypoints: unsafe extern "C" fn(VaDisplay) -> c_int,
    pub(super) query_config_profiles: unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> Status,
    pub(super) query_config_entrypoints: unsafe extern "C" fn(VaDisplay, c_int, *mut c_int, *mut c_int) -> Status,
    pub(super) query_surface_attributes: unsafe extern "C" fn(VaDisplay, Id, *mut SurfaceAttrib, *mut c_uint) -> Status,
    pub(super) get_config_attributes: unsafe extern "C" fn(VaDisplay, c_int, c_int, *mut ConfigAttrib, c_int) -> Status,
    pub(super) create_config:
        unsafe extern "C" fn(VaDisplay, c_int, c_int, *mut ConfigAttrib, c_int, *mut Id) -> Status,
    pub(super) destroy_config: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) create_surfaces:
        unsafe extern "C" fn(VaDisplay, c_uint, c_uint, c_uint, *mut Id, c_uint, *mut SurfaceAttrib, c_uint) -> Status,
    pub(super) destroy_surfaces: unsafe extern "C" fn(VaDisplay, *mut Id, c_int) -> Status,
    pub(super) create_context:
        unsafe extern "C" fn(VaDisplay, Id, c_int, c_int, c_int, *mut Id, c_int, *mut Id) -> Status,
    pub(super) destroy_context: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) create_buffer:
        unsafe extern "C" fn(VaDisplay, Id, c_int, c_uint, c_uint, *mut c_void, *mut Id) -> Status,
    pub(super) destroy_buffer: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) map_buffer: unsafe extern "C" fn(VaDisplay, Id, *mut *mut c_void) -> Status,
    pub(super) unmap_buffer: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) begin_picture: unsafe extern "C" fn(VaDisplay, Id, Id) -> Status,
    pub(super) render_picture: unsafe extern "C" fn(VaDisplay, Id, *mut Id, c_int) -> Status,
    pub(super) end_picture: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) sync_surface: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) derive_image: unsafe extern "C" fn(VaDisplay, Id, *mut Image) -> Status,
    pub(super) create_image: unsafe extern "C" fn(VaDisplay, *mut ImageFormat, c_int, c_int, *mut Image) -> Status,
    pub(super) destroy_image: unsafe extern "C" fn(VaDisplay, Id) -> Status,
    pub(super) put_image:
        unsafe extern "C" fn(VaDisplay, Id, Id, c_int, c_int, c_uint, c_uint, c_int, c_int, c_uint, c_uint) -> Status,
}

impl Lib {
    /// 载入 libva，一个进程只载一次、不卸载；载不进来时错误是 `io::ErrorKind::Unsupported`，带着原因。
    pub(super) fn get() -> io::Result<&'static Self> {
        static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
        LIB.get_or_init(|| {
            let open = |name: &str| {
                // SAFETY: 载入系统的 libva，它载入时不做别的事；驱动等到 `vaInitialize` 才载。
                let library =
                    unsafe { libloading::Library::new(name) }.map_err(|err| format!("cannot load {name}: {err}"))?;
                Ok::<_, String>(&*Box::leak(Box::new(library)))
            };
            let va = open("libva.so.2")?;
            let drm = open("libva-drm.so.2")?;
            macro_rules! sym {
                ($library:ident, $name:literal) => {
                    // SAFETY: 符号的签名照 libva 的头文件写，库一直不卸载。
                    *unsafe { $library.get(concat!($name, "\0").as_bytes()) }
                        .map_err(|err| format!("libva has no {}: {err}", $name))?
                };
            }
            Ok(Lib {
                get_display_drm: sym!(drm, "vaGetDisplayDRM"),
                set_error_callback: sym!(va, "vaSetErrorCallback"),
                set_info_callback: sym!(va, "vaSetInfoCallback"),
                initialize: sym!(va, "vaInitialize"),
                terminate: sym!(va, "vaTerminate"),
                query_vendor_string: sym!(va, "vaQueryVendorString"),
                error_str: sym!(va, "vaErrorStr"),
                max_num_profiles: sym!(va, "vaMaxNumProfiles"),
                max_num_entrypoints: sym!(va, "vaMaxNumEntrypoints"),
                query_config_profiles: sym!(va, "vaQueryConfigProfiles"),
                query_config_entrypoints: sym!(va, "vaQueryConfigEntrypoints"),
                query_surface_attributes: sym!(va, "vaQuerySurfaceAttributes"),
                get_config_attributes: sym!(va, "vaGetConfigAttributes"),
                create_config: sym!(va, "vaCreateConfig"),
                destroy_config: sym!(va, "vaDestroyConfig"),
                create_surfaces: sym!(va, "vaCreateSurfaces"),
                destroy_surfaces: sym!(va, "vaDestroySurfaces"),
                create_context: sym!(va, "vaCreateContext"),
                destroy_context: sym!(va, "vaDestroyContext"),
                create_buffer: sym!(va, "vaCreateBuffer"),
                destroy_buffer: sym!(va, "vaDestroyBuffer"),
                map_buffer: sym!(va, "vaMapBuffer"),
                unmap_buffer: sym!(va, "vaUnmapBuffer"),
                begin_picture: sym!(va, "vaBeginPicture"),
                render_picture: sym!(va, "vaRenderPicture"),
                end_picture: sym!(va, "vaEndPicture"),
                sync_surface: sym!(va, "vaSyncSurface"),
                derive_image: sym!(va, "vaDeriveImage"),
                create_image: sym!(va, "vaCreateImage"),
                destroy_image: sym!(va, "vaDestroyImage"),
                put_image: sym!(va, "vaPutImage"),
            })
        })
        .as_ref()
        .map_err(|err| io::Error::new(io::ErrorKind::Unsupported, err.clone()))
    }

    /// `status` 不是成功时换成带 libva 说法的错误，`what` 说是哪一步。
    pub(super) fn check(&self, status: Status, what: &str) -> io::Result<()> {
        if status == SUCCESS {
            return Ok(());
        }
        // SAFETY: `vaErrorStr` 对任何值都返回静态的、以 NUL 结尾的字符串。
        let reason = unsafe { CStr::from_ptr((self.error_str)(status)) }.to_string_lossy();
        Err(io::Error::other(format!("{what} failed: {reason} ({status:#x})")))
    }

    /// 把 libva 和驱动打的消息转给 `tracing`，不让它们印到 stderr。
    pub(super) fn route_messages(&self, display: VaDisplay) {
        // SAFETY: `display` 是 `vaGetDisplayDRM` 给的，回调不用上下文指针。
        unsafe {
            (self.set_error_callback)(display, Some(log_message), std::ptr::null_mut());
            (self.set_info_callback)(display, Some(log_message), std::ptr::null_mut());
        }
    }
}

/// libva 的消息回调。打不开的设备、不支持的驱动也会报错，那时会换软件编码，所以都只记成 debug。
unsafe extern "C" fn log_message(_context: *mut c_void, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: libva 交来的是以 NUL 结尾的字符串，回调期间有效。
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    tracing::debug!("libva: {}", message.trim_end());
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    /// 位域从低位往高位排：期望值是 GCC 给 `VAEncSequenceParameterBufferH264::seq_fields` 和
    /// `VAEncMiscParameterRateControl::rc_flags` 设同样的字段后读出的整数。
    #[test]
    fn packs_bit_fields_like_gcc() {
        assert_eq!(pack(&[(1, 2), (1, 1), (0, 1), (0, 1), (1, 1), (4, 4), (2, 2)]), 2341);
        assert_eq!(pack(&[(1, 1), (1, 1), (1, 1)]), 7);
        assert_eq!(pack(&[(0xdead_beef, 32)]), 0xdead_beef);
    }

    /// 布局按 libva 1.22 的头文件在 x86-64、AArch64 上用 GCC 量出的 `sizeof`、`offsetof`。
    #[test]
    fn layouts_match_libva() {
        assert_eq!(size_of::<ConfigAttrib>(), 8);
        assert_eq!(size_of::<SurfaceAttrib>(), 24);
        assert_eq!(offset_of!(SurfaceAttrib, value), 16);
        assert_eq!(size_of::<ImageFormat>(), 48);
        assert_eq!(size_of::<Image>(), 120);
        assert_eq!(offset_of!(Image, buf), 52);
        assert_eq!(offset_of!(Image, data_size), 60);
        assert_eq!(offset_of!(Image, pitches), 68);
        assert_eq!(offset_of!(Image, offsets), 80);
        assert_eq!(size_of::<CodedBufferSegment>(), 48);
        assert_eq!(offset_of!(CodedBufferSegment, buf), 16);
        assert_eq!(offset_of!(CodedBufferSegment, next), 24);
        assert_eq!(size_of::<PackedHeaderParameter>(), 28);
        assert_eq!(offset_of!(PackedHeaderParameter, has_emulation_bytes), 8);
        assert_eq!(size_of::<RateControl>(), 60);
        assert_eq!(offset_of!(Misc<RateControl>, data), 4);
        assert_eq!(size_of::<FrameRate>(), 24);
        assert_eq!(size_of::<Hrd>(), 24);

        assert_eq!(size_of::<PictureH264>(), 36);
        assert_eq!(size_of::<SequenceH264>(), 1132);
        assert_eq!(offset_of!(SequenceH264, seq_fields), 28);
        assert_eq!(offset_of!(SequenceH264, offset_for_ref_frame), 44);
        assert_eq!(offset_of!(SequenceH264, frame_cropping_flag), 1068);
        assert_eq!(offset_of!(SequenceH264, vui_fields), 1092);
        assert_eq!(offset_of!(SequenceH264, time_scale), 1112);
        assert_eq!(size_of::<PictureParameterH264>(), 648);
        assert_eq!(offset_of!(PictureParameterH264, coded_buf), 612);
        assert_eq!(offset_of!(PictureParameterH264, frame_num), 620);
        assert_eq!(offset_of!(PictureParameterH264, pic_fields), 628);
        assert_eq!(size_of::<SliceParameterH264>(), 3140);
        assert_eq!(offset_of!(SliceParameterH264, ref_pic_list0), 36);
        assert_eq!(offset_of!(SliceParameterH264, chroma_weight_l0), 2474);
        assert_eq!(offset_of!(SliceParameterH264, cabac_init_idc), 3118);

        assert_eq!(size_of::<PictureHevc>(), 28);
        assert_eq!(size_of::<SequenceHevc>(), 116);
        assert_eq!(offset_of!(SequenceHevc, seq_fields), 24);
        assert_eq!(offset_of!(SequenceHevc, pcm_sample_bit_depth_luma_minus1), 36);
        assert_eq!(offset_of!(SequenceHevc, vui_fields), 56);
        assert_eq!(offset_of!(SequenceHevc, min_spatial_segmentation_idc), 80);
        assert_eq!(size_of::<PictureParameterHevc>(), 576);
        assert_eq!(offset_of!(PictureParameterHevc, coded_buf), 448);
        assert_eq!(offset_of!(PictureParameterHevc, row_height_minus1), 479);
        assert_eq!(offset_of!(PictureParameterHevc, pic_fields), 508);
        assert_eq!(size_of::<SliceParameterHevc>(), 1076);
        assert_eq!(offset_of!(SliceParameterHevc, ref_pic_list1), 432);
        assert_eq!(offset_of!(SliceParameterHevc, max_num_merge_cand), 1034);
        assert_eq!(offset_of!(SliceParameterHevc, slice_fields), 1040);
    }
}
