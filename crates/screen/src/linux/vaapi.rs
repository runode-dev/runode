//! VA-API 的硬件编码器：在 DRM 渲染节点上打开显卡（不经 X 或 Wayland），挑观看端想要的编码里显卡编得了的第一种，
//! 按远程桌面的需要配成低延迟：没有 B 帧，只在要关键帧时出 IDR（第一帧也是），码率控制用 CBR（驱动不支持时用
//! VBR），码率缓冲限在 `BURST_WINDOW` 那么长，单帧不会太大。驱动收打包头时 VPS、SPS、PPS 和片头自己写（`h264`、
//! `hevc`），带上 BT.709 的色彩信息和不重排的约束；不收时用驱动自己写的。
//!
//! libva 在运行时载入（`ffi`），没装 libva、没有显卡、显卡编不了时 `Vaapi::open` 报错，调用方改用软件编码。建好以后
//! 先试编一帧，驱动建得了上下文却编不出能用的码流时也在这里报错，不等到采集开始以后。
//!
//! 编码是同步的：上传 NV12、提交、等编完、读出码流都在调用的线程上，一次一帧，所以只要一个输入画面和两个轮流用的
//! 重建画面（这一帧写一个，上一帧的那个当参考）。编出的帧丢了（码流缓冲放不下、驱动没交出东西）时下一帧编成 IDR，
//! 解码端不会拿着缺了参考的 P 帧。

mod bits;
mod ffi;
mod h264;
mod hevc;

use std::{
    ffi::{CStr, c_int, c_uint, c_void},
    fs::{File, OpenOptions},
    io,
    mem::size_of_val,
    os::fd::AsRawFd as _,
    ptr, slice,
};

use self::{
    bits::Packed,
    ffi::{ConfigAttrib, INVALID_ID, Id, Image, Lib, Misc, VaDisplay, Zeroable as _},
    h264::H264,
    hevc::Hevc,
};
use super::encoder::VideoEncoder;
use crate::{BURST_HEADROOM, Codec, VideoFrame, nal, nv12};

/// 依次试的 DRM 渲染节点的编号（`/dev/dri/renderD128` 起）。
const RENDER_NODES: std::ops::Range<u32> = 128..136;
/// 想用的打包头：序列头（VPS、SPS、PPS）和片头。驱动只收片头、不收序列头时都不用：自己写的片头得和参数集对得上。
const WANTED_PACKED_HEADERS: u32 = ffi::PACKED_HEADER_SEQUENCE | ffi::PACKED_HEADER_SLICE;
/// 给驱动的 I 帧间隔。关键帧由调用方定，驱动只拿它当码率控制的提示；有的驱动按 16 位存，取 16 位放得下的最大值。
const INTRA_PERIOD: u32 = u16::MAX as u32;

pub(crate) struct Vaapi {
    device: Device,
    /// 建在 `device` 上的对象，`Drop` 里先放掉它们再终止显示。
    objects: Objects,
    /// 上传画面用的图像：驱动认 `vaDeriveImage` 时为空，每帧直接映射输入画面。
    image: Option<Image>,
    codec: Codec,
    syntax: Syntax,
    /// `ffi::RC_CBR` 或 `ffi::RC_VBR`。
    rate_control: u32,
    /// 用着的打包头（`ffi::PACKED_HEADER_*`），驱动不收时为 0。
    packed_headers: u32,
    /// 画面的像素尺寸和按块对齐后编码的尺寸。
    size: (u32, u32),
    coded_size: (u32, u32),
    fps: u32,
    /// 平均码率，bit/s；`rate_changed` 时还没交给驱动。
    bitrate: u32,
    rate_changed: bool,
    /// 下一帧离上一个 IDR 第几帧；为空时下一帧编成 IDR。
    next_index: Option<u32>,
    /// 编过几个 IDR。
    idrs: u32,
    /// 下一帧写进 `Objects::recon` 的哪一个。
    next_recon: usize,
    parameter_sets: Vec<Vec<u8>>,
}

// SAFETY: VA 显示和它上面的对象只经 `&mut self` 用，一次只有一个线程碰；libva 和驱动不把这些对象绑在建它们的线程上，
// 换个线程接着用没问题。
unsafe impl Send for Vaapi {}

/// 在显卡上建的对象，还没建的是 `INVALID_ID`。
struct Objects {
    config: Id,
    context: Id,
    /// 上传 NV12 的输入画面。
    input: Id,
    recon: [Id; 2],
    /// 编出的码流，每帧重用。
    coded: Id,
    /// `Vaapi::image` 的编号。
    image: Id,
}

