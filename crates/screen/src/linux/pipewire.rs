//! 从 PipeWire 收 Wayland 桌面交来的屏幕画面：Mutter 和 xdg-desktop-portal 都把屏幕录成一个 PipeWire 节点，这里
//! 连上它、只要内存里的 BGRx 类画面（不要 DMA-BUF），每来一帧拷进 `Shared` 交给采集线程。
//!
//! libpipewire 在运行时 dlopen（`Lib`），没装它的系统上可执行文件照样跑；用到的结构体和常量照它的头文件手写，
//! 只碰 0.3 起稳定的那部分 ABI。回调在 PipeWire 的线程循环（`pw_thread_loop`）上跑，从别的线程动流之前要拿着
//! 循环的锁。

use std::{
    ffi::{CStr, c_char, c_int, c_void},
    io,
    os::fd::{IntoRawFd as _, OwnedFd},
    ptr,
    sync::{Arc, Condvar, Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

use super::capture::{Picture, Source};

const PW_DIRECTION_INPUT: u32 = 0;
const PW_STREAM_FLAG_AUTOCONNECT: u32 = 1 << 0;
const PW_STREAM_FLAG_MAP_BUFFERS: u32 = 1 << 2;
const PW_VERSION_STREAM_EVENTS: u32 = 2;
const PW_STREAM_STATE_ERROR: c_int = -1;
const PW_STREAM_STATE_UNCONNECTED: c_int = 0;

const SPA_TYPE_ID: u32 = 3;
const SPA_TYPE_INT: u32 = 4;
const SPA_TYPE_RECTANGLE: u32 = 10;
const SPA_TYPE_FRACTION: u32 = 11;
const SPA_TYPE_OBJECT: u32 = 15;
const SPA_TYPE_CHOICE: u32 = 19;
const SPA_TYPE_OBJECT_FORMAT: u32 = 0x40003;
const SPA_TYPE_OBJECT_PARAM_BUFFERS: u32 = 0x40004;
const SPA_CHOICE_RANGE: u32 = 1;
const SPA_CHOICE_ENUM: u32 = 3;
const SPA_CHOICE_FLAGS: u32 = 4;
const SPA_PARAM_ENUM_FORMAT: u32 = 3;
const SPA_PARAM_FORMAT: u32 = 4;
const SPA_PARAM_BUFFERS: u32 = 5;
const SPA_PARAM_BUFFERS_DATA_TYPE: u32 = 6;
const SPA_FORMAT_MEDIA_TYPE: u32 = 1;
const SPA_FORMAT_MEDIA_SUBTYPE: u32 = 2;
const SPA_FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const SPA_FORMAT_VIDEO_SIZE: u32 = 0x20003;
const SPA_FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const SPA_MEDIA_TYPE_VIDEO: u32 = 2;
const SPA_MEDIA_SUBTYPE_RAW: u32 = 1;
const SPA_DATA_MEM_PTR: u32 = 1;
const SPA_DATA_MEM_FD: u32 = 2;
const SPA_CHUNK_FLAG_CORRUPTED: i32 = 1;

/// `spa_video_format` 里收的几种，每像素四字节；先列的优先。
const VIDEO_FORMATS: [u32; 4] = [BGRX, BGRA, RGBX, RGBA];
const RGBX: u32 = 7;
const BGRX: u32 = 8;
const RGBA: u32 = 11;
const BGRA: u32 = 12;

/// 等流开始出画面最多这么久。
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

type StreamEventFn = unsafe extern "C" fn(*mut c_void);

/// `struct pw_stream_events`。
#[repr(C)]
struct StreamEvents {
    version: u32,
    destroy: Option<StreamEventFn>,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    io_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const SpaPod)>,
    add_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    remove_buffer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    process: Option<StreamEventFn>,
    drained: Option<StreamEventFn>,
    command: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
    trigger_done: Option<StreamEventFn>,
}

/// `struct spa_pod` 的头。
#[repr(C)]
struct SpaPod {
    size: u32,
    kind: u32,
}

/// `struct pw_buffer` 的开头。
#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

/// `struct spa_buffer`。
#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut c_void,
    datas: *mut SpaData,
}

