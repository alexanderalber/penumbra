//! The NDI runtime, loaded when first needed: finding sources, receiving and sending.
//!
//! penumbra neither links against the NDI SDK nor ships its library. The runtime comes from
//! NDI's own installer (NDI Tools or the NDI Runtime), which names its folder in the
//! environment variable `NDI_RUNTIME_DIR_V6`; without it every NDI node fails with a message
//! that says where to get it. The declarations below follow the SDK's C headers (which NDI
//! publishes for open-source projects under the MIT licence), version 6.
//!
//! This module holds the crate's `unsafe`. Every function is looked up once by its exported
//! name with the header's signature; the library is loaded once and never unloaded, so the
//! function pointers stay valid for the process. Frames the runtime hands out are wrapped in
//! guards that free them, and their bytes are only read within the size the header promises.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

/// Where to download the runtime, from the SDK's `NDILIB_REDIST_URL`.
pub const RUNTIME_URL: &str =
    if cfg!(windows) { "http://ndi.link/NDIRedistV6" } else { "https://ndi.video/tools/" };

/// The largest picture accepted from the runtime, per side.
const MAX_SIDE: c_int = 16384;

#[repr(C)]
struct RawSource {
    name: *const c_char,
    url: *const c_char,
}

#[repr(C)]
struct FindCreate {
    show_local_sources: bool,
    groups: *const c_char,
    extra_ips: *const c_char,
}

#[repr(C)]
struct RecvCreate {
    source: RawSource,
    color_format: c_int,
    bandwidth: c_int,
    allow_video_fields: bool,
    name: *const c_char,
}

#[repr(C)]
struct SendCreate {
    name: *const c_char,
    groups: *const c_char,
    clock_video: bool,
    clock_audio: bool,
}

#[repr(C)]
struct RawVideo {
    xres: c_int,
    yres: c_int,
    fourcc: u32,
    frame_rate_n: c_int,
    frame_rate_d: c_int,
    aspect: f32,
    frame_format: c_int,
    timecode: i64,
    data: *mut u8,
    /// `line_stride_in_bytes` for uncompressed formats.
    stride: c_int,
    metadata: *const c_char,
    timestamp: i64,
}

#[repr(C)]
struct RawAudio {
    sample_rate: c_int,
    channels: c_int,
    samples: c_int,
    timecode: i64,
    fourcc: u32,
    data: *mut u8,
    /// `channel_stride_in_bytes` for planar float.
    stride: c_int,
    metadata: *const c_char,
    timestamp: i64,
}

#[repr(C)]
struct RawMetadata {
    length: c_int,
    timecode: i64,
    data: *mut c_char,
}

#[repr(C)]
#[derive(Default)]
struct RawPerformance {
    video: i64,
    audio: i64,
    metadata: i64,
}

type Instance = *mut c_void;

const fn code(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

/// The pixel formats penumbra sends and takes.
pub mod fourcc {
    use super::code;
    pub const UYVY: u32 = code(b"UYVY");
    /// UYVY followed by a plane of 8-bit alpha.
    pub const UYVA: u32 = code(b"UYVA");
    pub const BGRA: u32 = code(b"BGRA");
    pub const BGRX: u32 = code(b"BGRX");
    pub const RGBA: u32 = code(b"RGBA");
    pub const RGBX: u32 = code(b"RGBX");
    /// Planar 32-bit float audio.
    pub const FLTP: u32 = code(b"FLTp");

    /// A FourCC as text, for messages.
    pub fn name(code: u32) -> String {
        code.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
    }
}

const FRAME_NONE: c_int = 0;
const FRAME_VIDEO: c_int = 1;
const FRAME_AUDIO: c_int = 2;
const FRAME_METADATA: c_int = 3;
const FRAME_ERROR: c_int = 4;
const FRAME_STATUS_CHANGE: c_int = 100;
const FRAME_SOURCE_CHANGE: c_int = 101;
const FORMAT_PROGRESSIVE: c_int = 1;
const FORMAT_FIELD_0: c_int = 2;
const FORMAT_FIELD_1: c_int = 3;
/// `NDIlib_send_timecode_synthesize`.
const TIMECODE_SYNTHESIZE: i64 = i64::MAX;
/// `NDIlib_recv_timestamp_undefined`.
const TIMESTAMP_UNDEFINED: i64 = i64::MAX;

/// The video quality a receiver asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bandwidth {
    /// The stream as sent.
    Highest,
    /// A smaller, lower-quality stream the sender offers beside it.
    Lowest,
}