impl Vaapi {
    /// 挑 `codecs` 里排在前面、某块显卡编得了的那种，在它上面建好编码器。`size` 是偶数的画面尺寸。没有 libva、没有
    /// 能用的显卡、显卡编不了这几种编码时错误是 `io::ErrorKind::Unsupported`。
    pub(crate) fn open(size: (u32, u32), fps: u32, bitrate: u32, codecs: &[Codec]) -> io::Result<Self> {
        let va = Lib::get()?;
        let mut devices: Vec<(String, Option<Device>)> = RENDER_NODES
            .filter_map(|number| {
                let path = format!("/dev/dri/renderD{number}");
                match Device::open(va, &path) {
                    Ok(device) => Some((path, Some(device))),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                    Err(err) => {
                        tracing::debug!("cannot use {path} for VA-API: {err}");
                        None
                    }
                }
            })
            .collect();
        if devices.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "no GPU with a VA-API driver"));
        }
        let fps = fps.max(1);
        let mut failures = Vec::new();
        for &codec in codecs {
            for (path, slot) in &mut devices {
                let Some(device) = slot.take() else { continue };
                let plan = match device.plan(codec, size, fps, bitrate) {
                    Ok(plan) => plan,
                    Err(err) => {
                        failures.push(format!("{codec:?} on {path}: {err}"));
                        *slot = Some(device);
                        continue;
                    }
                };
                match Self::start(device, codec, plan, size, fps, bitrate) {
                    Ok(encoder) => return Ok(encoder),
                    Err(err) => {
                        failures.push(format!("{codec:?} on {path}: {err}"));
                        // 建不成的编码器连同显示一起放掉了，另一种编码再打开一次。
                        *slot = Device::open(va, path).ok();
                    }
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::Unsupported, failures.join("; ")))
    }

    fn start(device: Device, codec: Codec, plan: Plan, size: (u32, u32), fps: u32, bitrate: u32) -> io::Result<Self> {
        let syntax = plan.syntax(size, (1, 1), fps, bitrate)?;
        let coded_size = syntax.coded_size();
        let mut encoder = Self {
            device,
            objects: Objects {
                config: INVALID_ID,
                context: INVALID_ID,
                input: INVALID_ID,
                recon: [INVALID_ID; 2],
                coded: INVALID_ID,
                image: INVALID_ID,
            },
            image: None,
            codec,
            syntax,
            rate_control: plan.rate_control,
            packed_headers: plan.packed_headers,
            size,
            coded_size,
            fps,
            bitrate,
            rate_changed: false,
            next_index: None,
            idrs: 0,
            next_recon: 0,
            parameter_sets: Vec::new(),
        };
        encoder.create(&plan)?;
        encoder.probe()?;
        tracing::info!(
            "encoding {codec:?} through VA-API with {}: {} profile, {} entrypoint, {}, {} headers",
            encoder.device.vendor,
            match plan.profile {
                ffi::PROFILE_H264_HIGH => "High",
                ffi::PROFILE_H264_CONSTRAINED_BASELINE => "Constrained Baseline",
                _ => "Main",
            },
            if plan.entrypoint == ffi::ENTRYPOINT_ENC_SLICE_LP { "low-power" } else { "full" },
            if plan.rate_control == ffi::RC_CBR { "CBR" } else { "VBR" },
            if plan.packed_headers == 0 { "driver-written" } else { "own" },
        );
        Ok(encoder)
    }

    /// 建配置、画面、上下文和码流缓冲，挑上传画面的办法。驱动要求画面另外对齐时按它重定编码的尺寸。
    fn create(&mut self, plan: &Plan) -> io::Result<()> {
        let (va, display) = (self.device.va, self.device.display);
        let mut attributes = vec![
            ConfigAttrib { kind: ffi::ATTRIB_RT_FORMAT, value: ffi::RT_FORMAT_YUV420 },
            ConfigAttrib { kind: ffi::ATTRIB_RATE_CONTROL, value: self.rate_control },
        ];
        if self.packed_headers != 0 {
            attributes.push(ConfigAttrib { kind: ffi::ATTRIB_ENC_PACKED_HEADERS, value: self.packed_headers });
        }
        // SAFETY: 属性数组和输出参数在调用期间有效，长度如实给出。
        let status = unsafe {
            (va.create_config)(
                display,
                plan.profile,
                plan.entrypoint,
                attributes.as_mut_ptr(),
                attributes.len() as c_int,
                &mut self.objects.config,
            )
        };
        va.check(status, "vaCreateConfig")?;
        let align = self.surface_alignment();
        if align != (1, 1) {
            self.syntax = plan.syntax(self.size, align, self.fps, self.bitrate)?;
            self.coded_size = self.syntax.coded_size();
            tracing::debug!("the VA-API driver aligns pictures to {align:?}, encoding {:?}", self.coded_size);
        }

        let (width, height) = self.coded_size;
        let mut surfaces = [INVALID_ID; 3];
        let mut format = ffi::SurfaceAttrib {
            kind: ffi::SURFACE_ATTRIB_PIXEL_FORMAT,
            flags: ffi::SURFACE_ATTRIB_SETTABLE,
            value_type: ffi::GENERIC_VALUE_INTEGER,
            value: ffi::GenericValueData { i: ffi::FOURCC_NV12 as i32 },
        };
        // SAFETY: 输出数组有三项，属性在调用期间有效。
        let status = unsafe {
            (va.create_surfaces)(
                display,
                ffi::RT_FORMAT_YUV420,
                width,
                height,
                surfaces.as_mut_ptr(),
                surfaces.len() as c_uint,
                &mut format,
                1,
            )
        };
        va.check(status, "vaCreateSurfaces")?;
        let [input, recon @ ..] = surfaces;
        (self.objects.input, self.objects.recon) = (input, recon);

        // SAFETY: 配置是刚建的，重建画面的数组在调用期间有效。
        let status = unsafe {
            (va.create_context)(
                display,
                self.objects.config,
                width as c_int,
                height as c_int,
                ffi::PROGRESSIVE,
                self.objects.recon.as_mut_ptr(),
                self.objects.recon.len() as c_int,
                &mut self.objects.context,
            )
        };
        va.check(status, "vaCreateContext")?;

        // 放得下两帧没压缩的画面：码率控制下的帧远比这小，留够余量不让驱动截断。
        let coded_len = 2 * nv12::frame_len(self.coded_size) + (1 << 16);
        // SAFETY: 不给初始数据，驱动自己分配。
        let status = unsafe {
            (va.create_buffer)(
                display,
                self.objects.context,
                ffi::BUFFER_ENC_CODED,
                coded_len as c_uint,
                1,
                ptr::null_mut(),
                &mut self.objects.coded,
            )
        };
        va.check(status, "vaCreateBuffer for the bitstream")?;

        self.choose_upload()
    }

    /// 驱动要求画面的宽、高对齐到的像素数（`SURFACE_ATTRIB_ALIGNMENT_SIZE`），不要求或查不了时为 1。
    fn surface_alignment(&self) -> (u32, u32) {
        let (va, display) = (self.device.va, self.device.display);
        let mut count: c_uint = 0;
        // SAFETY: 配置是刚建的；不给数组时只报个数。
        let status =
            unsafe { (va.query_surface_attributes)(display, self.objects.config, ptr::null_mut(), &mut count) };
        if status != ffi::SUCCESS || count == 0 {
            return (1, 1);
        }
        let mut attributes = vec![ffi::SurfaceAttrib::zeroed(); count as usize];
        // SAFETY: 数组按驱动报的个数分配，`count` 如实给出。
        let status =
            unsafe { (va.query_surface_attributes)(display, self.objects.config, attributes.as_mut_ptr(), &mut count) };
        if status != ffi::SUCCESS {
            return (1, 1);
        }
        attributes
            .iter()
            .take(count as usize)
            .find(|attribute| {
                attribute.kind == ffi::SURFACE_ATTRIB_ALIGNMENT_SIZE
                    && attribute.value_type == ffi::GENERIC_VALUE_INTEGER
            })
            .map_or((1, 1), |attribute| {
                // SAFETY: 值的类型是整数，联合体里放的是 `i`。
                let value = unsafe { attribute.value.i } as u32;
                (1 << (value & 0xf), 1 << (value >> 4 & 0xf))
            })
    }

    /// 驱动认 `vaDeriveImage` 且映射出的是够大的 NV12 时直接写输入画面，不然建一张 NV12 图像，写好后 `vaPutImage`。
    fn choose_upload(&mut self) -> io::Result<()> {
        let (va, display) = (self.device.va, self.device.display);
        let mut image = Image::zeroed();
        // SAFETY: 输入画面是刚建的，输出参数指向本地变量。
        if unsafe { (va.derive_image)(display, self.objects.input, &mut image) } == ffi::SUCCESS {
            let usable = image.format.fourcc == ffi::FOURCC_NV12 && Layout::new(&image, self.coded_size).is_some();
            // SAFETY: 刚导出的图像，之后不再用。
            unsafe { (va.destroy_image)(display, image.image_id) };
            if usable {
                return Ok(());
            }
            tracing::debug!(
                "the VA-API driver derives a {:#x} image, uploading through vaPutImage",
                image.format.fourcc
            );
        }
        let mut format = ffi::ImageFormat {
            fourcc: ffi::FOURCC_NV12,
            byte_order: ffi::LSB_FIRST,
            bits_per_pixel: 12,
            depth: 0,
            red_mask: 0,
            green_mask: 0,
            blue_mask: 0,
            alpha_mask: 0,
            va_reserved: [0; 4],
        };
        let (width, height) = self.coded_size;
        // SAFETY: 格式和输出参数在调用期间有效。
        let status = unsafe { (va.create_image)(display, &mut format, width as c_int, height as c_int, &mut image) };
        va.check(status, "vaCreateImage")?;
        self.objects.image = image.image_id;
        if Layout::new(&image, self.coded_size).is_none() {
            return Err(io::Error::other("the VA-API driver made an NV12 image of the wrong size"));
        }
        self.image = Some(image);
        Ok(())
    }

    /// 试编一帧灰的关键帧，确认驱动编得出带参数集的关键帧；之后照常从 IDR 开始。
    fn probe(&mut self) -> io::Result<()> {
        let gray = vec![128; nv12::frame_len(self.size)];
        let (annex_b, _) = self.encode_frame(&gray, true)?;
        let frame = nal::video_frame(self.codec, &annex_b, &mut self.parameter_sets, Vec::new, 0);
        if !frame.is_some_and(|frame| frame.keyframe && !frame.parameter_sets.is_empty()) {
            return Err(io::Error::other("the VA-API encoder gave no usable keyframe"));
        }
        self.next_index = None;
        self.idrs = 0;
        self.next_recon = 0;
        self.parameter_sets.clear();
        Ok(())
    }

    /// 编一帧，返回 Annex B 的码流和它放不放得下码流缓冲（放不下时是截断的）。
    fn encode_frame(&mut self, nv12: &[u8], keyframe: bool) -> io::Result<(Vec<u8>, bool)> {
        let (va, display) = (self.device.va, self.device.display);
        let index = self.next_index.filter(|_| !keyframe).unwrap_or(0);
        let frame = Frame {
            index,
            idr: self.idrs,
            recon: self.objects.recon[self.next_recon],
            reference: (index > 0).then(|| self.objects.recon[1 - self.next_recon]),
        };
        self.upload(nv12)?;

        let context = self.objects.context;
        let mut params = Params { va, display, ids: Vec::with_capacity(12) };
        if frame.is_idr() {
            let bitrate = self.peak_bitrate();
            match &self.syntax {
                Syntax::H264(h264) => {
                    params.push(context, ffi::BUFFER_ENC_SEQUENCE_PARAMETER, &h264.sequence(INTRA_PERIOD, bitrate))?;
                }
                Syntax::Hevc(hevc) => {
                    params.push(context, ffi::BUFFER_ENC_SEQUENCE_PARAMETER, &hevc.sequence(INTRA_PERIOD, bitrate))?;
                }
            }
            let rate = ffi::FrameRate { framerate: self.fps, ..ffi::FrameRate::zeroed() };
            params.push(context, ffi::BUFFER_ENC_MISC_PARAMETER, &Misc { kind: ffi::MISC_FRAME_RATE, data: rate })?;
        }
        if frame.is_idr() || self.rate_changed {
            let (rate, hrd) = self.rate_parameters(!frame.is_idr());
            params.push(context, ffi::BUFFER_ENC_MISC_PARAMETER, &Misc { kind: ffi::MISC_RATE_CONTROL, data: rate })?;
            params.push(context, ffi::BUFFER_ENC_MISC_PARAMETER, &Misc { kind: ffi::MISC_HRD, data: hrd })?;
        }
        let coded = self.objects.coded;
        match &self.syntax {
            Syntax::H264(h264) => {
                params.push(context, ffi::BUFFER_ENC_PICTURE_PARAMETER, &h264.picture(&frame, coded))?
            }
            Syntax::Hevc(hevc) => {
                params.push(context, ffi::BUFFER_ENC_PICTURE_PARAMETER, &hevc.picture(&frame, coded))?
            }
        }
        if frame.is_idr() && self.packed_headers & ffi::PACKED_HEADER_SEQUENCE != 0 {
            params.push_packed(context, ffi::PACKED_HEADER_TYPE_SEQUENCE, &self.syntax.parameter_sets())?;
        }
        if self.packed_headers & ffi::PACKED_HEADER_SLICE != 0 {
            params.push_packed(context, ffi::PACKED_HEADER_TYPE_SLICE, &self.syntax.slice_header(&frame))?;
        }
        match &self.syntax {
            Syntax::H264(h264) => params.push(context, ffi::BUFFER_ENC_SLICE_PARAMETER, &h264.slice(&frame))?,
            Syntax::Hevc(hevc) => params.push(context, ffi::BUFFER_ENC_SLICE_PARAMETER, &hevc.slice(&frame))?,
        }

        // SAFETY: 上下文、输入画面和参数缓冲都是这个显示上建的，缓冲编号的数组在调用期间有效。
        unsafe {
            va.check((va.begin_picture)(display, context, self.objects.input), "vaBeginPicture")?;
            let rendered = va.check(
                (va.render_picture)(display, context, params.ids.as_mut_ptr(), params.ids.len() as c_int),
                "vaRenderPicture",
            );
            // 提交出错也要结束这一帧，上下文才能接着用。
            let ended = va.check((va.end_picture)(display, context), "vaEndPicture");
            rendered.and(ended)?;
        }
        drop(params);
        // SAFETY: 输入画面是这个显示上建的、刚交去编的那张，等它编完。
        va.check(unsafe { (va.sync_surface)(display, self.objects.input) }, "vaSyncSurface")?;
        let coded = self.read_coded()?;

        self.rate_changed = false;
        self.next_index = index.checked_add(1);
        self.next_recon = 1 - self.next_recon;
        if frame.is_idr() {
            self.idrs = self.idrs.wrapping_add(1);
        }
        Ok(coded)
    }

    /// 把整帧 NV12 写进输入画面。
    fn upload(&mut self, nv12: &[u8]) -> io::Result<()> {
        let (va, display) = (self.device.va, self.device.display);
        if nv12.len() < nv12::frame_len(self.size) {
            return Err(io::Error::other("the NV12 frame is smaller than the encoder"));
        }
        match self.image {
            Some(image) => {
                self.fill(&image, nv12)?;
                let (width, height) = self.coded_size;
                // SAFETY: 图像和输入画面都是这个显示上建的，尺寸是两者共同的大小。
                let status = unsafe {
                    (va.put_image)(
                        display,
                        self.objects.input,
                        image.image_id,
                        0,
                        0,
                        width,
                        height,
                        0,
                        0,
                        width,
                        height,
                    )
                };
                va.check(status, "vaPutImage")
            }
            None => {
                let mut image = Image::zeroed();
                // SAFETY: 输入画面是这个显示上建的，输出参数指向本地变量。
                va.check(unsafe { (va.derive_image)(display, self.objects.input, &mut image) }, "vaDeriveImage")?;
                let filled = self.fill(&image, nv12);
                // SAFETY: 刚导出的图像，之后不再用；放掉以后输入画面才能拿去编。
                unsafe { (va.destroy_image)(display, image.image_id) };
                filled
            }
        }
    }

    /// 映射 `image` 的缓冲，按它的布局写进这一帧。
    fn fill(&self, image: &Image, nv12: &[u8]) -> io::Result<()> {
        let (va, display) = (self.device.va, self.device.display);
        let layout = Layout::new(image, self.coded_size)
            .ok_or_else(|| io::Error::other("the VA-API image does not fit the encoder"))?;
        let mut data: *mut c_void = ptr::null_mut();
        // SAFETY: 图像的缓冲是驱动建的，输出参数指向本地变量。
        va.check(unsafe { (va.map_buffer)(display, image.buf, &mut data) }, "vaMapBuffer for the image")?;
        if !data.is_null() {
            // SAFETY: 驱动映射出 `data_size` 字节，解除映射前只有这里写。
            let target = unsafe { slice::from_raw_parts_mut(data.cast::<u8>(), image.data_size as usize) };
            copy_nv12(nv12, self.size, self.coded_size, &layout, target);
        }
        // SAFETY: 上面映射的，`target` 已经不用了。
        va.check(unsafe { (va.unmap_buffer)(display, image.buf) }, "vaUnmapBuffer for the image")?;
        if data.is_null() {
            return Err(io::Error::other("the VA-API driver mapped the image at null"));
        }
        Ok(())
    }

    /// 读出码流缓冲里的各段，接成一段 Annex B；有一段放不下时第二个值为真。
    fn read_coded(&self) -> io::Result<(Vec<u8>, bool)> {
        let (va, display) = (self.device.va, self.device.display);
        let mut segment: *mut c_void = ptr::null_mut();
        // SAFETY: 码流缓冲是这个显示上建的，输出参数指向本地变量。
        va.check(
            unsafe { (va.map_buffer)(display, self.objects.coded, &mut segment) },
            "vaMapBuffer for the bitstream",
        )?;
        let mut out = Vec::new();
        let mut overflow = false;
        let mut segment = segment.cast_const().cast::<ffi::CodedBufferSegment>();
        while !segment.is_null() {
            // SAFETY: 链表和各段的数据由驱动建，解除映射前有效。
            let current = unsafe { &*segment };
            overflow |= current.status & ffi::CODED_BUF_STATUS_SLICE_OVERFLOW != 0;
            if !current.buf.is_null() && current.size > 0 {
                // SAFETY: 同上，`buf` 指向 `size` 字节。
                out.extend_from_slice(unsafe {
                    slice::from_raw_parts(current.buf.cast::<u8>(), current.size as usize)
                });
            }
            segment = current.next.cast_const().cast();
        }
        // SAFETY: 上面映射的，各段的数据已经拷走。
        va.check(unsafe { (va.unmap_buffer)(display, self.objects.coded) }, "vaUnmapBuffer for the bitstream")?;
        Ok((out, overflow))
    }

    /// 给驱动的码率：CBR 是平均码率，VBR 是峰值（平均码率的 `BURST_HEADROOM` 倍）。
    fn peak_bitrate(&self) -> u32 {
        if self.rate_control == ffi::RC_CBR { self.bitrate } else { (f64::from(self.bitrate) * BURST_HEADROOM) as u32 }
    }

    /// 码率控制和 HRD 的参数；`reset` 时让驱动按新码率重来，不插关键帧。不许跳帧，也不许补填充数据：画面不变时
    /// 本来就不编，补到平均码率只是白占带宽。
    fn rate_parameters(&self, reset: bool) -> (ffi::RateControl, ffi::Hrd) {
        let bitrate = self.bitrate.max(1);
        let buffer = crate::burst_bits(bitrate);
        let rate = ffi::RateControl {
            bits_per_second: self.peak_bitrate(),
            target_percentage: if self.rate_control == ffi::RC_CBR { 100 } else { (100. / BURST_HEADROOM) as u32 },
            window_size: (u64::from(buffer) * 1000 / u64::from(bitrate)) as u32,
            rc_flags: ffi::pack(&[(u32::from(reset), 1), (1, 1), (1, 1)]),
            ..ffi::RateControl::zeroed()
        };
        let hrd = ffi::Hrd { initial_buffer_fullness: buffer, buffer_size: buffer, ..ffi::Hrd::zeroed() };
        (rate, hrd)
    }
}

