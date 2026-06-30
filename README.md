# crabjuice

`crabjuice` is a small Rust workspace for experimenting with audio processing
building blocks. It currently provides core parameter types, MIDI event models,
non-interleaved audio buffers, a realtime-style processor trait, and a few DSP
processors.

The project is early-stage and focused on reusable library crates rather than a
finished audio application or plugin host.

## Workspace

| Crate | Purpose |
| --- | --- |
| `crabjuice-core` | Shared error type, parameter identifiers, and floating-point parameter handling. |
| `crabjuice-midi` | Basic MIDI event and message representation with block-relative sample offsets. |
| `crabjuice-audio` | Channel-major `AudioBuffer<f32>`, `ProcessContext`, and `AudioProcessor` trait. |
| `crabjuice-dsp` | DSP building blocks such as gain, delay line, processor chain, and one-pole low-pass filter. |
| `basic_gain` | Example binary that processes a stereo buffer with `GainProcessor`. |

## Requirements

- Rust 1.75 or newer
- Cargo

The workspace uses Rust 2021 edition.

## Quick Start

Run the full test suite:

```sh
cargo test
```

Run the basic gain example:

```sh
cargo run -p basic_gain
```

Expected example output:

```text
before: left=[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8], right=[-0.1, -0.2, -0.3, -0.4, -0.5, -0.6, -0.7, -0.8]
after:  left=[0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4], right=[-0.05, -0.1, -0.15, -0.2, -0.25, -0.3, -0.35, -0.4]
```

## Example

```rust
use crabjuice_audio::{AudioBuffer, AudioProcessor, ProcessContext};
use crabjuice_dsp::GainProcessor;

let mut buffer = AudioBuffer::new(2, 128);
let mut gain = GainProcessor::new();

gain.set_gain(0.5);
gain.prepare(48_000.0, buffer.num_samples());

let mut ctx = ProcessContext::new(&mut buffer, &[]);
gain.process(&mut ctx);
```

## Architecture Notes

- Audio buffers are non-interleaved and channel-major: each channel owns a
  contiguous `Vec<f32>`.
- `AudioProcessor::prepare` is intended for setup that depends on sample rate or
  maximum block size.
- `AudioProcessor::process` receives a mutable audio block and a slice of MIDI
  events for that block.
- `ProcessorChain::push` allocates and should be used during setup, not from a
  realtime audio callback.
- `FloatParameter` stores plain values and provides linear conversion to and from
  normalized `0.0..=1.0` values.

## Development

Useful commands:

```sh
cargo fmt
cargo clippy --all-targets --all-features
cargo test
```

Run tests for a single crate:

```sh
cargo test -p crabjuice-dsp
```

Run the example binary:

```sh
cargo run -p basic_gain
```

## Current DSP Components

- `GainProcessor`: multiplies every sample by a linear gain value in
  `0.0..=4.0`.
- `DelayLine`: fixed-capacity circular delay line for `f32` samples.
- `ProcessorChain`: sequential container for boxed `AudioProcessor`
  implementations.
- `OnePoleLowPass`: simple one-pole low-pass filter with per-channel state.

## License

The workspace metadata declares dual licensing under `MIT OR Apache-2.0`.
