use anyhow::{Context, Result};
use real_audio::process_wav_file;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() -> Result<()> {
    let input_path = prompt_required_path("Input WAV path")?;
    let output_path = prompt_required_path("Output WAV path")?;
    let gain = prompt_gain()?;

    process_wav_file(&input_path, &output_path, gain)
}

fn prompt_required_path(label: &str) -> Result<PathBuf> {
    loop {
        print!("{label}: ");
        io::stdout().flush().context("failed to flush stdout")?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("failed to read path")?;
        let value = line.trim();

        if !value.is_empty() {
            return Ok(PathBuf::from(value));
        }
    }
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