impl VideoEncoder for Vaapi {
    fn codec(&self) -> Codec {
        self.codec
    }

    fn encode(&mut self, nv12: &[u8], keyframe: bool, pts_us: i64) -> io::Result<Option<VideoFrame>> {
        let (annex_b, overflow) = self.encode_frame(nv12, keyframe)?;
        if overflow {
            tracing::warn!("a VA-API frame did not fit the bitstream buffer, dropping it");
            self.next_index = None;
            return Ok(None);
        }
        let frame = nal::video_frame(self.codec, &annex_b, &mut self.parameter_sets, Vec::new, pts_us);
        if frame.is_none() {
            self.next_index = None;
        }
        Ok(frame.map(|mut frame| {
            drop_filler(self.codec, &mut frame.data);
            frame
        }))
    }

    fn set_bitrate(&mut self, bitrate: u32) -> io::Result<()> {
        self.bitrate = bitrate;
        self.rate_changed = true;
        Ok(())
    }
}

impl Drop for Vaapi {
    fn drop(&mut self) {
        let (va, display) = (self.device.va, self.device.display);
        let objects = &mut self.objects;
        // SAFETY: 都是这个显示上建的、还没放掉的对象，之后不再用；`device` 在这之后才终止显示。
        unsafe {
            if objects.image != INVALID_ID {
                (va.destroy_image)(display, objects.image);
            }
            if objects.coded != INVALID_ID {
                (va.destroy_buffer)(display, objects.coded);
            }
            if objects.context != INVALID_ID {
                (va.destroy_context)(display, objects.context);
            }
            let mut surfaces: Vec<Id> = [objects.input, objects.recon[0], objects.recon[1]]
                .into_iter()
                .filter(|&id| id != INVALID_ID)
                .collect();
            if !surfaces.is_empty() {
                (va.destroy_surfaces)(display, surfaces.as_mut_ptr(), surfaces.len() as c_int);
            }
            if objects.config != INVALID_ID {
                (va.destroy_config)(display, objects.config);
            }
        }
    }
}

