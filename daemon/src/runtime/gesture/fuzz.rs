//! Randomized invariant testing: thousands of chaotic-but-realistic
//! multi-finger sequences thrown at the state machine, with a shadow
//! model of the synthetic clone tracking every event the machine emits.
//! After every single step we assert the invariants that, if ever
//! violated, produce exactly the class of bug this project has been
//! bitten by before: a clone left holding phantom touches or stuck tool
//! bits, a virtual button that never releases, or a panic.
//!
//! The PRNG is a fixed-seed LCG, so every failure is perfectly
//! reproducible: re-run the test, get the same sequence.

use super::*;
use std::time::Duration;

/// Deterministic PRNG (Knuth MMIX LCG).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const BTN_TOUCH: u16 = 0x14a;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
const BTN_TOOL_QUADTAP: u16 = 0x14f;
const BTN_TOOL_QUINTTAP: u16 = 0x148;

const TOOL_BITS: [u16; 5] = [
    BTN_TOOL_FINGER,
    BTN_TOOL_DOUBLETAP,
    BTN_TOOL_TRIPLETAP,
    BTN_TOOL_QUADTAP,
    BTN_TOOL_QUINTTAP,
];

/// Shadow model of the synthetic clone: replays everything the machine
/// emits, exactly as the kernel/libinput would accumulate it.
#[derive(Default, Clone)]
struct ShadowClone {
    active: [bool; MAX_SLOTS],
    keys: Vec<(u16, i32)>,
    current_slot: usize,
}

impl ShadowClone {
    fn feed(&mut self, evs: &[Ev]) {
        for ev in evs {
            match (ev.type_, ev.code) {
                (EV_ABS, ABS_MT_SLOT) => {
                    self.current_slot = (ev.value.max(0) as usize).min(MAX_SLOTS - 1);
                }
                (EV_ABS, ABS_MT_TRACKING_ID) => {
                    self.active[self.current_slot] = ev.value >= 0;
                }
                (EV_KEY, code) => {
                    if let Some(e) = self.keys.iter_mut().find(|e| e.0 == code) {
                        e.1 = ev.value;
                    } else {
                        self.keys.push((code, ev.value));
                    }
                }
                _ => {}
            }
        }
    }
    fn active_count(&self) -> usize {
        self.active.iter().filter(|a| **a).count()
    }
    fn pressed_keys(&self) -> Vec<u16> {
        self.keys.iter().filter(|e| e.1 != 0).map(|e| e.0).collect()
    }
}

/// One simulated "hand": up to 5 fingers occupying random slots, with
/// realistic tool-bit reporting (BTN_TOUCH + BTN_TOOL_<count>).
struct Hand {
    fingers: [Option<(i32, i32, i32)>; MAX_SLOTS], // (id, x, y) per slot
    next_id: i32,
    last_tool: Option<u16>,
    touching: bool,
    /// Per-touch hand motion every wiggle adds, so fingers move together
    /// the way a scroll or flick does (and gesture momentum gets fuzzed).
    drift: (i32, i32),
}

impl Hand {
    fn new() -> Self {
        Hand {
            fingers: [None; MAX_SLOTS],
            next_id: 1,
            last_tool: None,
            touching: false,
            drift: (0, 0),
        }
    }

    fn count(&self) -> usize {
        self.fingers.iter().filter(|f| f.is_some()).count()
    }

    /// Events updating BTN_TOUCH / BTN_TOOL_* to match the new count,
    /// the way real touchpad firmware reports it.
    fn tool_events(&mut self) -> Vec<Ev> {
        let mut evs = Vec::new();
        let count = self.count();
        let tool = if count == 0 {
            None
        } else {
            Some(TOOL_BITS[(count - 1).min(4)])
        };
        if tool != self.last_tool {
            if let Some(old) = self.last_tool {
                evs.push(Ev::new(EV_KEY, old, 0));
            }
            if let Some(new) = tool {
                evs.push(Ev::new(EV_KEY, new, 1));
            }
            self.last_tool = tool;
        }
        let touching = count > 0;
        if touching != self.touching {
            evs.push(Ev::new(EV_KEY, BTN_TOUCH, i32::from(touching)));
            self.touching = touching;
        }
        evs
    }

