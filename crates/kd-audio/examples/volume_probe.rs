//! Measures what the driver's volume control does to the audio it hands the
//! app, which is not the same question as whether the control reads back.
//!
//!     cargo run -p kd-audio --example volume_probe
//!
//! The driver's volume is a *control surface*: the menu bar and the volume keys
//! write to it, the app reads it, and the app applies it on the way to the
//! hardware. A driver that also applies it to the shared ring multiplies the
//! two — and since the driver's taper is cubic, half volume becomes a sixteenth
//! and the machine goes quiet with the EQ on and loud with it off.
//!
//! So: play a tone of known amplitude into the device, move the device's volume
//! across its range, and measure the ring at each setting. The level must not
//! move. Nothing here needs the app running, and it restores the default output
//! it took.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kd_audio::devices;
use kd_audio::shared::SharedRing;
use kd_sys::audio;

/// Amplitude of the test tone, well below full scale so nothing on the way
/// clips and turns a level change into a shape change.
const AMPLITUDE: f32 = 0.5;
const TONE_HZ: f32 = 440.0;
const RATE: u32 = 48_000;
const SECONDS: u32 = 40;

/// Volume settings to measure. 1.0 first: it is the one setting where the
/// cubic taper is the identity, so it reads the same either way and makes the
/// baseline every other reading is compared against.
const SETTINGS: [f64; 4] = [1.0, 0.75, 0.5, 0.25];

/// How far a reading may sit from the baseline before the driver is judged to
/// be attenuating. Generous: the tone is periodic and the window is short, so a
/// few tenths of a dB of measurement noise is expected.
const TOLERANCE_DB: f64 = 1.0;

fn main() {
    if !devices::driver_is_installed() {
        eprintln!("the DisEQ plug-in is not loaded — run 'make driver-install'");
        std::process::exit(1);
    }

    let tone = match write_tone() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("could not write the test tone: {error}");
            std::process::exit(1);
        }
    };

    let previous_output = audio::default_output_device();
    let previous_system = audio::default_system_output_device();

    let Some(device) = devices::publish() else {
        eprintln!("the plug-in did not publish its device");
        std::process::exit(1);
    };

    let mut probe = Probe {
        device_id: device.id,
        previous_output,
        previous_system,
        tone,
        player: None,
    };
    let code = probe.run();
    probe.restore();
    // The ring check proves the driver stopped applying the volume. This proves
    // that exactly one stage still does — the other half of the same bug.
    std::process::exit(code | check_gain_staging());
}