/// 打开的一块显卡：DRM 渲染节点和在它上面初始化好的 VA 显示，丢掉时终止显示、关掉节点。
struct Device {
    va: &'static Lib,
    display: VaDisplay,
    /// 驱动报的名字和版本，记日志用。
    vendor: String,
    /// 显示用着它的文件描述符，要比显示活得久：字段按声明的顺序放，它在 `Drop::drop` 之后才关。
    _node: File,
}

impl Device {
    fn open(va: &'static Lib, path: &str) -> io::Result<Self> {
        let node = OpenOptions::new().read(true).write(true).open(path)?;
        // SAFETY: 节点在 `Device` 里一直开着，比显示活得久。
        let display = unsafe { (va.get_display_drm)(node.as_raw_fd()) };
        if display.is_null() {
            return Err(io::Error::other("libva cannot use this device"));
        }
        va.route_messages(display);
        let (mut major, mut minor) = (0, 0);
        // SAFETY: 显示是刚拿到的，输出参数指向本地变量。
        let status = unsafe { (va.initialize)(display, &mut major, &mut minor) };
        if let Err(err) = va.check(status, "vaInitialize") {
            // SAFETY: 初始化失败的显示也要终止，才放掉 `vaGetDisplayDRM` 建的上下文。
            unsafe { (va.terminate)(display) };
            return Err(err);
        }
        // SAFETY: 初始化好的显示；返回的字符串归驱动，这里马上拷走。
        let vendor = unsafe { (va.query_vendor_string)(display) };
        let vendor = if vendor.is_null() {
            String::from("an unnamed driver")
        } else {
            // SAFETY: 不为空时是以 NUL 结尾的字符串。
            unsafe { CStr::from_ptr(vendor) }.to_string_lossy().into_owned()
        };
        tracing::debug!("{path}: VA-API {major}.{minor}, {vendor}");
        Ok(Self { va, display, vendor, _node: node })
    }