/// The pixel formats a receiver takes: without alpha, with alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorFormat {
    /// UYVY without alpha, BGRA with: no conversion in the runtime for NDI's own codec.
    UyvyBgra,
    /// BGRX or BGRA: converted by the runtime on the CPU.
    BgrxBgra,
    /// What the runtime decodes to, unconverted: normally UYVY, UYVA with alpha, single fields
    /// for interlaced sources.
    Fastest,
}

impl ColorFormat {
    fn raw(self) -> c_int {
        match self {
            Self::BgrxBgra => 0,
            Self::UyvyBgra => 1,
            Self::Fastest => 100,
        }
    }
}

type InitFn = unsafe extern "C" fn() -> bool;
type VersionFn = unsafe extern "C" fn() -> *const c_char;
type FindCreateFn = unsafe extern "C" fn(*const FindCreate) -> Instance;
type DestroyFn = unsafe extern "C" fn(Instance);
type FindSourcesFn = unsafe extern "C" fn(Instance, *mut u32) -> *const RawSource;
type FindWaitFn = unsafe extern "C" fn(Instance, u32) -> bool;
type RecvCreateFn = unsafe extern "C" fn(*const RecvCreate) -> Instance;
type RecvCaptureFn =
    unsafe extern "C" fn(Instance, *mut RawVideo, *mut RawAudio, *mut RawMetadata, u32) -> c_int;
type RecvFreeVideoFn = unsafe extern "C" fn(Instance, *const RawVideo);
type RecvFreeAudioFn = unsafe extern "C" fn(Instance, *const RawAudio);
type RecvFreeMetadataFn = unsafe extern "C" fn(Instance, *const RawMetadata);
type RecvPerformanceFn = unsafe extern "C" fn(Instance, *mut RawPerformance, *mut RawPerformance);
type RecvConnectionsFn = unsafe extern "C" fn(Instance) -> c_int;
type SendCreateFn = unsafe extern "C" fn(*const SendCreate) -> Instance;
type SendVideoFn = unsafe extern "C" fn(Instance, *const RawVideo);
type SendConnectionsFn = unsafe extern "C" fn(Instance, u32) -> c_int;
type SendSourceNameFn = unsafe extern "C" fn(Instance) -> *const RawSource;

/// The loaded and initialised runtime.
pub struct Ndi {
    version: String,
    path: String,
    find_create: FindCreateFn,
    find_destroy: DestroyFn,
    find_sources: FindSourcesFn,
    find_wait: FindWaitFn,
    recv_create: RecvCreateFn,
    recv_destroy: DestroyFn,
    recv_capture: RecvCaptureFn,
    recv_free_video: RecvFreeVideoFn,
    recv_free_audio: RecvFreeAudioFn,
    recv_free_metadata: RecvFreeMetadataFn,
    recv_performance: RecvPerformanceFn,
    recv_connections: RecvConnectionsFn,
    send_create: SendCreateFn,
    send_destroy: DestroyFn,
    send_video: SendVideoFn,
    send_connections: SendConnectionsFn,
    send_source_name: SendSourceNameFn,
    // Kept for the process: the functions above point into it.
    _lib: libloading::Library,
}

static RUNTIME: OnceLock<Result<Ndi, String>> = OnceLock::new();

