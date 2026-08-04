//! Phase D's acceptance test: two applications playing at once, and moving one
//! fader changes only one of them.
//!
//!     cargo run -p kd-audio --example mixer                 # list what is playing
//!     cargo run -p kd-audio --example mixer -- --run        # fade each in turn
//!     cargo run -p kd-audio --example mixer -- --run --gain brave=0.2
//!
//! `--run` taps every application that is currently playing and mixes them into
//! the current output device. Without `--gain`, it walks each fader down to
//! silence and back so the effect is audible one application at a time.
//!
//! Every tap is undone on the way out: an application left tapped by a crashed
//! process is an application with no sound.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kd_audio::devices;
use kd_audio::mixer::Mixer;
use kd_audio::processes::{self, Process};
use kd_audio::tap;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match tap::are_permitted() {
        Ok(()) => println!("taps:    permitted"),
        Err(error) => {
            eprintln!("taps:    unavailable — {error}");
            std::process::exit(1);
        }
    }

    let playing = processes::playing();
    println!("playing: {}", playing.len());
    for process in &playing {
        println!(
            "  {:<40} pid {}",
            process.bundle_id.as_deref().unwrap_or("<no bundle>"),
            process.pid
        );
    }

    if !arguments.iter().any(|argument| argument == "--run") {
        println!("\nAdd --run to tap these and mix them yourself.");
        return;
    }

    if playing.is_empty() {
        eprintln!("\nnothing is playing — start some audio first");
        std::process::exit(1);
    }

    let Some(destination) = devices::default_output() else {
        eprintln!("\nno default output device");
        std::process::exit(1);
    };
    println!(
        "\nmixing into {} ({})",
        destination.name,
        destination.transport_type_name()
    );

    let mixer = match Mixer::start(&playing, &destination) {
        Ok(mixer) => mixer,
        Err(error) => {
            eprintln!("could not start the mixer: {error}");
            std::process::exit(1);
        }
    };
    println!("mixer running — every fader at unity\n");

    let interrupted = Arc::new(AtomicBool::new(false));
    watch_for_interrupt(Arc::clone(&interrupted));

    match fixed_gains(&arguments, &playing) {
        gains if !gains.is_empty() => {
            for (process, gain) in &gains {
                mixer.set_gain(process.id, *gain);
                println!(
                    "  {} → {gain:.2}",
                    process.bundle_id.as_deref().unwrap_or("<no bundle>")
                );
            }
            println!("\nholding for 30 seconds — Ctrl-C to stop early");
            hold(&interrupted, Duration::from_secs(30));
        }
        _ => sweep(&mixer, &playing, &interrupted),
    }

    println!("\nstopping — untapping every application");
    drop(mixer);
}

/// Walks each application's fader down to silence and back, one at a time. The
/// others stay at unity, which is what makes the test conclusive: if they go
/// quiet too, the mixer is not per-application at all.
fn sweep(mixer: &Mixer, playing: &[Process], interrupted: &AtomicBool) {
    for process in playing {
        let name = process.bundle_id.as_deref().unwrap_or("<no bundle>");
        println!("  {name}");
        for gain in [0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75, 1.0] {
            if interrupted.load(Ordering::Relaxed) {
                return;
            }
            mixer.set_gain(process.id, gain);
            println!("    {gain:.2}");
            std::thread::sleep(Duration::from_millis(1_200));
        }
        mixer.set_gain(process.id, 1.0);
    }
}

fn fixed_gains(arguments: &[String], playing: &[Process]) -> Vec<(Process, f32)> {
    let mut gains = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--gain" {
            if let Some(pair) = arguments.get(index + 1) {
                if let Some((name, value)) = pair.split_once('=') {
                    if let Ok(gain) = value.parse::<f32>() {
                        if let Some(process) = playing.iter().find(|process| {
                            process
                                .bundle_id
                                .as_deref()
                                .unwrap_or_default()
                                .to_lowercase()
                                .contains(&name.to_lowercase())
                        }) {
                            gains.push((process.clone(), gain));
                        } else {
                            eprintln!("nothing playing matches {name:?}");
                        }
                    }
                }
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    gains
}

fn hold(interrupted: &AtomicBool, duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline && !interrupted.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Ctrl-C sets the flag rather than killing the process, so the taps are
/// destroyed and every application gets its audio back.
fn watch_for_interrupt(flag: Arc<AtomicBool>) {
    static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    let _ = FLAG.set(flag);

    extern "C" fn handle(_signal: i32) {
        if let Some(flag) = FLAG.get() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    // SAFETY: the handler only stores into an atomic, which is all a signal
    // handler is allowed to do.
    unsafe {
        libc::signal(libc::SIGINT, handle as *const () as libc::sighandler_t);
    }
}
