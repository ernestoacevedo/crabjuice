use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, Host, SampleFormat as CpalSampleFormat, Stream, StreamConfig, SupportedStreamConfig,
};
use crabjuice_audio::{AudioBuffer, AudioProcessor, ProcessContext};
use crabjuice_dsp::GainProcessor;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::path::Path;
use std::sync::{Arc, Mutex};

const BLOCK_SIZE: usize = 512;

pub type SharedProcessor = Arc<Mutex<Box<dyn AudioProcessor + Send>>>;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AudioStats {
    pub peak: f32,
    pub rms: f32,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
    pub is_default: bool,
}

pub struct LiveAudioSession {
    input_stream: Stream,
    output_stream: Stream,
    pub input_name: String,
    pub output_name: String,
    pub input_config: SupportedStreamConfig,
    pub output_config: SupportedStreamConfig,
    input_stats: Arc<Mutex<AudioStats>>,
    output_stats: Arc<Mutex<AudioStats>>,
}

impl LiveAudioSession {
    pub fn play(&self) -> Result<()> {
        self.input_stream
            .play()
            .context("failed to start input stream")?;
        self.output_stream
            .play()
            .context("failed to start output stream")?;
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.input_stream
            .pause()
            .context("failed to pause input stream")?;
        self.output_stream
            .pause()
            .context("failed to pause output stream")?;
        Ok(())
    }

    pub fn input_stats(&self) -> AudioStats {
        current_stats(&self.input_stats)
    }

    pub fn output_stats(&self) -> AudioStats {
        current_stats(&self.output_stats)
    }
}

impl Drop for LiveAudioSession {
    fn drop(&mut self) {
        let _ = self.input_stream.pause();
        let _ = self.output_stream.pause();
    }
}

pub fn input_devices(host: &Host) -> Result<Vec<DeviceInfo>> {
    device_infos(host, true)
}

pub fn output_devices(host: &Host) -> Result<Vec<DeviceInfo>> {
    device_infos(host, false)
}

pub fn default_input_index(host: &Host, devices: &[DeviceInfo]) -> Option<usize> {
    default_device_index(host, devices, true)
}

pub fn default_output_index(host: &Host, devices: &[DeviceInfo]) -> Option<usize> {
    default_device_index(host, devices, false)
}

pub fn select_input_device(host: &Host, index: usize) -> Result<Device> {
    select_device_by_index(host, true, index)
}

pub fn select_output_device(host: &Host, index: usize) -> Result<Device> {
    select_device_by_index(host, false, index)
}

pub fn new_gain_processor(gain: f32) -> SharedProcessor {
    let mut processor = GainProcessor::new();
    processor.set_gain(gain);
    Arc::new(Mutex::new(Box::new(processor)))
}

pub fn start_live_audio(
    input_device: Device,
    output_device: Device,
    processor: SharedProcessor,
) -> Result<LiveAudioSession> {
    let input_config = input_device
        .default_input_config()
        .context("selected input device has no default input config")?;
    let output_config = output_device
        .default_output_config()
        .context("selected output device has no default output config")?;
    let input_channels = usize::from(input_config.channels());
    let output_channels = usize::from(output_config.channels());
    let sample_rate = output_config.sample_rate().0 as f32;
    let ring_capacity = input_channels * input_config.sample_rate().0 as usize;
    let ring = HeapRb::<f32>::new(ring_capacity.max(input_channels * 256));
    let (producer, consumer) = ring.split();
    let producer = Arc::new(Mutex::new(producer));
    let consumer = Arc::new(Mutex::new(consumer));
    let input_stats = Arc::new(Mutex::new(AudioStats::default()));
    let output_stats = Arc::new(Mutex::new(AudioStats::default()));

    if let Ok(mut processor) = processor.lock() {
        processor.prepare(sample_rate, 0);
    }

    let output_runtime = OutputRuntime {
        input_channels,
        output_channels,
        consumer: Arc::clone(&consumer),
        processor,
        sample_rate,
        output_stats: Arc::clone(&output_stats),
    };
    let input_stream = build_input_stream(
        &input_device,
        &input_config.config(),
        input_config.sample_format(),
        Arc::clone(&producer),
        Arc::clone(&input_stats),
    )?;
    let output_stream = build_output_stream(
        &output_device,
        &output_config.config(),
        output_config.sample_format(),
        output_runtime,
    )?;

    Ok(LiveAudioSession {
        input_stream,
        output_stream,
        input_name: device_name(&input_device),
        output_name: device_name(&output_device),
        input_config,
        output_config,
        input_stats,
        output_stats,
    })
}