    /// 这块显卡怎么编 `codec`：先试低功耗的入口（编码电路，延迟低、不占着色器），没有再用普通的；驱动的哪项能力
    /// 不够时错误是 `io::ErrorKind::Unsupported`，带着原因。
    fn plan(&self, codec: Codec, size: (u32, u32), fps: u32, bitrate: u32) -> io::Result<Plan> {
        let profiles = self.profiles()?;
        let mut failure = io::Error::new(io::ErrorKind::Unsupported, format!("the driver cannot encode {codec:?}"));
        let candidates: Vec<(c_int, Option<h264::Profile>)> = match codec {
            Codec::H264 => h264::Profile::PREFERRED.iter().map(|&profile| (profile.va(), Some(profile))).collect(),
            Codec::Hevc => vec![(ffi::PROFILE_HEVC_MAIN, None)],
        };
        for (profile, h264) in candidates {
            if !profiles.contains(&profile) {
                continue;
            }
            let entrypoints = self.entrypoints(profile)?;
            for entrypoint in [ffi::ENTRYPOINT_ENC_SLICE_LP, ffi::ENTRYPOINT_ENC_SLICE] {
                if !entrypoints.contains(&entrypoint) {
                    continue;
                }
                match self.plan_with(profile, entrypoint, h264, size, fps, bitrate) {
                    Ok(plan) => return Ok(plan),
                    Err(err) => {
                        tracing::debug!("{codec:?} profile {profile} entrypoint {entrypoint}: {err}");
                        failure = err;
                    }
                }
            }
        }
        Err(failure)
    }

