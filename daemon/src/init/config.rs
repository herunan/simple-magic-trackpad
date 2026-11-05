use serde::Deserialize;
use serde_json::from_str;
use std::{
    fs::{read_to_string, File, OpenOptions},
    io::ErrorKind,
    path::PathBuf,
    time::Duration,
};

use tracing_subscriber::{
    filter::LevelFilter,
    fmt::{
        format::{DefaultFields, Format, Full},
        time::ChronoLocal,
        SubscriberBuilder,
    },
};

use crate::runtime::gesture::{Momentum, MomentumOutput, Timing, PX_PER_MM};

// This is simply a wrapper to allow deserialization of the
// logLevel field into a tracing LevelFilter, albeit in
// a roundabout way.
#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    OFF,
    ERROR,
    WARN,
    INFO,
    DEBUG,
    TRACE,
}

impl From<LogLevel> for LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::OFF => LevelFilter::OFF,
            LogLevel::ERROR => LevelFilter::ERROR,
            LogLevel::WARN => LevelFilter::WARN,
            LogLevel::INFO => LevelFilter::INFO,
            LogLevel::DEBUG => LevelFilter::DEBUG,
            LogLevel::TRACE => LevelFilter::TRACE,
        }
    }
}

#[serde_with::serde_as] // this has to be before the #[derive]
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    #[serde(default = "default_1")]
    pub acceleration: f64,

    #[serde(default = "default_0ms")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    pub drag_end_delay: Duration, // in milliseconds

    #[serde(default = "default_stdout")]
    pub log_file: String,

    #[serde(default = "default_info")]
    pub log_level: LogLevel,

    // NOTE: the old `responseTime` knob is gone: the event loop is now
    // fully event-driven (it sleeps on the device fd and on exact
    // decision deadlines), so there is no poll interval to configure.
    // A leftover `responseTime` in an existing config file is simply
    // ignored (serde skips unknown fields).

    // How long a fresh touch that's still ambiguous (2 or 3 fingers,
    // possibly still growing) is held back from the compositor before a
    // final decision is made: commit to a 3-finger drag, or release it as
    // an ordinary gesture. Without this, an asynchronous (non-simultaneous)
    // 3-finger touchdown briefly looks like a real 2-finger touch to the
    // compositor, which libinput can register as a 2-finger tap
    // (right-click) the instant the 3rd finger arrives and that touch gets
    // withdrawn -- and a 4-finger swipe that starts with only 3 fingers
    // detected in the first instant can get mistaken for a drag before the
    // 4th finger is seen. Same idea in reverse handles liftoff: see
    // mt_proxy.rs.
    #[serde(default = "default_50ms")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    pub entry_debounce: Duration, // in milliseconds

    // How long a touch that starts (and so far stays) at exactly 1 finger
    // is held back before being relayed live. Deliberately much shorter
    // than entry_debounce: ordinary single-finger pointer movement is by
    // far the most common gesture, including the touch-lift-reposition
    // cycle people use to cover long distances on a small trackpad, so it
    // must never feel delayed. This only needs to be long enough to catch
    // a 2nd finger landing a beat behind the 1st.
    #[serde(default = "default_15ms")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    pub probe_delay: Duration, // in milliseconds

    // How long a committed 3-finger drag defers its button press while
    // the fingers haven't moved. The press fires at the first real drag
    // motion or when this expires, whichever comes first. The window
    // exists so a 4th finger landing late (a fast 4-finger swipe -- the
    // faster the hand, the bigger the finger stagger) can abort a
    // misclassified drag without a phantom click ever being sent.
    #[serde(default = "default_75ms")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    pub press_grace: Duration, // in milliseconds

    // A 3-finger touch drags (the macOS "Three Finger Drag" style). Off by
    // default, as on macOS and so desktop 3-finger gestures keep working: the
    // touch then reaches the compositor like any other gesture.
    #[serde(default)]
    pub three_finger_drag: bool,

    // Momentum scrolling: a 2-finger flick keeps scrolling after the
    // fingers lift, slowing down like macOS, in every app (Qt and GPUI
    // apps have no fling of their own; GTK/Firefox/Chromium get this one
    // instead of theirs). Touching the pad stops it.
    #[serde(default = "default_true")]
    pub momentum: bool,

    // The glide, as on macOS (see docs/apple-momentum-research.md and
    // the Momentum section of gesture.rs): every 8 ms the finger speed
    // is multiplied by momentumFastDecay, blending linearly down to
    // momentumTailDecay below momentumTailSpeed (mm/s); it ends once
    // each axis is at or below momentumStopSpeed (mm/s). Mac values are
    // the defaults. momentumMinSpeed (mm/s finger speed at lift to start
    // a glide) is measured from a Mac; momentumStopHold (ms) is this
    // daemon's, not Apple's.
    #[serde(default = "mac_fast_decay")]
    pub momentum_fast_decay: f64,
    #[serde(default = "mac_tail_decay")]
    pub momentum_tail_decay: f64,
    #[serde(default = "mac_tail_speed")]
    pub momentum_tail_speed: f64, // mm/s
    #[serde(default = "mac_stop_speed")]
    pub momentum_stop_speed: f64, // mm/s
    #[serde(default = "default_min_speed")]
    pub momentum_min_speed: f64, // mm/s
    #[serde(default = "default_stop_hold")]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    pub momentum_stop_hold: Duration, // in milliseconds

    // How the glide reaches apps. "fingers" (default): two synthetic
    // fingers keep moving on the touchpad clone until the glide ends.
    // "wheel" (prototype): the lift reaches apps at once and the glide is
    // high-resolution wheel events on a separate "Momentum wheel" device,
    // so apps that keep a page stretched while fingers are down (Firefox,
    // Zen) see the touch end. Applies from the next glide.
    #[serde(default)]
    pub momentum_output: MomentumOutputSetting,
    // Wheel mode only: hi-res wheel units (1/120 notch) per mm of finger
    // glide (libinput's scroll curve doesn't apply to wheel events). The
    // default 24 is a notch per 5 mm, about the macOS distance in KDE apps;
    // see Momentum::DEFAULT_WHEEL_GAIN.
    #[serde(default = "default_wheel_gain")]
    pub momentum_wheel_gain: f64,
    // Wheel mode only: flip the glide's direction. The default assumes
    // natural scrolling on the touchpad and not on the Momentum wheel
    // device (KWin sets it per device); set this if the glide goes the
    // wrong way.
    #[serde(default)]
    pub momentum_wheel_invert: bool,
}

