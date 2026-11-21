use std::f32::consts::TAU;

use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
use windows::core::PCWSTR;

const SAMPLE_RATE: u32 = 44100;
const LEAD_IN_SECONDS: f32 = 0.2;
const NOTE_SECONDS: f32 = 0.085;
const LAST_NOTE_SECONDS: f32 = 0.2;
const ATTACK_SECONDS: f32 = 0.004;
const DECAY_PER_SECOND: f32 = 18.0;
const VOLUME: f32 = 0.22;

#[derive(Clone, Copy)]
pub enum Cue {
    RecordStart,
    RecordStop,
    ReplayOn,
    ReplayOff,
}

impl Cue {
    fn notes(self) -> &'static [f32] {
        match self {
            Cue::RecordStart => &[587.33, 880.0],
            Cue::RecordStop => &[880.0, 587.33],
            Cue::ReplayOn => &[1046.5, 1318.51, 1567.98],
            Cue::ReplayOff => &[1567.98, 1318.51, 1046.5],
        }
    }
}

pub fn play(cue: Cue) {
    let wave = wave(cue.notes());

    _ = unsafe { PlaySoundW(PCWSTR(wave.as_ptr().cast()), None, SND_MEMORY | SND_SYNC | SND_NODEFAULT) };
}

fn wave(notes: &[f32]) -> Vec<u8> {
    let lead_in = (LEAD_IN_SECONDS * SAMPLE_RATE as f32) as usize;
    let tones = notes.iter().enumerate().flat_map(|(index, &frequency)| {
        let seconds = if index + 1 == notes.len() { LAST_NOTE_SECONDS } else { NOTE_SECONDS };
        let count = (seconds * SAMPLE_RATE as f32) as u32;

        (0..count).map(move |sample| {
            let time = sample as f32 / SAMPLE_RATE as f32;
            let envelope = (time / ATTACK_SECONDS).min(1.0) * (-time * DECAY_PER_SECOND).exp() * (1.0 - sample as f32 / count as f32);

            ((TAU * frequency * time).sin() * envelope * VOLUME * i16::MAX as f32) as i16
        })
    });
    let samples: Vec<i16> = std::iter::repeat_n(0, lead_in).chain(tones).collect();
    let data = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data as usize);

    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    samples.iter().for_each(|sample| bytes.extend_from_slice(&sample.to_le_bytes()));
    bytes
}
