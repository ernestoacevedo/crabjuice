//! Basic MIDI event representation.

/// MIDI event with a block-relative sample offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiEvent {
    /// Offset in samples from the start of the current audio block.
    pub sample_offset: usize,
    /// MIDI message payload.
    pub message: MidiMessage,
}

impl MidiEvent {
    /// Creates a MIDI event.
    pub const fn new(sample_offset: usize, message: MidiMessage) -> Self {
        Self {
            sample_offset,
            message,
        }
    }
}

/// Supported MIDI message variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiMessage {
    /// Note-on event.
    NoteOn { channel: u8, note: u8, velocity: u8 },
    /// Note-off event.
    NoteOff { channel: u8, note: u8, velocity: u8 },
    /// MIDI control change event.
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// Pitch bend event using the MIDI 14-bit range `0..=16383`.
    PitchBend { channel: u8, value: u16 },
    /// Raw MIDI bytes for messages not modeled yet.
    Raw(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_keeps_sample_offset() {
        let event = MidiEvent::new(
            12,
            MidiMessage::NoteOn {
                channel: 1,
                note: 60,
                velocity: 100,
            },
        );

        assert_eq!(event.sample_offset, 12);
    }
}
