//! Media Foundation: listing cameras and reading one through a source reader, synchronously
//! on the capture thread. The FFI module of this crate: every `unsafe` lives here.
//!
//! The source reader runs with converters disabled (frames come as the device packs them, so a
//! mode means what the device does) and in low-latency mode (no frames held back for smoothing).

#![allow(unsafe_code)]

use std::ffi::c_void;

use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer2, IMFActivate, IMFAttributes, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_E_NO_MORE_TYPES, MF_LOW_LATENCY, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_READWRITE_DISABLE_CONVERTERS,
    MF_SOURCE_READER_ALL_STREAMS, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READERF_ERROR,
    MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED, MF_VERSION, MF2DBuffer_LockFlags_Read, MFCreateAttributes,
    MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFGetSystemTime, MFNominalRange_0_255,
    MFSTARTUP_LITE, MFShutdown, MFStartup, MFVideoFormat_I420, MFVideoFormat_IYUV, MFVideoFormat_NV12,
    MFVideoFormat_UYVY, MFVideoFormat_YUY2, MFVideoTransferMatrix_BT601, MFVideoTransferMatrix_BT709,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{GUID, Interface, PWSTR};

use crate::frame::{Matrix, PixelFormat, Planes, rearrange};
use crate::mode::{DeviceInfo, Mode, Wanted, choose, sort};

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const ALL: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;
/// Most modes read from one device (a hostile driver could list without end).
const MAX_MODES: u32 = 1000;

fn text(e: windows::core::Error) -> String {
    format!("{} (0x{:08X})", e.message().trim(), e.code().0)
}

/// COM and Media Foundation for the current thread, released when dropped.
struct Runtime {
    com: bool,
}

impl Runtime {
    fn start() -> Result<Self, String> {
        // SAFETY: plain initialisation calls, paired in `drop`.
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
                if com {
                    CoUninitialize();
                }
                return Err(format!("Media Foundation does not start: {}", text(e)));
            }
            Ok(Self { com })
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: pairs the calls in `start` on the same thread.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// A device from the enumeration.
struct Found {
    activate: IMFActivate,
    name: String,
    id: String,
}

fn string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: the out pointers are valid; the string is freed with CoTaskMemFree as documented.
    unsafe {
        attrs.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn enumerate() -> Result<Vec<Found>, String> {
    // SAFETY: Media Foundation calls with valid pointers; the array is freed after its entries
    // were taken out (each `Option` moved out leaves `None`, so nothing is released twice).
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 1).map_err(text)?;
        let attrs = attrs.ok_or("no attribute store")?;
        attrs
            .SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)
            .map_err(text)?;
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut array, &mut count)
            .map_err(|e| format!("cannot list cameras: {}", text(e)))?;
        let mut out = Vec::new();
        if !array.is_null() {
            let entries = std::slice::from_raw_parts_mut(array, count as usize);
            for slot in entries.iter_mut() {
                if let Some(activate) = slot.take() {
                    let name = string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
                    let id = string(&activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)
                        .unwrap_or_default();
                    out.push(Found { activate, name, id });
                }
            }
            CoTaskMemFree(Some(array as *const c_void));
        }
        Ok(out)
    }
}

fn format_of(subtype: &GUID) -> PixelFormat {
    match *subtype {
        s if s == MFVideoFormat_NV12 => PixelFormat::Nv12,
        s if s == MFVideoFormat_YUY2 => PixelFormat::Yuy2,
        s if s == MFVideoFormat_UYVY => PixelFormat::Uyvy,
        s if s == MFVideoFormat_I420 || s == MFVideoFormat_IYUV => PixelFormat::I420,
        _ => PixelFormat::Other,
    }
}

fn split(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

fn mode_of(t: &IMFMediaType, index: u32) -> Option<Mode> {
    // SAFETY: attribute reads with valid keys.
    unsafe {
        let subtype = t.GetGUID(&MF_MT_SUBTYPE).ok()?;
        let (width, height) = split(t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?);
        let rate = t.GetUINT64(&MF_MT_FRAME_RATE).map(split).unwrap_or((0, 1));
        Some(Mode { width, height, rate, format: format_of(&subtype), index })
    }
}

fn native_types(reader: &IMFSourceReader) -> Vec<(Mode, IMFMediaType)> {
    let mut out = Vec::new();
    for i in 0..MAX_MODES {
        // SAFETY: a plain query; it fails with MF_E_NO_MORE_TYPES after the last.
        match unsafe { reader.GetNativeMediaType(VIDEO, i) } {
            Ok(t) => {
                if let Some(m) = mode_of(&t, i) {
                    out.push((m, t));
                }
            }
            Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
            Err(_) => break,
        }
    }
    out
}

/// Opens the device behind `activate` and a source reader on it.
fn reader(activate: &IMFActivate) -> Result<(IMFMediaSource, IMFSourceReader), String> {
    // SAFETY: Media Foundation calls with valid arguments.
    unsafe {
        let source: IMFMediaSource =
            activate.ActivateObject().map_err(|e| format!("cannot open the camera: {}", text(e)))?;
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).map_err(text)?;
        let attrs = attrs.ok_or("no attribute store")?;
        attrs.SetUINT32(&MF_LOW_LATENCY, 1).map_err(text)?;
        attrs.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1).map_err(text)?;
        match MFCreateSourceReaderFromMediaSource(&source, &attrs) {
            Ok(r) => Ok((source, r)),
            Err(e) => {
                let _ = source.Shutdown();
                let _ = activate.ShutdownObject();
                Err(format!("cannot read the camera: {}", text(e)))
            }
        }
    }
}

