//! What survives a quit: `~/Library/Application Support/DisEQ/settings.json`.
//!
//! Everything the Sound card can change is in here, so a relaunch comes up the
//! way the app was left — the equaliser as it was set, the switches where they
//! were, each application at the level it was given, and the volume on the
//! device it was playing to.
//!
//! Every field has a default and every read tolerates a missing or malformed
//! file. A settings file that cannot be parsed costs the settings, not the
//! launch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kd_audio::eq::{self, Settings};
use serde::{Deserialize, Serialize};

const DIRECTORY: &str = "Library/Application Support/DisEQ";
const FILE: &str = "settings.json";
/// Bumped when a field changes meaning rather than merely appearing. Unknown
/// versions are read anyway — `serde` defaults cover what is missing — but the
/// number is what lets a future release tell "old" from "wrong".
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// The hardware the route was last playing to, by UID. Object IDs are
    /// reassigned every boot and names change; the UID identifies the same
    /// physical device across both.
    pub target_uid: Option<String>,
    /// Master volume, 0..1.
    pub volume: f64,
    /// Whether audio goes through DisEQ. On unless bypassed in Settings: the
    /// route is what the equaliser and the volume control on outputs without
    /// one of their own depend on, so it is not a choice the Sound card asks
    /// for. A file from when it was a switch there keeps what was chosen.
    pub routing: bool,
    /// Restored as it was left. It costs a process tap, and therefore the
    /// system-audio-recording prompt — but only for someone who had the mixer
    /// switched on when they quit.
    pub mixing: bool,
    pub eq: EqConfig,
    /// Display cards opened far enough to show brightness and resolution,
    /// keyed by stable hardware identity rather than the current display id.
    pub open_displays: BTreeSet<String>,
    /// Whether the Sound card shows its volume control and advanced disclosure.
    pub sound_open: bool,
    /// Per-application gains, keyed by bundle identifier. Processes do not
    /// survive a relaunch; their bundle IDs do.
    pub app_gains: BTreeMap<String, f32>,
    /// Set when the offer to install the audio driver was declined, so the
    /// launch that follows does not ask again. A newer driver than the one
    /// installed asks regardless — that offer is about a different version.
    pub driver_prompt_declined: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: VERSION,
            target_uid: None,
            volume: 1.0,
            routing: true,
            mixing: false,
            eq: EqConfig::default(),
            open_displays: BTreeSet::new(),
            sound_open: false,
            app_gains: BTreeMap::new(),
            driver_prompt_declined: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EqConfig {
    pub enabled: bool,
    pub auto_preamp: bool,
    pub preamp: f32,
    /// One per band. A file with the wrong number of them is padded or
    /// truncated rather than rejected, so a build with more bands can still
    /// read a file written by one with fewer.
    pub gains: Vec<f32>,
    /// The preset id, or `manual` once the bands have been moved by hand.
    pub preset: String,
}

impl Default for EqConfig {
    fn default() -> Self {
        Self::from(&Settings::default(), eq::MANUAL)
    }
}

impl EqConfig {
    pub fn from(settings: &Settings, preset: &str) -> Self {
        Self {
            enabled: settings.enabled,
            auto_preamp: settings.auto_preamp,
            preamp: settings.preamp,
            gains: settings.gains.to_vec(),
            preset: preset.to_string(),
        }
    }

    /// The engine's view of these settings. Out-of-range values are clamped by
    /// the equaliser itself, so a hand-edited file cannot ask for a gain the
    /// unit will not take.
    pub fn settings(&self) -> Settings {
        let mut gains = eq::FLAT;
        for (slot, saved) in gains.iter_mut().zip(&self.gains) {
            *slot = *saved;
        }
        Settings {
            enabled: self.enabled,
            gains,
            preamp: self.preamp,
            auto_preamp: self.auto_preamp,
        }
    }
}

fn directory() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(DIRECTORY))
}

fn path() -> Option<PathBuf> {
    Some(directory()?.join(FILE))
}

impl Config {
    /// Reads the settings, falling back to defaults for anything missing.
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::migrated();
        };
        serde_json::from_str(&text).unwrap_or_else(|_| Self::migrated())
    }

    /// The first build that persisted anything wrote a bare UID to a file of
    /// its own. Reading it here means an upgrade keeps the device it was using.
    fn migrated() -> Self {
        let mut config = Self::default();
        if let Some(directory) = directory() {
            if let Ok(uid) = std::fs::read_to_string(directory.join("target-uid")) {
                let uid = uid.trim();
                if !uid.is_empty() {
                    config.target_uid = Some(uid.to_string());
                }
            }
        }
        config
    }

    /// Writes the settings, atomically.
    ///
    /// Through a temporary file and a rename: a quit that lands in the middle
    /// of a write would otherwise leave a truncated file, and the next launch
    /// would come up with nothing rather than with what was saved a second
    /// earlier. Failures are ignored — settings that cannot be written are not
    /// worth interrupting a quit for.
    pub fn save(&self) {
        let (Some(directory), Some(path)) = (directory(), path()) else {
            return;
        };
        if std::fs::create_dir_all(&directory).is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_err() {
            return;
        }
        let _ = std::fs::rename(&temporary, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_object_reads_as_the_defaults() {
        let config: Config = serde_json::from_str("{}").expect("every field has a default");
        assert_eq!(config.volume, 1.0);
        assert!(config.routing);
        assert!(!config.mixing);
        assert!(config.open_displays.is_empty());
        assert!(!config.sound_open);
        assert_eq!(config.eq.gains.len(), eq::BAND_COUNT);
    }

    #[test]
    fn a_file_with_fewer_bands_than_this_build_keeps_the_ones_it_has() {
        let config: Config =
            serde_json::from_str(r#"{"eq":{"gains":[6.0,-3.0]}}"#).expect("partial eq is allowed");
        let settings = config.eq.settings();
        assert_eq!(settings.gains[0], 6.0);
        assert_eq!(settings.gains[1], -3.0);
        // The rest stay flat rather than becoming zero-length or garbage.
        assert_eq!(settings.gains[2], 0.0);
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let mut config = Config::default();
        config.eq.preamp = -4.5;
        config.eq.gains[3] = 7.0;
        config.eq.preset = "rock".into();
        config.volume = 0.42;
        config.open_displays.insert("10ac:41b5:1234".into());
        config.sound_open = true;
        config.app_gains.insert("com.spotify.client".into(), 0.3);

        let text = serde_json::to_string(&config).expect("serialises");
        let read: Config = serde_json::from_str(&text).expect("parses");

        assert_eq!(read.eq.settings().preamp, -4.5);
        assert_eq!(read.eq.settings().gains[3], 7.0);
        assert_eq!(read.eq.preset, "rock");
        assert_eq!(read.volume, 0.42);
        assert!(read.open_displays.contains("10ac:41b5:1234"));
        assert!(read.sound_open);
        assert_eq!(read.app_gains.get("com.spotify.client"), Some(&0.3));
    }

    #[test]
    fn a_malformed_file_does_not_stop_the_app_starting() {
        assert!(serde_json::from_str::<Config>("{ not json").is_err());
    }
}
