//! Phase A's acceptance test: the EQ's effect has to be measurable, not merely
//! plausible. Every case renders offline through the real `AVAudioUnitEQ` and
//! compares band energy against a flat render of the same signal, so player and
//! mixer gain cancel out.

use kd_audio::eq::{preset, Settings};
use kd_audio::offline::{gain_db, tones, OfflineRenderer};

const SAMPLE_RATE: f64 = 48_000.0;
const SECONDS: f64 = 1.5;
/// Frames dropped before analysis: enough for the player to start and for the
/// IIR bands to settle, so what is measured is steady state.
const SETTLE: usize = 8_192;
/// Probe tones, one per region the presets move. Deliberately off the band
/// centres — a filter that only works at 64.000 Hz would be a coincidence.
const BASS: f64 = 60.0;
const MID: f64 = 950.0;
const TREBLE: f64 = 7_500.0;

struct Fixture {
    renderer: OfflineRenderer,
    input: Vec<f32>,
    flat: Vec<f32>,
}

impl Fixture {
    fn new() -> Self {
        let renderer = OfflineRenderer::new(SAMPLE_RATE).expect("offline renderer");
        // 0.2 leaves headroom for a 12 dB boost without clipping the mixer.
        let input = tones(SAMPLE_RATE, &[BASS, MID, TREBLE], SECONDS, 0.2);
        let flat = renderer
            .render(&input, &flat_settings())
            .expect("flat render");
        Self {
            renderer,
            input,
            flat,
        }
    }

    /// dB change at each probe tone, relative to the flat render.
    fn response(&self, settings: &Settings) -> [f64; 3] {
        let rendered = self.renderer.render(&self.input, settings).expect("render");
        let before = &self.flat[SETTLE..];
        let after = &rendered[SETTLE..];
        [BASS, MID, TREBLE].map(|frequency| gain_db(before, after, SAMPLE_RATE, frequency))
    }
}

fn flat_settings() -> Settings {
    Settings {
        auto_preamp: false,
        ..Settings::default()
    }
}

fn with_preset(id: &str) -> Settings {
    Settings {
        gains: preset(id).unwrap().gains,
        auto_preamp: false,
        ..Settings::default()
    }
}

fn assert_near(measured: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (measured - expected).abs() <= tolerance,
        "{what}: measured {measured:.2} dB, expected {expected:.2} ±{tolerance} dB"
    );
}

#[test]
fn flat_render_is_reproducible() {
    let fixture = Fixture::new();
    let [bass, mid, treble] = fixture.response(&flat_settings());
    assert_near(bass, 0.0, 0.2, "flat at 60 Hz");
    assert_near(mid, 0.0, 0.2, "flat at 950 Hz");
    assert_near(treble, 0.0, 0.2, "flat at 7.5 kHz");
}

/// The Phase A acceptance criterion, stated as a number.
#[test]
fn bass_boost_is_measurably_present() {
    let fixture = Fixture::new();
    let [bass, mid, treble] = fixture.response(&with_preset("bass-booster"));
    assert!(bass > 6.0, "60 Hz only rose {bass:.2} dB");
    assert!(
        mid.abs() < 2.0,
        "950 Hz moved {mid:.2} dB and should not have"
    );
    assert!(
        treble.abs() < 1.0,
        "7.5 kHz moved {treble:.2} dB and should not have"
    );
}

#[test]
fn bass_reducer_is_the_mirror_of_bass_booster() {
    let fixture = Fixture::new();
    let boosted = fixture.response(&with_preset("bass-booster"))[0];
    let reduced = fixture.response(&with_preset("bass-reducer"))[0];
    assert!(reduced < -6.0, "60 Hz only fell {reduced:.2} dB");
    assert_near(reduced, -boosted, 1.5, "cut against boost at 60 Hz");
}

#[test]
fn treble_boost_leaves_the_bass_alone() {
    let fixture = Fixture::new();
    let [bass, _mid, treble] = fixture.response(&with_preset("treble-booster"));
    assert!(treble > 5.0, "7.5 kHz only rose {treble:.2} dB");
    assert!(
        bass.abs() < 1.0,
        "60 Hz moved {bass:.2} dB and should not have"
    );
}

#[test]
fn auto_preamp_pulls_the_peak_back_to_unity() {
    let fixture = Fixture::new();
    let settings = Settings {
        gains: preset("bass-booster").unwrap().gains,
        auto_preamp: true,
        ..Settings::default()
    };
    // The loudest band is +11 dB, so everything drops by 11: bass ends near
    // unity and the untouched treble sits 11 dB down.
    let [bass, _mid, treble] = fixture.response(&settings);
    assert!(
        bass < 3.0,
        "60 Hz should be near unity with auto-preamp on, got {bass:.2} dB"
    );
    assert_near(treble, -11.0, 1.0, "7.5 kHz under auto-preamp");
}

#[test]
fn preamp_moves_every_band_together() {
    let fixture = Fixture::new();
    let settings = Settings {
        preamp: -6.0,
        auto_preamp: false,
        ..Settings::default()
    };
    for (measured, what) in fixture
        .response(&settings)
        .iter()
        .zip(["60 Hz", "950 Hz", "7.5 kHz"])
    {
        assert_near(*measured, -6.0, 0.5, what);
    }
}

#[test]
fn disabling_bypasses_the_bands() {
    let fixture = Fixture::new();
    let settings = Settings {
        gains: preset("bass-booster").unwrap().gains,
        enabled: false,
        auto_preamp: false,
        ..Settings::default()
    };
    for (measured, what) in fixture
        .response(&settings)
        .iter()
        .zip(["60 Hz", "950 Hz", "7.5 kHz"])
    {
        assert_near(*measured, 0.0, 0.3, what);
    }
}

#[test]
fn every_preset_renders_without_error() {
    let fixture = Fixture::new();
    for preset in kd_audio::PRESETS {
        let settings = Settings {
            gains: preset.gains,
            ..Settings::default()
        };
        let rendered = fixture
            .renderer
            .render(&fixture.input, &settings)
            .unwrap_or_else(|error| panic!("{} failed to render: {error}", preset.id));
        assert_eq!(rendered.len(), fixture.input.len(), "{}", preset.id);
        assert!(
            rendered[SETTLE..].iter().all(|sample| sample.is_finite()),
            "{} produced a non-finite sample",
            preset.id
        );
        // Auto-preamp is on by default, so nothing should reach full scale.
        let peak = rendered[SETTLE..]
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak < 1.0, "{} clipped at {peak}", preset.id);
    }
}