    fn land(&mut self, rng: &mut Lcg) -> Vec<Ev> {
        let free: Vec<usize> = (0..MAX_SLOTS)
            .filter(|&s| self.fingers[s].is_none())
            .collect();
        if free.is_empty() || self.count() >= 5 {
            return Vec::new();
        }
        let slot = free[rng.below(free.len() as u64) as usize];
        if self.count() == 0 {
            self.drift = (rng.below(51) as i32 - 25, rng.below(51) as i32 - 25);
        }
        let id = self.next_id;
        self.next_id += 1;
        let x = rng.below(2000) as i32;
        let y = rng.below(1400) as i32;
        self.fingers[slot] = Some((id, x, y));
        let mut evs = vec![
            Ev::abs(ABS_MT_SLOT, slot as i32),
            Ev::abs(ABS_MT_TRACKING_ID, id),
            Ev::abs(ABS_MT_POSITION_X, x),
            Ev::abs(ABS_MT_POSITION_Y, y),
        ];
        evs.extend(self.tool_events());
        evs
    }

    fn lift(&mut self, rng: &mut Lcg) -> Vec<Ev> {
        let used: Vec<usize> = (0..MAX_SLOTS)
            .filter(|&s| self.fingers[s].is_some())
            .collect();
        if used.is_empty() {
            return Vec::new();
        }
        let slot = used[rng.below(used.len() as u64) as usize];
        self.fingers[slot] = None;
        let mut evs = vec![
            Ev::abs(ABS_MT_SLOT, slot as i32),
            Ev::abs(ABS_MT_TRACKING_ID, -1),
        ];
        evs.extend(self.tool_events());
        evs
    }

    fn wiggle(&mut self, rng: &mut Lcg) -> Vec<Ev> {
        let mut evs = Vec::new();
        for slot in 0..MAX_SLOTS {
            if let Some((_, x, y)) = self.fingers[slot].as_mut() {
                if rng.below(2) == 0 {
                    *x = (*x + self.drift.0 + rng.below(41) as i32 - 20).clamp(0, 2000);
                    *y = (*y + self.drift.1 + rng.below(41) as i32 - 20).clamp(0, 1400);
                    evs.push(Ev::abs(ABS_MT_SLOT, slot as i32));
                    evs.push(Ev::abs(ABS_MT_POSITION_X, *x));
                    evs.push(Ev::abs(ABS_MT_POSITION_Y, *y));
                }
            }
        }
        evs
    }
}

/// Returns how many steps a glide was running in.
fn run_scenario(seed: u64, drag_end_delay_ms: u64, momentum: bool) -> u32 {
    run_scenario_with(seed, drag_end_delay_ms, momentum.then_some(Momentum::mac()), false)
}