    fn plan_with(
        &self,
        profile: c_int,
        entrypoint: c_int,
        h264: Option<h264::Profile>,
        size: (u32, u32),
        fps: u32,
        bitrate: u32,
    ) -> io::Result<Plan> {
        let unsupported = |what: &str| io::Error::new(io::ErrorKind::Unsupported, what.to_owned());
        let supported = |value: u32| (value != ffi::ATTRIB_NOT_SUPPORTED).then_some(value);
        let [rt_format, rate_control, packed, max_refs, max_width, max_height] = self
            .attributes(
                profile,
                entrypoint,
                [
                    ffi::ATTRIB_RT_FORMAT,
                    ffi::ATTRIB_RATE_CONTROL,
                    ffi::ATTRIB_ENC_PACKED_HEADERS,
                    ffi::ATTRIB_ENC_MAX_REF_FRAMES,
                    ffi::ATTRIB_MAX_PICTURE_WIDTH,
                    ffi::ATTRIB_MAX_PICTURE_HEIGHT,
                ],
            )?
            .map(supported);
        if rt_format.is_none_or(|formats| formats & ffi::RT_FORMAT_YUV420 == 0) {
            return Err(unsupported("no 4:2:0 input"));
        }
        let rate_control = match rate_control {
            Some(modes) if modes & ffi::RC_CBR != 0 => ffi::RC_CBR,
            Some(modes) if modes & ffi::RC_VBR != 0 => ffi::RC_VBR,
            _ => return Err(unsupported("no bitrate control")),
        };
        let max_refs = max_refs.unwrap_or(0);
        if max_refs & 0xffff == 0 {
            return Err(unsupported("no P frames"));
        }
        let mut packed_headers = packed.unwrap_or(0) & WANTED_PACKED_HEADERS;
        if packed_headers & ffi::PACKED_HEADER_SEQUENCE == 0 {
            packed_headers = 0;
        }
        let kind = match h264 {
            Some(profile) => Kind::H264(profile),
            // 驱动的 HEVC 序列参数里没有裁边和色彩信息，驱动自己写的 SPS 会让观看端解出补过边的画面。
            None if packed_headers == 0 => return Err(unsupported("HEVC without packed headers")),
            None => {
                // 不是每个驱动都认这几项，查不了时当成驱动不报。
                let [direction, features, block_sizes] = self
                    .attributes(
                        profile,
                        entrypoint,
                        [
                            ffi::ATTRIB_PREDICTION_DIRECTION,
                            ffi::ATTRIB_ENC_HEVC_FEATURES,
                            ffi::ATTRIB_ENC_HEVC_BLOCK_SIZES,
                        ],
                    )
                    .unwrap_or([ffi::ATTRIB_NOT_SUPPORTED; 3])
                    .map(supported);
                // 驱动要求 B 片的两个参考表都不空时不收 P 片，P 帧改按两个表都是上一帧的 B 片编。
                let p_as_b = direction.is_some_and(|direction| direction & ffi::PREDICTION_DIRECTION_BI_NOT_EMPTY != 0)
                    && max_refs >> 16 != 0;
                Kind::Hevc { features, block_sizes, p_as_b }
            }
        };
        let plan = Plan { profile, entrypoint, rate_control, packed_headers, kind };
        let (width, height) = plan.syntax(size, (1, 1), fps, bitrate)?.coded_size();
        if max_width.is_some_and(|max| width > max) || max_height.is_some_and(|max| height > max) {
            return Err(unsupported("the picture is too large"));
        }
        Ok(plan)
    }

    fn profiles(&self) -> io::Result<Vec<c_int>> {
        // SAFETY: 初始化好的显示。
        let max = unsafe { (self.va.max_num_profiles)(self.display) }.max(0);
        let mut profiles = vec![0; max as usize];
        let mut count = 0;
        // SAFETY: 数组按驱动报的最多个数分配。
        let status = unsafe { (self.va.query_config_profiles)(self.display, profiles.as_mut_ptr(), &mut count) };
        self.va.check(status, "vaQueryConfigProfiles")?;
        profiles.truncate(count.clamp(0, max) as usize);
        Ok(profiles)
    }

