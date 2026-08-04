//! Renders a preset offline and prints the response it actually produced.
//!
//! The gains a preset asks for and the gains a ten-band parametric EQ delivers
//! are not the same thing — neighbouring bands overlap. This shows the second.
//!
//!     cargo run -p kd-audio --example eq_probe -- bass-booster

use kd_audio::eq::{preset, Settings, FREQUENCIES, PRESETS};
use kd_audio::offline::{gain_db, tones, OfflineRenderer};

const SAMPLE_RATE: f64 = 48_000.0;
const SECONDS: f64 = 1.0;
const SETTLE: usize = 8_192;

fn main() {
    let id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bass-booster".into());
    let Some(chosen) = preset(&id) else {
        eprintln!("unknown preset: {id}\n");
        eprintln!("try one of:");
        for preset in PRESETS {
            eprintln!("  {:<16} {}", preset.id, preset.name);
        }
        std::process::exit(1);
    };

    let auto_preamp = std::env::args().any(|argument| argument == "--auto-preamp");

    let renderer = match OfflineRenderer::new(SAMPLE_RATE) {
        Ok(renderer) => renderer,
        Err(error) => {
            eprintln!("could not build the offline renderer: {error}");
            std::process::exit(1);
        }
    };

    // Probe at the band centres. One render carries all ten tones; Goertzel
    // picks them apart afterwards.
    let probes: Vec<f64> = FREQUENCIES.iter().map(|f| *f as f64).collect();
    let input = tones(SAMPLE_RATE, &probes, SECONDS, 0.05);

    let flat = Settings {
        auto_preamp: false,
        ..Settings::default()
    };
    let reference = renderer.render(&input, &flat).expect("flat render");

    let settings = Settings {
        gains: chosen.gains,
        auto_preamp,
        ..Settings::default()
    };
    let rendered = renderer.render(&input, &settings).expect("render");

    println!(
        "{} — auto-preamp {}, global gain {:+.1} dB\n",
        chosen.name,
        if auto_preamp { "on" } else { "off" },
        settings.global_gain()
    );
    println!(
        "  {:>7}  {:>7}  {:>8}   response",
        "band", "asked", "measured"
    );

    for (index, frequency) in probes.iter().enumerate() {
        let measured = gain_db(
            &reference[SETTLE..],
            &rendered[SETTLE..],
            SAMPLE_RATE,
            *frequency,
        );
        println!(
            "  {:>7}  {:>+6.1}  {:>+7.2}   {}",
            label(*frequency),
            chosen.gains[index],
            measured,
            bar(measured)
        );
    }
}

fn label(frequency: f64) -> String {
    if frequency >= 1_000.0 {
        format!("{:.0}k", frequency / 1_000.0)
    } else {
        format!("{frequency:.0}")
    }
}

/// A 24 dB scale centred on zero, one column per dB.
fn bar(db: f64) -> String {
    const HALF: i32 = 24;
    let steps = (db.round() as i32).clamp(-HALF, HALF);
    let mut line = vec![' '; (HALF * 2 + 1) as usize];
    line[HALF as usize] = '|';
    let (from, to) = if steps >= 0 {
        (HALF, HALF + steps)
    } else {
        (HALF + steps, HALF)
    };
    for cell in line.iter_mut().take(to as usize + 1).skip(from as usize) {
        *cell = '#';
    }
    line.into_iter().collect()
}