/// The same chaos with any momentum settings. With `reloads`, the config
/// is also hot-reloaded at random moments (glide output, gain, invert,
/// momentum on/off), the way a user editing daemon.json mid-glide would.
fn run_scenario_with(
    seed: u64,
    drag_end_delay_ms: u64,
    momentum: Option<Momentum>,
    reloads: bool,
) -> u32 {
    let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let timing = Timing {
        probe_delay: Duration::from_millis(15),
        entry_debounce: Duration::from_millis(50),
        drag_end_delay: Duration::from_millis(drag_end_delay_ms),
        press_grace: Duration::from_millis(75),
        px_per_mm: PX_PER_MM,
        three_finger_drag: true,
        momentum,
    };
    let mut m = GestureMachine::new(timing, 10.0, 10.0, 16);
    let mut shadow = ShadowClone::default();
    let mut hand = Hand::new();
    let mut now = Instant::now();
    let mut expect_held = false;
    let mut glide_steps = 0u32;
    // wheel output may only appear when some configuration asked for it
    let wheel_allowed = reloads || momentum.is_some_and(|m| m.output == MomentumOutput::Wheel);

    let apply = |outs: &[Output], shadow: &mut ShadowClone, expect_held: &mut bool, ctx: &str| {
        for o in outs {
            match o {
                Output::EmitSynth(evs) => shadow.feed(evs),
                Output::MouseDown => {
                    assert!(!*expect_held, "double MouseDown ({ctx}, seed {seed})");
                    *expect_held = true;
                }
                Output::MouseUp => {
                    assert!(*expect_held, "MouseUp while not held ({ctx}, seed {seed})");
                    *expect_held = false;
                }
                Output::MouseMove { .. } => {}
                Output::Wheel { v120, h120, v, h } => {
                    check_wheel(wheel_allowed, *v120, *h120, *v, *h, &format!("{ctx}, seed {seed}"))
                }
            }
        }
    };

    for step in 0..2500u32 {
        // a hot reload, at any moment (mid-glide included)
        if reloads && rng.below(40) == 0 {
            m.set_timing(Timing {
                momentum: random_momentum(&mut rng, &m),
                ..timing
            });
        }
        let count_before = hand.count();

        // random action: land / lift / wiggle / idle
        let evs = match rng.below(10) {
            0..=2 => hand.land(&mut rng),
            3..=5 => hand.lift(&mut rng),
            6..=8 => hand.wiggle(&mut rng),
            _ => Vec::new(),
        };

        // advance time by a random small amount, delivering ticks the
        // way the event loop would (deadline-driven)
        let dt = 1 + rng.below(60);
        for _ in 0..2 {
            // fire any deadline that falls inside this window, like the
            // real select! loop does
            if let Some(deadline) = m.next_deadline() {
                if deadline <= now + Duration::from_millis(dt) {
                    let outs = m.on_tick(deadline);
                    apply(&outs, &mut shadow, &mut expect_held, "tick");
                }
            }
        }
        now += Duration::from_millis(dt);
        let outs = m.on_tick(now);
        apply(&outs, &mut shadow, &mut expect_held, "tick2");

        if !evs.is_empty() || rng.below(4) == 0 {
            let mut frame = evs;
            frame.push(Ev::syn());
            let outs = m.on_frame(&frame, now);
            apply(&outs, &mut shadow, &mut expect_held, "frame");
            // a finger landing stops a wheel glide at once
            if hand.count() > count_before {
                assert!(
                    !wheel_gliding(&m),
                    "wheel glide survived a touch (seed {seed}, step {step})"
                );
            }
        }

        // occasionally simulate a SYN_DROPPED resync with the hand's
        // true state (what EVIOCGMTSLOTS would report)
        if rng.below(97) == 0 {
            let snapshot: Vec<(i32, i32, i32)> = (0..16)
                .map(|s| match hand.fingers[s] {
                    Some((id, x, y)) => (id, x, y),
                    None => (-1, 0, 0),
                })
                .collect();
            let outs = m.on_resync(&snapshot, now);
            apply(&outs, &mut shadow, &mut expect_held, "resync");
            // a resync may swallow tool state legitimately; re-assert
            // the hand's tool bits on the next frame like hardware would
            let mut frame = hand.wiggle(&mut rng);
            frame.push(Ev::syn());
            let outs = m.on_frame(&frame, now);
            apply(&outs, &mut shadow, &mut expect_held, "post-resync");
        }

        if m.gliding() {
            glide_steps += 1;
        }

        // ---- INVARIANTS, every step --------------------------------
        // 1. the machine's belief about the clone matches the clone
        for slot in 0..MAX_SLOTS {
            assert_eq!(
                m.relayed_active[slot], shadow.active[slot],
                "clone slot-state desync at slot {slot} (seed {seed}, step {step})"
            );
        }
        // 2. machine's button belief matches emitted Down/Up pairing
        assert_eq!(
            m.held, expect_held,
            "button-state desync (seed {seed}, step {step})"
        );
        // 3. the clone never has MORE active slots than the real pad
        //    (it may briefly have fewer: buffered/suppressed touches),
        //    except for the two synthetic fingers of a glide
        assert!(
            shadow.active_count() <= hand.count()
                || (m.gliding() && shadow.active_count() <= 2),
            "clone shows {} touches, real pad has {} (seed {seed}, step {step})",
            shadow.active_count(),
            hand.count()
        );
        // 4. a wheel glide puts nothing on the clone: no exception there
        if wheel_gliding(&m) {
            assert!(
                shadow.active_count() <= hand.count(),
                "wheel glide left {} touches on the clone, real pad has {} (seed {seed}, step {step})",
                shadow.active_count(),
                hand.count()
            );
        }
    }

    // ---- quiescence: lift everything, let every deadline expire ----
    loop {
        let evs = hand.lift(&mut rng);
        if evs.is_empty() {
            break;
        }
        let mut frame = evs;
        frame.push(Ev::syn());
        now += Duration::from_millis(9);
        let outs = m.on_frame(&frame, now);
        apply(&outs, &mut shadow, &mut expect_held, "drain-lift");
    }
    // empty frame so a resync-less machine sees the final state
    let outs = m.on_frame(&[Ev::syn()], now);
    apply(&outs, &mut shadow, &mut expect_held, "drain-frame");
    for _ in 0..8 {
        now += Duration::from_millis(500);
        let outs = m.on_tick(now);
        apply(&outs, &mut shadow, &mut expect_held, "drain-tick");
    }

    assert!(
        !m.held,
        "virtual button left stuck down at quiescence (seed {seed})"
    );
    assert_eq!(
        shadow.active_count(),
        0,
        "clone left holding phantom touches at quiescence (seed {seed})"
    );
    assert!(
        shadow.pressed_keys().is_empty(),
        "clone left with stuck tool bits {:?} at quiescence (seed {seed})",
        shadow.pressed_keys()
    );
    assert_eq!(
        m.next_deadline(),
        None,
        "machine still scheduling wakeups at quiescence (seed {seed})"
    );
    glide_steps
}

