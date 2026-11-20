use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{E_ABORT, E_FAIL, E_INVALIDARG, HMODULE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Error, GUID, HSTRING, Result};

use crate::encoder::{RateControl, device_manager, encoder_parameters, video_type};
use crate::units::{format_bytes, format_time};

pub const WM_EXPORT_DONE: u32 = WM_APP + 31;
pub const WM_ESTIMATE_DONE: u32 = WM_APP + 33;

const PROGRESS_SCALE: u32 = 1000;
const SIZE_MARGIN: f64 = 0.96;
const TICKS: f64 = 10_000_000.0;
const SAMPLE_SHARE: f64 = 0.2;
const MIN_SAMPLE_SECONDS: f64 = 20.0;
const MAX_SAMPLE_SECONDS: f64 = 60.0;
const SAMPLE_COUNT: i64 = 12;
const MIN_FIT_BITRATE: u32 = 400_000;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Lossless,
    Exact,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Target {
    Quality(u32),
    Size(u64),
}

#[derive(Clone, Copy)]
pub enum Plan {
    Trim(Mode),
    Compress(Target),
}

pub struct Probe {
    pub seconds:       f64,
    pub video_bitrate: u32,
    pub audio_bitrate: u32,
}

impl Probe {
    pub fn sample_segments(&self) -> Vec<(i64, i64)> {
        let total = (self.seconds * TICKS) as i64;
        let slice = (self.sampled_seconds() / SAMPLE_COUNT as f64 * TICKS) as i64;

        if self.sampled_seconds() >= self.seconds {
            return vec![(0, total)];
        }

        (0..SAMPLE_COUNT).map(|i| total * (2 * i + 1) / (2 * SAMPLE_COUNT) - slice / 2).map(|start| (start, start + slice)).collect()
    }

    pub fn sampled_seconds(&self) -> f64 {
        (self.seconds * SAMPLE_SHARE).clamp(MIN_SAMPLE_SECONDS, MAX_SAMPLE_SECONDS).min(self.seconds)
    }

    pub fn fit_bitrate(&self, bytes: u64) -> Result<u32> {
        let budget = bytes as f64 * 8.0 * SIZE_MARGIN - self.audio_bitrate as f64 * self.seconds;
        let bitrate = budget / self.seconds;

        if bitrate < MIN_FIT_BITRATE as f64 {
            let smallest = ((MIN_FIT_BITRATE as f64 + self.audio_bitrate as f64) * self.seconds / 8.0 / SIZE_MARGIN).ceil() as u64;

            return Err(Error::new(E_INVALIDARG, format!("{} is too small for a {} video. The smallest size that still looks watchable is about {}.", format_bytes(bytes), format_time(self.seconds), format_bytes(smallest))));
        }

        Ok(bitrate as u32)
    }
}

pub struct Job {
    cancel:   Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
    handle:   JoinHandle<Result<()>>,
    output:   PathBuf,
}

impl Job {
    pub fn start(window: HWND, message: u32, source: PathBuf, output: PathBuf, segments: Vec<(i64, i64)>, plan: Plan) -> Job {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0));
        let handle = {
            let (cancel, progress, output) = (cancel.clone(), progress.clone(), output.clone());

            background(window, message, 0, move || {
                let mut task = Task { source: &source, output: &output, segments, cancel: &cancel, progress: &progress };

                match plan {
                    Plan::Trim(Mode::Lossless) => task.lossless(),
                    Plan::Trim(Mode::Exact) => task.transcode(None, RateControl::Source),
                    Plan::Compress(Target::Quality(quality)) => task.transcode(Some(MFVideoFormat_H264), RateControl::Quality(quality)),
                    Plan::Compress(Target::Size(bytes)) => task.fit(bytes),
                }
            })
        };

        Job { cancel, progress, handle, output }
    }

    pub fn progress(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / PROGRESS_SCALE as f32
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn finish(self) -> Result<PathBuf> {
        let result = self.handle.join().map_err(|_| Error::new(E_FAIL, "Export thread panicked"))?;

        match result {
            Ok(()) => Ok(self.output),
            Err(e) => {
                if self.output.exists() {
                    std::fs::remove_file(&self.output).map_err(|remove| Error::new(e.code(), format!("{}\n\nThe unfinished file could not be removed: {remove}", e.message())))?;
                }

                Err(e)
            }
        }
    }
}

