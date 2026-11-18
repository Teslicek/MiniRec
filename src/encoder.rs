use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::core::{Error, GUID, HSTRING, Interface, Result};

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u32 = 2;
const AAC_BYTES_PER_SECOND: u32 = 24000;
const KEYFRAMES_PER_SECOND: u32 = 2;
const SLOWEST_BEST_QUALITY: u32 = 100;
const B_FRAMES: u32 = 2;
const PEAK_TO_MEAN: u32 = 2;

pub struct Frame(IMFSample);

unsafe impl Send for Frame {}

pub struct FramePool(IMFVideoSampleAllocatorEx);

unsafe impl Send for FramePool {}

impl FramePool {
    pub fn new(device: &ID3D11Device, width: u32, height: u32, initial: u32, maximum: u32) -> Result<Self> {
        unsafe {
            let mut raw = std::ptr::null_mut();

            MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)?;

            let allocator = IMFVideoSampleAllocatorEx::from_raw(raw);
            let mut attributes = None;

            MFCreateAttributes(&mut attributes, 2)?;

            let attributes = attributes.ok_or_else(|| Error::new(E_FAIL, "MFCreateAttributes returned no attributes"))?;

            attributes.SetUINT32(&MF_SA_D3D11_BINDFLAGS, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32)?;
            attributes.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32)?;
            allocator.SetDirectXManager(&device_manager(device)?)?;
            allocator.InitializeSampleAllocatorEx(initial, maximum, &attributes, &video_type(&MFVideoFormat_ARGB32, width, height, (1, 1))?)?;
            Ok(Self(allocator))
        }
    }

    pub fn take(&self) -> Result<Option<PooledFrame>> {
        unsafe {
            let sample = match self.0.AllocateSample() {
                Ok(sample) => sample,
                Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => return Ok(None),
                Err(e) => return Err(e),
            };
            let mut raw = std::ptr::null_mut();

            sample.GetBufferByIndex(0)?.cast::<IMFDXGIBuffer>()?.GetResource(&ID3D11Texture2D::IID, &mut raw)?;

            let texture = ID3D11Texture2D::from_raw(raw);

            Ok(Some(PooledFrame { sample, texture }))
        }
    }
}

pub struct PooledFrame {
    sample:      IMFSample,
    pub texture: ID3D11Texture2D,
}

impl PooledFrame {
    pub fn finish(self, time: i64, duration: i64) -> Result<Frame> {
        unsafe {
            let buffer = self.sample.GetBufferByIndex(0)?;

            buffer.SetCurrentLength(buffer.cast::<IMF2DBuffer>()?.GetContiguousLength()?)?;
            self.sample.SetSampleTime(time)?;
            self.sample.SetSampleDuration(duration)?;
            Ok(Frame(self.sample))
        }
    }
}

#[derive(Clone)]
pub struct Packet {
    pub data:     Arc<[u8]>,
    pub time:     i64,
    pub duration: i64,
    pub keyframe: bool,
}

#[derive(Clone)]
pub struct AudioChunk {
    pub pcm:      Arc<[i16]>,
    pub time:     i64,
    pub duration: i64,
}

#[derive(Clone)]
pub struct StreamType(IMFMediaType);

unsafe impl Send for StreamType {}
unsafe impl Sync for StreamType {}

pub struct VideoEncoder {
    transform: IMFTransform,
    events:    IMFMediaEventGenerator,
    codec:     ICodecAPI,
    output:    StreamType,
    _manager:  IMFDXGIDeviceManager,
}

unsafe impl Send for VideoEncoder {}

impl VideoEncoder {
    pub fn new(device: &ID3D11Device, width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Self> {
        unsafe {
            let manager = device_manager(device)?;
            let transform = hardware_encoder(&MFVideoFormat_H264)?;

            transform.GetAttributes()?.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let codec: ICodecAPI = transform.cast()?;

            set_codec_value(&codec, &CODECAPI_AVEncMPVGOPSize, keyframe_interval(fps))?;
            set_codec_value(&codec, &CODECAPI_AVEncCommonMeanBitRate, bitrate)?;

            let output = video_type(&MFVideoFormat_H264, width, height, (fps, 1))?;

            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            transform.SetOutputType(0, &output, 0)?;
            transform.SetInputType(0, &video_type(&MFVideoFormat_ARGB32, width, height, (fps, 1))?, 0)?;

            if transform.GetOutputStreamInfo(0)?.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                return Err(Error::new(E_FAIL, "The hardware H.264 encoder does not provide its own output samples"));
            }

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            Ok(Self { events: transform.cast()?, output: StreamType(transform.GetOutputCurrentType(0)?), transform, codec, _manager: manager })
        }
    }