/// 192 scenarios x 2500 steps: drag-lock off/on x momentum off/on. Fully
/// deterministic; a failing seed reproduces exactly.
#[test]
fn randomized_stress_invariants() {
    let mut glide_steps = 0u32;
    for seed in 0..48u64 {
        run_scenario(seed, 0, false);
        run_scenario(seed, 300, false);
        glide_steps += run_scenario(seed, 0, true);
        glide_steps += run_scenario(seed, 300, true);
    }
    assert!(glide_steps > 0, "momentum runs never glided");
}

fn wheel_mac() -> Momentum {
    Momentum {
        output: MomentumOutput::Wheel,
        ..Momentum::mac()
    }
}

fn wheel_gliding(m: &GestureMachine) -> bool {
    m.glide.is_some_and(|g| g.wheel.is_some())
}

/// Every wheel output: only when some config asked for wheel mode, never
/// empty, and the whole notches never run ahead of (or against) the
/// hi-res units they come from.
fn check_wheel(allowed: bool, v120: i32, h120: i32, v: i32, h: i32, ctx: &str) {
    assert!(allowed, "wheel output in fingers mode ({ctx})");
    assert!(v120 != 0 || h120 != 0, "empty wheel output ({ctx})");
    for (hi, notch) in [(v120, v), (h120, h)] {
        assert!(notch == 0 || notch.signum() == hi.signum(), "notch against hi-res ({ctx})");
        assert!(notch.abs() <= hi.abs() / 120 + 1, "notch ahead of hi-res ({ctx})");
    }
}

/// A random reload target. Momentum is only switched off while no
/// fingers-mode glide is running: a fingers glide reads the live config
/// every frame and has no way to end with momentum gone (pre-existing,
/// see the wheel-mode report), which wheel mode doesn't share.
fn random_momentum(rng: &mut Lcg, m: &GestureMachine) -> Option<Momentum> {
    let fingers_glide = m.glide.is_some_and(|g| g.wheel.is_none());
    if !fingers_glide && rng.below(4) == 0 {
        return None;
    }
    Some(Momentum {
        output: if rng.below(2) == 0 {
            MomentumOutput::Fingers
        } else {
            MomentumOutput::Wheel
        },
        wheel_gain: [6.0, 24.0, 100.0][rng.below(3) as usize],
        wheel_invert: rng.below(2) == 0,
        ..Momentum::mac()
    })
}

/// The chaos fuzz again with wheel-mode glides, then with hot reloads
/// flipping between modes (and momentum off) at random moments.
#[test]
fn randomized_stress_invariants_wheel() {
    let mut glide_steps = 0u32;
    for seed in 0..48u64 {
        glide_steps += run_scenario_with(seed, 0, Some(wheel_mac()), false);
        glide_steps += run_scenario_with(seed, 300, Some(wheel_mac()), false);
        run_scenario_with(seed, 0, Some(wheel_mac()), true);
        run_scenario_with(seed, 300, Some(Momentum::mac()), true);
    }
    assert!(glide_steps > 0, "wheel runs never glided");
}

/// Flick-heavy fuzz for momentum: 2-finger touches that move together
/// and lift (staggered, sometimes leaving a finger down), followed by a
/// random mix of idling, touch-to-stop, taps, 1-3 finger touches and
/// resyncs -- with the same clone/button invariants checked every step.
fn run_flicks(seed: u64, drag_end_delay_ms: u64) -> u32 {
    run_flicks_with(seed, drag_end_delay_ms, MomentumOutput::Fingers)
}