pub fn is_cancelled(error: &Error) -> bool {
    error.code() == E_ABORT
}

pub struct Keyframes {
    reader: IMFSourceReader,
    video:  u32,
}

impl Keyframes {
    pub fn open(source: &Path) -> Result<Self> {
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), None)?;
            let streams = Streams::find(&reader)?;

            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(streams.video, true)?;
            Ok(Self { reader, video: streams.video })
        }
    }

    pub fn before(&self, time: i64) -> Result<i64> {
        seek(&self.reader, time)?;

        loop {
            let read = read_sample(&self.reader, self.video)?;

            if read.end_of_stream {
                return Err(Error::new(E_FAIL, format!("There is no video frame at {}", format_time(time as f64 / TICKS))));
            }

            let Some(sample) = read.sample else {
                continue;
            };

            if !unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.is_ok_and(|clean| clean != 0) {
                return Err(Error::new(E_FAIL, format!("Seeking to {} did not land on a keyframe", format_time(time as f64 / TICKS))));
            }

            return Ok(read.time);
        }
    }
}

pub fn probe(source: &Path) -> Result<Probe> {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(source.as_os_str()), None)?;
        let streams = Streams::find(&reader)?;
        let native = reader.GetNativeMediaType(streams.video, 0)?;
        let audio_bitrate = match streams.audio {
            Some(audio) => reader.GetNativeMediaType(audio, 0)?.GetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND)? * 8,
            None => 0,
        };

        Ok(Probe { seconds: duration(&reader)? as f64 / 10_000_000.0, video_bitrate: source_bitrate(&reader, &native)?, audio_bitrate })
    }
}

pub fn output_path(source: &Path, label: &str) -> Result<PathBuf> {
    let folder = source.parent().ok_or_else(|| Error::new(E_FAIL, format!("{} has no folder", source.display())))?;
    let stem = source.file_stem().ok_or_else(|| Error::new(E_FAIL, format!("{} has no file name", source.display())))?.to_string_lossy();

    (1..)
        .map(|n| match n {
            1 => folder.join(format!("{stem} ({label}).mp4")),
            n => folder.join(format!("{stem} ({label} {n}).mp4")),
        })
        .find(|path| !path.exists())
        .ok_or_else(|| Error::new(E_FAIL, "No free output file name"))
}

struct Task<'a> {
    source:   &'a Path,
    output:   &'a Path,
    segments: Vec<(i64, i64)>,
    cancel:   &'a AtomicBool,
    progress: &'a AtomicU32,
}

