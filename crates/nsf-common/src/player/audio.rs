use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, Stream, StreamConfig};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The audio output stream. Held by the spawning (GUI) thread because
/// `cpal::Stream` is `!Send` on Windows / macOS. Keep this alive for as
/// long as you want audio to play.
pub struct AudioSink {
    _stream: Stream,
}

/// `Send` half handed to the player thread: it owns the ring-buffer
/// producer and knows the device's sample rate / channel count.
pub struct AudioFeed {
    pub sample_rate: u32,
    pub channels: u16,
    pub producer: HeapProd<i16>,
    pub underruns: Arc<AtomicU64>,
    /// `true` whenever a track is actively playing. The audio callback
    /// suppresses the underrun count when this is `false`, since draining
    /// silence at idle is expected, not a glitch.
    pub audio_expected: Arc<AtomicBool>,
}

impl AudioSink {
    /// Open the default audio output device. Returns the sink (keep on GUI thread)
    /// and a feed (move to the player thread).
    pub fn open() -> Result<(Self, AudioFeed)> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("No default audio output device available")?;

        let supported_config = device
            .default_output_config()
            .context("Failed to query default output config")?;
        let sample_format = supported_config.sample_format();
        let stream_config: StreamConfig = supported_config.into();
        let sample_rate = stream_config.sample_rate.0;
        let channels = stream_config.channels;

        // ~200 ms of audio (scaled by channel count).
        let capacity = (sample_rate as usize / 5) * channels as usize;
        let rb = HeapRb::<i16>::new(capacity.max(4096));
        let (mut producer, consumer) = rb.split();

        // Pre-load ~100 ms of silence so the first audio callback never finds
        // an empty buffer. The player thread's real audio will arrive shortly.
        let silence_samples = (sample_rate as usize / 10) * channels as usize;
        for _ in 0..silence_samples {
            let _ = producer.try_push(0);
        }

        let underruns = Arc::new(AtomicU64::new(0));
        let audio_expected = Arc::new(AtomicBool::new(false));
        let stream = build_stream(
            &device,
            &stream_config,
            sample_format,
            consumer,
            underruns.clone(),
            audio_expected.clone(),
        )?;
        stream.play().context("Failed to start audio stream")?;

        Ok((
            Self { _stream: stream },
            AudioFeed {
                sample_rate,
                channels,
                producer,
                underruns,
                audio_expected,
            },
        ))
    }
}

fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    consumer: HeapCons<i16>,
    underruns: Arc<AtomicU64>,
    audio_expected: Arc<AtomicBool>,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("Audio stream error: {}", err);

    let stream = match sample_format {
        SampleFormat::I16 => {
            let mut consumer = consumer;
            let underruns = underruns.clone();
            let audio_expected = audio_expected.clone();
            device.build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    fill_buffer_i16(data, &mut consumer, &underruns, &audio_expected)
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::F32 => {
            let mut consumer = consumer;
            let underruns = underruns.clone();
            let audio_expected = audio_expected.clone();
            device.build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    fill_buffer_convert::<f32>(data, &mut consumer, &underruns, &audio_expected)
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let mut consumer = consumer;
            let underruns = underruns.clone();
            let audio_expected = audio_expected.clone();
            device.build_output_stream(
                config,
                move |data: &mut [u16], _| {
                    fill_buffer_convert::<u16>(data, &mut consumer, &underruns, &audio_expected)
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("Unsupported sample format: {:?}", other)),
    };

    Ok(stream)
}

fn fill_buffer_i16(
    out: &mut [i16],
    consumer: &mut HeapCons<i16>,
    underruns: &AtomicU64,
    audio_expected: &AtomicBool,
) {
    let mut filled = 0;
    while filled < out.len() {
        match consumer.try_pop() {
            Some(s) => {
                out[filled] = s;
                filled += 1;
            }
            None => break,
        }
    }
    if filled < out.len() {
        if audio_expected.load(Ordering::Relaxed) {
            underruns.fetch_add(1, Ordering::Relaxed);
        }
        for slot in &mut out[filled..] {
            *slot = 0;
        }
    }
}

fn fill_buffer_convert<T: Sample + cpal::FromSample<i16>>(
    out: &mut [T],
    consumer: &mut HeapCons<i16>,
    underruns: &AtomicU64,
    audio_expected: &AtomicBool,
) {
    let mut filled = 0;
    while filled < out.len() {
        match consumer.try_pop() {
            Some(s) => {
                out[filled] = T::from_sample_(s);
                filled += 1;
            }
            None => break,
        }
    }
    if filled < out.len() {
        if audio_expected.load(Ordering::Relaxed) {
            underruns.fetch_add(1, Ordering::Relaxed);
        }
        let silence = T::EQUILIBRIUM;
        for slot in &mut out[filled..] {
            *slot = silence;
        }
    }
}