/// The runtime, loaded on the first call; the same error on every call when it is missing.
pub fn runtime() -> Result<&'static Ndi, String> {
    RUNTIME.get_or_init(Ndi::load).as_ref().map_err(Clone::clone)
}

impl Ndi {
    fn load() -> Result<Ndi, String> {
        let (lib, path) = load_library()?;
        // SAFETY: each symbol is looked up by the name the SDK exports, with the signature its
        // headers give it; the copied pointers stay valid because `lib` is kept in the returned
        // value, which lives in a static and is never dropped.
        unsafe {
            fn get<T: Copy>(lib: &libloading::Library, name: &[u8]) -> Result<T, String> {
                // SAFETY: the caller names the type the header declares for `name`.
                unsafe { lib.get::<T>(name) }.map(|f| *f).map_err(|e| {
                    let shown =
                        String::from_utf8_lossy(name.strip_suffix(b"\0").unwrap_or(name)).into_owned();
                    format!("the NDI runtime lacks {shown} ({e}); install a current one from {RUNTIME_URL}")
                })
            }
            let initialize: InitFn = get(&lib, b"NDIlib_initialize\0")?;
            let version: VersionFn = get(&lib, b"NDIlib_version\0")?;
            let supported: InitFn = get(&lib, b"NDIlib_is_supported_CPU\0")?;
            if !supported() {
                return Err("NDI does not run on this CPU (it needs SSSE3)".into());
            }
            if !initialize() {
                return Err("the NDI runtime did not start".into());
            }
            let v = version();
            let version =
                if v.is_null() { String::new() } else { CStr::from_ptr(v).to_string_lossy().into_owned() };
            Ok(Ndi {
                version,
                path,
                find_create: get(&lib, b"NDIlib_find_create_v2\0")?,
                find_destroy: get(&lib, b"NDIlib_find_destroy\0")?,
                find_sources: get(&lib, b"NDIlib_find_get_current_sources\0")?,
                find_wait: get(&lib, b"NDIlib_find_wait_for_sources\0")?,
                recv_create: get(&lib, b"NDIlib_recv_create_v3\0")?,
                recv_destroy: get(&lib, b"NDIlib_recv_destroy\0")?,
                recv_capture: get(&lib, b"NDIlib_recv_capture_v3\0")?,
                recv_free_video: get(&lib, b"NDIlib_recv_free_video_v2\0")?,
                recv_free_audio: get(&lib, b"NDIlib_recv_free_audio_v3\0")?,
                recv_free_metadata: get(&lib, b"NDIlib_recv_free_metadata\0")?,
                recv_performance: get(&lib, b"NDIlib_recv_get_performance\0")?,
                recv_connections: get(&lib, b"NDIlib_recv_get_no_connections\0")?,
                send_create: get(&lib, b"NDIlib_send_create\0")?,
                send_destroy: get(&lib, b"NDIlib_send_destroy\0")?,
                send_video: get(&lib, b"NDIlib_send_send_video_v2\0")?,
                send_connections: get(&lib, b"NDIlib_send_get_no_connections\0")?,
                send_source_name: get(&lib, b"NDIlib_send_get_source_name\0")?,
                _lib: lib,
            })
        }
    }

    /// The runtime's version text, such as `NDI SDK WIN64 ... 6.3.2.0`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The file the runtime was loaded from.
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// Copies a C string from the runtime; empty for null.
///
/// # Safety
/// `p` is null or points to a NUL-terminated string that stays valid for the call.
unsafe fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: by the function's contract.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn c_string(s: &str, what: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("the {what} contains a NUL character"))
}

/// A source on the network: its full name (`MACHINE (Source)`) and address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub url: String,
}

/// Watches the network for sources (mDNS and discovery servers, as NDI is configured).
pub struct Finder {
    ndi: &'static Ndi,
    handle: Instance,
}

// SAFETY: the SDK allows using an instance from any thread; `Finder` is used by one owner at a
// time (`&mut` is not needed by the runtime, which locks internally).
unsafe impl Send for Finder {}
// SAFETY: as above; the find functions are thread-safe in the SDK.
unsafe impl Sync for Finder {}