/// `struct spa_data`。
#[repr(C)]
struct SpaData {
    kind: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

/// `struct spa_chunk`。
#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

/// `struct spa_hook`：六个指针，由调用方分配、PipeWire 填。多留几个字不碍事。
#[repr(C)]
struct SpaHook([usize; 8]);

/// 从 libpipewire 取的函数。
struct Lib {
    init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> *mut c_void,
    thread_loop_get_loop: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    thread_loop_start: unsafe extern "C" fn(*mut c_void) -> c_int,
    thread_loop_stop: unsafe extern "C" fn(*mut c_void),
    thread_loop_destroy: unsafe extern "C" fn(*mut c_void),
    thread_loop_lock: unsafe extern "C" fn(*mut c_void),
    thread_loop_unlock: unsafe extern "C" fn(*mut c_void),
    context_new: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void,
    context_destroy: unsafe extern "C" fn(*mut c_void),
    context_connect: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void,
    context_connect_fd: unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, usize) -> *mut c_void,
    core_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
    properties_new_string: unsafe extern "C" fn(*const c_char) -> *mut c_void,
    stream_new: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void,
    stream_add_listener: unsafe extern "C" fn(*mut c_void, *mut SpaHook, *const StreamEvents, *mut c_void),
    stream_connect: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut *const SpaPod, u32) -> c_int,
    stream_update_params: unsafe extern "C" fn(*mut c_void, *mut *const SpaPod, u32) -> c_int,
    stream_dequeue_buffer: unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer,
    stream_queue_buffer: unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int,
    stream_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
    stream_destroy: unsafe extern "C" fn(*mut c_void),
}

impl Lib {
    /// 载入 libpipewire、初始化一次；载不进来时记着原因。
    fn get() -> io::Result<&'static Self> {
        static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
        LIB.get_or_init(|| {
            // SAFETY: 载入系统的 libpipewire，它的初始化函数没有别的副作用。
            let library = unsafe { libloading::Library::new("libpipewire-0.3.so.0") }
                .map_err(|err| format!("cannot load libpipewire: {err}"))?;
            let library: &'static libloading::Library = Box::leak(Box::new(library));
            macro_rules! sym {
                ($name:literal) => {
                    // SAFETY: 符号的签名照 libpipewire 的头文件写，库一直不卸载。
                    *unsafe { library.get(concat!($name, "\0").as_bytes()) }
                        .map_err(|err| format!("libpipewire has no {}: {err}", $name))?
                };
            }
            let lib = Lib {
                init: sym!("pw_init"),
                thread_loop_new: sym!("pw_thread_loop_new"),
                thread_loop_get_loop: sym!("pw_thread_loop_get_loop"),
                thread_loop_start: sym!("pw_thread_loop_start"),
                thread_loop_stop: sym!("pw_thread_loop_stop"),
                thread_loop_destroy: sym!("pw_thread_loop_destroy"),
                thread_loop_lock: sym!("pw_thread_loop_lock"),
                thread_loop_unlock: sym!("pw_thread_loop_unlock"),
                context_new: sym!("pw_context_new"),
                context_destroy: sym!("pw_context_destroy"),
                context_connect: sym!("pw_context_connect"),
                context_connect_fd: sym!("pw_context_connect_fd"),
                core_disconnect: sym!("pw_core_disconnect"),
                properties_new_string: sym!("pw_properties_new_string"),
                stream_new: sym!("pw_stream_new"),
                stream_add_listener: sym!("pw_stream_add_listener"),
                stream_connect: sym!("pw_stream_connect"),
                stream_update_params: sym!("pw_stream_update_params"),
                stream_dequeue_buffer: sym!("pw_stream_dequeue_buffer"),
                stream_queue_buffer: sym!("pw_stream_queue_buffer"),
                stream_disconnect: sym!("pw_stream_disconnect"),
                stream_destroy: sym!("pw_stream_destroy"),
            };
            // SAFETY: 不传命令行参数。
            unsafe { (lib.init)(ptr::null_mut(), ptr::null_mut()) };
            Ok(lib)
        })
        .as_ref()
        .map_err(|err| io::Error::other(err.clone()))
    }
}