/// `momentumOutput` in daemon.json.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MomentumOutputSetting {
    Fingers,
    #[default]
    Wheel,
}

impl Configuration {
    /// The subset (plus derived scaling) the gesture machine needs;
    /// recomputed on every hot reload.
    pub fn timing(&self) -> Timing {
        Timing {
            probe_delay: self.probe_delay,
            entry_debounce: self.entry_debounce,
            drag_end_delay: self.drag_end_delay,
            press_grace: self.press_grace,
            px_per_mm: PX_PER_MM * self.acceleration,
            three_finger_drag: self.three_finger_drag,
            momentum: self.momentum.then_some(Momentum {
                // clamp so a typo can't make a glide run away or reverse
                fast_decay: self.momentum_fast_decay.clamp(0.0, 0.999),
                tail_decay: self.momentum_tail_decay.clamp(0.0, 0.999),
                tail_speed: self.momentum_tail_speed.max(0.0) / 1000.0,
                stop_speed: self.momentum_stop_speed.max(0.01) / 1000.0,
                min_speed: self.momentum_min_speed.max(0.0) / 1000.0,
                stop_hold: self.momentum_stop_hold,
                output: match self.momentum_output {
                    MomentumOutputSetting::Fingers => MomentumOutput::Fingers,
                    MomentumOutputSetting::Wheel => MomentumOutput::Wheel,
                },
                wheel_gain: if self.momentum_wheel_gain.is_finite()
                    && self.momentum_wheel_gain > 0.0
                {
                    self.momentum_wheel_gain.min(1000.0)
                } else {
                    Momentum::DEFAULT_WHEEL_GAIN
                },
                wheel_invert: self.momentum_wheel_invert,
            }),
        }
    }

