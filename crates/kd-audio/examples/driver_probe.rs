//! Reports whether the DisEQ HAL plug-in is installed, loaded, and
//! behaving. Copying the bundle into place is not the same as coreaudiod
//! agreeing to load it, and this tells the two apart.
//!
//!     cargo run -p kd-audio --example driver_probe
//!
//! Exit status is non-zero when the device is absent, so install scripts can
//! use it as a check.

use kd_audio::devices::{self, Device};
use kd_audio::router;
use kd_sys::audio;

const INSTALL_PATH: &str = "/Library/Audio/Plug-Ins/HAL/DisEQ.driver";

fn arg(name: &str) -> Option<String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let at = arguments.iter().position(|argument| argument == name)?;
    arguments.get(at + 1).cloned()
}

fn main() {
    let installed = std::path::Path::new(INSTALL_PATH).exists();
    println!("bundle at {INSTALL_PATH}: {}", yes_no(installed));

    println!(
        "plug-in object: {:?} (bundle {})",
        kd_sys::audio::plugin_for_bundle_id(devices::DRIVER_BUNDLE_ID),
        devices::DRIVER_BUNDLE_ID
    );
    if std::env::args().any(|argument| argument == "--publish") {
        println!("publishing: {:?}", devices::publish().map(|d| d.name));
    }
    if std::env::args().any(|argument| argument == "--unpublish") {
        println!("unpublishing: {}", devices::unpublish());
    }

    if std::env::args().any(|argument| argument == "--claim") {
        devices::unpublish();
        let gap: u64 = arg("--gap")
            .and_then(|value| value.parse().ok())
            .unwrap_or(500);
        std::thread::sleep(std::time::Duration::from_millis(gap));
        let started = std::time::Instant::now();
        let published = devices::publish();
        println!(
            "publish took {:?} -> {:?}",
            started.elapsed(),
            published.as_ref().map(|d| d.id)
        );
        if let Some(device) = &published {
            println!(
                "  returned id {} alive {} uid {:?} name {:?}",
                device.id,
                audio::is_alive(device.id),
                audio::device_uid(device.id),
                audio::device_name(device.id)
            );
        }
        for candidate in devices::all() {
            if candidate.uid.as_deref() == Some(devices::DISEQ_UID) {
                println!(
                    "  enumerated {} alive {} name {:?}",
                    candidate.id,
                    audio::is_alive(candidate.id),
                    candidate.name
                );
            }
        }
        if let Some(device) = &published {
            for attempt in 0..20 {
                let call = audio::set_default_output_device(device.id);
                let now = audio::default_output_device();
                println!(
                    "  attempt {attempt}: call {call}, default {now:?}, at {:?}",
                    started.elapsed()
                );
                if now == Some(device.id) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        return;
    }

    if let Some(wanted) = arg("--use") {
        match router::targets()
            .into_iter()
            .find(|device| device.name.to_lowercase().contains(&wanted.to_lowercase()))
        {
            Some(chosen) => {
                let moved = devices::set_default_output(&chosen);
                kd_sys::audio::set_default_system_output_device(chosen.id);
                println!("\ndefault output: {} ({})", chosen.name, yes_no(moved));
            }
            None => println!("\nno output device matching {wanted:?}"),
        }
    }

    let Some(device) = devices::diseq() else {
        println!("device:  absent\n");
        if installed {
            println!("The bundle is in place but coreaudiod did not publish its device.");
            println!("Usual causes, in order of likelihood:");
            println!("  - coreaudiod has not been restarted: sudo killall coreaudiod");
            println!("  - the bundle is unsigned, or its signature does not validate");
            println!("  - Info.plist is missing sandboxSafe, CFPlugInFactories or CFPlugInTypes");
            println!(
                "  - the factory function is not exported under the name in CFPlugInFactories"
            );
            println!("\nWhat coreaudiod thought of it:");
            println!(
                "  log show --last 5m --predicate 'process == \"coreaudiod\"' | grep -i DisEQ"
            );
        } else {
            println!("Build it with 'make driver', then install with 'make driver-install'.");
        }
        std::process::exit(1);
    };

    println!("device:  present\n");
    if std::env::args().any(|argument| argument == "--make-default") {
        let moved = audio::set_default_output_device(device.id);
        let system = audio::set_default_system_output_device(device.id);
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!(
            "\nmake default: output call {}, system call {}, now output {:?} system {:?}",
            yes_no(moved),
            yes_no(system),
            audio::default_output_device(),
            audio::default_system_output_device()
        );
    }

    report(&device);

    // Sample-rate changes go around through the host, which stops IO first —
    // so this is worth checking with the device in use, not only idle.
    if let Some(wanted) = arg("--rate").and_then(|value| value.parse::<f64>().ok()) {
        let before = device.sample_rate.unwrap_or(0.0);
        let accepted = audio::set_nominal_sample_rate(device.id, wanted);
        std::thread::sleep(std::time::Duration::from_millis(400));
        let now = audio::nominal_sample_rate(device.id).unwrap_or(f64::NAN);
        println!(
            "\nrate:    asked {wanted:.0}, call {}, now {now:.0} (was {before:.0}) — {}",
            yes_no(accepted),
            if (now - wanted).abs() < 1.0 {
                "took"
            } else {
                "REFUSED"
            }
        );
    }

    if let Some(wanted) = arg("--set-volume").and_then(|value| value.parse::<f64>().ok()) {
        let moved = audio::set_volume(device.id, wanted);
        std::thread::sleep(std::time::Duration::from_millis(200));
        println!(
            "\nvolume:  wrote {wanted:.2} ({}), now {:.2}",
            yes_no(moved),
            audio::volume(device.id).unwrap_or(f64::NAN)
        );
    }
    println!(
        "default output {:?}, default system output {:?}, ours {}",
        audio::default_output_device(),
        audio::default_system_output_device(),
        device.id
    );

    match kd_audio::shared::SharedRing::open() {
        Ok(ring) => {
            println!(
                "\nshared ring: {} frames, {} ch, {:.0} Hz, running {}, written {}",
                ring.capacity(),
                ring.channels(),
                ring.sample_rate(),
                yes_no(ring.is_running()),
                ring.written()
            );
            if std::env::args().any(|argument| argument == "--shared") {
                // Play something while this runs: it reports what the driver is
                // actually handing over, with no capture API in the path.
                let mut left = vec![0.0f32; 512];
                let mut right = vec![0.0f32; 512];
                for second in 1..=5 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let from = ring.written() - 512;
                    let mut peak = 0.0f32;
                    {
                        let (left, right) = (&mut left[..], &mut right[..]);
                        let mut planes = [left, right];
                        if ring.read(&mut planes, from, 512) {
                            for plane in planes.iter() {
                                for sample in plane.iter() {
                                    peak = peak.max(sample.abs());
                                }
                            }
                        }
                    }
                    let level = if peak > 0.0 {
                        format!("{:>6.1} dB", 20.0 * peak.log10())
                    } else {
                        "  SILENT".into()
                    };
                    println!("  {second}s  written {:>12}  level {level}", ring.written());
                }
            }
        }
        Err(error) => println!("\nshared ring: {error}"),
    }

    println!("\nhardware rates:");
    for other in router::targets() {
        println!(
            "  {:<34} {:.0} Hz",
            other.name,
            other.sample_rate.unwrap_or(0.0)
        );
    }

    // The whole point of the device: a volume that can actually be set.
    let settable = device.volume_is_settable;
    println!(
        "\nvolume control published and settable: {}",
        yes_no(settable)
    );

    if settable {
        let before = device.volume.unwrap_or(1.0);
        let probe = if before > 0.5 { 0.25 } else { 0.75 };
        let moved = audio::set_volume(device.id, probe);
        let readback = audio::volume(device.id).unwrap_or(f64::NAN);
        audio::set_volume(device.id, before);
        println!(
            "  wrote {probe:.2}, read back {readback:.2} ({}), restored {before:.2}",
            if moved && (readback - probe).abs() < 0.05 {
                "matches"
            } else {
                "MISMATCH"
            }
        );
    }

    println!("\noutputs on this machine:");
    let default = audio::default_output_device();
    for output in devices::outputs() {
        let marker = if Some(output.id) == default {
            "→"
        } else {
            " "
        };
        println!(
            "{marker} {:<34} {:<11} volume {}{}",
            truncate(&output.name, 34),
            output.transport_type_name(),
            if output.volume_is_settable {
                "settable"
            } else {
                "fixed"
            },
            if output.needs_software_volume() {
                "   ← needs routing for a slider"
            } else {
                ""
            }
        );
    }

    if devices::eqmac_is_installed() {
        println!("\nnote: eqMac's driver is also installed. Only one virtual device can be");
        println!("      the default output; select DisEQ's to test this one.");
    }
}

fn report(device: &Device) {
    println!("  name       {}", device.name);
    println!("  uid        {}", device.uid.as_deref().unwrap_or("<none>"));
    println!("  object id  {}", device.id);
    println!("  transport  {}", device.transport_type_name());
    println!("  alive      {}", yes_no(audio::is_alive(device.id)));
    println!(
        "  streams    output {}, input {}",
        yes_no(device.has_output),
        yes_no(device.has_input)
    );
    println!(
        "  rate       {}",
        device
            .sample_rate
            .map(|rate| format!("{rate:.0} Hz"))
            .unwrap_or_else(|| "unknown".into())
    );
    println!(
        "  buffers    output {} frames, input {} frames",
        show(audio::buffer_frame_size(device.id, true)),
        show(audio::buffer_frame_size(device.id, false))
    );
    println!(
        "  safety     output {} frames, input {} frames",
        show(audio::safety_offset(device.id, true)),
        show(audio::safety_offset(device.id, false))
    );
    println!(
        "  volume     {}",
        device
            .volume
            .map(|volume| format!("{volume:.2}"))
            .unwrap_or_else(|| "none".into())
    );
    println!(
        "  muted      {}",
        device
            .muted
            .map(|muted| yes_no(muted).to_string())
            .unwrap_or_else(|| "unknown".into())
    );
}

fn show(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "?".into())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        text.chars().take(width - 1).collect::<String>() + "…"
    }
}