pub fn process_wav_file(input_path: &Path, output_path: &Path, gain: f32) -> Result<()> {
    let mut reader = WavReader::open(input_path)
        .with_context(|| format!("failed to open input WAV {}", input_path.display()))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels);

    if channels == 0 {
        bail!("input WAV must have at least one channel");
    }

    let samples = read_wav_samples(&mut reader, spec)?;
    let mut writer = WavWriter::create(output_path, spec)
        .with_context(|| format!("failed to create output WAV {}", output_path.display()))?;
    let mut processor = GainProcessor::new();
    processor.set_gain(gain);
    processor.prepare(spec.sample_rate as f32, BLOCK_SIZE);

    let mut block = AudioBuffer::new(channels, 0);
    for interleaved_block in samples.chunks(BLOCK_SIZE * channels) {
        let frames = interleaved_block.len() / channels;
        block.resize(channels, frames);
        block.copy_from_interleaved(interleaved_block)?;

        let mut ctx = ProcessContext::new(&mut block, &[]);
        processor.process(&mut ctx);

        let mut processed = vec![0.0; interleaved_block.len()];
        block.copy_to_interleaved(&mut processed)?;
        write_wav_samples(&mut writer, spec, &processed)?;
    }

    writer.finalize().context("failed to finalize output WAV")?;
    Ok(())
}

fn read_wav_samples<R>(reader: &mut WavReader<R>, spec: WavSpec) -> Result<Vec<f32>>
where
    R: std::io::Read,
{
    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .map(|sample| sample.context("failed to read float WAV sample"))
            .collect(),
        (SampleFormat::Int, 8) => reader
            .samples::<i8>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / i8::MAX as f32)
                    .context("failed to read 8-bit WAV sample")
            })
            .collect(),
        (SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / i16::MAX as f32)
                    .context("failed to read 16-bit WAV sample")
            })
            .collect(),
        (SampleFormat::Int, 24 | 32) => {
            let max_amplitude = ((1_i64 << (u32::from(spec.bits_per_sample) - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| value as f32 / max_amplitude)
                        .context("failed to read integer WAV sample")
                })
                .collect()
        }
        _ => bail!(
            "unsupported WAV format: {:?}, {} bits per sample",
            spec.sample_format,
            spec.bits_per_sample
        ),
    }
}

fn write_wav_samples<W>(writer: &mut WavWriter<W>, spec: WavSpec, samples: &[f32]) -> Result<()>
where
    W: std::io::Write + std::io::Seek,
{
    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => {
            for sample in samples {
                writer.write_sample(*sample)?;
            }
        }
        (SampleFormat::Int, 8) => {
            for sample in samples {
                writer.write_sample(float_to_i8(*sample))?;
            }
        }
        (SampleFormat::Int, 16) => {
            for sample in samples {
                writer.write_sample(float_to_i16(*sample))?;
            }
        }
        (SampleFormat::Int, 24 | 32) => {
            let max_amplitude = ((1_i64 << (u32::from(spec.bits_per_sample) - 1)) - 1) as f32;
            let min_amplitude = -(1_i64 << (u32::from(spec.bits_per_sample) - 1)) as f32;
            for sample in samples {
                let scaled = (*sample * max_amplitude)
                    .round()
                    .clamp(min_amplitude, max_amplitude);
                writer.write_sample(scaled as i32)?;
            }
        }
        _ => bail!(
            "unsupported WAV format: {:?}, {} bits per sample",
            spec.sample_format,
            spec.bits_per_sample
        ),
    }

    Ok(())
}

fn float_to_i8(sample: f32) -> i8 {
    (sample * i8::MAX as f32)
        .round()
        .clamp(i8::MIN as f32, i8::MAX as f32) as i8
}

