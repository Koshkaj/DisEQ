//! Phase C's acceptance test, run by ear and by number: everything the system
//! plays arrives at our own virtual device, is equalised, and is played back to
//! real hardware, with a
//! volume slider that works even when the hardware has none.
//!
//!     cargo run -p kd-audio --example route              # default target
//!     cargo run -p kd-audio --example route -- --list    # what it could use
//!     cargo run -p kd-audio --example route -- "DELL U2720Q"
//!     cargo run -p kd-audio --example route -- --preset bass-booster
//!
//! Plays for 60 seconds unless `--seconds` says otherwise, reporting the drift
//! controller once a second, then un-mutes everything. Start some
//! audio before running it — it routes what the machine is already playing.
//!
//! Ctrl-C tears the route down too, which retracts the virtual device.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kd_audio::devices::Device;
use kd_audio::eq::{preset, Settings};
use kd_audio::playback::TICKS_PER_SECOND;
use kd_audio::router::{self, Route};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    if arguments.iter().any(|argument| argument == "--list") {
        list();
        return;
    }

    let seconds = value(&arguments, "--seconds")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60);
    let volume = value(&arguments, "--volume")
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(1.0);

    let mut settings = Settings::default();
    if let Some(id) = value(&arguments, "--preset") {
        match preset(&id) {
            Some(found) => {
                settings.gains = found.gains;
                println!("preset:  {} ({})", found.name, found.id);
            }
            None => {
                eprintln!("no preset called {id:?}");
                list_presets();
                std::process::exit(2);
            }
        }
    }

    let wanted = arguments
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .cloned();
    let Some(target) = choose(wanted.as_deref()) else {
        eprintln!("no hardware output device to route to");
        list();
        std::process::exit(1);
    };

    println!(
        "target:  {} ({})",
        target.name,
        target.transport_type_name()
    );
    println!(
        "         volume {} of its own",
        if target.volume_is_settable {
            "has one"
        } else {
            "has none — this is what the route is for"
        }
    );

    let mut route = match Route::start(&target, settings, volume) {
        Ok(route) => route,
        Err(error) => {
            eprintln!("\ncould not start: {error}");
            std::process::exit(1);
        }
    };

    println!("capture: the driver's shared ring — no capture permission");
    println!("lag:     {} frames wanted\n", route.health().wanted_offset);

    let interrupted = Arc::new(AtomicBool::new(false));
    watch_for_interrupt(Arc::clone(&interrupted));

    let period = Duration::from_secs_f64(1.0 / TICKS_PER_SECOND);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut ticks = 0usize;

    while Instant::now() < deadline && !interrupted.load(Ordering::Relaxed) {
        let health = route.tick();
        ticks += 1;

        if ticks % TICKS_PER_SECOND as usize == 0 {
            let drift = health
                .drift()
                .map(|frames| format!("{frames:+8.1}"))
                .unwrap_or_else(|| "       ?".into());
            // The level is the difference between "aligned and working" and
            // "aligned and carrying silence", which look identical otherwise.
            let level = if health.level > 0.0 {
                format!("{:>6.1} dB", 20.0 * health.level.log10())
            } else {
                "  SILENT".into()
            };
            println!(
                "  {:>3}s  rate {:.6} (nominal {:.6})  drift {drift} frames  level {level}  realignments {}{}",
                ticks / TICKS_PER_SECOND as usize,
                health.rate,
                health.nominal_rate,
                health.realignments,
                if health.running {
                    ""
                } else {
                    "   ENGINE STOPPED"
                }
            );
        }

        std::thread::sleep(period);
    }

    println!("\nstopping — un-muting everything");
    drop(route);
}

fn choose(wanted: Option<&str>) -> Option<Device> {
    match wanted {
        Some(name) => router::targets()
            .into_iter()
            .find(|device| device.name.to_lowercase().contains(&name.to_lowercase())),
        None => router::default_target(),
    }
}

fn list() {
    println!("targets:");
    for device in router::targets() {
        println!(
            "  {:<34} {:<11} volume {}",
            device.name,
            device.transport_type_name(),
            if device.volume_is_settable {
                "settable"
            } else {
                "fixed"
            }
        );
    }
    list_presets();
}

fn list_presets() {
    println!("presets:");
    for preset in kd_audio::PRESETS {
        println!("  {:<16} {}", preset.id, preset.name);
    }
}

fn value(arguments: &[String], flag: &str) -> Option<String> {
    let index = arguments.iter().position(|argument| argument == flag)?;
    arguments.get(index + 1).cloned()
}

/// Ctrl-C sets the flag rather than killing the process, so the route is
/// dropped, which un-mutes everything it tapped.
fn watch_for_interrupt(flag: Arc<AtomicBool>) {
    // SAFETY: the handler only stores into an atomic, which is all a signal
    // handler is allowed to do.
    static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    let _ = FLAG.set(flag);

    extern "C" fn handle(_signal: i32) {
        if let Some(flag) = FLAG.get() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    // SAFETY: installing a handler for SIGINT with a function that only touches
    // an atomic.
    unsafe {
        libc::signal(libc::SIGINT, handle as *const () as libc::sighandler_t);
    }
}