impl Task<'_> {
    fn lossless(&self) -> Result<()> {
        unsafe {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(self.source.as_os_str()), None)?;
            let streams = Streams::find(&reader)?;

            streams.select(&reader)?;

            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(self.output.as_os_str()), None, None)?;
            let video_out = writer.AddStream(&reader.GetNativeMediaType(streams.video, 0)?)?;
            let audio_out = streams.audio.map(|audio| writer.AddStream(&reader.GetNativeMediaType(audio, 0)?)).transpose()?;

            writer.BeginWriting()?;
            self.copy_segments(&reader, &writer, &streams, video_out, audio_out)?;
            writer.Finalize()
        }
    }

    fn fit(&mut self, bytes: u64) -> Result<()> {
        let mut probe = probe(self.source)?;

        if !self.segments.is_empty() {
            probe.seconds = self.segments.iter().map(|&(start, end)| end - start).sum::<i64>() as f64 / TICKS;
        }

        let bitrate = probe.fit_bitrate(bytes)?;

        self.transcode(Some(MFVideoFormat_H264), RateControl::Bitrate(bitrate))?;

        let first = file_size(self.output)?;

        if first <= bytes {
            return Ok(());
        }

        let corrected = (bitrate as f64 * bytes as f64 / first as f64 * SIZE_MARGIN) as u32;

        self.progress.store(0, Ordering::Relaxed);
        std::fs::remove_file(self.output).map_err(|e| Error::new(E_FAIL, format!("Cannot remove {}: {e}", self.output.display())))?;
        self.transcode(Some(MFVideoFormat_H264), RateControl::Bitrate(corrected))?;

        let second = file_size(self.output)?;

        if second > bytes {
            return Err(Error::new(E_FAIL, format!("The result was {}, still over {}. Try a slightly smaller target.", format_bytes(second), format_bytes(bytes))));
        }

        Ok(())
    }

    fn transcode(&mut self, codec: Option<GUID>, control: RateControl) -> Result<()> {
        unsafe {
            let manager = device_manager(&hardware_device()?)?;
            let reader_attributes = attributes(&[(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)], Some((&MF_SOURCE_READER_D3D_MANAGER, &manager)))?;
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(self.source.as_os_str()), &reader_attributes)?;
            let streams = Streams::find(&reader)?;

            streams.select(&reader)?;

            let native = reader.GetNativeMediaType(streams.video, 0)?;
            let source_subtype = native.GetGUID(&MF_MT_SUBTYPE)?;

            if source_subtype != MFVideoFormat_H264 {
                return Err(Error::new(E_FAIL, "Only H.264 videos can be re-encoded, because Windows 10 has no built-in decoder for other codecs. Use Lossless for this file."));
            }

            let subtype = codec.map_or(source_subtype, |codec| codec);

            if self.segments.is_empty() {
                self.segments = vec![(0, duration(&reader)?)];
            }

            let size = native.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let rate = native.GetUINT64(&MF_MT_FRAME_RATE)?;
            let (width, height) = ((size >> 32) as u32, size as u32);
            let frame_rate = ((rate >> 32) as u32, rate as u32);
            let bitrate = match control {
                RateControl::Bitrate(bitrate) => bitrate,
                RateControl::Source | RateControl::Quality(_) => source_bitrate(&reader, &native)?,
            };
            let decoded = MFCreateMediaType()?;

            decoded.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            decoded.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            reader.SetCurrentMediaType(streams.video, None, &decoded)?;

            let writer_attributes = attributes(&[(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)], Some((&MF_SINK_WRITER_D3D_MANAGER, &manager)))?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(self.output.as_os_str()), None, &writer_attributes)?;
            let encoded = video_type(&subtype, width, height, frame_rate)?;

            encoded.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;

            if subtype == MFVideoFormat_H264 {
                encoded.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            }

            let video_out = writer.AddStream(&encoded)?;

            writer.SetInputMediaType(video_out, &video_type(&MFVideoFormat_NV12, width, height, frame_rate)?, &encoder_parameters(frame_rate.0.div_ceil(frame_rate.1.max(1)), control)?)?;

            let audio_out = streams.audio.map(|audio| writer.AddStream(&reader.GetNativeMediaType(audio, 0)?)).transpose()?;

            writer.BeginWriting()?;
            self.copy_segments(&reader, &writer, &streams, video_out, audio_out)?;
            writer.Finalize()
        }
    }

    fn copy_segments(&self, reader: &IMFSourceReader, writer: &IMFSinkWriter, streams: &Streams, video_out: u32, audio_out: Option<u32>) -> Result<()> {
        let total: i64 = self.segments.iter().map(|&(start, end)| end - start).sum();
        let mut offset = 0i64;

        for &(start, end) in &self.segments {
            seek(reader, start)?;

            let mut video_done = false;
            let mut audio_done = streams.audio.is_none();

            while !(video_done && audio_done) {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(Error::new(E_ABORT, "Export cancelled"));
                }

                let read = read_sample(reader, MF_SOURCE_READER_ANY_STREAM.0 as u32)?;
                let is_video = read.stream == streams.video;
                let finished = read.end_of_stream || read.time >= end && read.sample.is_some();

                if finished {
                    match is_video {
                        true => video_done = true,
                        false => audio_done = true,
                    }

                    continue;
                }

                let Some(sample) = read.sample else {
                    continue;
                };

                if read.time < start || (is_video && video_done) || (!is_video && audio_done) {
                    continue;
                }

                let out = match is_video {
                    true => video_out,
                    false => audio_out.ok_or_else(|| Error::new(E_FAIL, "Audio sample without an audio stream"))?,
                };

                unsafe {
                    sample.SetSampleTime(offset + read.time - start)?;

                    if let Ok(decode) = sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) {
                        sample.SetUINT64(&MFSampleExtension_DecodeTimestamp, (offset + decode as i64 - start) as u64)?;
                    }

                    writer.WriteSample(out, &sample)?;
                }

                let done = offset + read.time - start;

                self.progress.store((done.clamp(0, total) as f64 / total as f64 * PROGRESS_SCALE as f64) as u32, Ordering::Relaxed);
            }

            offset += end - start;
        }

        Ok(())
    }
}

