use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Error, Result};

use crate::audio::AudioCapture;
use crate::encoder::{AudioChunk, FramePool, Muxer, Packet, StreamType, VideoEncoder};
use crate::replay::{Clip, Ring};
use crate::settings::Settings;
use crate::video::{Movable, PREVIEW_FPS, Source, VideoCapture};

pub const WM_SESSION_FAILED: u32 = WM_APP + 2;
pub const WM_PREVIEW_FAILED: u32 = WM_APP + 3;

const FRAME_QUEUE: usize = 4;
const FRAME_POOL_LIMIT: u32 = 16;

pub struct Clock {
    start:     i64,
    frequency: i64,
}

impl Clock {
    fn now() -> Result<Self> {
        let mut frequency = 0;
        let mut start = 0;

        unsafe {
            QueryPerformanceFrequency(&mut frequency)?;
            QueryPerformanceCounter(&mut start)?;
        }

        Ok(Self { start, frequency })
    }

    pub fn elapsed(&self) -> Result<i64> {
        let mut now = 0;

        unsafe { QueryPerformanceCounter(&mut now)? };
        Ok(((now - self.start) as i128 * 10_000_000 / self.frequency as i128) as i64)
    }
}

pub struct Target {
    pub window:       HWND,
    pub preview:      HWND,
    pub show_preview: Arc<AtomicBool>,
}

enum Recording {
    Idle,
    Waiting(PathBuf),
    Writing(Muxer),
}

struct Outputs {
    audio:     bool,
    recording: Mutex<Recording>,
    ring:      Mutex<Option<Ring>>,
    stream:    Mutex<Option<StreamType>>,
    keyframe:  AtomicBool,
}

impl Outputs {
    fn video(&self, stream: &StreamType, packet: Packet) -> Result<()> {
        lock(&self.stream)?.get_or_insert_with(|| stream.clone());

        {
            let mut recording = lock(&self.recording)?;

            if let Recording::Waiting(path) = &*recording
                && packet.keyframe
            {
                *recording = Recording::Writing(Muxer::new(path, stream, self.audio, packet.time)?);
            }

            if let Recording::Writing(muxer) = &*recording {
                muxer.video(&packet)?;
            }
        }

        if let Some(ring) = lock(&self.ring)?.as_mut() {
            ring.push_video(packet);
        }

        Ok(())
    }

    fn audio(&self, chunk: AudioChunk) -> Result<()> {
        if let Recording::Writing(muxer) = &*lock(&self.recording)? {
            muxer.audio(&chunk)?;
        }

        if let Some(ring) = lock(&self.ring)?.as_mut() {
            ring.push_audio(chunk);
        }

        Ok(())
    }
}

pub struct Session {
    stop:            Arc<AtomicBool>,
    threads:         Vec<(&'static str, JoinHandle<Result<()>>)>,
    outputs:         Arc<Outputs>,
    pub missing_mic: bool,
    pub size:        (u32, u32),
}

impl Session {
    pub fn start(source: &Source, settings: &Settings, target: &Target, replay: Option<u32>) -> Result<Self> {
        let video = VideoCapture::open(source, settings.output, target.preview, settings.cursor)?;
        let (width, height) = video.size();
        let device = Movable(video.device().clone());
        let (fps, bitrate) = (settings.fps, settings.bitrate as u32);
        let (system, microphone) = (settings.system_source(), settings.microphone_source());
        let (audio, encoder, pool) = in_mta(move || {
            let audio = AudioCapture::open(system, microphone)?;
            let encoder = VideoEncoder::new(&device.0, width, height, fps, bitrate)?;
            let pool = FramePool::new(&device.0, width, height, FRAME_QUEUE as u32, FRAME_POOL_LIMIT)?;

            Ok((audio, encoder, pool))
        })?;
        let outputs = Arc::new(Outputs {
            audio: audio.has_sources(),
            recording: Mutex::new(Recording::Idle),
            ring: Mutex::new(replay.map(Ring::new)),
            stream: Mutex::new(None),
            keyframe: AtomicBool::new(false),
        });
        let missing_mic = audio.missing_mic;
        let stop = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(Clock::now()?);
        let (frames, encoder_input) = sync_channel(FRAME_QUEUE);
        let mut threads = Vec::new();

        threads.push(("Video", {
            let (clock, stop, show_preview) = (clock.clone(), stop.clone(), target.show_preview.clone());

            spawn(target.window, WM_SESSION_FAILED, stop.clone(), move || video.run(fps, Some((frames, pool)), &clock, &stop, &show_preview))
        }));
        threads.push(("Encoder", {
            let outputs = outputs.clone();

            spawn(target.window, WM_SESSION_FAILED, stop.clone(), move || encoder.run(encoder_input, &outputs.keyframe, |stream, packet| outputs.video(stream, packet)))
        }));

        if audio.has_sources() {
            let (outputs, audio_stop) = (outputs.clone(), stop.clone());

            threads.push(("Audio", spawn(target.window, WM_SESSION_FAILED, stop.clone(), move || audio.run(|chunk| outputs.audio(chunk), &clock, &audio_stop))));
        }

        Ok(Self { stop, threads, outputs, missing_mic, size: (width, height) })
    }