    fn entrypoints(&self, profile: c_int) -> io::Result<Vec<c_int>> {
        // SAFETY: 初始化好的显示。
        let max = unsafe { (self.va.max_num_entrypoints)(self.display) }.max(0);
        let mut entrypoints = vec![0; max as usize];
        let mut count = 0;
        // SAFETY: 数组按驱动报的最多个数分配。
        let status =
            unsafe { (self.va.query_config_entrypoints)(self.display, profile, entrypoints.as_mut_ptr(), &mut count) };
        self.va.check(status, "vaQueryConfigEntrypoints")?;
        entrypoints.truncate(count.clamp(0, max) as usize);
        Ok(entrypoints)
    }

    /// 查几项配置属性，不支持的项是 `ATTRIB_NOT_SUPPORTED`。
    fn attributes<const N: usize>(&self, profile: c_int, entrypoint: c_int, kinds: [u32; N]) -> io::Result<[u32; N]> {
        let mut attributes = kinds.map(|kind| ConfigAttrib { kind, value: 0 });
        // SAFETY: 数组在调用期间有效，长度如实给出。
        let status = unsafe {
            (self.va.get_config_attributes)(self.display, profile, entrypoint, attributes.as_mut_ptr(), N as c_int)
        };
        self.va.check(status, "vaGetConfigAttributes")?;
        Ok(attributes.map(|attribute| attribute.value))
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: 显示上建的对象都已经放掉（`Vaapi::drop` 在字段放掉之前跑），之后不再用这个显示。
        unsafe { (self.va.terminate)(self.display) };
    }
}

/// 在一块显卡上编一种编码要的设置。
struct Plan {
    profile: c_int,
    entrypoint: c_int,
    rate_control: u32,
    packed_headers: u32,
    kind: Kind,
}

/// 按编码分开的设置。
enum Kind {
    H264(h264::Profile),
    /// 驱动报的 HEVC 能力（`Hevc::new`），和 P 帧要不要按 B 片编。
    Hevc {
        features: Option<u32>,
        block_sizes: Option<u32>,
        p_as_b: bool,
    },
}

impl Plan {
    /// 这一路码流的参数；`align` 是驱动要求画面对齐到的像素数。
    fn syntax(&self, size: (u32, u32), align: (u32, u32), fps: u32, bitrate: u32) -> io::Result<Syntax> {
        Ok(match self.kind {
            Kind::H264(profile) => Syntax::H264(H264::new(profile, size, align, fps, bitrate)),
            Kind::Hevc { features, block_sizes, p_as_b } => {
                Syntax::Hevc(Hevc::new(size, align, fps, bitrate, features, block_sizes, p_as_b)?)
            }
        })
    }
}

/// 按编码分开的码流参数。
enum Syntax {
    H264(H264),
    Hevc(Hevc),
}

impl Syntax {
    fn coded_size(&self) -> (u32, u32) {
        match self {
            Self::H264(h264) => h264.coded_size(),
            Self::Hevc(hevc) => hevc.coded_size(),
        }
    }

    fn parameter_sets(&self) -> Packed {
        match self {
            Self::H264(h264) => h264.parameter_sets(),
            Self::Hevc(hevc) => hevc.parameter_sets(),
        }
    }

    fn slice_header(&self, frame: &Frame) -> Packed {
        match self {
            Self::H264(h264) => h264.slice_header(frame),
            Self::Hevc(hevc) => hevc.slice_header(frame),
        }
    }
}

/// 要编的一帧在码流里的位置。
struct Frame {
    /// 离上一个 IDR 第几帧，IDR 自己是 0。
    index: u32,
    /// 前面编过几个 IDR：连着的两个 IDR 要用不同的编号。
    idr: u32,
    /// 这一帧的重建画面。
    recon: Id,
    /// 上一帧的重建画面，P 帧参考它；IDR 时为空。
    reference: Option<Id>,
}

impl Frame {
    fn is_idr(&self) -> bool {
        self.reference.is_none()
    }
}

/// 一帧的参数缓冲，丢掉时放掉：`vaRenderPicture` 不替调用方放。
struct Params {
    va: &'static Lib,
    display: VaDisplay,
    ids: Vec<Id>,
}

impl Params {
    /// 建一个 `kind` 类的缓冲，内容是 `value` 的字节（按 libva 的布局写的结构体，或打包头的数据）。
    fn push<T: ?Sized>(&mut self, context: Id, kind: c_int, value: &T) -> io::Result<()> {
        let mut id = INVALID_ID;
        // SAFETY: `value` 在调用期间有效，驱动把 `size_of_val(value)` 字节拷走，不留着指针。
        let status = unsafe {
            (self.va.create_buffer)(
                self.display,
                context,
                kind,
                size_of_val(value) as c_uint,
                1,
                ptr::from_ref(value).cast::<c_void>().cast_mut(),
                &mut id,
            )
        };
        self.va.check(status, "vaCreateBuffer")?;
        self.ids.push(id);
        Ok(())
    }

    /// 一段打包头：先是说明它的参数缓冲，紧跟着数据缓冲。数据里已经加好防竞争字节。
    fn push_packed(&mut self, context: Id, kind: u32, packed: &Packed) -> io::Result<()> {
        let parameter = ffi::PackedHeaderParameter {
            kind,
            bit_length: packed.bits,
            has_emulation_bytes: 1,
            ..ffi::PackedHeaderParameter::zeroed()
        };
        self.push(context, ffi::BUFFER_ENC_PACKED_HEADER_PARAMETER, &parameter)?;
        self.push(context, ffi::BUFFER_ENC_PACKED_HEADER_DATA, packed.data.as_slice())
    }
}

impl Drop for Params {
    fn drop(&mut self) {
        for &id in &self.ids {
            // SAFETY: 这个显示上建的缓冲，之后不再用。
            unsafe { (self.va.destroy_buffer)(self.display, id) };
        }
    }
}

