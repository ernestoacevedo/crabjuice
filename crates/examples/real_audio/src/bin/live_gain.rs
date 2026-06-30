use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use crabjuice_audio::{AudioBuffer, AudioProcessor, ProcessContext};
use crabjuice_dsp::GainProcessor;
use ringbuf::{HeapConsumer, HeapProducer, HeapRb};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

fn main() -> Result<()> {
    let host = cpal::default_host();
    let input_device = select_device(&host, true)?;
    let output_device = select_device(&host, false)?;
    let input_config = input_device
        .default_input_config()
        .context("selected input device has no default input config")?;
    let output_config = output_device
        .default_output_config()
        .context("selected output device has no default output config")?;
    let gain = prompt_gain()?;

    println!(
        "Input: {} ({:?}, {} Hz, {} channels)",
        input_device
            .name()
            .unwrap_or_else(|_| "unknown".to_string()),
        input_config.sample_format(),
        input_config.sample_rate().0,
        input_config.channels()
    );
    println!(
        "Output: {} ({:?}, {} Hz, {} channels)",
        output_device
            .name()
            .unwrap_or_else(|_| "unknown".to_string()),
        output_config.sample_format(),
        output_config.sample_rate().0,
        output_config.channels()
    );
    if input_config.sample_rate() != output_config.sample_rate() {
        println!("Input and output sample rates differ; this example does not resample.");
    }

    let input_channels = usize::from(input_config.channels());
    let output_channels = usize::from(output_config.channels());
    let sample_rate = output_config.sample_rate().0 as f32;
    let ring_capacity = input_channels * input_config.sample_rate().0 as usize;
    let ring = HeapRb::<f32>::new(ring_capacity.max(input_channels * 256));
    let (producer, consumer) = ring.split();
    let producer = Arc::new(Mutex::new(producer));
    let consumer = Arc::new(Mutex::new(consumer));
    let processor = Arc::new(Mutex::new({
        let mut processor = GainProcessor::new();
        processor.set_gain(gain);
        processor.prepare(sample_rate, 0);
        processor
    }));
    let output_runtime = OutputRuntime {
        input_channels,
        output_channels,
        consumer: Arc::clone(&consumer),
        processor: Arc::clone(&processor),
        sample_rate,
    };

    let input_stream = build_input_stream(
        &input_device,
        &input_config.config(),
        input_config.sample_format(),
        Arc::clone(&producer),
    )?;
    let output_stream = build_output_stream(
        &output_device,
        &output_config.config(),
        output_config.sample_format(),
        output_runtime,
    )?;

    input_stream
        .play()
        .context("failed to start input stream")?;
    output_stream
        .play()
        .context("failed to start output stream")?;

    println!("Streaming. Press Enter to stop.");
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to wait for Enter")?;

    Ok(())
}

fn select_device(host: &cpal::Host, input: bool) -> Result<Device> {
    let label = if input { "input" } else { "output" };
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

    println!("Available {label} devices:");
    for (index, device) in devices.iter().enumerate() {
        let name = device.name().unwrap_or_else(|_| "unknown".to_string());
        let marker = if same_device(default.as_ref(), device) {
            " [default]"
        } else {
            ""
        };
        println!("  {index}: {name}{marker}");
    }

    if devices.is_empty() {
        bail!("no {label} devices available");
    }

    loop {
        print!("Select {label} device index [default]: ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("failed to read device selection")?;
        let value = line.trim();

        if value.is_empty() {
            if let Some(device) = default {
                return Ok(device);
            }
            println!("No default {label} device is available.");
            continue;
        }

        match value.parse::<usize>() {
            Ok(index) if index < devices.len() => return Ok(devices[index].clone()),
            _ => println!("Enter a valid device index."),
        }
    }
}

fn same_device(default: Option<&Device>, device: &Device) -> bool {
    default
        .and_then(|default| default.name().ok())
        .zip(device.name().ok())
        .is_some_and(|(left, right)| left == right)
}

fn prompt_gain() -> Result<f32> {
    loop {
        print!("Linear gain [1.0]: ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("failed to read gain")?;
        let value = line.trim();

        if value.is_empty() {
            return Ok(1.0);
        }

        match value.parse::<f32>() {
            Ok(gain) if (0.0..=4.0).contains(&gain) => return Ok(gain),
            _ => println!("Enter a finite gain between 0.0 and 4.0."),
        }
    }
}

fn build_input_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    producer: Arc<Mutex<HeapProducer<f32>>>,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("input stream error: {err}");

    match sample_format {
        SampleFormat::F32 => device
            .build_input_stream(
                config,
                move |data: &[f32], _| write_input(data.iter().copied(), &producer),
                err_fn,
                None,
            )
            .context("failed to build f32 input stream"),
        SampleFormat::I16 => device
            .build_input_stream(
                config,
                move |data: &[i16], _| {
                    write_input(
                        data.iter().map(|sample| *sample as f32 / i16::MAX as f32),
                        &producer,
                    )
                },
                err_fn,
                None,
            )
            .context("failed to build i16 input stream"),
        SampleFormat::U16 => device
            .build_input_stream(
                config,
                move |data: &[u16], _| {
                    write_input(
                        data.iter()
                            .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0),
                        &producer,
                    )
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
    processor: Arc<Mutex<GainProcessor>>,
    sample_rate: f32,
}

fn build_output_stream(
    device: &Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    runtime: OutputRuntime,
) -> Result<Stream> {
    let err_fn = |err| eprintln!("output stream error: {err}");

    match sample_format {
        SampleFormat::F32 => device
            .build_output_stream(
                config,
                move |data: &mut [f32], _| fill_output(data, &runtime),
                err_fn,
                None,
            )
            .context("failed to build f32 output stream"),
        SampleFormat::I16 => device
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
        SampleFormat::U16 => device
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
}