    /// Whether the momentum wheel device is needed (created on demand,
    /// then kept until exit so a glide in flight can finish).
    pub fn wants_wheel_device(&self) -> bool {
        self.momentum && self.momentum_output == MomentumOutputSetting::Wheel
    }
}

impl Default for Configuration {
    fn default() -> Self {
        Configuration {
            acceleration: 1.0,
            drag_end_delay: Duration::from_millis(0),
            log_file: "stdout".to_string(),
            log_level: LogLevel::INFO,
            entry_debounce: Duration::from_millis(50),
            probe_delay: Duration::from_millis(15),
            press_grace: Duration::from_millis(75),
            three_finger_drag: false,
            momentum: true,
            momentum_fast_decay: Momentum::MAC_FAST_DECAY,
            momentum_tail_decay: Momentum::MAC_TAIL_DECAY,
            momentum_tail_speed: Momentum::MAC_TAIL_SPEED * 1000.0,
            momentum_stop_speed: Momentum::MAC_STOP_SPEED * 1000.0,
            momentum_min_speed: Momentum::DEFAULT_MIN_SPEED * 1000.0,
            momentum_stop_hold: Momentum::DEFAULT_STOP_HOLD,
            momentum_output: MomentumOutputSetting::Wheel,
            momentum_wheel_gain: Momentum::DEFAULT_WHEEL_GAIN,
            momentum_wheel_invert: false,
        }
    }
}

// for some reason, default literals don't seem to be okay
// with the serde crate, despite several issues and PRs on the
// subject. Using functions to yield the values is the only
// accepted way.
fn default_1() -> f64 {
    1.0
}
fn default_0ms() -> Duration {
    Duration::from_millis(0)
}
fn default_15ms() -> Duration {
    Duration::from_millis(15)
}
fn default_75ms() -> Duration {
    Duration::from_millis(75)
}
fn mac_fast_decay() -> f64 {
    Momentum::MAC_FAST_DECAY
}
fn mac_tail_decay() -> f64 {
    Momentum::MAC_TAIL_DECAY
}
fn mac_tail_speed() -> f64 {
    Momentum::MAC_TAIL_SPEED * 1000.0
}
fn mac_stop_speed() -> f64 {
    Momentum::MAC_STOP_SPEED * 1000.0
}
fn default_min_speed() -> f64 {
    Momentum::DEFAULT_MIN_SPEED * 1000.0
}
fn default_stop_hold() -> Duration {
    Momentum::DEFAULT_STOP_HOLD
}
fn default_wheel_gain() -> f64 {
    Momentum::DEFAULT_WHEEL_GAIN
}
fn default_true() -> bool {
    true
}
fn default_50ms() -> Duration {
    Duration::from_millis(50)
}
fn default_stdout() -> String {
    "stdout".to_string()
}
fn default_info() -> LogLevel {
    LogLevel::INFO
}

pub fn get_config_file_path() -> Result<PathBuf, std::io::Error> {
    let config_folder = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(config_dir) => PathBuf::from(config_dir),
        None => {
            // yes, this case has in fact happened to me, so it IS worth catching
            if let Some(home) = std::env::var_os("HOME") {
                PathBuf::from(home).join(".config")
            } else {
                return Err(std::io::Error::new(
                    ErrorKind::NotFound,
                    "Neither $XDG_CONFIG_HOME or $HOME defined in environment",
                ));
            }
        }
    };
    let filepath = config_folder.join("mac-trackpad/daemon.json");
    Ok(filepath)
}