/// 回调和采集线程共用的。
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    /// 最新一帧，BGRx，行与行紧挨着。
    frame: Vec<u8>,
    size: (u32, u32),
    /// `frame` 还没被采集线程取走。
    fresh: bool,
    /// 商定的像素格式（`VIDEO_FORMATS` 之一）和尺寸。
    format: Option<(u32, (u32, u32))>,
    /// 流断了或出错了，带原因。
    error: Option<String>,
}

/// 回调拿到的指针指向它。
struct Callback {
    lib: &'static Lib,
    /// `pw_stream`，建好流以后才设上；回调都在设上之后。
    stream: *mut c_void,
    shared: Arc<Shared>,
}

static EVENTS: StreamEvents = StreamEvents {
    version: PW_VERSION_STREAM_EVENTS,
    destroy: None,
    state_changed: Some(on_state_changed),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param_changed),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

/// 连着一个 PipeWire 屏幕节点的流，丢掉时断开。
pub(crate) struct Stream {
    lib: &'static Lib,
    thread_loop: *mut c_void,
    context: *mut c_void,
    core: *mut c_void,
    stream: *mut c_void,
    hook: Box<SpaHook>,
    callback: Box<Callback>,
    shared: Arc<Shared>,
    /// 交给采集线程的那一帧。
    frame: Vec<u8>,
    size: (u32, u32),
    have_frame: bool,
}

// SAFETY: 指针指向的 PipeWire 对象只在拿着线程循环的锁时动（建、断开、销毁），回调在循环的线程上；`Stream` 本身
// 一次只在一个线程上用。
unsafe impl Send for Stream {}

impl Stream {
    /// 连上节点 `node`。`fd` 是 portal 交来的 PipeWire 连接（`OpenPipeWireRemote`），为空时连本用户的 PipeWire
    /// 守护进程。`size` 是节点的大致尺寸，商量格式时当默认值。
    pub(crate) fn connect(node: u32, fd: Option<OwnedFd>, size: (u32, u32)) -> io::Result<Self> {
        let lib = Lib::get()?;
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), changed: Condvar::new() });
        // SAFETY: 照 PipeWire 的用法依次建线程循环、上下文、连接和流；建好的部分放进 `Stream`，失败时由它的 `Drop`
        // 拆掉。流的回调数据指向堆上的 `Callback`，它和 `SpaHook` 活到 `Stream` 丢掉、流销毁以后。
        unsafe {
            let thread_loop = (lib.thread_loop_new)(c"runode-screen".as_ptr(), ptr::null());
            if thread_loop.is_null() {
                return Err(io::Error::other("cannot create a PipeWire loop"));
            }
            let context = (lib.context_new)((lib.thread_loop_get_loop)(thread_loop), ptr::null_mut(), 0);
            if context.is_null() {
                (lib.thread_loop_destroy)(thread_loop);
                return Err(io::Error::other("cannot create a PipeWire context"));
            }
            let mut this = Self {
                lib,
                thread_loop,
                context,
                core: ptr::null_mut(),
                stream: ptr::null_mut(),
                hook: Box::new(SpaHook([0; 8])),
                callback: Box::new(Callback { lib, stream: ptr::null_mut(), shared: shared.clone() }),
                shared,
                frame: Vec::new(),
                size: (0, 0),
                have_frame: false,
            };
            if (lib.thread_loop_start)(thread_loop) < 0 {
                return Err(io::Error::other("cannot start the PipeWire loop"));
            }
            (lib.thread_loop_lock)(thread_loop);
            this.core = match fd {
                Some(fd) => (lib.context_connect_fd)(context, fd.into_raw_fd(), ptr::null_mut(), 0),
                None => (lib.context_connect)(context, ptr::null_mut(), 0),
            };
            if this.core.is_null() {
                let err = io::Error::last_os_error();
                (lib.thread_loop_unlock)(thread_loop);
                return Err(io::Error::other(format!("cannot connect to PipeWire: {err}")));
            }
            let props =
                (lib.properties_new_string)(c"media.type=Video media.category=Capture media.role=Screen".as_ptr());
            this.stream = (lib.stream_new)(this.core, c"runode-screen".as_ptr(), props);
            if this.stream.is_null() {
                (lib.thread_loop_unlock)(thread_loop);
                return Err(io::Error::other("cannot create a PipeWire stream"));
            }
            this.callback.stream = this.stream;
            (lib.stream_add_listener)(this.stream, &mut *this.hook, &EVENTS, (&raw mut *this.callback).cast());
            let format = enum_format(size);
            let mut params = [format.as_ptr().cast::<SpaPod>()];
            let connected = (lib.stream_connect)(
                this.stream,
                PW_DIRECTION_INPUT,
                node,
                PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS,
                params.as_mut_ptr(),
                1,
            );
            (lib.thread_loop_unlock)(thread_loop);
            if connected < 0 {
                return Err(io::Error::other(format!("cannot connect to PipeWire node {node}: {connected}")));
            }
            this.wait_first()?;
            Ok(this)
        }
    }

    /// 等到第一帧或者出错，最多 `FIRST_FRAME_TIMEOUT`：节点不存在、只肯给 DMA-BUF 时这里就报出来。
    fn wait_first(&self) -> io::Result<()> {
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(error) = &state.error {
                return Err(io::Error::other(error.clone()));
            }
            if state.fresh {
                return Ok(());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the screen stream gave no picture"));
            }
            state = self.shared.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

impl Source for Stream {
    fn wait(&mut self, timeout: Duration) -> io::Result<bool> {
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.fresh && state.error.is_none() {
            state = self.shared.changed.wait_timeout(state, timeout).unwrap_or_else(PoisonError::into_inner).0;
        }
        if let Some(error) = &state.error {
            return Err(io::Error::other(error.clone()));
        }
        if !state.fresh {
            return Ok(false);
        }
        std::mem::swap(&mut self.frame, &mut state.frame);
        self.size = state.size;
        state.fresh = false;
        self.have_frame = true;
        Ok(true)
    }

    fn picture(&self) -> Option<Picture<'_>> {
        self.have_frame.then(|| Picture { data: &self.frame, stride: self.size.0 as usize * 4, size: self.size })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let lib = self.lib;
        // SAFETY: 拿着循环的锁断开、销毁流和连接，再停掉循环、销毁上下文和循环；之后不会再有回调。循环没起来时
        // 锁和停都不碍事。
        unsafe {
            (lib.thread_loop_lock)(self.thread_loop);
            if !self.stream.is_null() {
                (lib.stream_disconnect)(self.stream);
                (lib.stream_destroy)(self.stream);
            }
            if !self.core.is_null() {
                (lib.core_disconnect)(self.core);
            }
            (lib.thread_loop_unlock)(self.thread_loop);
            (lib.thread_loop_stop)(self.thread_loop);
            (lib.context_destroy)(self.context);
            (lib.thread_loop_destroy)(self.thread_loop);
        }
    }
}

