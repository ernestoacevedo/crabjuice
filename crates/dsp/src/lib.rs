//! Small DSP building blocks for crabjuice.

use crabjuice_audio::{AudioProcessor, ProcessContext};
use crabjuice_core::{FloatParameter, ParameterId};

/// Multiplies every sample by a linear gain value.
#[derive(Debug, Clone)]
pub struct GainProcessor {
    gain: FloatParameter,
}

impl GainProcessor {
    /// Creates a gain processor with unity gain.
    pub fn new() -> Self {
        Self {
            gain: FloatParameter::new(ParameterId::new("gain"), 0.0, 4.0, 1.0)
                .expect("hard-coded gain parameter range is valid"),
        }
    }

    /// Sets linear gain.
    pub fn set_gain(&mut self, gain: f32) {
        self.gain
            .set_value(gain)
            .expect("gain must be finite and in range 0.0..=4.0");
    }

    /// Returns linear gain.
    pub fn gain(&self) -> f32 {
        self.gain.value()
    }
}

impl Default for GainProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioProcessor for GainProcessor {
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let gain = self.gain();
        for channel in ctx.buffer.channels_mut() {
            for sample in channel {
                *sample *= gain;
            }
        }
    }

    fn reset(&mut self) {}
}

/// Feedback delay processor with dry/wet mix.
#[derive(Debug, Clone)]
pub struct DelayProcessor {
    sample_rate: f32,
    delay_ms: f32,
    feedback: f32,
    mix: f32,
    lines: Vec<DelayLine>,
}

impl DelayProcessor {
    const MAX_DELAY_SECONDS: f32 = 2.0;

    /// Creates a delay processor.
    pub fn new(delay_ms: f32, feedback: f32, mix: f32) -> Self {
        let mut processor = Self {
            sample_rate: 44_100.0,
            delay_ms: delay_ms.clamp(0.0, Self::MAX_DELAY_SECONDS * 1_000.0),
            feedback: feedback.clamp(0.0, 0.95),
            mix: mix.clamp(0.0, 1.0),
            lines: Vec::new(),
        };
        processor.update_delay_samples();
        processor
    }

    /// Sets the delay time in milliseconds.
    pub fn set_delay_ms(&mut self, delay_ms: f32) {
        self.delay_ms = delay_ms.clamp(0.0, Self::MAX_DELAY_SECONDS * 1_000.0);
        self.update_delay_samples();
    }

    /// Returns the delay time in milliseconds.
    pub fn delay_ms(&self) -> f32 {
        self.delay_ms
    }

    /// Sets feedback amount in `0.0..=0.95`.
    pub fn set_feedback(&mut self, feedback: f32) {
        self.feedback = feedback.clamp(0.0, 0.95);
    }

    /// Returns feedback amount.
    pub fn feedback(&self) -> f32 {
        self.feedback
    }

    /// Sets wet mix in `0.0..=1.0`.
    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Returns wet mix.
    pub fn mix(&self) -> f32 {
        self.mix
    }

    fn max_delay_samples(&self) -> usize {
        (self.sample_rate * Self::MAX_DELAY_SECONDS).round() as usize
    }

    fn delay_samples(&self) -> usize {
        ((self.delay_ms / 1_000.0) * self.sample_rate).round() as usize
    }

    fn update_delay_samples(&mut self) {
        let delay_samples = self.delay_samples();
        for line in &mut self.lines {
            line.set_delay_samples(delay_samples);
        }
    }
}

impl Default for DelayProcessor {
    fn default() -> Self {
        Self::new(250.0, 0.25, 0.35)
    }
}

impl AudioProcessor for DelayProcessor {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate.max(1.0);
        let max_delay_samples = self.max_delay_samples();
        for line in &mut self.lines {
            if line.max_delay_samples() != max_delay_samples {
                *line = DelayLine::new(max_delay_samples);
            }
        }
        self.update_delay_samples();
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let channels = ctx.buffer.num_channels();
        let max_delay_samples = self.max_delay_samples();
        self.lines
            .resize_with(channels, || DelayLine::new(max_delay_samples));
        self.update_delay_samples();

        for (channel_index, channel) in ctx.buffer.channels_mut().enumerate() {
            let line = &mut self.lines[channel_index];
            for sample in channel {
                let input = *sample;
                let delayed = line.process_sample_with_feedback(input, self.feedback);
                *sample = input * (1.0 - self.mix) + delayed * self.mix;
            }
        }
    }

    fn reset(&mut self) {
        for line in &mut self.lines {
            line.reset();
        }
    }
}

/// Soft-clipping distortion processor.
#[derive(Debug, Clone)]
pub struct DistortionProcessor {
    drive: f32,
    mix: f32,
}

impl DistortionProcessor {
    /// Creates a distortion processor.
    pub fn new(drive: f32, mix: f32) -> Self {
        Self {
            drive: drive.clamp(1.0, 20.0),
            mix: mix.clamp(0.0, 1.0),
        }
    }