fn run_flicks_with(seed: u64, drag_end_delay_ms: u64, output: MomentumOutput) -> u32 {
    let wheel = output == MomentumOutput::Wheel;
    let mut rng = Lcg(seed.wrapping_mul(0xD1B54A32D192ED03) | 1);
    let mut m = GestureMachine::new(
        Timing {
            probe_delay: Duration::from_millis(15),
            entry_debounce: Duration::from_millis(50),
            drag_end_delay: Duration::from_millis(drag_end_delay_ms),
            press_grace: Duration::from_millis(75),
            px_per_mm: PX_PER_MM,
            three_finger_drag: true,
            momentum: Some(Momentum {
                fast_decay: 0.9 + 0.095 * rng.below(100) as f64 / 100.0,
                tail_decay: 0.8 + 0.19 * rng.below(100) as f64 / 100.0,
                tail_speed: 0.06 * rng.below(100) as f64 / 100.0,
                output,
                ..Momentum::mac()
            }),
        },
        10.0,
        10.0,
        16,
    );
    let mut shadow = ShadowClone::default();
    let mut hand = Hand::new();
    let mut now = Instant::now();
    let mut held = false;
    let mut glides = 0u32;

    // advance the clock to `until`, firing every deadline on the way
    // exactly like the select! loop does
    fn advance(
        m: &mut GestureMachine,
        now: &mut Instant,
        until: Instant,
        sink: &mut dyn FnMut(&[Output]),
    ) {
        while let Some(d) = m.next_deadline() {
            if d > until {
                break;
            }
            *now = d.max(*now);
            let outs = m.on_tick(*now);
            sink(&outs);
        }
        *now = until.max(*now);
    }

    for round in 0..300u32 {
        let mut log: Vec<Output> = Vec::new();
        let step = |m: &mut GestureMachine,
                        now: &mut Instant,
                        hand: &Hand,
                        ms: u64,
                        evs: Vec<Ev>,
                        log: &mut Vec<Output>| {
            let until = *now + Duration::from_millis(ms);
            advance(m, now, until, &mut |o| log.extend_from_slice(o));
            if !evs.is_empty() {
                let mut f = evs;
                f.push(Ev::syn());
                log.extend(m.on_frame(&f, *now));
            }
            let _ = hand;
        };

        // land two fingers, maybe staggered
        let first = hand.land(&mut rng);
        step(&mut m, &mut now, &hand, 1 + rng.below(20), first, &mut log);
        let second = hand.land(&mut rng);
        step(&mut m, &mut now, &hand, rng.below(25), second, &mut log);
        // move together for a while at a random speed
        let v = (rng.below(81) as i32 - 40, rng.below(81) as i32 - 40);
        hand.drift = v;
        for _ in 0..(3 + rng.below(15)) {
            let mut evs = Vec::new();
            for slot in 0..MAX_SLOTS {
                if let Some((_, x, y)) = hand.fingers[slot].as_mut() {
                    *x = (*x + v.0).clamp(0, 2000);
                    *y = (*y + v.1).clamp(0, 1400);
                    evs.push(Ev::abs(ABS_MT_SLOT, slot as i32));
                    evs.push(Ev::abs(ABS_MT_POSITION_X, *x));
                    evs.push(Ev::abs(ABS_MT_POSITION_Y, *y));
                }
            }
            step(&mut m, &mut now, &hand, 8 + rng.below(6), evs, &mut log);
        }
        // lift: staggered, or one finger stays down
        let lift1 = hand.lift(&mut rng);
        step(&mut m, &mut now, &hand, 8, lift1, &mut log);
        if rng.below(4) != 0 {
            let lift2 = hand.lift(&mut rng);
            step(&mut m, &mut now, &hand, rng.below(90), lift2, &mut log);
        }
        if m.gliding() {
            glides += 1;
        }
        if wheel && hand.count() == 0 {
            // wheel mode: the lift reached the clone at once, glide or not
            let mut s = shadow.clone();
            for o in &log {
                if let Output::EmitSynth(evs) = o {
                    s.feed(evs);
                }
            }
            assert_eq!(s.active_count(), 0, "lift withheld in wheel mode (seed {seed}, round {round})");
        }
        // then something random
        match rng.below(7) {
            0 | 1 => step(&mut m, &mut now, &hand, rng.below(400), Vec::new(), &mut log),
            2 | 3 => {
                // touch(es) during the glide, held a bit, then lifted
                for _ in 0..(1 + rng.below(3)) {
                    let evs = hand.land(&mut rng);
                    step(&mut m, &mut now, &hand, rng.below(120), evs, &mut log);
                    assert!(!wheel_gliding(&m), "wheel glide survived a touch (seed {seed}, round {round})");
                }
                let evs = hand.wiggle(&mut rng);
                step(&mut m, &mut now, &hand, rng.below(200), evs, &mut log);
                while hand.count() > 0 {
                    let evs = hand.lift(&mut rng);
                    step(&mut m, &mut now, &hand, rng.below(40), evs, &mut log);
                }
            }
            4 => {
                // quick tap
                let evs = hand.land(&mut rng);
                step(&mut m, &mut now, &hand, rng.below(200), evs, &mut log);
                assert!(!wheel_gliding(&m), "wheel glide survived a tap (seed {seed}, round {round})");
                let evs = hand.lift(&mut rng);
                step(&mut m, &mut now, &hand, 20 + rng.below(80), evs, &mut log);
            }
            5 => {
                let snapshot: Vec<(i32, i32, i32)> = (0..16)
                    .map(|s| match hand.fingers[s] {
                        Some((id, x, y)) => (id, x, y),
                        None => (-1, 0, 0),
                    })
                    .collect();
                step(&mut m, &mut now, &hand, rng.below(100), Vec::new(), &mut log);
                log.extend(m.on_resync(&snapshot, now));
            }
            _ => {}
        }
        // everything up, then long enough for any glide to finish
        while hand.count() > 0 {
            let evs = hand.lift(&mut rng);
            step(&mut m, &mut now, &hand, 1 + rng.below(30), evs, &mut log);
        }
        if rng.below(3) == 0 {
            step(&mut m, &mut now, &hand, 3500, Vec::new(), &mut log);
        }

        for o in &log {
            match o {
                Output::EmitSynth(evs) => shadow.feed(evs),
                Output::MouseDown => {
                    assert!(!held, "double MouseDown (seed {seed}, round {round})");
                    held = true;
                }
                Output::MouseUp => {
                    assert!(held, "MouseUp while not held (seed {seed}, round {round})");
                    held = false;
                }
                Output::MouseMove { .. } => {}
                Output::Wheel { v120, h120, v, h } => check_wheel(
                    wheel,
                    *v120,
                    *h120,
                    *v,
                    *h,
                    &format!("seed {seed}, round {round}"),
                ),
            }
        }
        for slot in 0..MAX_SLOTS {
            assert_eq!(
                m.relayed_active[slot], shadow.active[slot],
                "clone slot-state desync at slot {slot} (seed {seed}, round {round})"
            );
        }
        assert_eq!(m.held, held, "button-state desync (seed {seed}, round {round})");
        assert!(
            shadow.active_count() <= hand.count() || (m.gliding() && shadow.active_count() <= 2),
            "clone shows {} touches, real pad has {} (seed {seed}, round {round})",
            shadow.active_count(),
            hand.count()
        );
        if !m.gliding() || wheel {
            // between touches the clone holds nothing and no stuck bits
            assert_eq!(shadow.active_count(), 0, "phantom touch (seed {seed}, round {round})");
        }
    }

    // quiescence
    let mut log = Vec::new();
    let until = now + Duration::from_secs(5);
    advance(&mut m, &mut now, until, &mut |o| log.extend_from_slice(o));
    for o in &log {
        match o {
            Output::EmitSynth(evs) => shadow.feed(evs),
            Output::MouseUp => held = false,
            Output::MouseDown => held = true,
            _ => {}
        }
    }
    assert!(!m.gliding(), "glide never ended (seed {seed})");
    assert!(!held && !m.held, "button stuck (seed {seed})");
    assert_eq!(shadow.active_count(), 0, "phantom touch at quiescence (seed {seed})");
    assert!(
        shadow.pressed_keys().is_empty(),
        "stuck tool bits {:?} at quiescence (seed {seed})",
        shadow.pressed_keys()
    );
    assert_eq!(m.next_deadline(), None, "wakeups at quiescence (seed {seed})");
    glides
}

#[test]
fn randomized_flick_invariants() {
    let mut glides = 0;
    for seed in 0..40u64 {
        glides += run_flicks(seed, 0);
        glides += run_flicks(seed, 300);
    }
    eprintln!("flick fuzz: {glides} glides");
    assert!(glides > 3000, "flick fuzz only produced {glides} glides");
}

/// The flick fuzz with wheel-mode glides: the lift always reaches the
/// clone, any touch stops the glide at once, and every glide ends.
#[test]
fn randomized_flick_invariants_wheel() {
    let mut glides = 0;
    for seed in 0..40u64 {
        glides += run_flicks_with(seed, 0, MomentumOutput::Wheel);
        glides += run_flicks_with(seed, 300, MomentumOutput::Wheel);
    }
    eprintln!("wheel flick fuzz: {glides} glides");
    assert!(glides > 3000, "wheel flick fuzz only produced {glides} glides");
}
