use magnetic_tape_audio::{AudioRecording, enumerate_audio_devices};
use std::{path::PathBuf, thread, time::Duration};

/// Smoke test for the resilience path: record, rebuild both streams while the
/// writer stays alive, then record again and verify the session is still valid.
fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/audio-rebuild");
    std::fs::create_dir_all(&output)?;
    let mut recording = AudioRecording::start(&microphone.id, &system.id, &output)?;
    thread::sleep(Duration::from_secs(1));
    recording.rebuild()?;
    thread::sleep(Duration::from_secs(1));
    let result = recording.finish()?;
    for chunk in &result.chunks {
        println!(
            "{:?} {} {} ms",
            chunk.track,
            chunk.path.display(),
            chunk.duration_ms
        );
    }
    println!("dropped blocks: {}", result.dropped_blocks);
    Ok(())
}