    pub fn run(mut self, frames: Receiver<Frame>, keyframe: &AtomicBool, mut deliver: impl FnMut(&StreamType, Packet) -> Result<()>) -> Result<()> {
        let mut draining = false;

        loop {
            let event = unsafe { self.events.GetEvent(MF_EVENT_FLAG_NONE)? };
            let kind = MF_EVENT_TYPE(unsafe { event.GetType()? } as i32);

            if kind == METransformDrainComplete {
                return Ok(());
            }

            if kind == METransformHaveOutput
                && let Some(packet) = self.take_output()?
            {
                deliver(&self.output, packet)?;
            }

            if kind != METransformNeedInput || draining {
                continue;
            }

            match frames.recv() {
                Ok(frame) => unsafe {
                    if keyframe.swap(false, Ordering::Relaxed) {
                        set_codec_value(&self.codec, &CODECAPI_AVEncVideoForceKeyFrame, 1)?;
                    }

                    self.transform.ProcessInput(0, &frame.0, 0)?;
                },
                Err(_) => unsafe {
                    draining = true;
                    self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                    self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                },
            }
        }
    }

    fn take_output(&mut self) -> Result<Option<Packet>> {
        unsafe {
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(None), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
            let mut status = 0u32;
            let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
            let [buffer] = buffers;
            let sample = ManuallyDrop::into_inner(buffer.pSample);

            drop(ManuallyDrop::into_inner(buffer.pEvents));

            match result {
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    let output = self.transform.GetOutputAvailableType(0, 0)?;

                    self.transform.SetOutputType(0, &output, 0)?;
                    self.output = StreamType(output);
                    return Ok(None);
                }
                result => result?,
            }

            let sample = sample.ok_or_else(|| Error::new(E_FAIL, "The encoder returned no sample"))?;

            Ok(Some(Packet {
                data: sample_bytes(&sample)?.into(),
                time: sample.GetSampleTime()?,
                duration: sample.GetSampleDuration()?,
                keyframe: sample.GetUINT32(&MFSampleExtension_CleanPoint).is_ok_and(|clean| clean != 0),
            }))
        }
    }
}

pub struct Muxer {
    writer:   IMFSinkWriter,
    video:    u32,
    audio:    Option<u32>,
    start:    i64,
    pub path: PathBuf,
}

unsafe impl Send for Muxer {}

impl Muxer {
    pub fn new(path: &Path, video: &StreamType, audio: bool, start: i64) -> Result<Self> {
        unsafe {
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, None)?;
            let video_stream = writer.AddStream(&video.0)?;
            let audio_stream = match audio {
                true => Some(add_audio_stream(&writer)?),
                false => None,
            };

            writer.BeginWriting()?;
            Ok(Self { writer, video: video_stream, audio: audio_stream, start, path: path.to_path_buf() })
        }
    }

    pub fn video(&self, packet: &Packet) -> Result<()> {
        unsafe {
            let sample = timed_sample(&memory_buffer(&packet.data)?, packet.time - self.start, packet.duration)?;

            if packet.keyframe {
                sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
            }

            self.writer.WriteSample(self.video, &sample)
        }
    }

    pub fn audio(&self, chunk: &AudioChunk) -> Result<()> {
        if chunk.time < self.start {
            return Ok(());
        }

        let stream = self.audio.ok_or_else(|| Error::new(E_FAIL, "This file has no audio stream"))?;
        let bytes = unsafe { std::slice::from_raw_parts(chunk.pcm.as_ptr() as *const u8, chunk.pcm.len() * 2) };

        unsafe { self.writer.WriteSample(stream, &timed_sample(&memory_buffer(bytes)?, chunk.time - self.start, chunk.duration)?) }
    }

    pub fn finish(self) -> Result<PathBuf> {
        unsafe { self.writer.Finalize()? };
        Ok(self.path)
    }
}

pub fn device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager> {
    unsafe {
        _ = device.cast::<ID3D10Multithread>()?.SetMultithreadProtected(true);

        let mut token = 0u32;
        let mut manager = None;

        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;

        let manager: IMFDXGIDeviceManager = manager.ok_or_else(|| Error::new(E_FAIL, "MFCreateDXGIDeviceManager returned no manager"))?;

        manager.ResetDevice(device, token)?;
        Ok(manager)
    }
}