// Configs are so optional that their absence should not crash the program,
// So if there is any issue with the JSON config file,
// the following default values will be returned:
//
// {
//     acceleration: 1.0,
//     dragEndDelay: 0,
//     logFile: "stdout",
//     logLevel: "info",
//     entryDebounce: 50,
//     probeDelay: 15
// }
//
// The user is also warned about this, so they can address the issues
// if they want to configure the way the program runs.
pub fn parse_config_file() -> Result<Configuration, std::io::Error> {
    let filepath = get_config_file_path()?;
    let jsonfile = read_to_string(&filepath).map_err(|_|
            // more descriptive error
            std::io::Error::new(
                ErrorKind::NotFound,
                format!("Unable to locate JSON file at {:?} ", filepath)
            ))?;

    // use serde's error as is
    let config = from_str::<Configuration>(&jsonfile)?;

    Ok(config)
}

impl Configuration {
    /// Clamp every knob into a range where the state machine behaves
    /// sensibly, warning about anything adjusted. Garbage in a config
    /// file must degrade to a working touchpad, never a broken one
    /// (a bad `acceleration` inverting drags, a `dragEndDelay` of an
    /// hour holding the button down, a `probeDelay` longer than the
    /// entry window starving classification...).
    fn sanitize(mut self) -> Configuration {
        let fix = |what: &str, before: String, after: String| {
            println!(
                "[PRE-LOG: WARNING]: config `{what}` = {before} is out of range; using {after}"
            );
        };
        if !self.acceleration.is_finite() || self.acceleration <= 0.0 {
            fix(
                "acceleration",
                format!("{}", self.acceleration),
                "1.0".into(),
            );
            self.acceleration = 1.0;
        } else if !(0.05..=20.0).contains(&self.acceleration) {
            let clamped = self.acceleration.clamp(0.05, 20.0);
            fix(
                "acceleration",
                format!("{}", self.acceleration),
                format!("{clamped}"),
            );
            self.acceleration = clamped;
        }
        if self.probe_delay > Duration::from_millis(200) {
            fix(
                "probeDelay",
                format!("{:?}", self.probe_delay),
                "200ms".into(),
            );
            self.probe_delay = Duration::from_millis(200);
        }
        if self.entry_debounce > Duration::from_millis(500) {
            fix(
                "entryDebounce",
                format!("{:?}", self.entry_debounce),
                "500ms".into(),
            );
            self.entry_debounce = Duration::from_millis(500);
        }
        if self.entry_debounce < self.probe_delay {
            fix(
                "entryDebounce",
                format!("{:?} (< probeDelay)", self.entry_debounce),
                format!("{:?}", self.probe_delay),
            );
            self.entry_debounce = self.probe_delay;
        }
        if self.press_grace > Duration::from_millis(1000) {
            fix(
                "pressGrace",
                format!("{:?}", self.press_grace),
                "1000ms".into(),
            );
            self.press_grace = Duration::from_millis(1000);
        }
        if self.drag_end_delay > Duration::from_millis(5000) {
            fix(
                "dragEndDelay",
                format!("{:?}", self.drag_end_delay),
                "5000ms".into(),
            );
            self.drag_end_delay = Duration::from_millis(5000);
        }
        self
    }
}

pub fn init_cfg() -> Configuration {
    println!("[PRE-LOG: INFO]: Loading configuration...");
    let configs = match parse_config_file() {
        Ok(cfg) => {
            let cfg = cfg.sanitize();
            println!("[PRE-LOG: INFO]: Successfully loaded your configuration (with defaults for unspecified values): \n{:#?}", &cfg);
            cfg
        }
        Err(err) => {
            let cfg = Default::default();
            println!(
                "\n[PRE-LOG: WARNING]: {err}\n\nThe configuration file could not be \
                loaded, so the program will continue with defaults of:\n{cfg:#?}",
            );
            cfg
        }
    };

    configs
}


