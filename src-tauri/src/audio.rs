//! Cross-platform audio capture via cpal.
//!
//! - Enumerate input devices for the UI picker.
//! - Open an input stream on the selected device, normalize to 16 kHz mono f32,
//!   accumulate ~3 s chunks, and ship each chunk to the parlyx streamer task.
//!
//! parlyx expects chunks at 16 kHz PCM (matches `STREAM_CHUNK_SECONDS = 3.0`
//! in `src/handlers/streaming.rs`). The web UI sends raw PCM via multipart;
//! this app encodes a tiny WAV per chunk so the server's symphonia decoder
//! has explicit format info.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use parking_lot::Mutex;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use serde::Serialize;
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info, warn};

/// Target sample rate the parlyx streaming pipeline expects.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Chunk size in seconds. Matches `STREAM_CHUNK_SECONDS` in parlyx.
pub const CHUNK_DURATION_S: f32 = 3.0;
/// Samples per chunk at target sample rate.
pub const SAMPLES_PER_CHUNK: usize = (TARGET_SAMPLE_RATE as f32 * CHUNK_DURATION_S) as usize;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub channels: u16,
    pub default_sample_rate: u32,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    let mut out = Vec::new();
    for device in host.input_devices()? {
        let name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let cfg = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                warn!(device = %name, error = %e, "skipping device with no default config");
                continue;
            }
        };
        out.push(AudioDevice {
            is_default: name == default_name,
            name,
            channels: cfg.channels(),
            default_sample_rate: cfg.sample_rate().0,
        });
    }
    Ok(out)
}

/// Handle returned by `start_capture`. Drop or call `stop()` to tear down.
pub struct CaptureHandle {
    stop_flag: Arc<Mutex<bool>>,
    pause_flag: Arc<Mutex<bool>>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureHandle {
    pub fn pause(&self) {
        *self.pause_flag.lock() = true;
    }
    pub fn resume(&self) {
        *self.pause_flag.lock() = false;
    }
    pub fn stop(mut self) {
        *self.stop_flag.lock() = true;
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn a dedicated OS thread that owns the cpal stream and pushes f32 16k
/// mono chunks into `chunk_tx`. cpal callbacks must not allocate or block;
/// the thread runs a small ring buffer and forwards finished chunks.
pub fn start_capture(
    device_name: Option<String>,
    chunk_tx: UnboundedSender<Vec<f32>>,
) -> Result<CaptureHandle> {
    let stop_flag = Arc::new(Mutex::new(false));
    let pause_flag = Arc::new(Mutex::new(false));
    let stop_flag_thread = stop_flag.clone();
    let pause_flag_thread = pause_flag.clone();

    let thread = std::thread::Builder::new()
        .name("parlyx-audio".into())
        .spawn(move || {
            if let Err(e) = run_capture(device_name, chunk_tx, stop_flag_thread, pause_flag_thread)
            {
                error!(error = ?e, "audio capture loop ended with error");
            }
        })
        .context("spawn audio thread")?;

    Ok(CaptureHandle {
        stop_flag,
        pause_flag,
        thread: Some(thread),
    })
}

fn run_capture(
    device_name: Option<String>,
    chunk_tx: UnboundedSender<Vec<f32>>,
    stop_flag: Arc<Mutex<bool>>,
    pause_flag: Arc<Mutex<bool>>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(ref name) if !name.is_empty() => host
            .input_devices()?
            .find(|d| d.name().map(|n| &n == name).unwrap_or(false))
            .ok_or_else(|| anyhow!("input device '{}' not found", name))?,
        _ => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device"))?,
    };
    let device_name_resolved = device.name().unwrap_or_else(|_| "<unknown>".into());
    let cfg = device
        .default_input_config()
        .context("default input config")?;
    info!(
        device = %device_name_resolved,
        sample_rate = cfg.sample_rate().0,
        channels = cfg.channels(),
        format = ?cfg.sample_format(),
        "opening capture",
    );

    let in_sample_rate = cfg.sample_rate().0;
    let in_channels = cfg.channels() as usize;

    // Buffer that the cpal callback appends into. The main thread of this
    // function pulls fixed-size windows out and dispatches them as chunks.
    let pending: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(in_sample_rate as usize)));
    let pending_clone = pending.clone();
    let pause_for_cb = pause_flag.clone();

