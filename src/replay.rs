use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::E_FAIL;
use windows::core::{Error, Result};

use crate::encoder::{AudioChunk, Muxer, Packet, StreamType};

const TICKS_PER_SECOND: i64 = 10_000_000;

pub struct Ring {
    length: i64,
    video:  VecDeque<Packet>,
    audio:  VecDeque<AudioChunk>,
}

impl Ring {
    pub fn new(seconds: u32) -> Self {
        Self { length: seconds as i64 * TICKS_PER_SECOND, video: VecDeque::new(), audio: VecDeque::new() }
    }

    pub fn set_length(&mut self, seconds: u32) {
        self.length = seconds as i64 * TICKS_PER_SECOND;
    }

    pub fn push_video(&mut self, packet: Packet) {
        if self.video.is_empty() && !packet.keyframe {
            return;
        }

        let cutoff = packet.time - self.length;

        self.video.push_back(packet);

        while let Some(next) = self.video.iter().skip(1).position(|p| p.keyframe).map(|index| index + 1)
            && self.video[next].time <= cutoff
        {
            self.video.drain(..next);
        }

        self.trim_audio();
    }

    pub fn is_empty(&self) -> bool {
        self.video.is_empty()
    }

    pub fn push_audio(&mut self, chunk: AudioChunk) {
        self.audio.push_back(chunk);
        self.trim_audio();
    }

    pub fn snapshot(&self) -> Clip {
        Clip { video: self.video.iter().cloned().collect(), audio: self.audio.iter().cloned().collect() }
    }

    pub fn bytes(&self) -> usize {
        self.video.iter().map(|p| p.data.len()).sum::<usize>() + self.audio.iter().map(|c| c.pcm.len() * 2).sum::<usize>()
    }

    fn trim_audio(&mut self) {
        let Some(start) = self.video.front().map(|front| front.time) else {
            return;
        };

        while self.audio.front().is_some_and(|front| front.time < start) {
            self.audio.pop_front();
        }
    }
}

pub struct Clip {
    video: Vec<Packet>,
    audio: Vec<AudioChunk>,
}

impl Clip {
    pub fn save(self, path: &Path, stream: &StreamType, audio: bool) -> Result<PathBuf> {
        let first = self.video.first().ok_or_else(|| Error::new(E_FAIL, "Replay clip has no video"))?;

        if !first.keyframe {
            return Err(Error::new(E_FAIL, "Replay clip does not start on a keyframe"));
        }

        let muxer = Muxer::new(path, stream, audio, first.time)?;
        let mut video = self.video.iter().peekable();
        let mut sound = self.audio.iter().peekable();

        loop {
            match (video.peek(), sound.peek()) {
                (Some(packet), Some(chunk)) if chunk.time < packet.time => muxer.audio(sound.next().ok_or_else(missing)?)?,
                (Some(_), _) => muxer.video(video.next().ok_or_else(missing)?)?,
                (None, Some(_)) => muxer.audio(sound.next().ok_or_else(missing)?)?,
                (None, None) => break,
            }
        }

        muxer.finish()
    }
}

fn missing() -> Error {
    Error::new(E_FAIL, "Replay clip iterator ended unexpectedly")
}