impl Finder {
    pub fn new(ndi: &'static Ndi) -> Result<Finder, String> {
        let settings =
            FindCreate { show_local_sources: true, groups: std::ptr::null(), extra_ips: std::ptr::null() };
        // SAFETY: `settings` is a valid struct of the header's layout, read during the call.
        let handle = unsafe { (ndi.find_create)(&raw const settings) };
        if handle.is_null() {
            return Err("NDI could not start looking for sources".into());
        }
        Ok(Finder { ndi, handle })
    }

    /// Waits up to `timeout_ms` for the list to change; true when it did.
    pub fn wait(&self, timeout_ms: u32) -> bool {
        // SAFETY: `handle` is a live finder.
        unsafe { (self.ndi.find_wait)(self.handle, timeout_ms) }
    }

    /// The sources seen so far.
    pub fn sources(&self) -> Vec<Source> {
        let mut n = 0u32;
        // SAFETY: `handle` is a live finder; the array it returns holds `n` entries and stays
        // valid until the next call on this finder, which cannot happen while `&self` copies it
        // out here (the runtime only replaces it on the next call from this instance).
        unsafe {
            let list = (self.ndi.find_sources)(self.handle, &raw mut n);
            if list.is_null() {
                return Vec::new();
            }
            // More than a few thousand would be a broken answer.
            let n = n.min(4096) as usize;
            std::slice::from_raw_parts(list, n)
                .iter()
                .map(|s| Source { name: text(s.name), url: text(s.url) })
                .filter(|s| !s.name.is_empty())
                .collect()
        }
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        // SAFETY: `handle` came from find_create and is not used after.
        unsafe { (self.ndi.find_destroy)(self.handle) }
    }
}

/// Counts of frames since a receiver connected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub video: i64,
    pub audio: i64,
}

/// A receiver connected to one source by name.
pub struct Recv {
    ndi: &'static Ndi,
    handle: Instance,
}

// SAFETY: the SDK documents capture as thread-safe ("NDIlib_recv_capture_v3 is multi-thread
// safe") and frames may be freed from any thread.
unsafe impl Send for Recv {}
// SAFETY: as above.
unsafe impl Sync for Recv {}

impl Recv {
    /// Connects to `source`; the connection is made in the background and kept up by the
    /// runtime. Without a URL the runtime looks the name up itself, which takes seconds; with
    /// the one a [`Finder`] listed it connects at once. `name` is how this receiver is listed.
    pub fn new(
        ndi: &'static Ndi,
        source: &Source,
        bandwidth: Bandwidth,
        color: ColorFormat,
        name: &str,
    ) -> Result<Recv, String> {
        let source_name = c_string(&source.name, "source name")?;
        let url = if source.url.is_empty() { None } else { Some(c_string(&source.url, "source address")?) };
        let name = c_string(name, "receiver name")?;
        let settings = RecvCreate {
            source: RawSource {
                name: source_name.as_ptr(),
                url: url.as_ref().map_or(std::ptr::null(), |u| u.as_ptr()),
            },
            color_format: color.raw(),
            bandwidth: match bandwidth {
                Bandwidth::Highest => 100,
                Bandwidth::Lowest => 0,
            },
            allow_video_fields: false,
            name: name.as_ptr(),
        };
        // SAFETY: `settings` and the strings it points to live through the call; the runtime
        // copies what it keeps.
        let handle = unsafe { (ndi.recv_create)(&raw const settings) };
        if handle.is_null() {
            return Err("NDI could not create a receiver".into());
        }
        Ok(Recv { ndi, handle })
    }