    /// Sets pre-clip drive in `1.0..=20.0`.
    pub fn set_drive(&mut self, drive: f32) {
        self.drive = drive.clamp(1.0, 20.0);
    }

    /// Returns pre-clip drive.
    pub fn drive(&self) -> f32 {
        self.drive
    }

    /// Sets wet mix in `0.0..=1.0`.
    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Returns wet mix.
    pub fn mix(&self) -> f32 {
        self.mix
    }

    /// Processes one sample.
    pub fn process_sample(&self, input: f32) -> f32 {
        let normalizer = self.drive.tanh().max(f32::EPSILON);
        let wet = (input * self.drive).tanh() / normalizer;
        input * (1.0 - self.mix) + wet * self.mix
    }
}

impl Default for DistortionProcessor {
    fn default() -> Self {
        Self::new(3.0, 1.0)
    }
}

impl AudioProcessor for DistortionProcessor {
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        for channel in ctx.buffer.channels_mut() {
            for sample in channel {
                *sample = self.process_sample(*sample);
            }
        }
    }

    fn reset(&mut self) {}
}

/// Fixed-capacity circular delay line for `f32` samples.
#[derive(Debug, Clone)]
pub struct DelayLine {
    buffer: Vec<f32>,
    write_pos: usize,
    delay_samples: usize,
}

impl DelayLine {
    /// Creates a delay line with the given maximum delay in samples.
    pub fn new(max_delay_samples: usize) -> Self {
        let capacity = max_delay_samples.saturating_add(1).max(1);
        Self {
            buffer: vec![0.0; capacity],
            write_pos: 0,
            delay_samples: 0,
        }
    }

    /// Sets the current delay in samples, clamped to the delay line capacity.
    pub fn set_delay_samples(&mut self, delay_samples: usize) {
        self.delay_samples = delay_samples.min(self.max_delay_samples());
    }

    /// Returns the maximum delay in samples.
    pub fn max_delay_samples(&self) -> usize {
        self.buffer.len().saturating_sub(1)
    }

    /// Pushes one sample and returns the delayed output.
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let read_pos =
            (self.write_pos + self.buffer.len() - self.delay_samples) % self.buffer.len();
        let output = self.buffer[read_pos];
        self.buffer[self.write_pos] = input;
        self.write_pos = (self.write_pos + 1) % self.buffer.len();
        output
    }

    /// Pushes one sample plus delayed feedback and returns the delayed output.
    pub fn process_sample_with_feedback(&mut self, input: f32, feedback: f32) -> f32 {
        let read_pos =
            (self.write_pos + self.buffer.len() - self.delay_samples) % self.buffer.len();
        let output = self.buffer[read_pos];
        self.buffer[self.write_pos] = input + output * feedback.clamp(0.0, 0.95);
        self.write_pos = (self.write_pos + 1) % self.buffer.len();
        output
    }

    /// Clears the delay memory.
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.write_pos = 0;
    }
}

/// Sequential processor container.
#[derive(Default)]
pub struct ProcessorChain {
    processors: Vec<Box<dyn AudioProcessor>>,
}

impl ProcessorChain {
    /// Creates an empty processor chain.
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// Adds a processor to the end of the chain.
    ///
    /// This allocates and should be called during setup, not from the realtime audio thread.
    pub fn push<P>(&mut self, processor: P)
    where
        P: AudioProcessor + 'static,
    {
        self.processors.push(Box::new(processor));
    }

    /// Returns the number of processors in the chain.
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// Returns `true` when the chain contains no processors.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }
}

impl AudioProcessor for ProcessorChain {
    fn prepare(&mut self, sample_rate: f32, max_block_size: usize) {
        for processor in &mut self.processors {
            processor.prepare(sample_rate, max_block_size);
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        for processor in &mut self.processors {
            processor.process(ctx);
        }
    }

    fn reset(&mut self) {
        for processor in &mut self.processors {
            processor.reset();
        }
    }
}

/// One-pole low-pass filter.
#[derive(Debug, Clone)]
pub struct OnePoleLowPass {
    sample_rate: f32,
    cutoff_hz: f32,
    coefficient: f32,
    z1: Vec<f32>,
}

impl OnePoleLowPass {
    /// Creates a one-pole low-pass filter.
    pub fn new(cutoff_hz: f32) -> Self {
        let mut filter = Self {
            sample_rate: 44_100.0,
            cutoff_hz,
            coefficient: 0.0,
            z1: Vec::new(),
        };
        filter.update_coefficient();
        filter
    }

    /// Sets the cutoff frequency in Hz.
    pub fn set_cutoff_hz(&mut self, cutoff_hz: f32) {
        self.cutoff_hz = cutoff_hz.max(0.0);
        self.update_coefficient();
    }

    /// Processes one sample for `channel`.
    pub fn process_sample(&mut self, channel: usize, input: f32) -> f32 {
        if channel >= self.z1.len() {
            return input;
        }

        let output = self.z1[channel] + self.coefficient * (input - self.z1[channel]);
        self.z1[channel] = output;
        output
    }