fn fail(shared: &Shared, error: String) {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    state.error.get_or_insert(error);
    shared.changed.notify_all();
}

unsafe extern "C" fn on_state_changed(data: *mut c_void, _old: c_int, state: c_int, error: *const c_char) {
    // SAFETY: `data` 是 `Stream::connect` 交的 `Callback`，活得比流久。
    let callback = unsafe { &*data.cast::<Callback>() };
    match state {
        PW_STREAM_STATE_ERROR => {
            let error = if error.is_null() {
                "unknown error".to_owned()
            } else {
                // SAFETY: PipeWire 交来的是以 NUL 结尾的字符串，回调期间有效。
                unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned()
            };
            fail(&callback.shared, format!("the screen stream failed: {error}"));
        }
        PW_STREAM_STATE_UNCONNECTED => fail(&callback.shared, "the screen stream ended".to_owned()),
        _ => {}
    }
}

unsafe extern "C" fn on_param_changed(data: *mut c_void, id: u32, param: *const SpaPod) {
    if id != SPA_PARAM_FORMAT || param.is_null() {
        return;
    }
    // SAFETY: `data` 是 `Stream::connect` 交的 `Callback`，活得比流久；`param` 是一个完整的 pod，长度在它的头里。
    let (callback, pod) = unsafe {
        let pod = std::slice::from_raw_parts(param.cast::<u8>(), 8 + (*param).size as usize);
        (&*data.cast::<Callback>(), pod)
    };
    let Some((format, size)) = parse_format(pod) else {
        fail(&callback.shared, "the screen stream offered no usable picture format".to_owned());
        return;
    };
    tracing::info!("screen stream format {format} at {}x{}", size.0, size.1);
    callback.shared.state.lock().unwrap_or_else(PoisonError::into_inner).format = Some((format, size));
    // 只要内存里的画面：不认 DMA-BUF。
    let buffers = buffers_param();
    let mut params = [buffers.as_ptr().cast::<SpaPod>()];
    // SAFETY: 在循环线程上、流的回调里调，参数在调用期间有效。
    unsafe { (callback.lib.stream_update_params)(callback.stream, params.as_mut_ptr(), 1) };
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    // SAFETY: `data` 是 `Stream::connect` 交的 `Callback`，活得比流久。
    let callback = unsafe { &*data.cast::<Callback>() };
    let lib = callback.lib;
    // 排着几帧时只要最新的，旧的直接还回去。
    let mut latest: *mut PwBuffer = ptr::null_mut();
    loop {
        // SAFETY: 在流的回调里取、还缓冲。
        let next = unsafe { (lib.stream_dequeue_buffer)(callback.stream) };
        if next.is_null() {
            break;
        }
        if !latest.is_null() {
            // SAFETY: 同上。
            unsafe { (lib.stream_queue_buffer)(callback.stream, latest) };
        }
        latest = next;
    }
    if latest.is_null() {
        return;
    }
    // SAFETY: 缓冲在还回去之前有效；`MAP_BUFFERS` 让内存类的数据已经映射好。
    unsafe {
        copy_buffer(callback, latest);
        (lib.stream_queue_buffer)(callback.stream, latest);
    }
}