    /// Waits up to `timeout_ms` for the next frame of video or audio.
    pub fn capture(&self, timeout_ms: u32) -> Capture<'_> {
        let mut video = RawVideo {
            xres: 0,
            yres: 0,
            fourcc: 0,
            frame_rate_n: 0,
            frame_rate_d: 0,
            aspect: 0.0,
            frame_format: 0,
            timecode: 0,
            data: std::ptr::null_mut(),
            stride: 0,
            metadata: std::ptr::null(),
            timestamp: 0,
        };
        let mut audio = RawAudio {
            sample_rate: 0,
            channels: 0,
            samples: 0,
            timecode: 0,
            fourcc: 0,
            data: std::ptr::null_mut(),
            stride: 0,
            metadata: std::ptr::null(),
            timestamp: 0,
        };
        let mut meta = RawMetadata { length: 0, timecode: 0, data: std::ptr::null_mut() };
        // SAFETY: `handle` is a live receiver; the out structs are of the header's layout. Each
        // frame type filled in is freed below or by its guard.
        let kind = unsafe {
            (self.ndi.recv_capture)(self.handle, &raw mut video, &raw mut audio, &raw mut meta, timeout_ms)
        };
        match kind {
            FRAME_VIDEO => Capture::Video(VideoFrame { recv: self, raw: video }),
            FRAME_AUDIO => Capture::Audio(AudioFrame { recv: self, raw: audio }),
            FRAME_METADATA => {
                // SAFETY: a metadata frame was filled in and is freed once.
                unsafe { (self.ndi.recv_free_metadata)(self.handle, &raw const meta) };
                Capture::Other
            }
            FRAME_NONE => Capture::Nothing,
            FRAME_ERROR => Capture::Lost,
            FRAME_STATUS_CHANGE | FRAME_SOURCE_CHANGE => Capture::Other,
            _ => Capture::Other,
        }
    }

    /// Frames received and dropped since the connection.
    pub fn counts(&self) -> (Counts, Counts) {
        let mut total = RawPerformance::default();
        let mut dropped = RawPerformance::default();
        // SAFETY: `handle` is live; both out structs are of the header's layout.
        unsafe { (self.ndi.recv_performance)(self.handle, &raw mut total, &raw mut dropped) };
        (
            Counts { video: total.video, audio: total.audio },
            Counts { video: dropped.video, audio: dropped.audio },
        )
    }

    /// Whether the source is connected now.
    pub fn connected(&self) -> bool {
        // SAFETY: `handle` is live.
        unsafe { (self.ndi.recv_connections)(self.handle) > 0 }
    }
}

impl Drop for Recv {
    fn drop(&mut self) {
        // SAFETY: `handle` came from recv_create; frames borrow `self`, so none is left.
        unsafe { (self.ndi.recv_destroy)(self.handle) }
    }
}

/// What one [`Recv::capture`] got.
pub enum Capture<'a> {
    Video(VideoFrame<'a>),
    Audio(AudioFrame<'a>),
    /// Nothing within the timeout.
    Nothing,
    /// The connection was lost; the runtime reconnects on its own.
    Lost,
    /// Metadata or a change of the source's settings, nothing to take.
    Other,
}

/// A received picture, freed when dropped.
pub struct VideoFrame<'a> {
    recv: &'a Recv,
    raw: RawVideo,
}

