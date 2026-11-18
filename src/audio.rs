use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::core::{Error, Result};

use crate::devices::{self, Flow};

use crate::encoder::{AudioChunk, CHANNELS, SAMPLE_RATE};
use crate::recorder::Clock;

const LAG_FRAMES: i64 = SAMPLE_RATE as i64 * 50 / 1000;
const MIN_CHUNK_FRAMES: i64 = SAMPLE_RATE as i64 / 100;
const MAX_QUEUE_SAMPLES: usize = (SAMPLE_RATE * CHANNELS) as usize * 150 / 1000;
const KEEP_QUEUE_SAMPLES: usize = (SAMPLE_RATE * CHANNELS) as usize * 50 / 1000;
const BUFFER_DURATION: i64 = 2_000_000;

struct Source {
    client:  IAudioClient,
    capture: IAudioCaptureClient,
    queue:   VecDeque<f32>,
}

impl Source {
    fn open(device: &IMMDevice, loopback: bool) -> Result<Self> {
        unsafe {
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let block = (CHANNELS * 4) as u16;
            let format = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
                nChannels: CHANNELS as u16,
                nSamplesPerSec: SAMPLE_RATE,
                nAvgBytesPerSec: SAMPLE_RATE * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

            if loopback {
                flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
            }

            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_DURATION, 0, &format, None)?;

            let capture = client.GetService()?;

            Ok(Self { client, capture, queue: VecDeque::new() })
        }
    }

    fn read(&mut self) -> Result<()> {
        unsafe {
            while self.capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;

                self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;

                let samples = (frames * CHANNELS) as usize;

                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    self.queue.extend(std::iter::repeat_n(0.0, samples));
                } else {
                    self.queue.extend(std::slice::from_raw_parts(data as *const f32, samples));
                }

                self.capture.ReleaseBuffer(frames)?;
            }
        }

        if self.queue.len() > MAX_QUEUE_SAMPLES {
            let excess = self.queue.len() - KEEP_QUEUE_SAMPLES;

            self.queue.drain(..excess - excess % CHANNELS as usize);
        }

        Ok(())
    }
}

pub struct AudioCapture {
    sources:         Vec<Source>,
    pub missing_mic: bool,
}

unsafe impl Send for AudioCapture {}

impl AudioCapture {
    pub fn open(system: Option<Option<String>>, microphone: Option<Option<String>>) -> Result<Self> {
        let mut sources = Vec::new();
        let mut missing_mic = false;

        if let Some(id) = system {
            let device = devices::open(Flow::Output, id.as_deref())?.ok_or_else(|| Error::new(E_FAIL, "No audio output device found for system audio"))?;

            sources.push(Source::open(&device, true)?);
        }

        if let Some(id) = microphone {
            match devices::open(Flow::Input, id.as_deref())? {
                Some(device) => sources.push(Source::open(&device, false)?),
                None => missing_mic = true,
            }
        }

        Ok(Self { sources, missing_mic })
    }

    pub fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    pub fn run(mut self, mut deliver: impl FnMut(AudioChunk) -> Result<()>, clock: &Clock, stop: &AtomicBool) -> Result<()> {
        for source in &self.sources {
            unsafe { source.client.Start()? };
        }

        let result = self.mix_loop(&mut deliver, clock, stop);

        for source in &self.sources {
            unsafe { source.client.Stop()? };
        }

        result
    }

    fn mix_loop(&mut self, deliver: &mut impl FnMut(AudioChunk) -> Result<()>, clock: &Clock, stop: &AtomicBool) -> Result<()> {
        let mut written: i64 = 0;
        let mut pcm = Vec::new();

        loop {
            let stopping = stop.load(Ordering::Relaxed);

            for source in &mut self.sources {
                source.read()?;
            }

            let lag    = if stopping { 0 } else { LAG_FRAMES };
            let target = clock.elapsed()? * SAMPLE_RATE as i64 / 10_000_000 - lag;
            let frames = target - written;

            if frames >= MIN_CHUNK_FRAMES || (stopping && frames > 0) {
                self.mix(frames as usize * CHANNELS as usize, &mut pcm);
                deliver(AudioChunk { pcm: pcm.as_slice().into(), time: frame_time(written), duration: frame_time(written + frames) - frame_time(written) })?;
                written += frames;
            }

            if stopping {
                return Ok(());
            }

            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn mix(&mut self, samples: usize, pcm: &mut Vec<i16>) {
        pcm.clear();
        pcm.resize(samples, 0);

        let mut mixed = vec![0.0f32; samples];

        for source in &mut self.sources {
            let available = samples.min(source.queue.len());

            for (slot, value) in mixed.iter_mut().zip(source.queue.drain(..available)) {
                *slot += value;
            }
        }

        for (out, value) in pcm.iter_mut().zip(mixed) {
            *out = (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
    }
}

fn frame_time(frames: i64) -> i64 {
    frames * 10_000_000 / SAMPLE_RATE as i64
}