/// 把缓冲里的画面换成 BGRx 拷进 `Shared`。
///
/// # Safety
///
/// `buffer` 是刚从流里取出、还没还回去的缓冲。
unsafe fn copy_buffer(callback: &Callback, buffer: *mut PwBuffer) {
    let mut state = callback.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    let Some((format, (width, height))) = state.format else { return };
    // SAFETY: 调用方保证缓冲有效。
    let (data, chunk) = unsafe {
        let spa = (*buffer).buffer;
        if spa.is_null() || (*spa).n_datas == 0 {
            return;
        }
        let data = &*(*spa).datas;
        if data.chunk.is_null() {
            return;
        }
        (data, &*data.chunk)
    };
    if data.kind != SPA_DATA_MEM_PTR && data.kind != SPA_DATA_MEM_FD {
        drop(state);
        fail(&callback.shared, format!("the screen stream gives buffers of type {}, not memory", data.kind));
        return;
    }
    if data.data.is_null() || chunk.size == 0 || chunk.flags & SPA_CHUNK_FLAG_CORRUPTED != 0 {
        return;
    }
    if height == 0 {
        return;
    }
    let stride = if chunk.stride > 0 { chunk.stride as usize } else { width as usize * 4 };
    let row = width as usize * 4;
    let needed = stride * (height as usize - 1) + row;
    let available = (data.maxsize as usize).saturating_sub(chunk.offset as usize);
    if needed > available {
        return;
    }
    // SAFETY: 映射的数据从 `chunk.offset` 起至少有 `needed` 字节，上面核对过。
    let source = unsafe { std::slice::from_raw_parts(data.data.cast::<u8>().add(chunk.offset as usize), needed) };
    let frame = &mut state.frame;
    frame.resize(row * height as usize, 0);
    for y in 0..height as usize {
        let from = &source[y * stride..y * stride + row];
        let to = &mut frame[y * row..(y + 1) * row];
        match format {
            RGBX | RGBA => {
                for (to, from) in to.as_chunks_mut::<4>().0.iter_mut().zip(from.as_chunks::<4>().0) {
                    *to = [from[2], from[1], from[0], 255];
                }
            }
            _ => to.copy_from_slice(from),
        }
    }
    state.size = (width, height);
    state.fresh = true;
    callback.shared.changed.notify_all();
}

/// 拼 SPA 的 pod：每个 pod 是四字节的长度、四字节的类型，再是内容，补齐到八字节。
#[derive(Default)]
struct PodBuilder {
    bytes: Vec<u8>,
}