impl VideoFrame<'_> {
    pub fn width(&self) -> u32 {
        self.raw.xres.max(0) as u32
    }

    pub fn height(&self) -> u32 {
        self.raw.yres.max(0) as u32
    }

    pub fn fourcc(&self) -> u32 {
        self.raw.fourcc
    }

    pub fn stride(&self) -> usize {
        self.raw.stride.max(0) as usize
    }

    /// Frames per second as the sender states it.
    pub fn frame_rate(&self) -> f64 {
        if self.raw.frame_rate_d > 0 {
            f64::from(self.raw.frame_rate_n) / f64::from(self.raw.frame_rate_d)
        } else {
            0.0
        }
    }

    /// When the sender sent it, in 100 ns units since 1970 (UTC), if it says.
    pub fn timestamp(&self) -> Option<i64> {
        (self.raw.timestamp != TIMESTAMP_UNDEFINED && self.raw.timestamp > 0).then_some(self.raw.timestamp)
    }

    /// Whether the picture is whole frames (not fields).
    pub fn progressive(&self) -> bool {
        self.raw.frame_format == FORMAT_PROGRESSIVE
    }

    /// Whether the picture is one field of an interlaced frame (half its height).
    pub fn is_field(&self) -> bool {
        matches!(self.raw.frame_format, FORMAT_FIELD_0 | FORMAT_FIELD_1)
    }

    /// The pixel bytes, rows of [`VideoFrame::stride`] bytes (for UYVA followed by the alpha
    /// plane, `width` bytes per row); `None` for a format penumbra does not take or a size
    /// that does not add up.
    pub fn data(&self) -> Option<&[u8]> {
        let r = &self.raw;
        if r.data.is_null() || r.xres <= 0 || r.yres <= 0 || r.xres > MAX_SIDE || r.yres > MAX_SIDE {
            return None;
        }
        let (w, h, stride) = (r.xres as usize, r.yres as usize, r.stride.max(0) as usize);
        let len = match r.fourcc {
            fourcc::UYVY if stride >= w * 2 => stride * h,
            fourcc::UYVA if stride >= w * 2 => stride * h + w * h,
            fourcc::BGRA | fourcc::BGRX | fourcc::RGBA | fourcc::RGBX if stride >= w * 4 => stride * h,
            _ => return None,
        };
        // SAFETY: for these uncompressed formats the header defines the buffer as `yres` rows of
        // `line_stride_in_bytes` (plus the alpha plane for UYVA); it stays valid until freed in
        // `drop`, and the slice borrows `self`.
        Some(unsafe { std::slice::from_raw_parts(r.data, len) })
    }
}

impl Drop for VideoFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: the frame came from this receiver's capture and is freed once.
        unsafe { (self.recv.ndi.recv_free_video)(self.recv.handle, &raw const self.raw) }
    }
}

/// Received audio, planar 32-bit float, freed when dropped.
pub struct AudioFrame<'a> {
    recv: &'a Recv,
    raw: RawAudio,
}

impl AudioFrame<'_> {
    pub fn sample_rate(&self) -> u32 {
        self.raw.sample_rate.max(0) as u32
    }

    pub fn channels(&self) -> usize {
        self.raw.channels.clamp(0, 64) as usize
    }

    pub fn samples(&self) -> usize {
        self.raw.samples.max(0) as usize
    }

    /// When the sender sent it, in 100 ns units since 1970 (UTC), if it says.
    pub fn timestamp(&self) -> Option<i64> {
        (self.raw.timestamp != TIMESTAMP_UNDEFINED && self.raw.timestamp > 0).then_some(self.raw.timestamp)
    }

    /// Channel `c`'s samples; `None` past the last channel or for a format other than planar
    /// float.
    pub fn channel(&self, c: usize) -> Option<&[f32]> {
        let r = &self.raw;
        let n = self.samples();
        if r.data.is_null() || r.fourcc != fourcc::FLTP || c >= self.channels() || n > 1 << 20 {
            return None;
        }
        let stride = r.stride.max(0) as usize;
        if stride < n * 4 || !stride.is_multiple_of(4) || !(r.data as usize).is_multiple_of(4) {
            return None;
        }
        // SAFETY: planar float is `no_channels` planes of `channel_stride_in_bytes`, each holding
        // `no_samples` floats; the pointer is aligned (checked), valid until freed in `drop`.
        Some(unsafe { std::slice::from_raw_parts(r.data.add(c * stride).cast::<f32>(), n) })
    }
}

impl Drop for AudioFrame<'_> {
    fn drop(&mut self) {
        // SAFETY: the frame came from this receiver's capture and is freed once.
        unsafe { (self.recv.ndi.recv_free_audio)(self.recv.handle, &raw const self.raw) }
    }
}