    fn update_coefficient(&mut self) {
        if self.sample_rate <= 0.0 || self.cutoff_hz <= 0.0 {
            self.coefficient = 0.0;
            return;
        }

        let x = (-2.0 * core::f32::consts::PI * self.cutoff_hz / self.sample_rate).exp();
        self.coefficient = 1.0 - x;
    }
}

impl AudioProcessor for OnePoleLowPass {
    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.update_coefficient();
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        if self.z1.len() != ctx.buffer.num_channels() {
            self.z1.resize(ctx.buffer.num_channels(), 0.0);
        }

        for (channel_index, channel) in ctx.buffer.channels_mut().enumerate() {
            for sample in channel {
                *sample = self.process_sample(channel_index, *sample);
            }
        }
    }

    fn reset(&mut self) {
        self.z1.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabjuice_audio::AudioBuffer;

    fn process<P: AudioProcessor>(processor: &mut P, buffer: &mut AudioBuffer<f32>) {
        let mut ctx = ProcessContext::new(buffer, &[]);
        processor.process(&mut ctx);
    }

    #[test]
    fn gain_processor_multiplies_samples() {
        let mut buffer = AudioBuffer::new(2, 3);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[1.0, -0.5, 0.25]);
        buffer
            .channel_mut(1)
            .expect("channel exists")
            .copy_from_slice(&[0.5, 0.0, -1.0]);

        let mut gain = GainProcessor::new();
        gain.set_gain(2.0);
        process(&mut gain, &mut buffer);

        assert_eq!(
            buffer.channel(0).expect("channel exists"),
            &[2.0, -1.0, 0.5]
        );
        assert_eq!(
            buffer.channel(1).expect("channel exists"),
            &[1.0, 0.0, -2.0]
        );
    }

    #[test]
    fn delay_line_outputs_delayed_samples() {
        let mut delay = DelayLine::new(2);
        delay.set_delay_samples(2);

        assert_eq!(delay.process_sample(1.0), 0.0);
        assert_eq!(delay.process_sample(2.0), 0.0);
        assert_eq!(delay.process_sample(3.0), 1.0);
        assert_eq!(delay.process_sample(4.0), 2.0);

        delay.reset();
        assert_eq!(delay.process_sample(5.0), 0.0);
    }

    #[test]
    fn delay_line_applies_feedback() {
        let mut delay = DelayLine::new(1);
        delay.set_delay_samples(1);

        assert_eq!(delay.process_sample_with_feedback(1.0, 0.5), 0.0);
        assert_eq!(delay.process_sample_with_feedback(0.0, 0.5), 1.0);
        assert_eq!(delay.process_sample_with_feedback(0.0, 0.5), 0.5);
    }

    #[test]
    fn delay_processor_outputs_delayed_signal() {
        let mut buffer = AudioBuffer::new(1, 4);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[1.0, 0.0, 0.0, 0.0]);

        let mut delay = DelayProcessor::new(2.0, 0.0, 1.0);
        delay.prepare(1_000.0, 4);
        process(&mut delay, &mut buffer);

        assert_eq!(
            buffer.channel(0).expect("channel exists"),
            &[0.0, 0.0, 1.0, 0.0]
        );
    }

    #[test]
    fn distortion_processor_soft_clips_symmetrically() {
        let mut buffer = AudioBuffer::new(1, 3);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[-0.5, 0.0, 0.5]);

        let mut distortion = DistortionProcessor::new(4.0, 1.0);
        process(&mut distortion, &mut buffer);

        let channel = buffer.channel(0).expect("channel exists");
        assert!(channel[0] < -0.5);
        assert_eq!(channel[1], 0.0);
        assert!(channel[2] > 0.5);
        assert!((channel[0] + channel[2]).abs() < f32::EPSILON);
    }

    #[test]
    fn processor_chain_runs_in_order() {
        let mut buffer = AudioBuffer::new(1, 2);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[0.5, 1.0]);

        let mut first = GainProcessor::new();
        first.set_gain(2.0);
        let mut second = GainProcessor::new();
        second.set_gain(0.5);

        let mut chain = ProcessorChain::new();
        chain.push(first);
        chain.push(second);
        process(&mut chain, &mut buffer);

        assert_eq!(buffer.channel(0).expect("channel exists"), &[0.5, 1.0]);
    }

    #[test]
    fn one_pole_low_pass_smooths_step() {
        let mut buffer = AudioBuffer::new(1, 4);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[0.0, 1.0, 1.0, 1.0]);

        let mut filter = OnePoleLowPass::new(1_000.0);
        filter.prepare(48_000.0, 4);
        process(&mut filter, &mut buffer);

        let channel = buffer.channel(0).expect("channel exists");
        assert_eq!(channel[0], 0.0);
        assert!(channel[1] > 0.0 && channel[1] < 1.0);
        assert!(channel[2] > channel[1]);
        assert!(channel[3] > channel[2]);
    }
}
