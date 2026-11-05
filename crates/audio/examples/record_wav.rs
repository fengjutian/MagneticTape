use magnetic_tape_audio::{AudioRecording, enumerate_audio_devices};
use std::{path::PathBuf, thread, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let duration_seconds = arguments
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(2);
    let devices = enumerate_audio_devices()?;
    let microphone = devices
        .microphones
        .iter()
        .find(|device| device.is_default)
        .or(devices.microphones.first())
        .ok_or("no microphone available")?;
    let system = devices
        .system_outputs
        .iter()
        .find(|device| device.is_default)
        .or(devices.system_outputs.first())
        .ok_or("no system output available")?;
    let output = arguments.map(PathBuf::from).next().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/audio-smoke")
    });
    let recording = AudioRecording::start(&microphone.id, &system.id, &output)?;
    thread::sleep(Duration::from_secs(duration_seconds));
    let result = recording.finish()?;
    println!("{}", result.microphone_path.display());
    println!("{}", result.system_path.display());
    println!("dropped blocks: {}", result.dropped_blocks);
    Ok(())
}