/// A picture to send: rows of `stride` bytes in `fourcc`'s layout.
pub struct OutFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub stride: usize,
    pub data: &'a [u8],
    /// Frames per second, as numerator and denominator.
    pub rate: (u32, u32),
}

/// A source this program offers on the network.
pub struct Sender {
    ndi: &'static Ndi,
    handle: Instance,
    name: String,
}

// SAFETY: the SDK allows sending "at any time, off any thread"; one owner sends at a time.
unsafe impl Send for Sender {}
// SAFETY: as above; `connections` may be asked from another thread.
unsafe impl Sync for Sender {}

impl Sender {
    /// Announces a source named `name`; the runtime prefixes the machine's name. Sending is not
    /// clocked: frames go out when they are sent.
    pub fn new(ndi: &'static Ndi, name: &str) -> Result<Sender, String> {
        let wanted = c_string(name, "source name")?;
        let settings = SendCreate {
            name: wanted.as_ptr(),
            groups: std::ptr::null(),
            clock_video: false,
            clock_audio: false,
        };
        // SAFETY: `settings` and its string live through the call; the runtime copies the name.
        let handle = unsafe { (ndi.send_create)(&raw const settings) };
        if handle.is_null() {
            return Err(format!("NDI could not create the source `{name}`"));
        }
        // SAFETY: `handle` is live; the returned source belongs to it and is copied at once.
        let full = unsafe {
            let s = (ndi.send_source_name)(handle);
            if s.is_null() { String::new() } else { text((*s).name) }
        };
        Ok(Sender { ndi, handle, name: if full.is_empty() { name.to_string() } else { full } })
    }

    /// The full name receivers see, `MACHINE (name)`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sends one picture; returns when the runtime is done with the bytes (it compresses them
    /// on this thread). Refuses a frame whose bytes do not fit its size.
    pub fn send(&self, frame: &OutFrame<'_>) -> Result<(), String> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let min_row = match frame.fourcc {
            fourcc::UYVY | fourcc::UYVA => w * 2,
            fourcc::BGRA | fourcc::BGRX | fourcc::RGBA | fourcc::RGBX => w * 4,
            other => return Err(format!("cannot send {}", fourcc::name(other))),
        };
        // The last row needs only its pixels, not the whole stride; UYVA's alpha plane follows
        // the full rows.
        let need = if frame.fourcc == fourcc::UYVA {
            frame.stride * h + w * h
        } else {
            frame.stride * (h.max(1) - 1) + min_row
        };
        if w == 0
            || h == 0
            || frame.width > MAX_SIDE as u32
            || frame.height > MAX_SIDE as u32
            || frame.stride < min_row
            || frame.data.len() < need
            || frame.rate.0 == 0
            || frame.rate.1 == 0
        {
            return Err(format!("a {}x{} frame that does not add up", frame.width, frame.height));
        }
        let raw = RawVideo {
            xres: w as c_int,
            yres: h as c_int,
            fourcc: frame.fourcc,
            frame_rate_n: frame.rate.0.min(i32::MAX as u32) as c_int,
            frame_rate_d: frame.rate.1.min(i32::MAX as u32) as c_int,
            aspect: 0.0,
            frame_format: FORMAT_PROGRESSIVE,
            timecode: TIMECODE_SYNTHESIZE,
            // The runtime only reads it.
            data: frame.data.as_ptr().cast_mut(),
            stride: c_int::try_from(frame.stride).map_err(|_| "a row too long to send".to_string())?,
            metadata: std::ptr::null(),
            timestamp: 0,
        };
        // SAFETY: `handle` is live; `raw` describes `frame.data`, checked above to hold the rows
        // the header expects; the synchronous call is done with the bytes when it returns.
        unsafe { (self.ndi.send_video)(self.handle, &raw const raw) };
        Ok(())
    }

    /// How many receivers are connected now.
    pub fn connections(&self) -> usize {
        // SAFETY: `handle` is live; timeout 0 does not wait.
        let n = unsafe { (self.ndi.send_connections)(self.handle, 0) };
        n.max(0) as usize
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        // SAFETY: `handle` came from send_create; no send is running (sending takes `&self`, and
        // dropping takes the value).
        unsafe { (self.ndi.send_destroy)(self.handle) }
    }
}

