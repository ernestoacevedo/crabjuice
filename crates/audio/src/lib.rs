//! Audio buffers and processing traits.

use crabjuice_midi::MidiEvent;

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
}