#[derive(Clone, Copy)]
pub enum RateControl {
    Source,
    Quality(u32),
    Bitrate(u32),
}

pub fn encoder_parameters(fps: u32, rate: RateControl) -> Result<IMFAttributes> {
    unsafe {
        let mut parameters = None;

        MFCreateAttributes(&mut parameters, 6)?;

        let parameters: IMFAttributes = parameters.ok_or_else(|| Error::new(E_FAIL, "MFCreateAttributes returned no attributes"))?;

        parameters.SetUINT32(&CODECAPI_AVEncMPVGOPSize, keyframe_interval(fps))?;

        match rate {
            RateControl::Source => return Ok(parameters),
            RateControl::Quality(quality) => {
                parameters.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_Quality.0 as u32)?;
                parameters.SetUINT32(&CODECAPI_AVEncCommonQuality, quality)?;
            }
            RateControl::Bitrate(bitrate) => {
                parameters.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32)?;
                parameters.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, bitrate)?;
                parameters.SetUINT32(&CODECAPI_AVEncCommonMaxBitRate, bitrate.saturating_mul(PEAK_TO_MEAN))?;
            }
        }

        parameters.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, SLOWEST_BEST_QUALITY)?;
        parameters.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, B_FRAMES)?;
        Ok(parameters)
    }
}

pub fn video_type(subtype: &GUID, width: u32, height: u32, rate: (u32, u32)) -> Result<IMFMediaType> {
    unsafe {
        let media = MFCreateMediaType()?;

        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
        media.SetUINT64(&MF_MT_FRAME_RATE, pack(rate.0, rate.1))?;
        media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        Ok(media)
    }
}

fn keyframe_interval(fps: u32) -> u32 {
    (fps / KEYFRAMES_PER_SECOND).max(1)
}

fn pack(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn hardware_encoder(subtype: &GUID) -> Result<IMFTransform> {
    unsafe {
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: *subtype };
        let mut activates = std::ptr::null_mut();
        let mut count = 0u32;

        MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, None, Some(&output), &mut activates, &mut count)?;

        let list = std::slice::from_raw_parts_mut(activates, count as usize);
        let first = list.first_mut().and_then(Option::take);

        list.iter_mut().for_each(|activate| drop(activate.take()));
        CoTaskMemFree(Some(activates as *const _));

        first.ok_or_else(|| Error::new(E_FAIL, "No hardware H.264 encoder was found on this GPU"))?.ActivateObject()
    }
}

fn set_codec_value(codec: &ICodecAPI, api: &GUID, value: u32) -> Result<()> {
    let variant = VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 { vt: VT_UI4, wReserved1: 0, wReserved2: 0, wReserved3: 0, Anonymous: VARIANT_0_0_0 { ulVal: value } }),
        },
    };

    unsafe { codec.SetValue(api, &variant) }
}

fn sample_bytes(sample: &IMFSample) -> Result<Vec<u8>> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut data = std::ptr::null_mut();
        let mut length = 0u32;

        buffer.Lock(&mut data, None, Some(&mut length))?;

        let bytes = std::slice::from_raw_parts(data, length as usize).to_vec();

        buffer.Unlock()?;
        Ok(bytes)
    }
}

fn memory_buffer(bytes: &[u8]) -> Result<IMFMediaBuffer> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut data = std::ptr::null_mut();

        buffer.Lock(&mut data, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes.len() as u32)?;
        Ok(buffer)
    }
}

fn timed_sample(buffer: &IMFMediaBuffer, time: i64, duration: i64) -> Result<IMFSample> {
    unsafe {
        let sample = MFCreateSample()?;

        sample.AddBuffer(buffer)?;
        sample.SetSampleTime(time)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

fn add_audio_stream(writer: &IMFSinkWriter) -> Result<u32> {
    unsafe {
        let stream = writer.AddStream(&audio_type(&MFAudioFormat_AAC, AAC_BYTES_PER_SECOND)?)?;
        let input = audio_type(&MFAudioFormat_PCM, SAMPLE_RATE * CHANNELS * 2)?;

        input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS * 2)?;
        writer.SetInputMediaType(stream, &input, None)?;
        Ok(stream)
    }
}

fn audio_type(subtype: &GUID, bytes_per_second: u32) -> Result<IMFMediaType> {
    unsafe {
        let media = MFCreateMediaType()?;

        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
        media.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_second)?;
        Ok(media)
    }
}