const LIBRARY: &str = if cfg!(windows) {
    "Processing.NDI.Lib.x64.dll"
} else if cfg!(target_os = "macos") {
    "libndi.dylib"
} else {
    "libndi.so.6"
};

/// The folders the runtime may be in: the one the installer names first.
fn candidates() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("NDI_RUNTIME_DIR_V6").filter(|d| !d.is_empty()) {
        dirs.push(std::path::PathBuf::from(d));
    }
    if cfg!(windows) {
        // Where NDI's installers put it, for a process started before the variable was set.
        let program_files = std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
        let base = std::path::Path::new(&program_files).join("NDI");
        dirs.push(base.join("NDI 6 Runtime").join("v6"));
        dirs.push(base.join("NDI 6 Tools").join("Runtime"));
    }
    dirs.into_iter().map(|d| d.join(LIBRARY)).filter(|p| p.is_file()).collect()
}

#[cfg(windows)]
fn load_library() -> Result<(libloading::Library, String), String> {
    use libloading::os::windows::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, Library,
    };
    let mut errors = Vec::new();
    for path in candidates() {
        // SAFETY: loading runs the library's initialisers; the path is NDI's installation (or
        // the folder its installer named), and its own folder is searched for what it needs.
        let lib = unsafe {
            Library::load_with_flags(
                &path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        };
        match lib {
            Ok(lib) => return Ok((lib.into(), path.display().to_string())),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    Err(missing(&errors))
}

#[cfg(unix)]
fn load_library() -> Result<(libloading::Library, String), String> {
    let mut errors = Vec::new();
    let mut tries: Vec<std::ffi::OsString> = candidates().into_iter().map(Into::into).collect();
    tries.push(LIBRARY.into());
    for path in tries {
        // SAFETY: loading runs the library's initialisers; it is NDI's runtime, from the folder
        // its installer names or the system's library path.
        match unsafe { libloading::Library::new(&path) } {
            Ok(lib) => return Ok((lib, path.to_string_lossy().into_owned())),
            Err(e) => errors.push(format!("{}: {e}", path.to_string_lossy())),
        }
    }
    Err(missing(&errors))
}

#[cfg(not(any(windows, unix)))]
fn load_library() -> Result<(libloading::Library, String), String> {
    Err("NDI is not available on this system".into())
}

fn missing(errors: &[String]) -> String {
    if errors.is_empty() {
        format!("the NDI runtime is not installed: get it from {RUNTIME_URL} (or install NDI Tools)")
    } else {
        format!("the NDI runtime did not load ({})", errors.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_the_headers() {
        // 64-bit layouts as the C compiler lays out the header's structs.
        assert_eq!(std::mem::size_of::<RawSource>(), 16);
        assert_eq!(std::mem::size_of::<FindCreate>(), 24);
        assert_eq!(std::mem::size_of::<RecvCreate>(), 40);
        assert_eq!(std::mem::size_of::<SendCreate>(), 24);
        assert_eq!(std::mem::size_of::<RawVideo>(), 72);
        assert_eq!(std::mem::offset_of!(RawVideo, data), 40);
        assert_eq!(std::mem::offset_of!(RawVideo, timestamp), 64);
        assert_eq!(std::mem::size_of::<RawAudio>(), 64);
        assert_eq!(std::mem::offset_of!(RawAudio, data), 32);
        assert_eq!(std::mem::size_of::<RawMetadata>(), 24);
    }

    #[test]
    fn fourccs_read_as_text() {
        assert_eq!(fourcc::name(fourcc::UYVY), "UYVY");
        assert_eq!(fourcc::name(fourcc::FLTP), "FLTp");
        assert_eq!(fourcc::UYVY, 0x5956_5955);
    }
}
