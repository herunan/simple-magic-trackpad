//! The gesture state machine: the pure, I/O-free heart of the proxy.
//!
//! Everything time- and state-sensitive about classifying touches lives
//! here, decoupled from the devices themselves: the machine consumes raw
//! evdev frames (as plain `Ev` triples) plus an explicit `Instant`, and
//! emits explicit [`Output`] actions for the I/O shell (`mt_proxy`) to
//! carry out. No file descriptors, no wall clock, no side effects --
//! which means every historical regression this project has ever hit
//! (phantom right-clicks from staggered liftoffs, 4-finger swipes
//! misread as drags, stuck virtual buttons) is reproducible in a plain
//! `cargo test`, no fingers required. See the `tests` module at the
//! bottom: that suite is the project's institutional memory.
//!
//! Overview of the classification model (unchanged from the proven
//! design, now with the state isolated):
//!
//! * A "touch" spans from the first finger down to the last finger up,
//!   tracked as a whole -- never judged frame-by-frame, because real
//!   fingers land and lift asynchronously.
//! * A fresh touch is *buffered* (withheld from the compositor) until it
//!   is classified: a lone finger settles after a short `probe_delay`
//!   (so ordinary pointer motion never feels delayed), an ambiguous 2-3
//!   finger touch waits out `entry_debounce`, and reaching 4+ fingers
//!   settles it instantly (nothing with 4 fingers is ours).
//! * A touch that holds at exactly 3 fingers through the debounce window
//!   becomes a drag: buffered frames are discarded, the compositor never
//!   learns those fingers existed, and finger motion drives the virtual
//!   mouse instead.
//! * Once a drag starts it only ends when *every* finger lifts
//!   (hysteresis: staggered liftoff must not leak trailing 1-2 finger
//!   touches, which libinput would read as a right-click tap).
//! * A settled non-drag touch is relayed live, frame by frame, verbatim.
//!
//! # Momentum
//!
//! A live 2-finger scroll that lifts while moving (a flick) doesn't end:
//! the lift is withheld and its two fingers keep coasting on the clone
//! ([`Momentum`], `glide_tick`) until the glide is slow, too old, or a
//! touch stops it. The release velocity is the least-squares slope of the
//! centroid over the touch's last 80 ms (`VELOCITY_WINDOW`), which is what
//! predicts the Mac's first momentum delta (measured, research doc section 11).
//!
//! The glide follows macOS, as copied by Apple's WebKit engineers in
//! `MomentumEventDispatcher.cpp` (see docs/apple-momentum-research.md).
//! macOS decays the *unaccelerated* (finger) speed and runs every
//! momentum event through the scroll acceleration curve again, exactly
//! like finger scrolling, and so does this: the glide moves synthetic
//! fingers and libinput applies the Mac scroll curve to them. Every 8 ms
//! the finger speed `v` is multiplied by
//! `alpha(v) = fast - (fast - tail) * max(0, (tail_speed - v)/tail_speed)`
//! (Mac: fast 0.975 = tau 316 ms, tail 0.91 = tau 85 ms, tail speed
//! 15.875 mm/s), and the glide ends once each axis is at or below the
//! stop speed (Mac: 1.905 mm/s). The steeper decay at low speed is what
//! makes slow flicks fade out quickly and smoothly instead of trailing.
//!
//! ## Wheel output (`MomentumOutput::Wheel`, off by default)
//!
//! macOS keeps the momentum phase apart from the finger phase; Wayland
//! has no momentum phase, and glide fingers that are "still down" keep
//! Firefox/Zen's overscroll stretched at a page limit until the glide
//! ends. In wheel mode the lift is relayed to the clone at once (the app
//! sees the touch end) and the same decay model drives high-resolution
//! wheel events on a separate virtual device instead ([`Output::Wheel`]).
//! libinput's scroll curve doesn't apply to those, so finger travel is
//! converted linearly: `wheel units (1/120 notch) = finger mm *
//! wheel_gain`. The sign assumes natural scrolling on the touchpad and
//! not on the wheel device (KWin applies it per device); `wheel_invert`
//! flips both axes. A touch stops the glide at once: there are no glide
//! fingers, so nothing to hold. The glide keeps the knobs it started
//! with, so a hot reload applies from the next glide.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0x00;
pub const SYN_DROPPED: u16 = 0x03;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_TOUCH_MAJOR: u16 = 0x30;
pub const ABS_MT_TOUCH_MINOR: u16 = 0x31;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;

/// Hard upper bound on tracked slots; the effective count comes from the
/// device's ABS_MT_SLOT range at construction.
pub const MAX_SLOTS: usize = 16;

/// px-per-mm scale for turning the real finger delta into cursor
/// movement; combines with the `acceleration` config knob on top.
/// 12.0 (4.0 x the initial guess) is the value confirmed to feel right
/// live on the MacBookPro11,3 pad.
pub const PX_PER_MM: f64 = 12.0;

/// A raw evdev event stripped to the fields that matter. Mirrors
/// `input_event` minus the timestamp (the kernel re-stamps everything
/// written to uinput anyway).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ev {
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

impl Ev {
    pub const fn new(type_: u16, code: u16, value: i32) -> Self {
        Ev { type_, code, value }
    }
    pub const fn abs(code: u16, value: i32) -> Self {
        Ev::new(EV_ABS, code, value)
    }
    pub const fn syn() -> Self {
        Ev::new(EV_SYN, SYN_REPORT, 0)
    }
}

/// An effect the I/O shell must apply, in order.
#[derive(Clone, PartialEq, Debug)]
pub enum Output {
    /// Write these events to the synthetic touchpad clone.
    EmitSynth(Vec<Ev>),
    /// Press the virtual mouse's left button.
    MouseDown,
    /// Release the virtual mouse's left button.
    MouseUp,
    /// Move the cursor by whole pixels (fractional remainders are
    /// carried inside the machine so slow drags don't lose motion).
    MouseMove { dx: i32, dy: i32 },
    /// Scroll the momentum wheel device (wheel-mode glide only), in
    /// evdev's signs (positive = wheel up / right). `v120`/`h120` are
    /// REL_WHEEL_HI_RES / REL_HWHEEL_HI_RES (120 = one notch); `v`/`h`
    /// are the whole notches for REL_WHEEL / REL_HWHEEL. Both remainders
    /// are carried inside the machine for the rest of the glide.
    Wheel { v120: i32, h120: i32, v: i32, h: i32 },
}

/// The timing/scaling knobs the machine needs; derived from the user
/// config (and re-derivable on hot reload via [`GestureMachine::set_timing`]).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub probe_delay: Duration,
    pub entry_debounce: Duration,
    /// 0 disables drag-lock entirely (the default). When > 0, lifting
    /// all fingers keeps the virtual button held for this long; a new
    /// touch that settles as a 3-finger drag resumes the same drag,
    /// while a touch that settles as anything else releases the button
    /// *before* its buffered events are relayed -- so ordinary pointer
    /// motion after a drag can never smear the held button around
    /// (the exact regression the first drag-lock attempt shipped).
    pub drag_end_delay: Duration,
    /// How long after a drag commits the button press is deferred when
    /// the fingers haven't moved yet. The press fires at the first
    /// actual drag motion or when this grace expires -- whichever comes
    /// first -- so a 4th finger landing *after* the entry window (a
    /// fast/sloppy 4-finger swipe, whose fingers stagger more the
    /// faster the hand comes down) can abort the misclassified drag
    /// without a phantom click ever having been sent.
    pub press_grace: Duration,
    /// Combined px-per-mm * user acceleration factor.
    pub px_per_mm: f64,
    /// Whether a 3-finger touch becomes a drag; when false it's relayed
    /// like any other gesture. Applies from the next touch on a hot reload:
    /// a drag in progress carries on until its fingers lift.
    pub three_finger_drag: bool,
    /// Momentum scrolling after a 2-finger flick; `None` turns it off.
    pub momentum: Option<Momentum>,
}