/// 映射出的 NV12 图像里两个平面的位置，单位是字节。
struct Layout {
    pitches: [usize; 2],
    offsets: [usize; 2],
}

impl Layout {
    /// `image` 两个平面都放得下 `coded` 那么大的 NV12 时为它的布局。
    fn new(image: &Image, (width, height): (u32, u32)) -> Option<Self> {
        let pitches = [image.pitches[0] as usize, image.pitches[1] as usize];
        let offsets = [image.offsets[0] as usize, image.offsets[1] as usize];
        let (width, height) = (width as usize, height as usize);
        let fits = |plane: usize, rows: usize| {
            pitches[plane] >= width
                && rows
                    .checked_sub(1)
                    .and_then(|last| last.checked_mul(pitches[plane]))
                    .and_then(|start| start.checked_add(offsets[plane])?.checked_add(width))
                    .is_some_and(|end| end <= image.data_size as usize)
        };
        let usable = image.num_planes >= 2
            && usize::from(image.width) >= width
            && usize::from(image.height) >= height
            && fits(0, height)
            && fits(1, height / 2);
        usable.then_some(Self { pitches, offsets })
    }
}

/// 把 `size` 大小的整帧 NV12 拷进按 `layout` 排的 `target`，补到 `coded` 那么大：多出的列重复每行最右边的像素，
/// 多出的行重复最下面一行，看不见的部分不多花码率。只写不读 `target`：映射出的显存读起来很慢。
fn copy_nv12(nv12: &[u8], size: (u32, u32), coded: (u32, u32), layout: &Layout, target: &mut [u8]) {
    let (width, height) = (size.0 as usize, size.1 as usize);
    let coded_width = coded.0 as usize;
    for row in 0..coded.1 as usize {
        let source = &nv12[row.min(height - 1) * width..][..width];
        let line = &mut target[layout.offsets[0] + row * layout.pitches[0]..][..coded_width];
        line[..width].copy_from_slice(source);
        line[width..].fill(source[width - 1]);
    }
    let chroma = &nv12[width * height..];
    for row in 0..coded.1 as usize / 2 {
        let source = &chroma[row.min(height / 2 - 1) * width..][..width];
        let line = &mut target[layout.offsets[1] + row * layout.pitches[1]..][..coded_width];
        line[..width].copy_from_slice(source);
        for pair in line[width..].as_chunks_mut::<2>().0 {
            pair.copy_from_slice(&source[width - 2..]);
        }
    }
}

/// 去掉驱动为凑码率塞的填充数据（H.264 的 12 类、HEVC 的 38 类 NAL 单元），它们不带画面。
fn drop_filler(codec: Codec, data: &mut Vec<u8>) {
    let is_filler = |unit: &[u8]| match (codec, unit.first()) {
        (Codec::H264, Some(&header)) => header & 0x1f == 12,
        (Codec::Hevc, Some(&header)) => header >> 1 & 0x3f == 38,
        (_, None) => false,
    };
    let mut kept = Vec::with_capacity(data.len());
    let mut rest = data.as_slice();
    let mut dropped = false;
    while let Some((len, tail)) = rest.split_first_chunk::<{ nal::LENGTH_SIZE }>() {
        let len = u32::from_be_bytes(*len) as usize;
        let Some(unit) = tail.get(..len) else { break };
        if is_filler(unit) {
            dropped = true;
        } else {
            kept.extend_from_slice(&rest[..nal::LENGTH_SIZE + len]);
        }
        rest = &tail[len..];
    }
    if dropped {
        *data = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 宽高都补出来时，多出的列重复最右边的像素（色度按一对 UV），多出的行重复最下面一行；行距大于宽度的部分不碰。
    #[test]
    fn pads_nv12_to_the_coded_size() {
        let size = (4, 2);
        let nv12 = [1, 2, 3, 4, 5, 6, 7, 8, 10, 20, 30, 40];
        let layout = Layout { pitches: [8, 8], offsets: [0, 32] };
        let mut target = vec![0xee; 48];
        copy_nv12(&nv12, size, (6, 4), &layout, &mut target);
        #[rustfmt::skip]
        let expected = [
            1, 2, 3, 4, 4, 4, 0xee, 0xee,
            5, 6, 7, 8, 8, 8, 0xee, 0xee,
            5, 6, 7, 8, 8, 8, 0xee, 0xee,
            5, 6, 7, 8, 8, 8, 0xee, 0xee,
            10, 20, 30, 40, 30, 40, 0xee, 0xee,
            10, 20, 30, 40, 30, 40, 0xee, 0xee,
        ];
        assert_eq!(target, expected);
    }

    #[test]
    fn checks_the_image_layout() {
        let mut image = Image::zeroed();
        (image.width, image.height, image.num_planes, image.data_size) = (64, 32, 2, 64 * 48);
        (image.pitches, image.offsets) = ([64, 64, 0], [0, 64 * 32, 0]);
        assert!(Layout::new(&image, (64, 32)).is_some());
        assert!(Layout::new(&image, (64, 34)).is_none());
        image.data_size -= 1;
        assert!(Layout::new(&image, (64, 32)).is_none());
    }

    #[test]
    fn drops_filler_units() {
        let unit = |bytes: &[u8]| [&(bytes.len() as u32).to_be_bytes()[..], bytes].concat();
        let mut data = [unit(&[0x65, 1, 2]), unit(&[0x0c, 0xff, 0xff, 0x80]), unit(&[0x06, 5])].concat();
        drop_filler(Codec::H264, &mut data);
        assert_eq!(data, [unit(&[0x65, 1, 2]), unit(&[0x06, 5])].concat());
        let mut data = [unit(&[0x26, 1, 9]), unit(&[38 << 1, 1, 0xff, 0x80])].concat();
        drop_filler(Codec::Hevc, &mut data);
        assert_eq!(data, unit(&[0x26, 1, 9]));
    }
}