    pub fn begin_recording(&self, path: PathBuf) -> Result<()> {
        *lock(&self.outputs.recording)? = Recording::Waiting(path);
        self.outputs.keyframe.store(true, Ordering::Relaxed);
        Ok(())
    }

    pub fn end_recording(&self) -> Result<Option<PathBuf>> {
        let recording = std::mem::replace(&mut *lock(&self.outputs.recording)?, Recording::Idle);

        match recording {
            Recording::Writing(muxer) => in_mta(move || muxer.finish()).map(Some),
            Recording::Waiting(_) | Recording::Idle => Ok(None),
        }
    }

    pub fn set_replay(&self, seconds: Option<u32>) -> Result<()> {
        *lock(&self.outputs.ring)? = seconds.map(Ring::new);
        Ok(())
    }

    pub fn replay_bytes(&self) -> Result<Option<usize>> {
        Ok(lock(&self.outputs.ring)?.as_ref().map(Ring::bytes))
    }

    pub fn save_clip(&self, window: HWND, message: u32, path: PathBuf) -> Result<JoinHandle<Result<PathBuf>>> {
        let clip: Clip = lock(&self.outputs.ring)?.as_ref().ok_or_else(|| Error::new(E_FAIL, "The replay buffer is off"))?.snapshot();
        let stream = lock(&self.outputs.stream)?.clone().ok_or_else(|| Error::new(E_FAIL, "The replay buffer is still empty. Wait a moment and try again."))?;
        let audio = self.outputs.audio;
        let window = window.0 as isize;

        Ok(std::thread::spawn(move || {
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().and_then(|_| clip.save(&path, &stream, audio));

            unsafe { PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(0), LPARAM(0))? };
            result
        }))
    }

    pub fn stop(self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);

        let Self { threads, .. } = self;

        in_mta(move || {
            let results: Vec<Result<()>> = threads.into_iter().map(|(name, handle)| join(handle, name)).collect();

            results.into_iter().collect()
        })
    }
}

pub struct PreviewSession {
    stop:     Arc<AtomicBool>,
    handle:   JoinHandle<Result<()>>,
    pub size: (u32, u32),
}

impl PreviewSession {
    pub fn start(source: &Source, cursor: bool, target: &Target) -> Result<Self> {
        let capture = VideoCapture::open(source, None, target.preview, cursor)?;
        let size = capture.size();
        let clock = Clock::now()?;
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (stop, show_preview) = (stop.clone(), target.show_preview.clone());

            spawn(target.window, WM_PREVIEW_FAILED, stop.clone(), move || capture.run(PREVIEW_FPS, None, &clock, &stop, &show_preview))
        };

        Ok(Self { stop, handle, size })
    }

    pub fn stop(self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        join(self.handle, "Preview")
    }
}

pub fn recording_path(folder: &Path, prefix: &str) -> PathBuf {
    let t = unsafe { GetLocalTime() };

    folder.join(format!("{prefix}{:04}-{:02}-{:02} {:02}-{:02}-{:02}.mp4", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond))
}

pub fn in_mta<T: Send>(work: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
                work()
            })
            .join()
            .map_err(|_| Error::new(E_FAIL, "Worker thread panicked"))?
    })
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| Error::new(E_FAIL, "A capture thread crashed while holding shared state"))
}

fn spawn(window: HWND, failure: u32, stop: Arc<AtomicBool>, work: impl FnOnce() -> Result<()> + Send + 'static) -> JoinHandle<Result<()>> {
    let window = window.0 as isize;

    std::thread::spawn(move || {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().and_then(|_| work());

        if result.is_err() {
            stop.store(true, Ordering::Relaxed);
            unsafe { PostMessageW(Some(HWND(window as *mut _)), failure, WPARAM(0), LPARAM(0))? };
        }

        result
    })
}

fn join(handle: JoinHandle<Result<()>>, name: &str) -> Result<()> {
    handle.join().map_err(|_| Error::new(E_FAIL, format!("{name} thread panicked")))?
}