/// The cameras of this machine with their modes. Runs Media Foundation on a thread of its own,
/// so the caller's COM apartment does not matter.
pub(crate) fn devices() -> Result<Vec<DeviceInfo>, String> {
    let worker = std::thread::Builder::new().name("camera list".into()).spawn(|| {
        let _runtime = Runtime::start()?;
        let mut list = Vec::new();
        for f in enumerate()? {
            let mut info = DeviceInfo { name: f.name.clone(), id: f.id.clone(), ..DeviceInfo::default() };
            match reader(&f.activate) {
                Ok((source, reader)) => {
                    let mut modes: Vec<Mode> = native_types(&reader).into_iter().map(|(m, _)| m).collect();
                    sort(&mut modes);
                    modes.dedup_by(|a, b| a.label() == b.label());
                    info.modes = modes.iter().map(Mode::info).collect();
                    drop(reader);
                    // SAFETY: releases the device opened above.
                    unsafe {
                        let _ = source.Shutdown();
                        let _ = f.activate.ShutdownObject();
                    }
                }
                Err(e) => info.error = Some(e),
            }
            list.push(info);
        }
        Ok(list)
    });
    match worker {
        Ok(t) => t.join().unwrap_or_else(|_| Err("listing cameras panicked".into())),
        Err(e) => Err(format!("cannot start a thread: {e}")),
    }
}

/// What [`Capture::open`] opened.
pub(crate) struct Opened {
    pub device: String,
    pub mode: Mode,
    pub modes: Vec<Mode>,
}

/// One frame as read.
pub(crate) struct Sample {
    pub planes: Planes,
    pub matrix: Matrix,
    pub full_range: bool,
    /// The device's timestamp, 100 ns units.
    pub stamp: Option<i64>,
    pub delay_ms: Option<f64>,
    /// Milliseconds spent taking the frame out of the camera's buffer.
    pub copy_ms: f64,
}

/// An open camera. Fields drop in order: the reader before the device, the runtime last.
pub(crate) struct Capture {
    reader: IMFSourceReader,
    source: IMFMediaSource,
    activate: IMFActivate,
    mode: Mode,
    stride: usize,
    matrix: Matrix,
    full_range: bool,
    _runtime: Runtime,
}

impl Capture {
    /// Opens the camera `pattern` names (empty: the first; else an exact name, else the first
    /// containing it, case ignored) in the mode `wanted` picks.
    pub fn open(pattern: &str, wanted: &Wanted) -> Result<(Self, Opened), String> {
        let runtime = Runtime::start()?;
        let found = enumerate()?;
        if found.is_empty() {
            return Err("this machine has no camera".into());
        }
        let lower = pattern.to_lowercase();
        let pick = if pattern.is_empty() {
            Some(0)
        } else {
            found
                .iter()
                .position(|f| f.name.eq_ignore_ascii_case(pattern))
                .or_else(|| found.iter().position(|f| f.name.to_lowercase().contains(&lower)))
        };
        let Some(f) = pick.and_then(|i| found.into_iter().nth(i)) else {
            let names: Vec<String> = enumerate()?.into_iter().map(|f| f.name).collect();
            return Err(format!("no camera matches `{pattern}`; cameras: {}", names.join(", ")));
        };
        let (source, reader) = reader(&f.activate)?;
        let close = |e: String| {
            // SAFETY: releases the device opened above.
            unsafe {
                let _ = source.Shutdown();
                let _ = f.activate.ShutdownObject();
            }
            e
        };
        let types = native_types(&reader);
        let mut modes: Vec<Mode> = types.iter().map(|(m, _)| *m).collect();
        let mode = choose(&modes, wanted).map_err(close)?;
        let Some((_, t)) = types.iter().find(|(m, _)| m.index == mode.index) else {
            return Err(close("the mode list changed".into()));
        };
        // SAFETY: Media Foundation calls with valid arguments.
        let (stride, matrix, full_range) = unsafe {
            reader.SetStreamSelection(ALL, false).map_err(|e| close(text(e)))?;
            reader.SetStreamSelection(VIDEO, true).map_err(|e| close(text(e)))?;
            reader
                .SetCurrentMediaType(VIDEO, None, t)
                .map_err(|e| close(format!("the camera refuses {}: {}", mode.label(), text(e))))?;
            // Stored as a UINT32 holding a signed value; negative means bottom-up rows.
            let stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(0);
            let matrix = match t.GetUINT32(&MF_MT_YUV_MATRIX) {
                Ok(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => Matrix::Bt601,
                Ok(m) if m == MFVideoTransferMatrix_BT709.0 as u32 => Matrix::Bt709,
                // Unstated: HD sizes are BT.709, SD ones BT.601.
                _ if mode.height >= 720 => Matrix::Bt709,
                _ => Matrix::Bt601,
            };
            let full =
                t.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE).is_ok_and(|r| r == MFNominalRange_0_255.0 as u32);
            (stride, matrix, full)
        };
        if stride < 0 {
            return Err(close("cannot read this camera: it delivers bottom-up rows".into()));
        }
        let stride = usize::try_from(stride).unwrap_or(0).max(mode.format.min_stride(mode.width));
        sort(&mut modes);
        modes.dedup_by(|a, b| a.label() == b.label());
        let opened = Opened { device: f.name.clone(), mode, modes };
        Ok((
            Self {
                reader,
                source,
                activate: f.activate,
                mode,
                stride,
                matrix,
                full_range,
                _runtime: runtime,
            },
            opened,
        ))
    }

