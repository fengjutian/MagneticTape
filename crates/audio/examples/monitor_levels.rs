use magnetic_tape_audio::{AudioDeviceInfo, AudioLevelMonitor, enumerate_audio_devices};
use std::{thread, time::Duration};

fn preferred(devices: &[AudioDeviceInfo]) -> Option<&AudioDeviceInfo> {
    devices
        .iter()
        .find(|device| device.is_default)
        .or(devices.first())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let devices = enumerate_audio_devices()?;
    let microphone = preferred(&devices.microphones).ok_or("no microphone is available")?;
    let output = preferred(&devices.system_outputs).ok_or("no system output is available")?;
    println!("Microphone: {}", microphone.name);
    println!("System output: {}", output.name);

    let monitor = AudioLevelMonitor::start(&microphone.id, &output.id)?;
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(100));
        let levels = monitor.levels();
        println!("mic={:.3} system={:.3}", levels.microphone, levels.system);
    }
    Ok(())
}