/// Starts a real route and checks the volume is applied once, not twice.
///
/// `Route` puts the gain on the hardware's own volume control where there is
/// one and in our mixer where there is not. Whichever it picks, the product of
/// the two stages is what comes out, and it has to equal what was asked for.
fn check_gain_staging() -> i32 {
    use kd_audio::eq::Settings;
    use kd_audio::router::{self, Route};

    println!("\ngain staging on a real route:");
    // A named target so the other branch can be exercised too: hardware with a
    // volume control of its own takes the gain directly, and hardware without
    // one — HDMI and DisplayPort, the reason the route exists — takes it in our
    // mixer instead. Both have to come out at the requested volume.
    let wanted = std::env::args()
        .skip(1)
        .find(|argument| !argument.starts_with("--"));
    let target = match wanted {
        Some(name) => match router::targets()
            .into_iter()
            .find(|device| device.name.to_lowercase().contains(&name.to_lowercase()))
        {
            Some(device) => device,
            None => {
                println!("  (skipped — no output device matching {name:?})");
                return 0;
            }
        },
        None => match router::default_target() {
            Some(device) => device,
            None => {
                println!("  (skipped — no hardware output to route to)");
                return 0;
            }
        },
    };

    // Deliberately not 1.0: a route adopts the hardware's own volume when it has
    // one, and at unity every staging mistake multiplies out to the same answer.
    // A route started at part volume is the case that used to come up quiet.
    let started_at = 0.4;
    let mut route = match Route::start(&target, Settings::default(), started_at) {
        Ok(route) => route,
        Err(error) => {
            println!("  (skipped — could not start a route: {error})");
            return 0;
        }
    };
    println!(
        "  target: {} ({}, volume {})",
        target.name,
        target.transport_type_name(),
        if target.volume_is_settable {
            "its own"
        } else {
            "ours"
        }
    );

    let mut failures = 0;

    // Measured before anything is set, because setting the volume is what used
    // to repair the staging: the bug was only ever visible on a route that had
    // just started and not been touched since — which is every route, until the
    // user reaches for the slider and unknowingly fixes it.
    std::thread::sleep(Duration::from_millis(300));
    {
        let wanted = route.volume();
        let (mixer, hardware) = route.gain_stages();
        let applied = f64::from(mixer) * hardware.unwrap_or(1.0);
        let ok = (applied - f64::from(wanted)).abs() < 0.02;
        if !ok {
            failures += 1;
        }
        println!(
            "  [{}] as started, volume {wanted:.2}  mixer {mixer:.3} x hardware {}  = {applied:.3}",
            if ok { "pass" } else { "FAIL" },
            hardware
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "n/a".into()),
        );
    }

    for wanted in [1.0f32, 0.5, 0.25] {
        route.set_volume(wanted);
        std::thread::sleep(Duration::from_millis(300));
        let (mixer, hardware) = route.gain_stages();
        let applied = f64::from(mixer) * hardware.unwrap_or(1.0);
        let ok = (applied - f64::from(wanted)).abs() < 0.02;
        if !ok {
            failures += 1;
        }
        println!(
            "  [{}] volume {wanted:.2}  mixer {mixer:.3} x hardware {}  = {applied:.3}",
            if ok { "pass" } else { "FAIL" },
            hardware
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "n/a".into()),
        );
    }

    // Leaves the system on the hardware it is already playing through rather
    // than undoing a device choice the user may have made since.
    route.stop_on_target();

    println!();
    if failures == 0 {
        println!("PASS: the volume is applied at exactly one stage.");
        0
    } else {
        println!("FAIL: the stages do not multiply out to the requested volume.");
        1
    }
}

struct Probe {
    device_id: u32,
    previous_output: Option<u32>,
    previous_system: Option<u32>,
    tone: PathBuf,
    player: Option<Child>,
}

impl Probe {
    fn run(&mut self) -> i32 {
        audio::set_muted(self.device_id, false);
        audio::set_volume(self.device_id, 1.0);
        audio::set_nominal_sample_rate(self.device_id, f64::from(RATE));

        if !claim_default(self.device_id) {
            eprintln!("could not make the virtual device the default output");
            return 1;
        }
        println!(
            "default output is the virtual device (id {})",
            self.device_id
        );

        match Command::new("afplay")
            .arg(&self.tone)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => self.player = Some(child),
            Err(error) => {
                eprintln!("could not start afplay: {error}");
                return 1;
            }
        }

        let ring = match wait_for_audio() {
            Some(ring) => ring,
            None => {
                eprintln!(
                    "no audio reached the shared ring — is something else holding the output?"
                );
                return 1;
            }
        };
        println!(
            "shared ring running at {:.0} Hz, tone at {:.3} ({:.1} dBFS)\n",
            ring.sample_rate(),
            AMPLITUDE,
            20.0 * f64::from(AMPLITUDE).log10()
        );

        let mut readings = Vec::new();
        for setting in SETTINGS {
            audio::set_volume(self.device_id, setting);
            // The driver ramps its gain per sample rather than stepping, so a
            // reading taken immediately catches the ramp rather than the value.
            std::thread::sleep(Duration::from_millis(600));
            let peak = measure(&ring, Duration::from_millis(400));
            println!(
                "  volume {setting:.2}  ring peak {:>7.3}  {:>7.2} dBFS   (cubic would give {:.3})",
                peak,
                db(peak),
                AMPLITUDE as f64 * setting.powi(3)
            );
            readings.push((setting, peak));
        }

