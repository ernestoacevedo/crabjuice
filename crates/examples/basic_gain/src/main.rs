use crabjuice_audio::{AudioBuffer, AudioProcessor, ProcessContext};
use crabjuice_dsp::GainProcessor;

fn main() {
    let mut buffer = AudioBuffer::new(2, 8);

    for channel_index in 0..buffer.num_channels() {
        let channel = buffer
            .channel_mut(channel_index)
            .expect("channel index is within buffer channel count");

        for (sample_index, sample) in channel.iter_mut().enumerate() {
            *sample = (sample_index as f32 + 1.0) * if channel_index == 0 { 0.1 } else { -0.1 };
        }
    }

    println!(
        "before: left={:?}, right={:?}",
        buffer.channel(0).expect("left channel exists"),
        buffer.channel(1).expect("right channel exists")
    );

    let mut gain = GainProcessor::new();
    gain.set_gain(0.5);
    gain.prepare(48_000.0, buffer.num_samples());

    let mut ctx = ProcessContext::new(&mut buffer, &[]);
    gain.process(&mut ctx);

    println!(
        "after:  left={:?}, right={:?}",
        buffer.channel(0).expect("left channel exists"),
        buffer.channel(1).expect("right channel exists")
    );
}
