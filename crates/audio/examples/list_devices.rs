use magnetic_tape_audio::enumerate_audio_devices;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let snapshot = enumerate_audio_devices()?;

    println!("Microphones:");
    for device in snapshot.microphones {
        println!(
            "  {}{} — {} Hz, {} ch, {}",
            device.name,
            if device.is_default { " [default]" } else { "" },
            device.sample_rate,
            device.channels,
            device.sample_format
        );
    }

    println!("System outputs:");
    for device in snapshot.system_outputs {
        println!(
            "  {}{} — {} Hz, {} ch, {}",
            device.name,
            if device.is_default { " [default]" } else { "" },
            device.sample_rate,
            device.channels,
            device.sample_format
        );
    }

    Ok(())
}
