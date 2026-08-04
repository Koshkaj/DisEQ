//! Reports every output device and whether its volume can actually be set.

use kd_sys::audio;

fn main() {
    let default = audio::default_output_device();
    println!("default output: {default:?}\n");

    for device in audio::all_devices() {
        if !audio::has_output(device) {
            continue;
        }
        let name = audio::device_name(device).unwrap_or_else(|| "<unnamed>".into());
        println!("── {name} (id {device})");
        println!("   default:  {}", Some(device) == default);
        println!("   volume:   {:?}", audio::volume(device));
        println!("   settable: {}", audio::volume_is_settable(device));
        println!("   muted:    {:?}", audio::is_muted(device));
    }
}