impl PodBuilder {
    fn u32(&mut self, value: u32) {
        self.bytes.extend(value.to_ne_bytes());
    }

    fn pad(&mut self) {
        while !self.bytes.len().is_multiple_of(8) {
            self.bytes.push(0);
        }
    }

    fn id(&mut self, value: u32) {
        self.u32(4);
        self.u32(SPA_TYPE_ID);
        self.u32(value);
        self.pad();
    }

    /// 一组同类值的选择：`kind` 是 `SPA_CHOICE_*`，每个值 `child_size` 字节。
    fn choice(&mut self, kind: u32, child_type: u32, child_size: u32, values: &[&[u32]]) {
        let body = 8 + 8 + child_size * values.len() as u32;
        self.u32(body);
        self.u32(SPA_TYPE_CHOICE);
        self.u32(kind);
        self.u32(0);
        self.u32(child_size);
        self.u32(child_type);
        for value in values {
            for &word in *value {
                self.u32(word);
            }
        }
        self.pad();
    }

    /// 一个对象：`props` 往里写各个属性（`prop`）。
    fn object(&mut self, object_type: u32, id: u32, props: impl FnOnce(&mut Self)) {
        let start = self.bytes.len();
        self.u32(0);
        self.u32(SPA_TYPE_OBJECT);
        self.u32(object_type);
        self.u32(id);
        props(self);
        let size = (self.bytes.len() - start - 8) as u32;
        self.bytes[start..start + 4].copy_from_slice(&size.to_ne_bytes());
    }

    fn prop(&mut self, key: u32) {
        self.u32(key);
        self.u32(0);
    }

    /// 拼好的 pod，按八字节对齐交出去。
    fn finish(self) -> AlignedPod {
        let mut words = vec![0u64; self.bytes.len().div_ceil(8)];
        // SAFETY: `words` 至少有 `bytes.len()` 字节。
        unsafe { ptr::copy_nonoverlapping(self.bytes.as_ptr(), words.as_mut_ptr().cast::<u8>(), self.bytes.len()) };
        AlignedPod(words)
    }
}

/// 按八字节对齐存着的一个 pod。
struct AlignedPod(Vec<u64>);

impl AlignedPod {
    fn as_ptr(&self) -> *const u8 {
        self.0.as_ptr().cast()
    }
}

/// 列出收的格式：原始视频，`VIDEO_FORMATS` 里的像素格式，尺寸、帧率随对方。不带 modifier，对方就不给 DMA-BUF。
fn enum_format(size: (u32, u32)) -> AlignedPod {
    let mut pod = PodBuilder::default();
    pod.object(SPA_TYPE_OBJECT_FORMAT, SPA_PARAM_ENUM_FORMAT, |pod| {
        pod.prop(SPA_FORMAT_MEDIA_TYPE);
        pod.id(SPA_MEDIA_TYPE_VIDEO);
        pod.prop(SPA_FORMAT_MEDIA_SUBTYPE);
        pod.id(SPA_MEDIA_SUBTYPE_RAW);
        pod.prop(SPA_FORMAT_VIDEO_FORMAT);
        let formats: Vec<&[u32]> = std::iter::once(&VIDEO_FORMATS[..1]).chain(VIDEO_FORMATS.chunks(1)).collect();
        pod.choice(SPA_CHOICE_ENUM, SPA_TYPE_ID, 4, &formats);
        pod.prop(SPA_FORMAT_VIDEO_SIZE);
        let default = [size.0.max(1), size.1.max(1)];
        pod.choice(SPA_CHOICE_RANGE, SPA_TYPE_RECTANGLE, 8, &[&default, &[1, 1], &[16384, 16384]]);
        pod.prop(SPA_FORMAT_VIDEO_FRAMERATE);
        pod.choice(SPA_CHOICE_RANGE, SPA_TYPE_FRACTION, 8, &[&[30, 1], &[0, 1], &[1000, 1]]);
    });
    pod.finish()
}

