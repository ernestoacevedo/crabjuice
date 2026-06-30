use anyhow::{bail, Context, Result};
use crabjuice_audio::{AudioBuffer, AudioProcessor, ProcessContext};
use crabjuice_dsp::GainProcessor;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::path::Path;

const BLOCK_SIZE: usize = 512;

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
}