fn float_to_i16(sample: f32) -> i16 {
    (sample * i16::MAX as f32)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

fn device_infos(host: &Host, input: bool) -> Result<Vec<DeviceInfo>> {
    let default = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    let devices = if input {
        host.input_devices()
            .context("failed to list input devices")?
            .collect::<Vec<_>>()
    } else {
        host.output_devices()
            .context("failed to list output devices")?
            .collect::<Vec<_>>()
    };

    Ok(devices
        .iter()
        .enumerate()
        .map(|(index, device)| DeviceInfo {
            index,
            name: device_name(device),
            is_default: same_device(default.as_ref(), device),
        })
        .collect())
}

fn default_device_index(host: &Host, devices: &[DeviceInfo], input: bool) -> Option<usize> {
    let default = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    let default_name = default.as_ref().map(device_name)?;
    devices
        .iter()
        .find(|device| device.name == default_name)
        .map(|device| device.index)
}

fn select_device_by_index(host: &Host, input: bool, index: usize) -> Result<Device> {
    let mut devices = if input {
        host.input_devices()
            .context("failed to list input devices")?
            .collect::<Vec<_>>()
    } else {
        host.output_devices()
            .context("failed to list output devices")?
            .collect::<Vec<_>>()
    };

    if index >= devices.len() {
        bail!("device index {index} is out of range");
    }

    Ok(devices.swap_remove(index))
}

fn same_device(default: Option<&Device>, device: &Device) -> bool {
    default
        .and_then(|default| default.name().ok())
        .zip(device.name().ok())
        .is_some_and(|(left, right)| left == right)
}

fn device_name(device: &Device) -> String {
    device.name().unwrap_or_else(|_| "unknown".to_string())
}

fn build_input_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: CpalSampleFormat,
    producer: Arc<Mutex<HeapProducer<f32>>>,
    input_stats: Arc<Mutex<AudioStats>>,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("input stream error: {err}");

    match sample_format {
        CpalSampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _| {
                    update_stats(&input_stats, data);
                    write_input(data.iter().copied(), &producer);
                },
                err_fn,
                None,
            )
            .context("failed to build f32 input stream"),
        CpalSampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _| {
                    let samples = data
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32)
                        .collect::<Vec<_>>();
                    update_stats(&input_stats, &samples);
                    write_input(samples, &producer);
                },
                err_fn,
                None,
            )
            .context("failed to build i16 input stream"),
        CpalSampleFormat::U16 => device
            .build_input_stream(
                config,
                move |data: &[u16], _| {
                    let samples = data
                        .iter()
                        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect::<Vec<_>>();
                    update_stats(&input_stats, &samples);
                    write_input(samples, &producer);
                },
                err_fn,
                None,
            )
            .context("failed to build u16 input stream"),
        _ => bail!("unsupported input sample format: {sample_format:?}"),
    }
}

fn write_input<I>(samples: I, producer: &Arc<Mutex<HeapProducer<f32>>>)
where
    I: IntoIterator<Item = f32>,
{
    let Ok(mut producer) = producer.try_lock() else {
        return;
    };

    for sample in samples {
        let _ = producer.push(sample);
    }
}

#[derive(Clone)]
struct OutputRuntime {
    input_channels: usize,
    output_channels: usize,
    consumer: Arc<Mutex<HeapConsumer<f32>>>,
    processor: SharedProcessor,
    sample_rate: f32,
    output_stats: Arc<Mutex<AudioStats>>,
}

fn build_output_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: CpalSampleFormat,
    runtime: OutputRuntime,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("output stream error: {err}");

    match sample_format {
        CpalSampleFormat::F32 => device
            .build_output_stream(
                config,
                move |data: &mut [f32], _| fill_output(data, &runtime),
                err_fn,
                None,
            )
            .context("failed to build f32 output stream"),
        CpalSampleFormat::I16 => device
            .build_output_stream(
                config,
                move |data: &mut [i16], _| {
                    let mut buffer = vec![0.0; data.len()];
                    fill_output(&mut buffer, &runtime);
                    for (output, sample) in data.iter_mut().zip(buffer.iter()) {
                        *output = (*sample * i16::MAX as f32)
                            .round()
                            .clamp(i16::MIN as f32, i16::MAX as f32)
                            as i16;
                    }
                },
                err_fn,
                None,
            )
            .context("failed to build i16 output stream"),
        CpalSampleFormat::U16 => device
            .build_output_stream(
                config,
                move |data: &mut [u16], _| {
                    let mut buffer = vec![0.0; data.len()];
                    fill_output(&mut buffer, &runtime);
                    for (output, sample) in data.iter_mut().zip(buffer.iter()) {
                        *output = (((*sample).clamp(-1.0, 1.0) + 1.0) * 0.5 * u16::MAX as f32)
                            .round()
                            .clamp(u16::MIN as f32, u16::MAX as f32)
                            as u16;
                    }
                },
                err_fn,
                None,
            )
            .context("failed to build u16 output stream"),
        _ => bail!("unsupported output sample format: {sample_format:?}"),
    }
}