/// 缓冲只要内存类的（`MemPtr`、`MemFd`）。
fn buffers_param() -> AlignedPod {
    let mut pod = PodBuilder::default();
    pod.object(SPA_TYPE_OBJECT_PARAM_BUFFERS, SPA_PARAM_BUFFERS, |pod| {
        pod.prop(SPA_PARAM_BUFFERS_DATA_TYPE);
        let mask = (1 << SPA_DATA_MEM_PTR) | (1 << SPA_DATA_MEM_FD);
        pod.choice(SPA_CHOICE_FLAGS, SPA_TYPE_INT, 4, &[&[mask]]);
    });
    pod.finish()
}

/// 从商定好的格式（一个 `Format` 对象）读出像素格式和尺寸；不是收的格式时为空。
fn parse_format(pod: &[u8]) -> Option<(u32, (u32, u32))> {
    let word = |at: usize| pod.get(at..at + 4).map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap_or_default()));
    if word(4)? != SPA_TYPE_OBJECT {
        return None;
    }
    let end = (8 + word(0)? as usize).min(pod.len());
    let mut at = 16;
    let (mut format, mut size) = (None, None);
    while at + 16 <= end {
        let key = word(at)?;
        let value_size = word(at + 8)? as usize;
        let value_type = word(at + 12)?;
        let body = at + 16;
        // 选择类的值取它的第一个（默认值）。
        let (kind, value) =
            if value_type == SPA_TYPE_CHOICE { (word(body + 12)?, body + 16) } else { (value_type, body) };
        match (key, kind) {
            (SPA_FORMAT_VIDEO_FORMAT, SPA_TYPE_ID) => format = word(value),
            (SPA_FORMAT_VIDEO_SIZE, SPA_TYPE_RECTANGLE) => size = Some((word(value)?, word(value + 4)?)),
            _ => {}
        }
        at = body + value_size.next_multiple_of(8);
    }
    let format = format.filter(|format| VIDEO_FORMATS.contains(format))?;
    Some((format, size.filter(|(width, height)| *width > 0 && *height > 0)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(pod: &AlignedPod) -> Vec<u8> {
        // SAFETY: 读 pod 自己报的长度。
        unsafe {
            let size = 8 + *pod.as_ptr().cast::<u32>() as usize;
            std::slice::from_raw_parts(pod.as_ptr(), size).to_vec()
        }
    }

    #[test]
    fn builds_and_reads_back_formats() {
        let pod = bytes(&enum_format((1920, 1080)));
        // libpipewire 的 `spa_pod_builder_add_object` 拼同样的格式得到的字。
        let expected: [u32; 58] = [
            0xe0, 0xf, 0x40003, 0x3, 0x1, 0x0, 0x4, 0x3, 0x2, 0x0, 0x2, 0x0, 0x4, 0x3, 0x1, 0x0, 0x20001, 0x0, 0x24,
            0x13, 0x3, 0x0, 0x4, 0x3, 0x8, 0x8, 0xc, 0x7, 0xb, 0x0, 0x20003, 0x0, 0x28, 0x13, 0x1, 0x0, 0x8, 0xa,
            0x780, 0x438, 0x1, 0x1, 0x4000, 0x4000, 0x20004, 0x0, 0x28, 0x13, 0x1, 0x0, 0x8, 0xb, 0x1e, 0x1, 0x0, 0x1,
            0x3e8, 0x1,
        ];
        assert_eq!(pod, expected.iter().flat_map(|word| word.to_ne_bytes()).collect::<Vec<_>>());
        // 列出的格式里，选择的第一个值（默认的）就是读得出的那个。
        assert_eq!(parse_format(&pod), Some((BGRX, (1920, 1080))));
    }

    #[test]
    fn reads_a_fixed_format() {
        let mut pod = PodBuilder::default();
        pod.object(SPA_TYPE_OBJECT_FORMAT, SPA_PARAM_FORMAT, |pod| {
            pod.prop(SPA_FORMAT_MEDIA_TYPE);
            pod.id(SPA_MEDIA_TYPE_VIDEO);
            pod.prop(SPA_FORMAT_VIDEO_FORMAT);
            pod.id(RGBA);
            pod.prop(SPA_FORMAT_VIDEO_SIZE);
            pod.u32(8);
            pod.u32(SPA_TYPE_RECTANGLE);
            pod.u32(1280);
            pod.u32(800);
        });
        assert_eq!(parse_format(&bytes(&pod.finish())), Some((RGBA, (1280, 800))));
    }
}
