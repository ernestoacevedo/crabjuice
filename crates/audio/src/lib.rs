//! Audio buffers and processing traits.

use crabjuice_midi::MidiEvent;
use std::fmt;

/// Errors returned when moving samples between interleaved and channel-major buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioBufferError {
    /// The interleaved slice length does not match `channels * samples`.
    InterleavedLengthMismatch {
        expected: usize,
        actual: usize,
        channels: usize,
        samples: usize,
    },
}

impl fmt::Display for AudioBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InterleavedLengthMismatch {
                expected,
                actual,
                channels,
                samples,
            } => write!(
                f,
                "interleaved sample length mismatch: expected {expected} samples for {channels} channels x {samples} samples, got {actual}"
            ),
        }
    }
}

impl std::error::Error for AudioBufferError {}

/// Non-interleaved channel-major audio buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer<T> {
    channels: Vec<Vec<T>>,
    num_samples: usize,
}

impl AudioBuffer<f32> {
    /// Creates a zero-filled buffer with `num_channels` channels and `num_samples` samples.
    pub fn new(num_channels: usize, num_samples: usize) -> Self {
        Self {
            channels: vec![vec![0.0; num_samples]; num_channels],
            num_samples,
        }
    }

    /// Returns the number of channels.
    pub fn num_channels(&self) -> usize {
        self.channels.len()
    }

    /// Returns the number of samples per channel.
    pub fn num_samples(&self) -> usize {
        self.num_samples
    }

    /// Resizes the buffer and fills newly created samples with silence.
    pub fn resize(&mut self, num_channels: usize, num_samples: usize) {
        self.channels
            .resize_with(num_channels, || vec![0.0; num_samples]);

        for channel in &mut self.channels {
            channel.resize(num_samples, 0.0);
        }

        self.num_samples = num_samples;
    }

    /// Returns an immutable channel slice, or `None` if out of range.
    pub fn channel(&self, index: usize) -> Option<&[f32]> {
        self.channels.get(index).map(Vec::as_slice)
    }

    /// Returns a mutable channel slice, or `None` if out of range.
    pub fn channel_mut(&mut self, index: usize) -> Option<&mut [f32]> {
        self.channels.get_mut(index).map(Vec::as_mut_slice)
    }

    /// Clears all samples to zero.
    pub fn clear(&mut self) {
        for channel in &mut self.channels {
            channel.fill(0.0);
        }
    }

    /// Returns mutable channel slices for realtime processing.
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        self.channels.iter_mut().map(Vec::as_mut_slice)
    }

    /// Copies interleaved samples into this channel-major buffer.
    pub fn copy_from_interleaved(&mut self, input: &[f32]) -> Result<(), AudioBufferError> {
        self.check_interleaved_len(input.len())?;

        for (frame_index, frame) in input.chunks_exact(self.num_channels()).enumerate() {
            for (channel_index, sample) in frame.iter().enumerate() {
                self.channels[channel_index][frame_index] = *sample;
            }
        }

        Ok(())
    }

    /// Copies this channel-major buffer into an interleaved output slice.
    pub fn copy_to_interleaved(&self, output: &mut [f32]) -> Result<(), AudioBufferError> {
        self.check_interleaved_len(output.len())?;

        for frame_index in 0..self.num_samples {
            for channel_index in 0..self.num_channels() {
                output[frame_index * self.num_channels() + channel_index] =
                    self.channels[channel_index][frame_index];
            }
        }

        Ok(())
    }

    fn check_interleaved_len(&self, actual: usize) -> Result<(), AudioBufferError> {
        let channels = self.num_channels();
        let samples = self.num_samples();
        let expected = channels * samples;

        if actual == expected {
            Ok(())
        } else {
            Err(AudioBufferError::InterleavedLengthMismatch {
                expected,
                actual,
                channels,
                samples,
            })
        }
    }
}

/// Per-block context passed to an audio processor.
pub struct ProcessContext<'a> {
    /// Audio block being processed.
    pub buffer: &'a mut AudioBuffer<f32>,
    /// MIDI events for this block.
    pub midi_events: &'a [MidiEvent],
}