    /// The next frame; blocks until the camera delivers one. `None` for a gap the camera
    /// reported without a frame.
    pub fn read(&mut self) -> Result<Option<Sample>, String> {
        let mut flags = 0u32;
        let mut stamp = 0i64;
        let mut sample: Option<IMFSample> = None;
        // SAFETY: valid out pointers; the call blocks until a sample, an error or a gap.
        let now = unsafe {
            self.reader
                .ReadSample(VIDEO, 0, None, Some(&mut flags), Some(&mut stamp), Some(&mut sample))
                .map_err(|e| format!("reading the camera failed: {}", text(e)))?;
            MFGetSystemTime()
        };
        let has = |f: i32| flags & f as u32 != 0;
        if has(MF_SOURCE_READERF_ERROR.0) {
            return Err("the camera reported an error".into());
        }
        if has(MF_SOURCE_READERF_ENDOFSTREAM.0) {
            return Err("the camera ended the stream".into());
        }
        if has(MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0) || has(MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED.0)
        {
            return Err("the camera changed its format".into());
        }
        let Some(sample) = sample else { return Ok(None) };
        let started = std::time::Instant::now();
        let planes = self.planes(&sample)?;
        let delay = (now - stamp) as f64 / 10_000.0;
        Ok(Some(Sample {
            planes,
            matrix: self.matrix,
            full_range: self.full_range,
            stamp: Some(stamp),
            delay_ms: (0.0..10_000.0).contains(&delay).then_some(delay),
            copy_ms: started.elapsed().as_secs_f64() * 1000.0,
        }))
    }

    fn planes(&self, sample: &IMFSample) -> Result<Planes, String> {
        let m = self.mode;
        // SAFETY: the buffer is locked while its bytes are read and unlocked afterwards; the
        // slice is built from the length the buffer reports and never outlives the lock.
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer().map_err(text)?;
            // A 2D buffer tells its own pitch, which may differ from the media type's.
            if let Ok(b2) = buffer.cast::<IMF2DBuffer2>() {
                let mut line: *mut u8 = std::ptr::null_mut();
                let mut pitch = 0i32;
                let mut start: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                if b2
                    .Lock2DSize(MF2DBuffer_LockFlags_Read, &mut line, &mut pitch, &mut start, &mut len)
                    .is_ok()
                {
                    // The bytes from the first row to the end of the buffer.
                    let offset = (line as usize).wrapping_sub(start as usize);
                    let result = if line.is_null() || start.is_null() || pitch <= 0 || offset > len as usize {
                        Err("the camera's buffer has no usable rows".to_string())
                    } else {
                        let data = std::slice::from_raw_parts(line, len as usize - offset);
                        rearrange(m.format, m.width, m.height, pitch as usize, data)
                    };
                    let _ = b2.Unlock2D();
                    return result;
                }
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut current = 0u32;
            buffer.Lock(&mut data, None, Some(&mut current)).map_err(text)?;
            let result = if data.is_null() {
                Err("the camera's buffer is empty".to_string())
            } else {
                let bytes = std::slice::from_raw_parts(data, current as usize);
                rearrange(m.format, m.width, m.height, self.stride, bytes)
            };
            let _ = buffer.Unlock();
            result
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: releases the device; the reader is released with the fields afterwards.
        unsafe {
            let _ = self.source.Shutdown();
            let _ = self.activate.ShutdownObject();
        }
    }
}