/// Momentum (glide) knobs. See the "Momentum" section of the module doc.
/// Speeds are finger (unaccelerated) speeds in mm/ms.
#[derive(Clone, Copy, Debug)]
pub struct Momentum {
    /// Speed factor per 8 ms while the glide is fast.
    pub fast_decay: f64,
    /// Speed factor per 8 ms as the speed approaches zero; blended in
    /// linearly below `tail_speed`.
    pub tail_decay: f64,
    pub tail_speed: f64,
    /// The glide ends once each axis is at or below this.
    pub stop_speed: f64,
    /// Minimum finger speed at lift-off for a glide to start. macOS's rule
    /// isn't public; 20 mm/s is where its glides start when measured over
    /// `VELOCITY_WINDOW` (they overlap with non-glides from 17 to 24 mm/s).
    pub min_speed: f64,
    /// When a touch stops a glide, the glide fingers hold still this long
    /// before lifting. GTK, Firefox and Chromium start their own fling at
    /// scroll-stop from the last ~150 ms of motion; holding still past
    /// that window makes them see zero velocity, so touch-to-stop stops.
    /// Fingers mode only.
    pub stop_hold: Duration,
    /// How the glide reaches the compositor. Read when a glide starts.
    pub output: MomentumOutput,
    /// Wheel mode: hi-res wheel units (1/120 notch) per mm of finger
    /// glide. See [`Momentum::DEFAULT_WHEEL_GAIN`].
    pub wheel_gain: f64,
    /// Wheel mode: flip both wheel axes (the default assumes natural
    /// scrolling on the touchpad and not on the wheel device).
    pub wheel_invert: bool,
}

/// Where a glide goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MomentumOutput {
    /// Two synthetic fingers keep moving on the clone (the lift is
    /// withheld until the glide ends).
    #[default]
    Fingers,
    /// The lift is relayed at once; the glide is wheel events on a
    /// separate virtual device.
    Wheel,
}

impl Momentum {
    // WebKit MomentumEventDispatcher: defaultDecay, tailDecay, and
    // tailVelocity 250 / stop 0.5-per-60Hz-frame in 400 dpi counts.
    pub const MAC_FAST_DECAY: f64 = 0.975;
    pub const MAC_TAIL_DECAY: f64 = 0.91;
    pub const MAC_TAIL_SPEED: f64 = 250.0 / 400.0 * 25.4 / 1000.0; // 15.875 mm/s
    pub const MAC_STOP_SPEED: f64 = 30.0 / 400.0 * 25.4 / 1000.0; // 1.905 mm/s
    pub const DEFAULT_MIN_SPEED: f64 = 0.02; // mm/ms = 20 mm/s, measured on a Mac
    pub const DEFAULT_STOP_HOLD: Duration = Duration::from_millis(160);
    /// Wheel units per finger mm: 24 (one notch per 5 mm of glide).
    /// On macOS (scroll setting 0.3125, the curve in accel/) a glide
    /// scrolls ~8 px per finger mm from a 100 mm/s flick, 12 at 200 and
    /// 15.5 at 300 mm/s; 12 px/mm is the middle of a normal flick. KDE
    /// apps scroll about 60 px per notch (Qt and Firefox: 3 lines of
    /// ~20 px; GTK 4 scrolls page^(2/3), ~85 px on an 800 px view), so
    /// 12 px/mm = 0.2 notch/mm = 24 units/mm. Being linear, it glides
    /// slow flicks a bit further and fast ones a bit shorter than the
    /// Mac curve does.
    pub const DEFAULT_WHEEL_GAIN: f64 = 24.0;

    /// macOS's glide, with this daemon's start threshold and stop hold.
    pub fn mac() -> Self {
        Momentum {
            fast_decay: Self::MAC_FAST_DECAY,
            tail_decay: Self::MAC_TAIL_DECAY,
            tail_speed: Self::MAC_TAIL_SPEED,
            stop_speed: Self::MAC_STOP_SPEED,
            min_speed: Self::DEFAULT_MIN_SPEED,
            stop_hold: Self::DEFAULT_STOP_HOLD,
            output: MomentumOutput::Fingers,
            wheel_gain: Self::DEFAULT_WHEEL_GAIN,
            wheel_invert: false,
        }
    }

    /// Speed factor per 8 ms at finger speed `v` (mm/ms).
    pub fn alpha(&self, v: f64) -> f64 {
        let weak = if self.tail_speed > 0.0 {
            ((self.tail_speed - v) / self.tail_speed).max(0.0)
        } else {
            0.0
        };
        self.fast_decay - (self.fast_decay - self.tail_decay) * weak
    }

    /// Wheel units `(h, v)` in evdev's signs (+h = right, +v = up) for a
    /// finger travel of `(dx_mm, dy_mm)` (pad axes: +x right, +y down).
    /// With natural scrolling on the touchpad the content follows the
    /// fingers: fingers down = scroll up = +v, fingers right = scroll
    /// left = -h, which is what those signs do on a wheel device without
    /// natural scrolling (libinput's default for a new device).
    /// `wheel_invert` flips both axes for any other combination.
    pub fn wheel_units(&self, dx_mm: f64, dy_mm: f64) -> (f64, f64) {
        let gain = if self.wheel_invert {
            -self.wheel_gain
        } else {
            self.wheel_gain
        };
        (-dx_mm * gain, dy_mm * gain)
    }
}