fn fill_output(output: &mut [f32], runtime: &OutputRuntime) {
    output.fill(0.0);

    if runtime.input_channels == 0 || runtime.output_channels == 0 {
        return;
    }

    let frames = output.len() / runtime.output_channels;
    let mut interleaved = vec![0.0; frames * runtime.input_channels];

    if let Ok(mut consumer) = runtime.consumer.try_lock() {
        for sample in &mut interleaved {
            *sample = consumer.pop().unwrap_or(0.0);
        }
    }

    let mut buffer = AudioBuffer::new(runtime.input_channels, frames);
    if buffer.copy_from_interleaved(&interleaved).is_err() {
        return;
    }

    if let Ok(mut processor) = runtime.processor.try_lock() {
        processor.prepare(runtime.sample_rate, frames);
        let mut ctx = ProcessContext::new(&mut buffer, &[]);
        processor.process(&mut ctx);
    }

    for frame_index in 0..frames {
        for output_channel in 0..runtime.output_channels {
            let source_channel = output_channel.min(runtime.input_channels - 1);
            output[frame_index * runtime.output_channels + output_channel] = buffer
                .channel(source_channel)
                .and_then(|channel| channel.get(frame_index))
                .copied()
                .unwrap_or(0.0);
        }
    }

    update_stats(&runtime.output_stats, output);
}

fn update_stats(stats: &Arc<Mutex<AudioStats>>, samples: &[f32]) {
    let next = calculate_stats(samples);
    if let Ok(mut stats) = stats.try_lock() {
        *stats = next;
    }
}

fn current_stats(stats: &Arc<Mutex<AudioStats>>) -> AudioStats {
    stats.lock().map(|stats| *stats).unwrap_or_default()
}

fn calculate_stats(samples: &[f32]) -> AudioStats {
    if samples.is_empty() {
        return AudioStats::default();
    }

    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f32;
    for sample in samples {
        let abs = sample.abs();
        peak = peak.max(abs);
        sum_squares += sample * sample;
    }

    AudioStats {
        peak,
        rms: (sum_squares / samples.len() as f32).sqrt(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn process_wav_file_applies_gain_and_preserves_shape() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir();
        let input_path = dir.join(format!("crabjuice-input-{id}.wav"));
        let output_path = dir.join(format!("crabjuice-output-{id}.wav"));

        let spec = WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        {
            let mut writer =
                WavWriter::create(&input_path, spec).expect("test input WAV can be created");
            for sample in [0.5_f32, -0.5, 0.25, -0.25] {
                writer
                    .write_sample(float_to_i16(sample))
                    .expect("test sample can be written");
            }
            writer.finalize().expect("test WAV can be finalized");
        }

        process_wav_file(&input_path, &output_path, 0.5).expect("WAV processing succeeds");

        let mut reader = WavReader::open(&output_path).expect("output WAV can be opened");
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().sample_rate, 48_000);
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .expect("output samples can be read");
        let expected = [0.25_f32, -0.25, 0.125, -0.125].map(float_to_i16);
        assert_eq!(samples, expected);

        let _ = std::fs::remove_file(input_path);
        let _ = std::fs::remove_file(output_path);
    }

    #[test]
    fn calculate_stats_returns_peak_and_rms() {
        let stats = calculate_stats(&[0.5, -1.0, 0.0, 0.5]);

        assert_eq!(stats.peak, 1.0);
        assert!((stats.rms - 0.612_372_46).abs() < f32::EPSILON);
    }
}