    let err_cb = |e| error!(error = ?e, "cpal stream error");
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => device.build_input_stream(
            &cfg.clone().into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if *pause_for_cb.lock() {
                    return;
                }
                let mono = downmix_mono(data, in_channels);
                pending_clone.lock().extend(mono);
            },
            err_cb,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &cfg.clone().into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                if *pause_for_cb.lock() {
                    return;
                }
                let mut buf = Vec::with_capacity(data.len());
                for s in data {
                    buf.push(*s as f32 / i16::MAX as f32);
                }
                let mono = downmix_mono(&buf, in_channels);
                pending_clone.lock().extend(mono);
            },
            err_cb,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &cfg.clone().into(),
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                if *pause_for_cb.lock() {
                    return;
                }
                let mut buf = Vec::with_capacity(data.len());
                for s in data {
                    let centered = (*s as i32 - i16::MAX as i32) as f32 / i16::MAX as f32;
                    buf.push(centered);
                }
                let mono = downmix_mono(&buf, in_channels);
                pending_clone.lock().extend(mono);
            },
            err_cb,
            None,
        )?,
        fmt => return Err(anyhow!("unsupported sample format: {:?}", fmt)),
    };

    stream.play().context("starting cpal stream")?;

    let mut resampler = if in_sample_rate != TARGET_SAMPLE_RATE {
        Some(build_resampler(in_sample_rate, TARGET_SAMPLE_RATE)?)
    } else {
        None
    };

    // Pull chunks of `samples_at_input_rate` mono samples that, once
    // resampled, produce roughly `SAMPLES_PER_CHUNK` output samples.
    let samples_at_input_rate =
        ((TARGET_SAMPLE_RATE as f32 * CHUNK_DURATION_S) * (in_sample_rate as f32 / TARGET_SAMPLE_RATE as f32)) as usize;

    let mut chunk_idx: u64 = 0;
    let mut consecutive_silent: u32 = 0;
    loop {
        if *stop_flag.lock() {
            break;
        }
        // Drain enough samples for one chunk.
        let chunk_in = {
            let mut buf = pending.lock();
            if buf.len() < samples_at_input_rate {
                drop(buf);
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            buf.drain(..samples_at_input_rate).collect::<Vec<f32>>()
        };

        let chunk_out = match resampler.as_mut() {
            None => chunk_in,
            Some(r) => match r.process(&[chunk_in], None) {
                Ok(mut out) => out.remove(0),
                Err(e) => {
                    warn!(error = ?e, "resampler failed, dropping chunk");
                    continue;
                }
            },
        };

        // Audio level diagnostics: RMS and peak. If chunks are silent we
        // probably never got mic permission (macOS) or the wrong device is
        // selected; the user should know before they speak for 30 minutes
        // into the void.
        let (rms, peak) = level_stats(&chunk_out);
        if peak < 1e-4 {
            consecutive_silent += 1;
            if consecutive_silent == 1 || consecutive_silent % 5 == 0 {
                warn!(
                    chunk = chunk_idx,
                    peak,
                    rms,
                    consecutive_silent,
                    "audio chunk is essentially silent — check macOS Settings → \
                     Privacy & Security → Microphone, or pick a different input device"
                );
            }
        } else {
            if consecutive_silent > 0 {
                info!(consecutive_silent, "audio resumed");
            }
            consecutive_silent = 0;
            info!(chunk = chunk_idx, peak, rms, samples = chunk_out.len(), "captured chunk");
        }
        chunk_idx += 1;

        if chunk_tx.send(chunk_out).is_err() {
            // receiver dropped → session ended
            break;
        }
    }

    drop(stream);
    info!("audio capture loop exited cleanly");
    Ok(())
}

fn level_stats(samples: &[f32]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut sumsq = 0.0_f64;
    let mut peak = 0.0_f32;
    for &s in samples {
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        sumsq += (s as f64) * (s as f64);
    }
    let rms = (sumsq / samples.len() as f64).sqrt() as f32;
    (rms, peak)
}

fn downmix_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let frames = interleaved.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut acc = 0.0;
        for c in 0..channels {
            acc += interleaved[f * channels + c];
        }
        out.push(acc / channels as f32);
    }
    out
}

fn build_resampler(input_rate: u32, output_rate: u32) -> Result<SincFixedIn<f32>> {
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    SincFixedIn::<f32>::new(
        output_rate as f64 / input_rate as f64,
        2.0,
        params,
        // chunk size will be set per-call via the input vector length;
        // SincFixedIn re-uses internal buffers when chunk size matches.
        (TARGET_SAMPLE_RATE as f32 * CHUNK_DURATION_S * (input_rate as f32 / output_rate as f32))
            as usize,
        1,
    )
    .context("build resampler")
}

/// Encode a chunk of f32 mono samples at TARGET_SAMPLE_RATE as a WAV file
/// (in-memory). parlyx's symphonia pipeline decodes this trivially.
pub fn encode_wav(samples: &[f32]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(samples.len() * 2 + 44);
    {
        let mut cursor = std::io::Cursor::new(&mut buf);
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .context("hound writer")?;
        for s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            let v = (clamped * i16::MAX as f32) as i16;
            writer.write_sample(v)?;
        }
        writer.finalize()?;
    }
    Ok(buf)
}