impl<'a> ProcessContext<'a> {
    /// Creates a process context from an audio buffer and MIDI event slice.
    pub fn new(buffer: &'a mut AudioBuffer<f32>, midi_events: &'a [MidiEvent]) -> Self {
        Self {
            buffer,
            midi_events,
        }
    }
}

/// Realtime audio processor interface.
pub trait AudioProcessor {
    /// Prepares the processor for a sample rate and maximum block size.
    fn prepare(&mut self, sample_rate: f32, max_block_size: usize);

    /// Processes one audio block.
    fn process(&mut self, ctx: &mut ProcessContext<'_>);

    /// Resets internal state.
    fn reset(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_buffer_is_channel_major() {
        let mut buffer = AudioBuffer::new(2, 4);
        buffer.channel_mut(1).expect("channel exists")[2] = 0.75;

        assert_eq!(buffer.num_channels(), 2);
        assert_eq!(buffer.num_samples(), 4);
        assert_eq!(buffer.channel(0).expect("channel exists"), &[0.0; 4]);
        assert_eq!(
            buffer.channel(1).expect("channel exists"),
            &[0.0, 0.0, 0.75, 0.0]
        );
    }

    #[test]
    fn clear_zeroes_samples() {
        let mut buffer = AudioBuffer::new(1, 2);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[1.0, -1.0]);

        buffer.clear();

        assert_eq!(buffer.channel(0).expect("channel exists"), &[0.0, 0.0]);
    }

    #[test]
    fn copy_from_interleaved_writes_channel_major_samples() {
        let mut buffer = AudioBuffer::new(2, 3);

        buffer
            .copy_from_interleaved(&[0.1, -0.1, 0.2, -0.2, 0.3, -0.3])
            .expect("input length matches buffer shape");

        assert_eq!(
            buffer.channel(0).expect("left channel exists"),
            &[0.1, 0.2, 0.3]
        );
        assert_eq!(
            buffer.channel(1).expect("right channel exists"),
            &[-0.1, -0.2, -0.3]
        );
    }

    #[test]
    fn copy_to_interleaved_writes_frame_major_samples() {
        let mut buffer = AudioBuffer::new(2, 3);
        buffer
            .channel_mut(0)
            .expect("left channel exists")
            .copy_from_slice(&[0.1, 0.2, 0.3]);
        buffer
            .channel_mut(1)
            .expect("right channel exists")
            .copy_from_slice(&[-0.1, -0.2, -0.3]);
        let mut output = [0.0; 6];

        buffer
            .copy_to_interleaved(&mut output)
            .expect("output length matches buffer shape");

        assert_eq!(output, [0.1, -0.1, 0.2, -0.2, 0.3, -0.3]);
    }

    #[test]
    fn interleaved_copy_errors_when_length_does_not_match_shape() {
        let mut buffer = AudioBuffer::new(2, 3);

        let error = buffer
            .copy_from_interleaved(&[0.0; 5])
            .expect_err("input length should not match buffer shape");

        assert_eq!(
            error,
            AudioBufferError::InterleavedLengthMismatch {
                expected: 6,
                actual: 5,
                channels: 2,
                samples: 3,
            }
        );
    }

    #[test]
    fn resize_updates_shape_and_zero_fills_new_samples() {
        let mut buffer = AudioBuffer::new(1, 2);
        buffer
            .channel_mut(0)
            .expect("channel exists")
            .copy_from_slice(&[0.5, -0.5]);

        buffer.resize(2, 4);

        assert_eq!(buffer.num_channels(), 2);
        assert_eq!(buffer.num_samples(), 4);
        assert_eq!(
            buffer.channel(0).expect("first channel exists"),
            &[0.5, -0.5, 0.0, 0.0]
        );
        assert_eq!(
            buffer.channel(1).expect("second channel exists"),
            &[0.0, 0.0, 0.0, 0.0]
        );
    }
}