        println!();
        self.verdict(&readings)
    }

    /// Passes when the level the app receives is the same at every volume.
    fn verdict(&self, readings: &[(f64, f64)]) -> i32 {
        let Some((_, baseline)) = readings.first().copied() else {
            return 1;
        };
        if baseline <= 0.0 {
            eprintln!("FAIL: no signal at full volume — nothing was measured");
            return 1;
        }

        let mut worst = 0.0f64;
        let mut culprit = 1.0;
        for (setting, peak) in readings.iter().copied().skip(1) {
            let deviation = (db(peak) - db(baseline)).abs();
            if deviation > worst {
                worst = deviation;
                culprit = setting;
            }
        }

        if worst <= TOLERANCE_DB {
            println!("PASS: the ring level is flat across the volume range (worst {worst:.2} dB).");
            println!("      The driver hands the app the mix untouched; the app owns the gain.");
            0
        } else {
            println!("FAIL: the ring level moves with the device volume — {worst:.2} dB at {culprit:.2}.");
            println!("      The driver is attenuating audio the app then attenuates again.");
            1
        }
    }

    fn restore(&mut self) {
        if let Some(mut player) = self.player.take() {
            let _ = player.kill();
            let _ = player.wait();
        }
        audio::set_volume(self.device_id, 1.0);
        // The device has to go before the defaults come back, or the system
        // sits on a device nothing is draining for as long as the retract takes.
        if let Some(output) = self.previous_output {
            audio::set_default_output_device(output);
        }
        if let Some(system) = self.previous_system {
            audio::set_default_system_output_device(system);
        }
        devices::unpublish();
        let _ = std::fs::remove_file(&self.tone);
    }
}

/// The loudest sample the driver publishes over `window`.
fn measure(ring: &SharedRing, window: Duration) -> f64 {
    const FRAMES: usize = 512;
    let mut left = vec![0.0f32; FRAMES];
    let mut right = vec![0.0f32; FRAMES];
    let mut peak = 0.0f32;

    let until = Instant::now() + window;
    while Instant::now() < until {
        let from = ring.written() - FRAMES as i64;
        {
            let mut planes = [&mut left[..], &mut right[..]];
            if ring.read(&mut planes, from, FRAMES) {
                for plane in planes.iter() {
                    for sample in plane.iter() {
                        peak = peak.max(sample.abs());
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f64::from(peak)
}

/// Waits for the ring to exist and to be carrying something.
fn wait_for_audio() -> Option<SharedRing> {
    for _ in 0..60 {
        if let Ok(ring) = SharedRing::open() {
            if ring.is_running() && measure(&ring, Duration::from_millis(200)) > 0.0 {
                return Some(ring);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn claim_default(device: u32) -> bool {
    for _ in 0..20 {
        audio::set_default_output_device(device);
        audio::set_default_system_output_device(device);
        if audio::default_output_device() == Some(device) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn db(value: f64) -> f64 {
    if value > 0.0 {
        20.0 * value.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// A 16-bit stereo sine, written by hand so the probe carries no dependency for
/// the sake of forty seconds of tone.
fn write_tone() -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join("diseq-volume-probe.wav");
    let frames = RATE * SECONDS;
    let data_bytes = frames * 4;

    let mut file = std::fs::File::create(&path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?; // PCM header length
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&2u16.to_le_bytes())?; // stereo
    file.write_all(&RATE.to_le_bytes())?;
    file.write_all(&(RATE * 4).to_le_bytes())?; // bytes per second
    file.write_all(&4u16.to_le_bytes())?; // bytes per frame
    file.write_all(&16u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;

    let mut samples = Vec::with_capacity(data_bytes as usize);
    for frame in 0..frames {
        let phase = std::f32::consts::TAU * TONE_HZ * frame as f32 / RATE as f32;
        let value = (phase.sin() * AMPLITUDE * f32::from(i16::MAX)) as i16;
        samples.extend_from_slice(&value.to_le_bytes());
        samples.extend_from_slice(&value.to_le_bytes());
    }
    file.write_all(&samples)?;
    Ok(path)
}
