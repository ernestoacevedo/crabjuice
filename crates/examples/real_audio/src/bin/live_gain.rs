use anyhow::{bail, Context, Result};
use cpal::{Device, Host};
use real_audio::{
    default_input_index, default_output_index, input_devices, new_gain_processor, output_devices,
    select_input_device, select_output_device, start_live_audio, DeviceInfo,
};
use std::io::{self, Write};

fn main() -> Result<()> {
    let host = cpal::default_host();
    let input_device = select_device(&host, true)?;
    let output_device = select_device(&host, false)?;
    let gain = prompt_gain()?;
    let processor = new_gain_processor(gain);
    let session = start_live_audio(input_device, output_device, processor)?;

    println!(
        "Input: {} ({:?}, {} Hz, {} channels)",
        session.input_name,
        session.input_config.sample_format(),
        session.input_config.sample_rate().0,
        session.input_config.channels()
    );
    println!(
        "Output: {} ({:?}, {} Hz, {} channels)",
        session.output_name,
        session.output_config.sample_format(),
        session.output_config.sample_rate().0,
        session.output_config.channels()
    );
    if session.input_config.sample_rate() != session.output_config.sample_rate() {
        println!("Input and output sample rates differ; this example does not resample.");
    }

    session.play()?;

    println!("Streaming. Press Enter to stop.");
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to wait for Enter")?;

    Ok(())
}

fn select_device(host: &Host, input: bool) -> Result<Device> {
    let label = if input { "input" } else { "output" };
    let devices = if input {
        input_devices(host)?
    } else {
        output_devices(host)?
    };
    let default_index = if input {
        default_input_index(host, &devices)
    } else {
        default_output_index(host, &devices)
    };

    println!("Available {label} devices:");
    for device in &devices {
        let marker = if device.is_default { " [default]" } else { "" };
        println!("  {}: {}{}", device.index, device.name, marker);
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
            if let Some(index) = default_index {
                return select_real_device(host, input, index);
            }
            println!("No default {label} device is available.");
            continue;
        }

        match value.parse::<usize>() {
            Ok(index) if contains_index(&devices, index) => {
                return select_real_device(host, input, index);
            }
            _ => println!("Enter a valid device index."),
        }
    }
}

fn select_real_device(host: &Host, input: bool, index: usize) -> Result<Device> {
    if input {
        select_input_device(host, index)
    } else {
        select_output_device(host, index)
    }
}

fn contains_index(devices: &[DeviceInfo], index: usize) -> bool {
    devices.iter().any(|device| device.index == index)
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