/// Glide frames are emitted this often (~125 Hz, a bit above the pad's
/// own report rate, so libinput sees a smooth stream).
const GLIDE_FRAME: Duration = Duration::from_millis(8);
/// After the first finger of a flick lifts, how long the others get to
/// follow before the glide gives up and treats them as a new touch.
const GLIDE_AWAIT_LIFT: Duration = Duration::from_millis(60);
/// Release velocity is measured over this much of the touch's end...
const VELOCITY_WINDOW: Duration = Duration::from_millis(80);
/// ...and the fingers must still be moving over this last part of it, so a
/// scroll that stops just before the lift doesn't glide on its older motion...
const VELOCITY_MOVING_WINDOW: Duration = Duration::from_millis(40);
/// ...needs at least this span of samples...
const VELOCITY_MIN_SPAN: Duration = Duration::from_millis(15);
/// ...and the newest sample must be this recent (fingers that stopped,
/// then lifted, don't glide).
const VELOCITY_MAX_AGE: Duration = Duration::from_millis(30);
/// ...or after this long, whatever its speed.
const GLIDE_MAX: Duration = Duration::from_secs(3);
/// A wheel-mode glide frame later than this after the previous one means
/// the event loop stalled; the glide stops rather than scroll the gap.
const WHEEL_GLIDE_STALL: Duration = Duration::from_millis(100);
/// Release speeds above this (mm/ms) are clamped: no real flick is this
/// fast, so it's a tracking glitch.
const GLIDE_MAX_SPEED: f64 = 3.0;
/// libinput ends an Apple touch whose size drops below 10 (the quirk's
/// AttrTouchSizeRange=20:10), which fingers do in the frames before they
/// leave the pad. A 2-finger scroll's sizes are floored at this, so
/// libinput can't end the scroll before the lift that starts a glide.
/// 16 sits between the two thresholds: it keeps a live touch alive but
/// never makes a hovering one begin.
const SIZE_FLOOR: i32 = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
enum GlidePhase {
    /// The first finger has lifted; the rest are expected to follow.
    AwaitLift { deadline: Instant },
    /// All real fingers are up; the synthetic ones are coasting.
    Gliding,
    /// A touch stopped the glide; the synthetic fingers hold still
    /// until `until`, then lift.
    Holding { until: Instant },
}

/// Two synthetic fingers left on the clone after a real flick, coasting
/// (or, in wheel mode, the same coast emitted as wheel events).
#[derive(Clone, Copy, Debug)]
struct Glide {
    slots: [usize; 2],
    pos: [(f64, f64); 2],
    /// Current finger velocity, device units per ms, per axis.
    v: (f64, f64),
    last: Instant,
    started: Instant,
    phase: GlidePhase,
    /// Set for a wheel-mode glide: no glide fingers are on the clone.
    wheel: Option<WheelGlide>,
}

/// A wheel-mode glide's own state.
#[derive(Clone, Copy, Debug)]
struct WheelGlide {
    /// The knobs at the start: a hot reload applies from the next glide.
    m: Momentum,
    /// Hi-res units not yet emitted (always under one unit), (h, v).
    carry: (f64, f64),
    /// Hi-res units emitted but not yet a whole REL_WHEEL notch, (h, v).
    notch: (i32, i32),
}

#[derive(Clone, Copy)]
struct Slot {
    tracking_id: i32,
    x: i32,
    y: i32,
}

impl Default for Slot {
    fn default() -> Self {
        Slot {
            tracking_id: -1,
            x: 0,
            y: 0,
        }
    }
}

/// Authoritative per-slot state, as re-read from the kernel after a
/// SYN_DROPPED (EVIOCGMTSLOTS). `(tracking_id, x, y)`.
pub type SlotSnapshot = [(i32, i32, i32)];

pub struct GestureMachine {
    timing: Timing,
    x_res: f64,
    y_res: f64,
    slot_count: usize,

    slots: [Slot; MAX_SLOTS],
    current_slot: usize,
    /// Which slots the *synthetic clone* currently believes are active.
    relayed_active: [bool; MAX_SLOTS],

    /// True while a 3-finger drag owns the touch: nothing is relayed.
    suppressing: bool,
    drag_ref_slot: Option<usize>,
    drag_last_pos: Option<(i32, i32)>,
    /// Sub-pixel motion carried between frames.
    carry: (f64, f64),
    /// While a committed drag hasn't moved yet: when to press the button
    /// anyway (see [`Timing::press_grace`]). Cleared once pressed.
    press_deadline: Option<Instant>,

    /// Last EV_KEY values seen from the REAL device (BTN_TOUCH,
    /// BTN_TOOL_*...). The truth about tool state on the pad.
    real_keys: Vec<(u16, i32)>,
    /// Last EV_KEY values the CLONE has been told. Needed to emit
    /// consistent closing state (BTN_TOUCH=0 etc.) when suppression
    /// yanks a partially-relayed touch away -- leaving these stuck at 1
    /// desyncs libinput's finger accounting (and with tap-to-click on,
    /// a slot release without tool release reads as a 2-finger tap:
    /// phantom right-click).
    clone_keys: Vec<(u16, i32)>,

    /// Frames withheld from the compositor while a fresh touch is still
    /// being classified.
    pending: Vec<Ev>,
    touch_start: Option<Instant>,
    touch_max: usize,
    settled: bool,

    /// Virtual left button state (survives across touches for drag-lock).
    held: bool,
    /// When set, the button stays held until this instant unless a new
    /// touch resolves the lock first (see [`Timing::drag_end_delay`]).
    lock_deadline: Option<Instant>,

    /// Active real fingers before the current frame was folded in.
    prev_count: usize,
    /// Centroid samples `(time, x, y)` of the live 2-finger touch, for
    /// the release velocity.
    history: VecDeque<(Instant, f64, f64)>,
    /// The two slots of the live 2-finger touch and the positions last
    /// relayed for them (what the clone holds).
    two: Option<[(usize, i32, i32); 2]>,
    glide: Option<Glide>,
    /// The clone's slot positions/tool bits may differ from the real
    /// pad's (a glide moved or released them): the next flush re-asserts
    /// the real state.
    needs_sync: bool,
}

impl GestureMachine {
    pub fn new(timing: Timing, x_res: f64, y_res: f64, slot_count: usize) -> Self {
        GestureMachine {
            timing,
            x_res: x_res.max(1.0),
            y_res: y_res.max(1.0),
            slot_count: slot_count.clamp(1, MAX_SLOTS),
            slots: [Slot::default(); MAX_SLOTS],
            current_slot: 0,
            relayed_active: [false; MAX_SLOTS],
            suppressing: false,
            drag_ref_slot: None,
            drag_last_pos: None,
            carry: (0.0, 0.0),
            press_deadline: None,
            real_keys: Vec::new(),
            clone_keys: Vec::new(),
            pending: Vec::new(),
            touch_start: None,
            touch_max: 0,
            settled: false,
            held: false,
            lock_deadline: None,
            prev_count: 0,
            history: VecDeque::new(),
            two: None,
            glide: None,
            needs_sync: false,
        }
    }

    /// Whether a glide is running: synthetic glide fingers on the clone
    /// (fingers mode) or wheel events (wheel mode).
    pub fn gliding(&self) -> bool {
        self.glide.is_some()
    }

    /// Whether the clone is holding a fingers-mode glide's fingers, so
    /// the real pad's frames must wait behind them.
    fn glide_holds_clone(&self) -> bool {
        matches!(self.glide, Some(g) if g.wheel.is_none())
    }