struct Streams {
    video: u32,
    audio: Option<u32>,
}

impl Streams {
    fn find(reader: &IMFSourceReader) -> Result<Self> {
        let mut video = None;
        let mut audio = None;

        for index in 0.. {
            let media = match unsafe { reader.GetNativeMediaType(index, 0) } {
                Ok(media) => media,
                Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => break,
                Err(e) => return Err(e),
            };
            let major = unsafe { media.GetMajorType()? };

            if major == MFMediaType_Video && video.is_none() {
                video = Some(index);
            }

            if major == MFMediaType_Audio && audio.is_none() {
                audio = Some(index);
            }
        }

        Ok(Self { video: video.ok_or_else(|| Error::new(E_FAIL, "This file has no video stream"))?, audio })
    }

    fn select(&self, reader: &IMFSourceReader) -> Result<()> {
        unsafe {
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(self.video, true)?;

            if let Some(audio) = self.audio {
                reader.SetStreamSelection(audio, true)?;
            }

            Ok(())
        }
    }
}

struct Read {
    stream:        u32,
    time:          i64,
    end_of_stream: bool,
    sample:        Option<IMFSample>,
}

fn read_sample(reader: &IMFSourceReader, stream: u32) -> Result<Read> {
    let mut actual = 0u32;
    let mut flags = 0u32;
    let mut time = 0i64;
    let mut sample = None;

    unsafe { reader.ReadSample(stream, 0, Some(&mut actual), Some(&mut flags), Some(&mut time), Some(&mut sample))? };

    Ok(Read { stream: actual, time, end_of_stream: flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0, sample })
}

fn seek(reader: &IMFSourceReader, time: i64) -> Result<()> {
    let position = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 { vt: VT_I8, wReserved1: 0, wReserved2: 0, wReserved3: 0, Anonymous: PROPVARIANT_0_0_0 { hVal: time } }),
        },
    };

    unsafe { reader.SetCurrentPosition(&GUID::zeroed(), &position) }
}

fn source_bitrate(reader: &IMFSourceReader, native: &IMFMediaType) -> Result<u32> {
    unsafe {
        if let Ok(bitrate) = native.GetUINT32(&MF_MT_AVG_BITRATE) {
            return Ok(bitrate);
        }

        let value = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_VIDEO_ENCODING_BITRATE)
            .map_err(|e| Error::new(e.code(), "The video bitrate of this file is unknown, so it cannot be re-encoded at the same quality. Use Lossless instead."))?;

        Ok(value.Anonymous.Anonymous.Anonymous.ulVal)
    }
}

fn file_size(path: &Path) -> Result<u64> {
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| Error::new(E_FAIL, format!("Cannot read {}: {e}", path.display())))
}

fn duration(reader: &IMFSourceReader) -> Result<i64> {
    unsafe {
        let value = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)?;

        Ok(value.Anonymous.Anonymous.Anonymous.uhVal as i64)
    }
}

pub fn hardware_device() -> Result<ID3D11Device> {
    unsafe {
        let mut device: Option<ID3D11Device> = None;

        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;

        device.ok_or_else(|| Error::new(E_FAIL, "D3D11CreateDevice returned no device"))
    }
}

fn attributes(values: &[(&GUID, u32)], unknown: Option<(&GUID, &IMFDXGIDeviceManager)>) -> Result<IMFAttributes> {
    unsafe {
        let mut attributes = None;

        MFCreateAttributes(&mut attributes, values.len() as u32 + 1)?;

        let attributes: IMFAttributes = attributes.ok_or_else(|| Error::new(E_FAIL, "MFCreateAttributes returned no attributes"))?;

        for (key, value) in values {
            attributes.SetUINT32(*key, *value)?;
        }

        if let Some((key, value)) = unknown {
            attributes.SetUnknown(key, value)?;
        }

        Ok(attributes)
    }
}

fn background<T: Send + 'static>(window: HWND, message: u32, id: usize, work: impl FnOnce() -> Result<T> + Send + 'static) -> JoinHandle<Result<T>> {
    let window = window.0 as isize;

    std::thread::spawn(move || {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().and_then(|_| work());

        unsafe { PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(id), LPARAM(0))? };
        result
    })
}