pub fn init_file_logger(
    cfg: Configuration,
) -> Option<SubscriberBuilder<DefaultFields, Format<Full, ChronoLocal>, LevelFilter, File>> {
    let log_level: LevelFilter = cfg.log_level.into();

    // If the log file is either "stdout" or an invalid file,
    // bypass this block and go to the end, initializing a
    // SimpleLogger (for console logging)
    if cfg.log_file == "stdout" {
        return None;
    }

    // create(true): a fresh install has no log file yet, and failing to
    // *create* one shouldn't silently demote logging to stdout
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_file)
    {
        Ok(log_file) => {
            let file_logger = tracing_subscriber::fmt()
                .with_writer(log_file)
                .with_max_level(log_level)
                .with_timer(ChronoLocal::rfc_3339());
            println!(
                "[PRE-LOG: INFO]: Logging to '{}' at {}-level verbosity.",
                cfg.log_file, log_level
            );
            Some(file_logger)
        }

        Err(open_err) => {
            println!(
                "[PRE-LOG: WARN]: Failed to open logfile '{}' \
                due to the the following error: {}, {}.",
                cfg.log_file,
                open_err.kind(),
                open_err
            );
            println!("[PRE-LOG: WARN]: Logging to stdout at {log_level}-level verbosity.");
            None
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Configuration {
        from_str::<Configuration>(json).expect("valid config")
    }

    #[test]
    fn three_finger_drag_is_off_unless_asked_for() {
        assert!(!Configuration::default().three_finger_drag);
        let cfg: Configuration = from_str("{}").unwrap();
        assert!(!cfg.three_finger_drag);
        let cfg: Configuration = from_str(r#"{"threeFingerDrag": true}"#).unwrap();
        assert!(cfg.three_finger_drag);
    }

    #[test]
    fn momentum_output_defaults_to_wheel() {
        let cfg = parse("{}");
        let m = cfg.timing().momentum.expect("momentum on by default");
        assert_eq!(m.output, MomentumOutput::Wheel);
        assert_eq!(m.wheel_gain, Momentum::DEFAULT_WHEEL_GAIN);
        assert!(!m.wheel_invert);
        assert!(cfg.wants_wheel_device());
        assert_eq!(Configuration::default().momentum_output, MomentumOutputSetting::Wheel);
    }

    #[test]
    fn momentum_wheel_keys_parse() {
        let cfg = parse(
            r#"{"momentumOutput": "wheel", "momentumWheelGain": 40.5, "momentumWheelInvert": true}"#,
        );
        let m = cfg.timing().momentum.unwrap();
        assert_eq!(m.output, MomentumOutput::Wheel);
        assert_eq!(m.wheel_gain, 40.5);
        assert!(m.wheel_invert);
        assert!(cfg.wants_wheel_device());
        // momentum off: no device needed, no glide
        let cfg = parse(r#"{"momentum": false, "momentumOutput": "wheel"}"#);
        assert!(!cfg.wants_wheel_device());
        assert!(cfg.timing().momentum.is_none());
    }

    #[test]
    fn momentum_wheel_gain_out_of_range_falls_back() {
        for (json, want) in [
            (r#"{"momentumWheelGain": 0}"#, Momentum::DEFAULT_WHEEL_GAIN),
            (r#"{"momentumWheelGain": -3}"#, Momentum::DEFAULT_WHEEL_GAIN),
            (r#"{"momentumWheelGain": 1e9}"#, 1000.0),
        ] {
            assert_eq!(parse(json).timing().momentum.unwrap().wheel_gain, want, "{json}");
        }
        // an unknown output is a config error (the whole file falls back
        // to defaults, like any other invalid value)
        assert!(from_str::<Configuration>(r#"{"momentumOutput": "whee"}"#).is_err());
    }
}