    /// Hot-reload hook: swap in new timing/scaling without disturbing
    /// any in-flight touch state.
    pub fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
    }

    /// Whether the virtual button is currently held (used by the shell
    /// for a defensive release on shutdown).
    pub fn button_held(&self) -> bool {
        self.held
    }

    /// The next instant at which [`on_tick`](Self::on_tick) has work to
    /// do, if any. The I/O loop sleeps exactly until this, so decisions
    /// land on time instead of on the next poll interval.
    pub fn next_deadline(&self) -> Option<Instant> {
        let glide = self.glide.map(|g| match g.phase {
            GlidePhase::Holding { until } => until,
            GlidePhase::AwaitLift { deadline } => deadline.min(g.last + GLIDE_FRAME),
            GlidePhase::Gliding => g.last + GLIDE_FRAME,
        });
        match (self.touch_deadline(), glide) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn touch_deadline(&self) -> Option<Instant> {
        if self.suppressing {
            // a committed drag that hasn't moved yet still owes a
            // deferred button press
            return if self.held { None } else { self.press_deadline };
        }
        if let Some(start) = self.touch_start {
            if !self.settled {
                let window = if self.touch_max <= 1 {
                    self.timing.probe_delay
                } else {
                    self.timing.entry_debounce
                };
                return Some(start + window);
            }
            return None; // settled live touch: nothing timed left to decide
        }
        self.lock_deadline
    }

    /// Wall-clock-only work: classification windows closing on a touch
    /// that's holding perfectly still, the deferred drag press, and the
    /// drag-lock timeout.
    pub fn on_tick(&mut self, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        self.glide_tick(now, &mut out);
        if self.suppressing {
            // Stationary drag: no motion has pressed the button yet, and
            // no 4th finger has shown up to abort -- commit the press.
            if !self.held {
                if let Some(deadline) = self.press_deadline {
                    if now >= deadline {
                        self.press_button(&mut out);
                    }
                }
            }
            return out;
        }

        if self.touch_start.is_none() {
            if let Some(deadline) = self.lock_deadline {
                if now >= deadline {
                    self.lock_deadline = None;
                    self.release_button(&mut out);
                }
            }
            return out;
        }

        if self.settled {
            return out;
        }

        let count = self.active_count();
        let start = self.touch_start.expect("guarded by is_none() above");

        if count == 1 && self.touch_max == 1 && now >= start + self.timing.probe_delay {
            self.settled = true;
            self.flush_pending(&mut out);
            return out;
        }
        if now >= start + self.timing.entry_debounce {
            self.resolve_touch_decision(count, now, &mut out);
        }
        out
    }

    /// Feed one complete frame (everything up to and including its
    /// SYN_REPORT). Partial frames interrupted by SYN_DROPPED must not
    /// be fed; discard them and call [`on_resync`](Self::on_resync)
    /// with a fresh kernel snapshot instead.
    pub fn on_frame(&mut self, frame: &[Ev], now: Instant) -> Vec<Output> {
        self.prev_count = self.active_count();
        // 1. fold the frame's slot and key-state updates into our model
        for ev in frame {
            if ev.type_ == EV_KEY {
                Self::note_key(&mut self.real_keys, ev.code, ev.value);
                continue;
            }
            if ev.type_ != EV_ABS {
                continue;
            }
            match ev.code {
                ABS_MT_SLOT => {
                    self.current_slot = (ev.value.max(0) as usize).min(self.slot_count - 1);
                }
                ABS_MT_TRACKING_ID => self.slots[self.current_slot].tracking_id = ev.value,
                ABS_MT_POSITION_X => self.slots[self.current_slot].x = ev.value,
                ABS_MT_POSITION_Y => self.slots[self.current_slot].y = ev.value,
                _ => {}
            }
        }
        // 2. decide what to do about it
        let mut out = Vec::new();
        self.decide(frame, now, &mut out);
        out
    }

    /// Reconcile with an authoritative kernel snapshot after the kernel
    /// reported dropped events (SYN_DROPPED). Whatever the dropped
    /// events said is gone; `snapshot` is the truth now.
    pub fn on_resync(&mut self, snapshot: &SlotSnapshot, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        // Whatever the dropped events were, a glide can't sensibly go on:
        // lift its fingers first so the correction below starts clean.
        if self.glide.is_some() {
            self.end_glide(&mut out);
        }
        self.history.clear();
        self.two = None;

        for slot in 0..MAX_SLOTS {
            self.slots[slot] = match snapshot.get(slot) {
                Some(&(id, x, y)) => Slot {
                    tracking_id: id,
                    x,
                    y,
                },
                None => Slot::default(),
            };
        }
        // no 2 -> fewer transition may be read into a resync
        self.prev_count = self.active_count();

        if self.suppressing {
            // drive_drag reads self.slots directly next frame; the drop
            // may have moved the reference finger arbitrarily far, so
            // re-baseline rather than applying the gap as a cursor jump.
            self.drag_last_pos = None;
        } else if self.touch_start.is_some() && !self.settled && self.active_count() == 0 {
            // Every finger lifted *inside* the dropped window while the
            // touch was still buffered. The buffer holds touchdowns whose
            // matching releases were dropped -- flushing it would leave
            // the synthetic device holding a touch forever. Swallow the
            // (rare) truncated tap instead; consistency wins.
            self.pending.clear();
            self.touch_start = None;
            self.touch_max = 0;
        } else {
            // The synthetic device (or the buffer of a still-undecided
            // touch) may hold stale state. Correct it explicitly: release
            // any slot the clone believes is active but no longer is,
            // then assert the authoritative position of every live slot.
            let mut correction = Vec::new();
            for slot in 0..self.slot_count {
                if self.relayed_active[slot] && self.slots[slot].tracking_id < 0 {
                    correction.push(Ev::abs(ABS_MT_SLOT, slot as i32));
                    correction.push(Ev::abs(ABS_MT_TRACKING_ID, -1));
                }
            }
            correction.extend(self.active_slot_dump());
            if !correction.is_empty() {
                if self.touch_start.is_some() && !self.settled {
                    self.pending.extend(correction);
                    self.pending.push(Ev::syn());
                } else {
                    let mut frame = correction;
                    frame.push(Ev::syn());
                    self.mark_relayed();
                    out.push(Output::EmitSynth(frame));
                }
            }
        }

        // The dropped events may have included liftoffs (even the whole
        // touch ending). Run the normal decision logic against the new
        // state with an empty frame so we can't be left suppressing (or
        // buffering) a touch that no longer exists.
        self.decide(&[], now, &mut out);
        out
    }

    // ---- internals ----------------------------------------------------

    fn active_slots(&self) -> Vec<usize> {
        (0..self.slot_count)
            .filter(|&s| self.slots[s].tracking_id >= 0)
            .collect()
    }

    fn active_count(&self) -> usize {
        (0..self.slot_count)
            .filter(|&s| self.slots[s].tracking_id >= 0)
            .count()
    }

    fn decide(&mut self, frame: &[Ev], now: Instant, out: &mut Vec<Output>) {
        if self.glide.is_some() && !self.suppressing && self.glide_consumes(now, out) {
            return;
        }
        let active = self.active_slots();
        let count = active.len();

        // Once a drag has started, stay suppressed until every finger is
        // off, not just until the count first drops below 3. Fingers
        // never lift in perfect unison; without this hysteresis the
        // trailing 1-2 fingers of a liftoff would be relayed as a fresh
        // touch, which libinput reads as a 2-finger tap (right-click)
        // the moment they lift too.
        if self.suppressing {
            if count == 0 {
                self.suppressing = false;
                self.drag_ref_slot = None;
                self.drag_last_pos = None;
                self.press_deadline = None;
                // Reset touch bookkeeping: without this the next touch
                // would inherit touch_max/settled from this drag and
                // skip the debounce protection entirely.
                self.touch_start = None;
                self.touch_max = 0;
                self.settled = false;
                // A committed drag that never moved and lifted before
                // the press grace still owes its click: press now so the
                // release below (or the drag-lock) completes it.
                self.press_button(out);
                if self.timing.drag_end_delay > Duration::ZERO {
                    // Drag-lock: keep the button held; a new 3-finger
                    // touch inside the window resumes the drag, anything
                    // else releases it (see flush_pending / on_tick).
                    self.lock_deadline = Some(now + self.timing.drag_end_delay);
                } else {
                    self.release_button(out);
                }
                // The synth clone has nothing active on it (suppression
                // never relayed anything), so there's nothing to resync.
                return;
            }
            if count >= 4 {
                // A 4th finger arrived AFTER the entry window closed --
                // this was never a drag, it's a fast/sloppy 4-finger
                // gesture whose last finger staggered in late (the
                // faster the hand comes down, the bigger the stagger).
                // Abort: release the touch to the compositor mid-gesture
                // so the rest of the swipe still registers. Thanks to
                // the deferred press, in the common case no button was
                // ever pressed, so nothing to undo.
                if self.held {
                    warn!(
                        "4th finger after the drag already pressed the button; \
                        releasing (a brief phantom click was unavoidable)"
                    );
                } else {
                    debug!("late 4th finger: aborting committed drag, handing touch to compositor");
                }
                self.suppressing = false;
                self.drag_ref_slot = None;
                self.drag_last_pos = None;
                self.press_deadline = None;
                self.settled = true; // continues as an ordinary live touch
                self.release_button(out);
                // Introduce the touch to the clone as a fresh, complete,
                // consistent touchdown: all live slots plus the real
                // pad's current tool state (BTN_TOUCH/BTN_TOOL_*).
                let mut intro = self.active_slot_dump();
                for i in 0..self.real_keys.len() {
                    let (code, value) = self.real_keys[i];
                    if Self::key_value(&self.clone_keys, code) != value {
                        intro.push(Ev::new(EV_KEY, code, value));
                        Self::note_key(&mut self.clone_keys, code, value);
                    }
                }
                intro.push(Ev::syn());
                self.mark_relayed();
                out.push(Output::EmitSynth(intro));
                return;
            }
            self.drive_drag(&active, out);
            // frame intentionally not relayed
            return;
        }

        // A live 2-finger touch losing fingers while moving: a flick.
        // Withhold the lift and keep its fingers coasting on the clone.
        if self.settled
            && self.touch_start.is_some()
            && self.touch_max == 2
            && self.prev_count == 2
            && count < 2
            && self.try_start_glide(count, now)
        {
            return;
        }
        if count != 2 {
            self.history.clear();
            self.two = None;
        }

        if count == 0 {
            let had_pending = self.touch_start.is_some() && !self.settled;
            self.touch_start = None;
            self.touch_max = 0;
            self.settled = false;
            if had_pending {
                // Touch ended before a decision was reached (e.g. a
                // quick tap): flush everything buffered, including this
                // release frame, so the tap isn't silently swallowed.
                self.pending.extend_from_slice(frame);
                self.flush_pending(out);
                return;
            }
            // Already-settled touch ending (most touches): this frame
            // carries the release events the compositor needs to see.
            self.relay_frame(frame, out);
            return;
        }

        if self.touch_start.is_none() {
            // the first frame of a brand new touch
            self.touch_start = Some(now);
            self.touch_max = count;
            self.settled = false;
            self.pending.clear();
        } else {
            self.touch_max = self.touch_max.max(count);
        }

        if self.settled {
            // Already decided this touch is an ordinary gesture -- relay
            // live. One exception: a touch that *grew* to exactly 3
            // without ever lifting (1->2->3 well after the window
            // closed) is a deliberate late drag and must not leak
            // through as a real 3-finger touch. The touch_max == 3
            // guard is what keeps the *other* way of hitting count == 3
            // -- a 4-finger gesture shedding a finger (4->3), which
            // happens at the tail of every 4-finger swipe because
            // fingers never lift in unison -- from being hijacked into
            // a phantom drag + click.
            if self.timing.three_finger_drag && count == 3 && self.touch_max == 3 {
                self.commit_drag(&active, now, out);
                return;
            }
            if count == 2 && self.touch_max == 2 && self.timing.momentum.is_some() {
                self.note_two_finger_sample(&active, now);
                let floored = Self::floor_sizes(frame);
                self.relay_frame(&floored, out);
                return;
            }
            self.relay_frame(frame, out);
            return;
        }

        self.pending.extend_from_slice(frame);

        if self.touch_max >= 4 {
            // Unambiguously bigger than a 3-finger drag could ever be --
            // no need to wait out the rest of the window.
            self.settled = true;
            self.flush_pending(out);
            return;
        }

        let start = self.touch_start.expect("set above when the touch began");

        if count == 1 && self.touch_max == 1 && now >= start + self.timing.probe_delay {
            // Still just one finger after a short probe: ordinary
            // pointer movement, by far the most common case. Go live now
            // rather than waiting out the full entry_debounce, or every
            // touch-lift-reposition cycle of normal cursor use would add
            // a felt hitch.
            self.settled = true;
            self.flush_pending(out);
            return;
        }

        if now >= start + self.timing.entry_debounce {
            self.resolve_touch_decision(count, now, out);
        }
    }

    /// The entry_debounce window has closed: commit to a drag if the
    /// touch held stably at exactly 3 fingers the whole time, otherwise
    /// release it to the compositor as an ordinary gesture.
    fn resolve_touch_decision(&mut self, count: usize, now: Instant, out: &mut Vec<Output>) {
        if self.timing.three_finger_drag && count == 3 && self.touch_max == 3 {
            self.pending.clear();
            let active = self.active_slots();
            self.commit_drag(&active, now, out);
            return;
        }
        self.settled = true;
        self.flush_pending(out);
    }

    /// Commit the current touch as a 3-finger drag. The button press is
    /// DEFERRED: it fires at the first actual drag motion, or when
    /// press_grace expires -- so a late 4th finger (fast 4-finger swipe)
    /// can still abort without a phantom click having been sent.
    fn commit_drag(&mut self, active: &[usize], now: Instant, out: &mut Vec<Output>) {
        debug!("3-finger touch committed as a drag");
        self.settled = true;
        self.enter_suppress(out);
        if !self.held {
            self.press_deadline = Some(now + self.timing.press_grace);
        }
        self.drive_drag(active, out);
    }

    /// Releases a buffered touch to the compositor: it either never
    /// became a drag, or grew past 3 into a bigger gesture that isn't
    /// ours to intercept. If a drag-lock is pending, the button is
    /// released *first*, so the flushed motion can never drag anything.
    fn flush_pending(&mut self, out: &mut Vec<Output>) {
        if self.glide_holds_clone() {
            // the clone still holds the glide fingers: keep buffering
            // until end_glide lifts them, which flushes this
            return;
        }
        if self.lock_deadline.take().is_some() {
            self.release_button(out);
        }
        if !self.pending.is_empty() {
            let flushed = std::mem::take(&mut self.pending);
            self.note_clone_keys(&flushed);
            out.push(Output::EmitSynth(flushed));
        }
        if self.needs_sync {
            self.needs_sync = false;
            self.push_sync_frame(out);
        }
        self.mark_relayed();
    }

    /// After a glide: assert the real pad's live slots (positions the
    /// kernel won't resend if unchanged) and tool bits on the clone.
    fn push_sync_frame(&mut self, out: &mut Vec<Output>) {
        let mut sync = self.active_slot_dump();
        for i in 0..self.real_keys.len() {
            let (code, value) = self.real_keys[i];
            if Self::key_value(&self.clone_keys, code) != value {
                sync.push(Ev::new(EV_KEY, code, value));
                Self::note_key(&mut self.clone_keys, code, value);
            }
        }
        if !sync.is_empty() {
            sync.push(Ev::syn());
            out.push(Output::EmitSynth(sync));
        }
    }

    fn relay_frame(&mut self, frame: &[Ev], out: &mut Vec<Output>) {
        if self.glide_holds_clone() {
            self.pending.extend_from_slice(frame);
            return;
        }
        self.mark_relayed();
        if !frame.is_empty() {
            self.note_clone_keys(frame);
            out.push(Output::EmitSynth(frame.to_vec()));
        }
    }

    fn mark_relayed(&mut self) {
        for slot in 0..MAX_SLOTS {
            self.relayed_active[slot] = self.slots[slot].tracking_id >= 0;
        }
    }

    /// Record what EV_KEY state a batch of events tells the clone.
    fn note_clone_keys(&mut self, events: &[Ev]) {
        for ev in events {
            if ev.type_ == EV_KEY {
                Self::note_key(&mut self.clone_keys, ev.code, ev.value);
            }
        }
    }

    fn note_key(map: &mut Vec<(u16, i32)>, code: u16, value: i32) {
        for entry in map.iter_mut() {
            if entry.0 == code {
                entry.1 = value;
                return;
            }
        }
        map.push((code, value));
    }

    fn key_value(map: &[(u16, i32)], code: u16) -> i32 {
        map.iter().find(|e| e.0 == code).map(|e| e.1).unwrap_or(0)
    }

    /// Begin withholding everything from the compositor. Any slots the
    /// synthetic clone still believes are active are explicitly released
    /// first -- and any tool state (BTN_TOUCH, BTN_TOOL_*) it was told is
    /// pressed is explicitly released too, so it is never left holding a
    /// half-open touch or stuck finger-count bits (which desync
    /// libinput's tap/gesture accounting).
    fn enter_suppress(&mut self, out: &mut Vec<Output>) {
        // A drag during a glide's hold: the release below lifts the
        // glide fingers too (they're in relayed_active). A wheel glide
        // has none; it just stops.
        if let Some(g) = self.glide.take() {
            if g.wheel.is_none() {
                self.needs_sync = true;
            }
        }
        self.suppressing = true;
        self.lock_deadline = None; // a live drag owns the button now
        self.carry = (0.0, 0.0);

        let mut release = Vec::new();
        for slot in 0..MAX_SLOTS {
            if self.relayed_active[slot] {
                release.push(Ev::abs(ABS_MT_SLOT, slot as i32));
                release.push(Ev::abs(ABS_MT_TRACKING_ID, -1));
                self.relayed_active[slot] = false;
            }
        }
        for i in 0..self.clone_keys.len() {
            let (code, value) = self.clone_keys[i];
            if value != 0 {
                release.push(Ev::new(EV_KEY, code, 0));
                self.clone_keys[i].1 = 0;
            }
        }
        if !release.is_empty() {
            release.push(Ev::syn());
            out.push(Output::EmitSynth(release));
        }
    }

    fn press_button(&mut self, out: &mut Vec<Output>) {
        if !self.held {
            self.held = true;
            self.press_deadline = None;
            out.push(Output::MouseDown);
        }
        // if still held from a drag-lock, the drag just resumes --
        // no re-press, no glitch
    }

    fn release_button(&mut self, out: &mut Vec<Output>) {
        if self.held {
            self.held = false;
            out.push(Output::MouseUp);
        }
    }

    fn drive_drag(&mut self, active: &[usize], out: &mut Vec<Output>) {
        let reference = match self.drag_ref_slot {
            Some(s) if active.contains(&s) => s,
            _ => {
                // first frame of the gesture, or the previous reference
                // finger lifted and another took its place: re-baseline
                // without applying a delta this frame.
                let s = active[0];
                self.drag_ref_slot = Some(s);
                self.drag_last_pos = Some((self.slots[s].x, self.slots[s].y));
                return;
            }
        };

        let (x, y) = (self.slots[reference].x, self.slots[reference].y);
        if let Some((lx, ly)) = self.drag_last_pos {
            let px = (x - lx) as f64 / self.x_res * self.timing.px_per_mm + self.carry.0;
            let py = (y - ly) as f64 / self.y_res * self.timing.px_per_mm + self.carry.1;
            let dx = px.trunc() as i32;
            let dy = py.trunc() as i32;
            // carry the sub-pixel remainder instead of discarding it, so
            // slow, precise drags don't systematically lose motion
            self.carry = (px - dx as f64, py - dy as f64);
            if dx != 0 || dy != 0 {
                // real drag motion: the deferred press (if still pending)
                // must land before the movement it accompanies
                self.press_button(out);
                out.push(Output::MouseMove { dx, dy });
            }
        } else {
            self.drag_last_pos = Some((x, y));
            return;
        }
        self.drag_last_pos = Some((x, y));
    }

    // ---- momentum ------------------------------------------------------

    fn floor_sizes(frame: &[Ev]) -> Vec<Ev> {
        frame
            .iter()
            .map(|ev| {
                if ev.type_ == EV_ABS
                    && (ev.code == ABS_MT_TOUCH_MAJOR || ev.code == ABS_MT_TOUCH_MINOR)
                    && ev.value < SIZE_FLOOR
                {
                    Ev::abs(ev.code, SIZE_FLOOR)
                } else {
                    *ev
                }
            })
            .collect()
    }

    fn note_two_finger_sample(&mut self, active: &[usize], now: Instant) {
        let (a, b) = (active[0], active[1]);
        let (sa, sb) = (self.slots[a], self.slots[b]);
        self.two = Some([(a, sa.x, sa.y), (b, sb.x, sb.y)]);
        let cx = (sa.x as f64 + sb.x as f64) / 2.0;
        let cy = (sa.y as f64 + sb.y as f64) / 2.0;
        self.history.push_back((now, cx, cy));
        while let Some(&(t, _, _)) = self.history.front() {
            if now.duration_since(t) > VELOCITY_WINDOW {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }

    fn speed_mm(&self, v: (f64, f64)) -> f64 {
        (v.0 / self.x_res).hypot(v.1 / self.y_res)
    }

    /// Finger speed over the end of the history (`VELOCITY_MOVING_WINDOW`
    /// before `last`), the same way as the release velocity.
    fn recent_speed_mm(&self, last: Instant) -> f64 {
        let recent: VecDeque<_> = self
            .history
            .iter()
            .filter(|s| last.duration_since(s.0) <= VELOCITY_MOVING_WINDOW)
            .copied()
            .collect();
        self.speed_mm(Self::release_velocity(&recent))
    }

    /// Release velocity (device units/ms per axis): the least-squares
    /// slope of the centroid over the history window. Less thrown by the
    /// last frame or two than last-minus-first, which matters because the
    /// centroid jumps as the contacts shrink just before the lift.
    /// Callers ensure >= 3 samples spanning > 0.
    fn release_velocity(history: &VecDeque<(Instant, f64, f64)>) -> (f64, f64) {
        let t0 = history[0].0;
        let n = history.len() as f64;
        let ms = |t: Instant| t.duration_since(t0).as_secs_f64() * 1000.0;
        let (mut st, mut sx, mut sy) = (0.0, 0.0, 0.0);
        for &(t, x, y) in history {
            st += ms(t);
            sx += x;
            sy += y;
        }
        let (mt, mx, my) = (st / n, sx / n, sy / n);
        let (mut stt, mut stx, mut sty) = (0.0, 0.0, 0.0);
        for &(t, x, y) in history {
            let dt = ms(t) - mt;
            stt += dt * dt;
            stx += dt * (x - mx);
            sty += dt * (y - my);
        }
        if stt <= 0.0 {
            return (0.0, 0.0);
        }
        (stx / stt, sty / stt)
    }

    /// The frame that lifts finger(s) off a live 2-finger touch. Returns
    /// true (frame withheld) if it was a flick and a glide started. A
    /// wheel-mode glide starts too but returns false: its lift is relayed.
    fn try_start_glide(&mut self, count: usize, now: Instant) -> bool {
        let (Some(m), Some(two)) = (self.timing.momentum, self.two) else {
            return false;
        };
        if self.glide.is_some() {
            // a flick inside a glide's hold: its fingers never reached
            // the clone, so there is nothing of it to continue
            return false;
        }
        // the clone must be holding exactly these two fingers
        let clone_count = self.relayed_active.iter().filter(|&&a| a).count();
        if clone_count != 2 || !self.relayed_active[two[0].0] || !self.relayed_active[two[1].0] {
            return false;
        }
        let (Some(&first), Some(&last)) = (self.history.front(), self.history.back()) else {
            return false;
        };
        let span = last.0.duration_since(first.0);
        if self.history.len() < 3
            || span < VELOCITY_MIN_SPAN
            || now.duration_since(last.0) > VELOCITY_MAX_AGE
        {
            return false;
        }
        let mut v = Self::release_velocity(&self.history);
        let speed = self.speed_mm(v);
        if speed < m.min_speed || self.recent_speed_mm(last.0) < m.min_speed {
            return false;
        }
        if speed > GLIDE_MAX_SPEED {
            let k = GLIDE_MAX_SPEED / speed;
            v = (v.0 * k, v.1 * k);
        }
        let wheel = (m.output == MomentumOutput::Wheel).then_some(WheelGlide {
            m,
            carry: (0.0, 0.0),
            notch: (0, 0),
        });
        debug!(
            "2-finger flick at {speed:.2} mm/ms: gliding ({})",
            if wheel.is_some() { "wheel" } else { "fingers" }
        );
        self.glide = Some(Glide {
            slots: [two[0].0, two[1].0],
            pos: [
                (two[0].1 as f64, two[0].2 as f64),
                (two[1].1 as f64, two[1].2 as f64),
            ],
            v,
            last: now,
            started: now,
            phase: if count == 0 {
                GlidePhase::Gliding
            } else {
                GlidePhase::AwaitLift {
                    deadline: now + GLIDE_AWAIT_LIFT,
                }
            },
            wheel,
        });
        if wheel.is_some() {
            // Wheel mode: the lift goes on to the clone like any other
            // frame (the caller relays it), so the app sees the touch end.
            return false;
        }
        self.history.clear();
        self.two = None;
        if count == 0 {
            self.end_touch_bookkeeping();
        }
        true
    }

    fn end_touch_bookkeeping(&mut self) {
        self.touch_start = None;
        self.touch_max = 0;
        self.settled = false;
        self.pending.clear();
    }

    /// Whatever real fingers are down now become a fresh, unclassified
    /// touch. Nothing of it has reached the clone; the sync frame at its
    /// flush introduces it.
    fn restart_touch(&mut self, now: Instant) {
        let count = self.active_count();
        self.end_touch_bookkeeping();
        if count > 0 {
            self.touch_start = Some(now);
            self.touch_max = count;
        }
    }

    fn stop_glide(&mut self, now: Instant) {
        if let (Some(g), Some(m)) = (self.glide.as_mut(), self.timing.momentum) {
            g.phase = GlidePhase::Holding {
                until: now + m.stop_hold,
            };
        }
    }

    /// A real frame arrived during a glide. Returns true if the glide
    /// consumed it (nothing more to decide).
    ///
    /// Two or more fingers down means scrolling again (often the other
    /// way): the glide fingers lift at once and the new touch goes
    /// through normally. The stop hold is only for touch-to-stop -- held
    /// back behind it, a quick re-flick reached libinput as one burst
    /// with no time in it, scrolled almost nothing and never glided.
    /// Apps with their own fling cancel it when the new scroll begins.
    fn glide_consumes(&mut self, now: Instant, out: &mut Vec<Output>) -> bool {
        let Some(g) = self.glide else { return false };
        if g.wheel.is_some() {
            self.wheel_glide_sees_frame();
            return false;
        }
        let count = self.active_count();
        match g.phase {
            GlidePhase::AwaitLift { .. } => {
                if count == 0 {
                    // the flick's last finger is off: coast freely
                    if let Some(g) = self.glide.as_mut() {
                        g.phase = GlidePhase::Gliding;
                    }
                    self.end_touch_bookkeeping();
                } else if count > self.prev_count {
                    // a finger landed: stop, and take what's down as new
                    self.stop_glide(now);
                    self.restart_touch(now);
                    if count >= 2 {
                        self.end_glide(out);
                    }
                }
                // else: the flick's trailing fingers lifting; ignore
                true
            }
            GlidePhase::Gliding => {
                if count == 0 {
                    return true;
                }
                // this frame starts the new touch (buffered as usual)
                if count >= 2 {
                    self.end_glide(out);
                } else {
                    self.stop_glide(now); // touch-to-stop
                }
                false
            }
            GlidePhase::Holding { .. } => {
                if count >= 2 {
                    self.end_glide(out);
                }
                false
            }
        }
    }

    fn glide_tick(&mut self, now: Instant, out: &mut Vec<Output>) {
        if matches!(self.glide, Some(g) if g.wheel.is_some()) {
            self.wheel_glide_tick(now, out);
            return;
        }
        let Some(g) = self.glide else { return };
        let Some(m) = self.timing.momentum else {
            // momentum switched off mid-glide: off means off, and the glide
            // fingers mustn't stay on the clone
            if matches!(g.phase, GlidePhase::AwaitLift { .. }) {
                self.stop_glide(now);
                self.restart_touch(now);
            } else {
                self.end_glide(out);
            }
            return;
        };
        match g.phase {
            GlidePhase::Holding { until } => {
                if now >= until {
                    self.end_glide(out);
                }
                return;
            }
            GlidePhase::AwaitLift { deadline } if now >= deadline => {
                // a finger stayed down: not a flick after all
                self.stop_glide(now);
                self.restart_touch(now);
                return;
            }
            _ => {}
        }
        if now <= g.last {
            return;
        }
        let dt = now.duration_since(g.last).as_secs_f64() * 1000.0;
        // move at the current speed, then decay it: alpha is per 8 ms
        let step = (g.v.0 * dt, g.v.1 * dt);
        let k = m.alpha(self.speed_mm(g.v)).powf(dt / 8.0);
        let mut g = g;
        g.v = (g.v.0 * k, g.v.1 * k);
        g.last = now;
        let mut frame = Vec::with_capacity(7);
        for i in 0..2 {
            g.pos[i].0 += step.0;
            g.pos[i].1 += step.1;
            frame.push(Ev::abs(ABS_MT_SLOT, g.slots[i] as i32));
            frame.push(Ev::abs(ABS_MT_POSITION_X, g.pos[i].0.round() as i32));
            frame.push(Ev::abs(ABS_MT_POSITION_Y, g.pos[i].1.round() as i32));
        }
        frame.push(Ev::syn());
        out.push(Output::EmitSynth(frame));
        self.glide = Some(g);

        let spent = now.duration_since(g.started) >= GLIDE_MAX;
        // per axis, like WebKit's `|dx| > 0.5 || |dy| > 0.5` loop
        let slow = (g.v.0 / self.x_res).abs() <= m.stop_speed
            && (g.v.1 / self.y_res).abs() <= m.stop_speed;
        if g.phase == GlidePhase::Gliding && (slow || spent) {
            // slow enough that no app's own fling would pick it up
            self.end_glide(out);
        }
    }

    /// Lift the glide fingers off the clone, then release whatever real
    /// touch was buffered behind them.
    fn end_glide(&mut self, out: &mut Vec<Output>) {
        let Some(g) = self.glide.take() else { return };
        if g.wheel.is_some() {
            // nothing of a wheel glide is on the clone, nothing held back
            return;
        }
        let mut release = Vec::new();
        for &s in &g.slots {
            if self.relayed_active[s] {
                release.push(Ev::abs(ABS_MT_SLOT, s as i32));
                release.push(Ev::abs(ABS_MT_TRACKING_ID, -1));
                self.relayed_active[s] = false;
            }
        }
        for i in 0..self.clone_keys.len() {
            if self.clone_keys[i].1 != 0 {
                release.push(Ev::new(EV_KEY, self.clone_keys[i].0, 0));
                self.clone_keys[i].1 = 0;
            }
        }
        release.push(Ev::syn());
        out.push(Output::EmitSynth(release));
        self.needs_sync = true;

        // a touch that ended (tap) or was classified while held back
        if self.touch_start.is_none() {
            if !self.pending.is_empty() {
                self.flush_pending(out);
            }
        } else if self.settled {
            self.flush_pending(out);
        }
    }

    // ---- momentum: wheel output ------------------------------------------

    /// A real frame during a wheel-mode glide; it is relayed as usual
    /// after this. Any new contact stops the glide at once: there are no
    /// glide fingers to hold, and the touch must reach the app unchanged.
    fn wheel_glide_sees_frame(&mut self) {
        let count = self.active_count();
        let Some(g) = self.glide.as_mut() else { return };
        let stop = match g.phase {
            GlidePhase::AwaitLift { .. } => {
                if count == 0 {
                    // the flick's last finger is off (its lift is relayed)
                    g.phase = GlidePhase::Gliding;
                    false
                } else {
                    // a finger landed (trailing lifts just carry on)
                    count > self.prev_count
                }
            }
            GlidePhase::Gliding => count > 0,
            GlidePhase::Holding { .. } => true, // not used in wheel mode
        };
        if stop {
            debug!("touch during a wheel glide: stopped");
            self.glide = None;
        }
    }

    /// One wheel-mode glide frame: the fingers-mode decay, with the
    /// finger travel turned into wheel units ([`Momentum::wheel_units`])
    /// instead of moving fingers. Sub-unit and sub-notch remainders carry
    /// to the next frame.
    fn wheel_glide_tick(&mut self, now: Instant, out: &mut Vec<Output>) {
        let Some(mut g) = self.glide else { return };
        let Some(mut w) = g.wheel else { return };
        if self.timing.momentum.is_none() {
            // momentum switched off mid-glide: off means off
            self.glide = None;
            return;
        }
        match g.phase {
            GlidePhase::AwaitLift { deadline } if now >= deadline => {
                // a finger stayed down: not a flick after all. It is
                // already live on the clone, so nothing to hand over.
                self.glide = None;
                return;
            }
            GlidePhase::Holding { .. } => {
                self.glide = None;
                return;
            }
            _ => {}
        }
        if now <= g.last {
            return;
        }
        if now.duration_since(g.last) > WHEEL_GLIDE_STALL {
            // the loop stalled (suspend?): one frame for the whole gap
            // would be a huge jump of whole notches, so just stop
            self.glide = None;
            return;
        }
        let dt = now.duration_since(g.last).as_secs_f64() * 1000.0;
        // move at the current speed, then decay it: alpha is per 8 ms
        let step = (g.v.0 * dt, g.v.1 * dt);
        let k = w.m.alpha(self.speed_mm(g.v)).powf(dt / 8.0);
        g.v = (g.v.0 * k, g.v.1 * k);
        g.last = now;

        let (uh, uv) = w.m.wheel_units(step.0 / self.x_res, step.1 / self.y_res);
        w.carry = (w.carry.0 + uh, w.carry.1 + uv);
        let (h120, v120) = (w.carry.0.trunc() as i32, w.carry.1.trunc() as i32);
        w.carry = (w.carry.0 - h120 as f64, w.carry.1 - v120 as f64);
        w.notch = (w.notch.0 + h120, w.notch.1 + v120);
        // integer division truncates toward zero, so the remainder keeps
        // its sign and is always under one notch
        let (h, v) = (w.notch.0 / 120, w.notch.1 / 120);
        w.notch = (w.notch.0 - h * 120, w.notch.1 - v * 120);
        if h120 != 0 || v120 != 0 {
            out.push(Output::Wheel { v120, h120, v, h });
        }
        g.wheel = Some(w);
        self.glide = Some(g);

        let spent = now.duration_since(g.started) >= GLIDE_MAX;
        let slow = (g.v.0 / self.x_res).abs() <= w.m.stop_speed
            && (g.v.1 / self.y_res).abs() <= w.m.stop_speed;
        if g.phase == GlidePhase::Gliding && (slow || spent) {
            self.glide = None;
        }
    }

    /// SLOT/TRACKING_ID/X/Y events asserting the current state of every
    /// active slot (used to correct downstream state after a resync).
    fn active_slot_dump(&self) -> Vec<Ev> {
        let mut dump = Vec::new();
        for slot in 0..self.slot_count {
            let s = self.slots[slot];
            if s.tracking_id >= 0 {
                dump.push(Ev::abs(ABS_MT_SLOT, slot as i32));
                dump.push(Ev::abs(ABS_MT_TRACKING_ID, s.tracking_id));
                dump.push(Ev::abs(ABS_MT_POSITION_X, s.x));
                dump.push(Ev::abs(ABS_MT_POSITION_Y, s.y));
            }
        }
        dump
    }
}

#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod tests;
