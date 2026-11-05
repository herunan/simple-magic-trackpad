//! The regression suite: every failure mode this project has hit live is
//! encoded here as a deterministic test. If you change the state machine
//! and one of these fails, you are about to re-ship a bug that has
//! already burned a real user once.

use super::*;

const RES: f64 = 10.0; // units per mm -> 1 unit = 0.1mm = 1.2px at PX_PER_MM=12

fn timing(drag_end_delay_ms: u64) -> Timing {
    Timing {
        probe_delay: Duration::from_millis(15),
        entry_debounce: Duration::from_millis(50),
        drag_end_delay: Duration::from_millis(drag_end_delay_ms),
        press_grace: Duration::from_millis(75),
        px_per_mm: PX_PER_MM,
        three_finger_drag: true,
        momentum: None,
    }
}

/// Deterministic-clock harness around the machine.
struct Sim {
    m: GestureMachine,
    now: Instant,
}

impl Sim {
    fn new() -> Self {
        Self::with_delay(0)
    }
    fn with_delay(drag_end_delay_ms: u64) -> Self {
        Sim {
            m: GestureMachine::new(timing(drag_end_delay_ms), RES, RES, 16),
            now: Instant::now(),
        }
    }
    /// Advance the clock and deliver the tick the I/O loop would.
    fn tick(&mut self, ms: u64) -> Vec<Output> {
        self.now += Duration::from_millis(ms);
        self.m.on_tick(self.now)
    }
    /// Deliver one frame (SYN_REPORT appended automatically).
    fn frame(&mut self, evs: &[Ev]) -> Vec<Output> {
        let mut f = evs.to_vec();
        f.push(Ev::syn());
        self.m.on_frame(&f, self.now)
    }
    /// Advance the clock, then deliver a frame.
    fn frame_at(&mut self, ms: u64, evs: &[Ev]) -> Vec<Output> {
        self.now += Duration::from_millis(ms);
        self.frame(evs)
    }
}

// -- tiny event builders -------------------------------------------------

fn down(slot: i32, id: i32, x: i32, y: i32) -> Vec<Ev> {
    vec![
        Ev::abs(ABS_MT_SLOT, slot),
        Ev::abs(ABS_MT_TRACKING_ID, id),
        Ev::abs(ABS_MT_POSITION_X, x),
        Ev::abs(ABS_MT_POSITION_Y, y),
    ]
}

fn up(slot: i32) -> Vec<Ev> {
    vec![Ev::abs(ABS_MT_SLOT, slot), Ev::abs(ABS_MT_TRACKING_ID, -1)]
}

fn mv(slot: i32, x: i32, y: i32) -> Vec<Ev> {
    vec![
        Ev::abs(ABS_MT_SLOT, slot),
        Ev::abs(ABS_MT_POSITION_X, x),
        Ev::abs(ABS_MT_POSITION_Y, y),
    ]
}

fn cat(parts: &[&[Ev]]) -> Vec<Ev> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

// -- output inspectors ---------------------------------------------------

fn mouse_downs(outs: &[Output]) -> usize {
    outs.iter()
        .filter(|o| matches!(o, Output::MouseDown))
        .count()
}

fn mouse_ups(outs: &[Output]) -> usize {
    outs.iter().filter(|o| matches!(o, Output::MouseUp)).count()
}

fn synth_events(outs: &[Output]) -> Vec<Ev> {
    outs.iter()
        .filter_map(|o| match o {
            Output::EmitSynth(evs) => Some(evs.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn total_move(outs: &[Output]) -> (i32, i32) {
    outs.iter().fold((0, 0), |acc, o| match o {
        Output::MouseMove { dx, dy } => (acc.0 + dx, acc.1 + dy),
        _ => acc,
    })
}

fn collect(mut acc: Vec<Output>, more: Vec<Output>) -> Vec<Output> {
    acc.extend(more);
    acc
}

/// Drive a full staggered 3-finger touchdown into a committed drag,
/// then let the press grace expire so the button is actually pressed.
/// Returns everything emitted along the way.
fn start_drag(sim: &mut Sim) -> Vec<Output> {
    let mut outs = commit_drag_only(sim);
    outs = collect(outs, sim.tick(80)); // press grace expires -> MouseDown
    outs
}

/// Like start_drag, but stops right after the drag commits -- inside the
/// press-grace window, before any button press.
fn commit_drag_only(sim: &mut Sim) -> Vec<Output> {
    let mut outs = sim.frame(&down(0, 100, 500, 500));
    outs = collect(outs, sim.frame_at(5, &down(1, 101, 600, 500)));
    outs = collect(outs, sim.frame_at(5, &down(2, 102, 700, 500)));
    outs = collect(outs, sim.tick(45)); // debounce window closes: committed
    outs
}

// =========================================================================
// touchdown classification
// =========================================================================

/// Fingers land asynchronously (the normal case). The compositor must
/// see NOTHING -- the historical bug was the transient 1-2 finger states
/// leaking out as phantom taps / right-clicks.
#[test]
fn staggered_3finger_touchdown_becomes_drag_without_leaking() {
    let mut sim = Sim::new();
    let outs = start_drag(&mut sim);
    assert_eq!(mouse_downs(&outs), 1, "drag must press the button once");
    assert!(
        synth_events(&outs).is_empty(),
        "no trace of the 3-finger touch may reach the compositor, got {:?}",
        synth_events(&outs)
    );
}

/// A quick 3-finger tap (ends before the debounce window) is NOT a drag:
/// it must be replayed to the compositor byte-identically so 3-finger
/// tap (middle-click paste) keeps working.
#[test]
fn quick_3finger_tap_replays_verbatim() {
    let mut sim = Sim::new();
    let f1 = cat(&[
        &down(0, 1, 100, 100),
        &down(1, 2, 200, 100),
        &down(2, 3, 300, 100),
    ]);
    let f2 = cat(&[&up(0), &up(1), &up(2)]);

    let mut outs = sim.frame(&f1);
    outs = collect(outs, sim.frame_at(25, &f2)); // all up at 25ms < 50ms

    assert_eq!(mouse_downs(&outs), 0, "a tap must not start a drag");
    let mut expected = f1.clone();
    expected.push(Ev::syn());
    expected.extend(f2);
    expected.push(Ev::syn());
    assert_eq!(
        synth_events(&outs),
        expected,
        "tap must be replayed exactly as it happened"
    );
}

/// A 4-finger swipe whose 4th finger lands a beat late passes through a
/// transient "exactly 3" state. It must NOT be classified as a drag.
#[test]
fn four_finger_touchdown_with_late_4th_is_not_a_drag() {
    let mut sim = Sim::new();
    let mut outs = sim.frame(&down(0, 1, 100, 100));
    outs = collect(outs, sim.frame_at(5, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.frame_at(5, &down(2, 3, 300, 100)));
    outs = collect(outs, sim.frame_at(20, &down(3, 4, 400, 100)));

    assert_eq!(mouse_downs(&outs), 0);
    // reaching 4 settles instantly: the whole touchdown must have flushed
    assert!(
        !synth_events(&outs).is_empty(),
        "4-finger touch must be released to the compositor"
    );

    // and staying longer changes nothing
    let outs = sim.tick(100);
    assert_eq!(mouse_downs(&outs), 0);
}

/// THE 4->3 LIFTOFF BUG: fingers never lift in unison, so the tail of
/// every 4-finger swipe passes through exactly 3 active fingers. That
/// moment must NOT hijack the gesture into a drag (which ended as a
/// phantom click at the end of every 4-finger desktop switch).
#[test]
fn four_finger_liftoff_through_three_is_not_hijacked() {
    let mut sim = Sim::new();
    // full 4-finger touchdown, settled
    let mut outs = sim.frame(&cat(&[
        &down(0, 1, 100, 100),
        &down(1, 2, 200, 100),
        &down(2, 3, 300, 100),
        &down(3, 4, 400, 100),
    ]));
    // swipe...
    outs = collect(
        outs,
        sim.frame_at(30, &cat(&[&mv(0, 150, 100), &mv(1, 250, 100)])),
    );
    // ...and a staggered liftoff: 4 -> 3 -> 2 -> 1 -> 0
    outs = collect(outs, sim.frame_at(10, &up(3)));
    outs = collect(outs, sim.frame_at(8, &up(2)));
    outs = collect(outs, sim.frame_at(8, &up(1)));
    outs = collect(outs, sim.frame_at(8, &up(0)));
    outs = collect(outs, sim.tick(100));

    assert_eq!(
        mouse_downs(&outs),
        0,
        "4-finger liftoff passing through count==3 must never become a drag"
    );
    assert_eq!(mouse_ups(&outs), 0);
    // every liftoff frame must have been relayed (the compositor needs
    // to see the gesture end)
    let release_count = synth_events(&outs)
        .iter()
        .filter(|e| e.code == ABS_MT_TRACKING_ID && e.value == -1)
        .count();
    assert_eq!(
        release_count, 4,
        "all four liftoffs must reach the compositor"
    );
}

/// A lone finger settles after the short probe and is then live --
/// ordinary pointer movement must never feel debounced.
#[test]
fn one_finger_settles_after_probe_then_relays_live() {
    let mut sim = Sim::new();
    let outs = sim.frame(&down(0, 7, 100, 100));
    assert!(
        synth_events(&outs).is_empty(),
        "still inside the probe window"
    );

    let outs = sim.tick(15);
    assert!(
        !synth_events(&outs).is_empty(),
        "probe over: buffered touch flushes"
    );

    // subsequent motion relays immediately, same frame
    let outs = sim.frame_at(5, &mv(0, 110, 100));
    assert_eq!(
        synth_events(&outs).len(),
        4,
        "live motion must relay instantly"
    );
}

/// Two fingers landing staggered (a scroll) must flush after the
/// debounce and never become a drag.
#[test]
fn two_finger_scroll_relays_after_debounce() {
    let mut sim = Sim::new();
    let mut outs = sim.frame(&down(0, 1, 100, 100));
    outs = collect(outs, sim.frame_at(8, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.tick(50));

    assert_eq!(mouse_downs(&outs), 0);
    assert!(
        !synth_events(&outs).is_empty(),
        "scroll touch must flush to compositor"
    );

    // live scrolling from here on
    let outs = sim.frame_at(5, &cat(&[&mv(0, 100, 120), &mv(1, 200, 120)]));
    assert!(!synth_events(&outs).is_empty());
}

/// Growing an already-settled touch to exactly 3 (1 -> settle -> +2)
/// still becomes a drag, and the already-relayed slots are explicitly
/// released on the clone first (never left half-open). The relayed tool
/// state (BTN_TOUCH etc.) must be released too: with tap-to-click on, a
/// slot release without its tool release reads as a tap (phantom
/// click), and stuck BTN_TOOL_* bits desync libinput's finger counting.
#[test]
fn growth_to_three_after_settle_becomes_drag_with_clean_release() {
    const BTN_TOUCH: u16 = 0x14a;
    let mut sim = Sim::new();
    let f = cat(&[&down(0, 1, 100, 100), &[Ev::new(EV_KEY, BTN_TOUCH, 1)][..]]);
    sim.frame(&f);
    sim.tick(15); // settles as 1-finger, flushed live (incl. BTN_TOUCH=1)

    let mut outs = sim.frame_at(10, &cat(&[&down(1, 2, 200, 100), &down(2, 3, 300, 100)]));
    let evs = synth_events(&outs);
    assert!(
        evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)),
        "the relayed slot must be released on the clone before suppressing"
    );
    assert!(
        evs.contains(&Ev::new(EV_KEY, BTN_TOUCH, 0)),
        "relayed tool state must be released along with the slots: {evs:?}"
    );

    // the press is deferred, but motion commits it
    outs = sim.frame_at(10, &mv(0, 150, 100));
    assert_eq!(
        mouse_downs(&outs),
        1,
        "late growth to 3 is a deliberate drag"
    );
}

// =========================================================================
// deferred press & the late-4th-finger bailout
// =========================================================================

/// A fast 4-finger swipe staggers its fingers hard: the 4th can land
/// AFTER the entry window closed on a stable-looking 3-finger touch.
/// The committed drag must abort -- with no click, since nothing had
/// pressed the button yet -- and the touch must be handed to the
/// compositor mid-gesture so the rest of the swipe still registers.
#[test]
fn late_4th_finger_aborts_drag_without_click() {
    const BTN_TOUCH: u16 = 0x14a;
    let mut sim = Sim::new();
    let mut outs = sim.frame(&cat(&[
        &down(0, 1, 100, 100),
        &[Ev::new(EV_KEY, BTN_TOUCH, 1)][..],
    ]));
    outs = collect(outs, sim.frame_at(5, &down(1, 2, 200, 100)));
    outs = collect(outs, sim.frame_at(5, &down(2, 3, 300, 100)));
    outs = collect(outs, sim.tick(45)); // committed as drag at ~55ms
    assert_eq!(mouse_downs(&outs), 0, "press must be deferred at commit");

    // the 4th finger lands 15ms after commit
    let outs = sim.frame_at(15, &down(3, 4, 400, 100));
    assert_eq!(mouse_downs(&outs), 0, "no press may ever have happened");
    assert_eq!(mouse_ups(&outs), 0);
    let evs = synth_events(&outs);
    let ids: Vec<i32> = evs
        .iter()
        .filter(|e| e.code == ABS_MT_TRACKING_ID)
        .map(|e| e.value)
        .collect();
    assert_eq!(
        ids,
        vec![1, 2, 3, 4],
        "all four fingers must be introduced to the clone: {evs:?}"
    );
    assert!(
        evs.contains(&Ev::new(EV_KEY, BTN_TOUCH, 1)),
        "real tool state must accompany the handoff: {evs:?}"
    );

    // and the rest of the swipe relays live
    let outs = sim.frame_at(10, &cat(&[&mv(0, 100, 200), &mv(1, 200, 200)]));
    assert!(
        !synth_events(&outs).is_empty(),
        "post-handoff motion must relay"
    );
    assert_eq!(mouse_downs(&outs), 0);
}

/// If the drag already pressed (motion happened before the 4th finger),
/// the abort must still recover: release the button and hand off.
#[test]
fn late_4th_after_motion_releases_and_hands_off() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let outs = sim.frame_at(10, &mv(0, 520, 500)); // motion presses
    assert_eq!(mouse_downs(&outs), 1);

    let outs = sim.frame_at(10, &down(3, 9, 400, 100));
    assert_eq!(mouse_ups(&outs), 1, "held button must be released on abort");
    let up_idx = outs
        .iter()
        .position(|o| matches!(o, Output::MouseUp))
        .unwrap();
    let synth_idx = outs
        .iter()
        .position(|o| matches!(o, Output::EmitSynth(_)))
        .unwrap();
    assert!(
        up_idx < synth_idx,
        "release before the compositor sees the touch"
    );
}

/// The deferred press must not change what a drag feels like: the press
/// lands in the same output batch as (and before) the first motion.
#[test]
fn deferred_press_lands_before_first_motion() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let outs = sim.frame_at(10, &mv(0, 510, 500));
    let down_idx = outs.iter().position(|o| matches!(o, Output::MouseDown));
    let move_idx = outs
        .iter()
        .position(|o| matches!(o, Output::MouseMove { .. }));
    assert!(down_idx.is_some() && move_idx.is_some());
    assert!(
        down_idx < move_idx,
        "MouseDown must precede the motion it enables"
    );
}

/// A stationary 3-finger hold must still press (after the grace) so
/// press-and-hold semantics survive the deferral...
#[test]
fn stationary_hold_presses_after_grace() {
    let mut sim = Sim::new();
    let outs = commit_drag_only(&mut sim);
    assert_eq!(mouse_downs(&outs), 0);
    let outs = sim.tick(80); // grace (75ms) expires
    assert_eq!(
        mouse_downs(&outs),
        1,
        "stationary drag must press after the grace"
    );
}

/// ...and a stationary 3-finger touch that lifts before the grace still
/// produces its click (press+release at liftoff), preserving the old
/// "3-finger hold = click" behavior.
#[test]
fn stationary_hold_lifting_before_grace_still_clicks() {
    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let outs = sim.frame_at(20, &cat(&[&up(0), &up(1), &up(2)])); // lift inside grace
    assert_eq!(
        mouse_downs(&outs),
        1,
        "the owed click must be pressed at liftoff"
    );
    assert_eq!(mouse_ups(&outs), 1, "and released");
    let d = outs
        .iter()
        .position(|o| matches!(o, Output::MouseDown))
        .unwrap();
    let u = outs
        .iter()
        .position(|o| matches!(o, Output::MouseUp))
        .unwrap();
    assert!(d < u);
}

// =========================================================================
// drag behavior
// =========================================================================

/// Motion of the reference finger drives the virtual mouse, scaled by
/// resolution and px_per_mm, with sub-pixel remainders carried (not
/// discarded) so slow drags don't lose motion.
#[test]
fn drag_motion_scales_and_carries_subpixels() {
    let mut sim = Sim::new();
    start_drag(&mut sim);

    // 10 units = 1mm = 12px exactly
    let outs = sim.frame_at(10, &mv(0, 510, 500));
    assert_eq!(total_move(&outs), (12, 0));

    // five frames of 0.3px each (0.25 units): trunc-only would emit 0
    // forever; the carry must accumulate to exactly 1px total (0.3*5)
    let mut acc = Vec::new();
    for i in 1..=5 {
        // 0.25 units per frame is below integer resolution; simulate by
        // moving 1 unit every 4th frame is NOT the same thing -- so use
        // y axis: 1 unit = 0.1mm = 1.2px... instead go smaller: 0.4px =
        // not representable. Use repeated 1-unit moves: 1.2px -> 1px + 0.2 carry
        acc = collect(acc, sim.frame_at(8, &mv(0, 510 + i, 500)));
    }
    // 5 units = 0.5mm = 6.0px total; trunc-per-frame would give 5px
    let (dx, _) = total_move(&acc);
    assert_eq!(dx, 6, "sub-pixel carry must not lose motion (got {dx})");
}

/// Once dragging, a partial liftoff (3 -> 2 -> 1) continues the drag
/// with the remaining finger, and NOTHING leaks to the compositor --
/// the historical phantom-right-click bug.
#[test]
fn drag_partial_liftoff_continues_and_never_leaks() {
    let mut sim = Sim::new();
    start_drag(&mut sim);

    let mut outs = sim.frame_at(10, &up(1)); // 3 -> 2
    outs = collect(outs, sim.frame_at(10, &mv(0, 520, 500))); // still drags
    let (dx, _) = total_move(&outs);
    assert!(dx > 0, "drag must continue on remaining fingers");
    assert!(
        synth_events(&outs).is_empty(),
        "leaked events during drag liftoff"
    );

    outs = sim.frame_at(10, &up(0)); // 2 -> 1: re-baselines onto slot 2 (700,500)
    outs = collect(outs, sim.frame_at(10, &mv(2, 710, 500))); // +10 units = 12px
    outs = collect(outs, sim.frame_at(10, &mv(2, 720, 500))); // +10 units = 12px
    let (dx, _) = total_move(&outs);
    // 24px total proves a clean handoff: a stale baseline from slot 0's
    // position (500) would have produced a 210-unit = 252px jump instead
    assert_eq!(
        dx, 24,
        "after reference handoff, motion resumes from new baseline"
    );

    outs = sim.frame_at(10, &up(2)); // all up
    assert_eq!(mouse_ups(&outs), 1, "drag ends when the last finger lifts");
    assert!(synth_events(&outs).is_empty());
}

/// After a drag fully ends, the next touch must get fresh debounce
/// treatment (the bookkeeping-reset bug: inheriting settled/touch_max
/// from the drag would skip protection entirely).
#[test]
fn post_drag_touch_state_is_fully_reset() {
    let mut sim = Sim::new();
    start_drag(&mut sim);
    sim.frame_at(50, &cat(&[&up(0), &up(1), &up(2)]));

    // a new 1-finger touch: must be buffered (not instantly relayed)
    let outs = sim.frame_at(20, &down(4, 50, 400, 400));
    assert!(
        synth_events(&outs).is_empty(),
        "next touch after a drag must be re-classified from scratch"
    );
    // and a new 3-finger touch must become a fresh drag
    let mut sim2 = Sim::new();
    start_drag(&mut sim2);
    sim2.frame_at(50, &cat(&[&up(0), &up(1), &up(2)]));
    let outs = start_drag(&mut sim2);
    assert_eq!(
        mouse_downs(&outs),
        1,
        "a second drag must work after the first"
    );
}

/// One finger moving immediately after a drag (delay = 0) must not drag:
/// the button is already up before the motion is relayed.
#[test]
fn no_drag_smearing_after_drag_with_zero_delay() {
    let mut sim = Sim::new();
    start_drag(&mut sim);
    let outs = sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    assert_eq!(
        mouse_ups(&outs),
        1,
        "delay=0 releases immediately on liftoff"
    );

    let mut outs = sim.frame_at(10, &down(0, 60, 100, 100));
    outs = collect(outs, sim.tick(15));
    outs = collect(outs, sim.frame_at(5, &mv(0, 200, 100)));
    assert_eq!(mouse_downs(&outs), 0);
    assert_eq!(mouse_ups(&outs), 0, "button state must not change again");
}

// =========================================================================
// drag-lock (drag_end_delay > 0)
// =========================================================================

/// Lift + re-3-touch inside the window: the SAME hold continues -- no
/// MouseUp, and no duplicate MouseDown.
#[test]
fn drag_lock_resumes_held_drag() {
    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    let outs = sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    assert_eq!(
        mouse_ups(&outs),
        0,
        "inside the lock window the button stays held"
    );

    let outs = start_drag(&mut sim); // ~55ms later, still inside 300ms
    assert_eq!(
        mouse_downs(&outs),
        0,
        "resuming a held drag must not re-press"
    );
    assert_eq!(mouse_ups(&outs), 0);

    // and the resumed drag still moves
    let outs = sim.frame_at(10, &mv(0, 520, 500));
    let (dx, _) = total_move(&outs);
    assert!(dx > 0);
}

/// THE DRAG-LOCK REGRESSION: one finger moving during the lock window
/// dragged whatever was under the cursor. The button must be released
/// BEFORE the finger's buffered motion reaches the compositor.
#[test]
fn drag_lock_releases_before_relaying_other_touches() {
    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));

    // one finger lands and settles inside the lock window
    let mut outs = sim.frame_at(50, &down(0, 70, 100, 100));
    outs = collect(outs, sim.tick(15));

    assert_eq!(mouse_ups(&outs), 1, "lock must break for a non-drag touch");
    // ordering: MouseUp strictly before the flushed touch events
    let up_idx = outs
        .iter()
        .position(|o| matches!(o, Output::MouseUp))
        .unwrap();
    let synth_idx = outs
        .iter()
        .position(|o| matches!(o, Output::EmitSynth(_)))
        .unwrap();
    assert!(
        up_idx < synth_idx,
        "button must be released BEFORE the compositor sees the new touch"
    );
}

/// No new touch: the lock expires on its own and releases exactly once.
#[test]
fn drag_lock_times_out() {
    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));

    let outs = sim.tick(299);
    assert_eq!(mouse_ups(&outs), 0, "not yet");
    let outs = sim.tick(1);
    assert_eq!(mouse_ups(&outs), 1, "lock expires");
    let outs = sim.tick(500);
    assert_eq!(mouse_ups(&outs), 0, "and only once");
}

// =========================================================================
// SYN_DROPPED recovery
// =========================================================================

/// Resync during a live (settled) touch: the clone gets an explicit
/// correction frame asserting the authoritative positions.
#[test]
fn resync_live_touch_emits_correction() {
    let mut sim = Sim::new();
    sim.frame(&down(0, 1, 100, 100));
    sim.tick(15); // settled live

    let mut snapshot = vec![(-1, 0, 0); 16];
    snapshot[0] = (1, 150, 150); // moved during the drop
    let outs = sim.m.on_resync(&snapshot, sim.now);
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_POSITION_X, 150)));
}

/// Resync that reveals a finger lifted during the drop: the clone must
/// get a release for it, or it holds a phantom touch forever.
#[test]
fn resync_releases_slots_that_lifted_during_the_drop() {
    let mut sim = Sim::new();
    sim.frame(&cat(&[&down(0, 1, 100, 100), &down(1, 2, 200, 100)]));
    sim.tick(50); // settled live 2-finger touch, relayed

    let mut snapshot = vec![(-1, 0, 0); 16];
    snapshot[0] = (1, 100, 100); // slot 1 lifted during the drop
    let outs = sim.m.on_resync(&snapshot, sim.now);
    let evs = synth_events(&outs);
    let has_release = evs
        .windows(2)
        .any(|w| w[0] == Ev::abs(ABS_MT_SLOT, 1) && w[1] == Ev::abs(ABS_MT_TRACKING_ID, -1));
    assert!(
        has_release,
        "slot 1 must be explicitly released on the clone: {evs:?}"
    );
}

/// Resync during a drag must re-baseline instead of applying the gap as
/// one giant cursor jump.
#[test]
fn resync_during_drag_rebaselines_without_jump() {
    let mut sim = Sim::new();
    start_drag(&mut sim);
    sim.frame_at(10, &mv(0, 510, 500)); // establish motion

    let mut snapshot = vec![(-1, 0, 0); 16];
    snapshot[0] = (100, 900, 900); // reference finger "teleported" in the drop
    snapshot[1] = (101, 600, 500);
    snapshot[2] = (102, 700, 500);
    let outs = sim.m.on_resync(&snapshot, sim.now);
    assert_eq!(
        total_move(&outs),
        (0, 0),
        "the drop gap must not become a jump"
    );

    // next real motion moves normally from the new (900,900) baseline
    let outs = sim.frame_at(10, &mv(0, 910, 910));
    assert_eq!(total_move(&outs), (12, 12));
}

/// Resync revealing the whole touch ended during the drop while
/// dragging: the drag must end (button released), not hang forever.
#[test]
fn resync_with_all_lifted_ends_drag() {
    let mut sim = Sim::new();
    start_drag(&mut sim);

    let snapshot = vec![(-1, 0, 0); 16];
    let outs = sim.m.on_resync(&snapshot, sim.now);
    assert_eq!(
        mouse_ups(&outs),
        1,
        "drag must end when resync shows no fingers"
    );
}

/// Resync revealing a still-buffered touch fully ended during the drop:
/// the truncated tap is swallowed (never half-relayed), leaving the
/// clone consistent.
#[test]
fn resync_with_all_lifted_during_pending_swallows_cleanly() {
    let mut sim = Sim::new();
    sim.frame(&cat(&[&down(0, 1, 100, 100), &down(1, 2, 200, 100)]));

    let snapshot = vec![(-1, 0, 0); 16];
    let outs = sim.m.on_resync(&snapshot, sim.now);
    assert!(
        synth_events(&outs).is_empty(),
        "a touchdown whose release was dropped must not be flushed"
    );

    // and the machine is fully usable afterwards
    let outs = sim.frame_at(10, &down(0, 9, 100, 100));
    assert!(synth_events(&outs).is_empty()); // buffered, fresh touch
    let outs = sim.tick(15);
    assert!(!synth_events(&outs).is_empty()); // settles normally
}

// =========================================================================
// robustness details
// =========================================================================

/// Out-of-range slot indices must clamp, not panic or corrupt memory.
#[test]
fn out_of_range_slot_clamps() {
    let mut sim = Sim::new();
    sim.frame(&[
        Ev::abs(ABS_MT_SLOT, 99),
        Ev::abs(ABS_MT_TRACKING_ID, 5),
        Ev::abs(ABS_MT_SLOT, -3),
        Ev::abs(ABS_MT_TRACKING_ID, -1),
    ]);
    // no panic = pass; also the machine must still classify sanely
    let outs = sim.frame_at(5, &down(0, 1, 100, 100));
    assert!(synth_events(&outs).is_empty());
}

/// A device with fewer slots than MAX_SLOTS must not grow phantom
/// touches from snapshot entries beyond its real slot range.
#[test]
fn small_slot_count_has_no_phantom_slots() {
    let mut m = GestureMachine::new(timing(0), RES, RES, 5);
    let now = Instant::now();
    // kernel snapshot buffers are MAX_SLOTS long; entries past the
    // device's 5 real slots arrive zeroed (tracking_id 0 looks "active")
    let snapshot = [(0, 0, 0); 16];
    // slots 0-4 zeroed = 5 "active" (a genuinely full pad); the machine
    // must at least never count slots 5-15
    m.on_resync(&snapshot[..5.min(snapshot.len())], now);
    assert_eq!(m.active_count(), 5);
    let m2 = GestureMachine::new(timing(0), RES, RES, 5);
    assert_eq!(m2.slot_count, 5);
}

/// next_deadline steers the event loop: probe deadline for a lone
/// finger, debounce deadline once ambiguous, none while dragging or
/// idle, lock deadline while a lock is pending.
#[test]
fn next_deadline_tracks_state() {
    let mut sim = Sim::new();
    assert_eq!(sim.m.next_deadline(), None, "idle: nothing scheduled");

    sim.frame(&down(0, 1, 100, 100));
    let d = sim.m.next_deadline().expect("probe deadline");
    assert_eq!(d, sim.now + Duration::from_millis(15));

    sim.frame_at(5, &down(1, 2, 200, 100));
    let d = sim.m.next_deadline().expect("debounce deadline");
    assert_eq!(
        d,
        sim.now - Duration::from_millis(5) + Duration::from_millis(50)
    );

    let mut sim = Sim::new();
    commit_drag_only(&mut sim);
    let d = sim
        .m
        .next_deadline()
        .expect("press-grace deadline while unmoved");
    assert_eq!(d, sim.now + Duration::from_millis(75));
    sim.tick(80); // grace fires -> pressed
    assert_eq!(sim.m.next_deadline(), None, "dragging: purely event-driven");

    let mut sim = Sim::with_delay(300);
    start_drag(&mut sim);
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    let d = sim.m.next_deadline().expect("lock deadline");
    assert_eq!(d, sim.now + Duration::from_millis(300));
}

/// Non-touch events inside frames (BTN_TOUCH, BTN_TOOL_*, legacy ABS_X)
/// ride along verbatim: buffered while pending, relayed when live,
/// withheld while dragging.
#[test]
fn non_mt_events_ride_along_with_their_frames() {
    const EV_KEY: u16 = 0x01;
    const BTN_TOUCH: u16 = 0x14a;
    let mut sim = Sim::new();
    let f = cat(&[&down(0, 1, 100, 100), &[Ev::new(EV_KEY, BTN_TOUCH, 1)][..]]);
    sim.frame(&f);
    let outs = sim.tick(15);
    assert!(
        synth_events(&outs).contains(&Ev::new(EV_KEY, BTN_TOUCH, 1)),
        "key events must flush with their touch"
    );
}

/// button_held reflects reality for the shutdown safety-release.
#[test]
fn button_held_tracks_drag_state() {
    let mut sim = Sim::new();
    assert!(!sim.m.button_held());
    start_drag(&mut sim);
    assert!(sim.m.button_held());
    sim.frame_at(30, &cat(&[&up(0), &up(1), &up(2)]));
    assert!(!sim.m.button_held());
}

// ---- momentum ----------------------------------------------------------

const BTN_TOUCH_K: u16 = 0x14a;
const BTN_TOOL_FINGER_K: u16 = 0x145;
const BTN_TOOL_DOUBLETAP_K: u16 = 0x14d;

fn momentum_sim() -> Sim {
    momentum_sim_with(Momentum::mac())
}

fn momentum_sim_with(m: Momentum) -> Sim {
    let mut t = timing(0);
    t.momentum = Some(m);
    Sim {
        m: GestureMachine::new(t, RES, RES, 16),
        now: Instant::now(),
    }
}

fn key(code: u16, v: i32) -> Ev {
    Ev::new(EV_KEY, code, v)
}

/// Two fingers land together and settle, then move +y by `step` units
/// every 10 ms for `frames` frames. Returns the final y.
fn two_finger_scroll(sim: &mut Sim, step: i32, frames: i32) -> i32 {
    sim.frame(&cat(&[
        &down(0, 1, 500, 500),
        &down(1, 2, 700, 500),
        &[key(BTN_TOUCH_K, 1), key(BTN_TOOL_DOUBLETAP_K, 1)][..],
    ]));
    sim.tick(50);
    let mut y = 500;
    for _ in 0..frames {
        y += step;
        sim.frame_at(10, &cat(&[&mv(0, 500, y), &mv(1, 700, y)]));
    }
    y
}

fn lift_both() -> Vec<Ev> {
    cat(&[
        &up(0),
        &up(1),
        &[key(BTN_TOUCH_K, 0), key(BTN_TOOL_DOUBLETAP_K, 0)][..],
    ])
}

/// Run the machine's own deadlines for up to `ms`, like the I/O loop.
fn run_deadlines(sim: &mut Sim, ms: u64) -> Vec<Output> {
    let end = sim.now + Duration::from_millis(ms);
    let mut outs = Vec::new();
    while let Some(d) = sim.m.next_deadline() {
        if d > end {
            break;
        }
        sim.now = d.max(sim.now);
        outs.extend(sim.m.on_tick(sim.now));
    }
    sim.now = end.max(sim.now);
    outs
}

/// Every Y position written for `slot`, in order.
fn ys_for(evs: &[Ev], slot: i32) -> Vec<i32> {
    let mut cur = -1;
    let mut ys = Vec::new();
    for ev in evs {
        if ev.type_ == EV_ABS && ev.code == ABS_MT_SLOT {
            cur = ev.value;
        } else if cur == slot && ev.type_ == EV_ABS && ev.code == ABS_MT_POSITION_Y {
            ys.push(ev.value);
        }
    }
    ys
}

fn index_of(evs: &[Ev], ev: Ev) -> Option<usize> {
    evs.iter().position(|e| *e == ev)
}

/// The point of the feature: a flick keeps scrolling after lift-off,
/// slowing down, travelling about release speed x decay, then lifts
/// cleanly (slots and tool bits).
#[test]
fn flick_glides_then_lifts_cleanly() {
    let mut sim = momentum_sim();
    let y = two_finger_scroll(&mut sim, 30, 6); // 3 units/ms = 0.3 mm/ms
    let outs = sim.frame_at(10, &lift_both());
    let evs = synth_events(&outs);
    assert!(
        index_of(&evs, Ev::abs(ABS_MT_TRACKING_ID, -1)).is_none(),
        "the lift must be withheld while gliding: {evs:?}"
    );
    assert!(sim.m.gliding());

    let outs = run_deadlines(&mut sim, 4000);
    let evs = synth_events(&outs);
    let ys = ys_for(&evs, 0);
    assert!(ys.len() > 50, "glide should emit a stream of frames, got {}", ys.len());
    assert_eq!(ys, ys_for(&evs, 1), "both fingers glide together");
    assert!(ys.windows(2).all(|w| w[1] >= w[0]), "glide keeps the flick's direction");
    let early = ys[5] - ys[0];
    let late = ys[ys.len() - 1] - ys[ys.len() - 6];
    assert!(late < early, "glide slows down ({early} then {late})");
    let travel = ys[ys.len() - 1] - y;
    assert!(
        (880..=980).contains(&travel),
        "a 300 mm/s flick glides ~93 mm (~930 units) on the Mac curve, got {travel}"
    );
    assert!(!sim.m.gliding());
    for ev in [
        Ev::abs(ABS_MT_TRACKING_ID, -1),
        key(BTN_TOUCH_K, 0),
        key(BTN_TOOL_DOUBLETAP_K, 0),
    ] {
        assert!(evs.contains(&ev), "glide end must release {ev:?}");
    }
    assert_eq!(sim.m.next_deadline(), None);
}

/// Fingers that stop, then lift, don't glide: the lift goes straight
/// through.
#[test]
fn resting_lift_does_not_glide() {
    let mut sim = momentum_sim();
    let y = two_finger_scroll(&mut sim, 30, 6);
    for _ in 0..5 {
        sim.frame_at(10, &cat(&[&mv(0, 500, y), &mv(1, 700, y)]));
    }
    let outs = sim.frame_at(10, &lift_both());
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(!sim.m.gliding());
}

/// Momentum off: behaviour is exactly the old relay.
#[test]
fn momentum_off_relays_the_lift() {
    let mut sim = Sim::new();
    two_finger_scroll(&mut sim, 30, 6);
    let outs = sim.frame_at(10, &lift_both());
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(!sim.m.gliding());
}

/// Fingers never lift in unison: the second finger leaving 25 ms later
/// is part of the same flick, and nothing of it reaches the clone.
#[test]
fn staggered_lift_is_one_glide() {
    let mut sim = momentum_sim();
    two_finger_scroll(&mut sim, 30, 6);
    let mut outs = sim.frame_at(10, &cat(&[&up(0), &[key(BTN_TOOL_DOUBLETAP_K, 0), key(BTN_TOOL_FINGER_K, 1)][..]]));
    outs = collect(outs, run_deadlines(&mut sim, 25));
    outs = collect(outs, sim.frame(&cat(&[&up(1), &[key(BTN_TOUCH_K, 0), key(BTN_TOOL_FINGER_K, 0)][..]])));
    let evs = synth_events(&outs);
    assert!(index_of(&evs, Ev::abs(ABS_MT_TRACKING_ID, -1)).is_none(), "{evs:?}");
    assert!(index_of(&evs, key(BTN_TOOL_FINGER_K, 1)).is_none(), "{evs:?}");
    assert!(sim.m.gliding());
    let outs = run_deadlines(&mut sim, 4000);
    assert!(!sim.m.gliding());
    let evs = synth_events(&outs);
    assert!(evs.contains(&key(BTN_TOUCH_K, 0)));
    assert!(index_of(&evs, key(BTN_TOOL_FINGER_K, 1)).is_none(), "never shows the 1-finger stage");
}

/// Touch to stop: the glide freezes at once, the glide fingers hold
/// still for stop_hold (so GTK/Firefox see zero velocity at the stop),
/// and only then lift -- after which the new touch reaches the clone.
#[test]
fn touch_stops_glide_then_hands_over() {
    let mut sim = momentum_sim();
    two_finger_scroll(&mut sim, 30, 6);
    sim.frame_at(10, &lift_both());
    run_deadlines(&mut sim, 100);
    assert!(sim.m.gliding());

    let touch = cat(&[&down(2, 3, 300, 300), &[key(BTN_TOUCH_K, 1), key(BTN_TOOL_FINGER_K, 1)][..]]);
    let mut outs = sim.frame(&touch);
    outs = collect(outs, run_deadlines(&mut sim, 150));
    let evs = synth_events(&outs);
    assert!(ys_for(&evs, 0).is_empty(), "glide must freeze on touch: {evs:?}");
    assert!(index_of(&evs, Ev::abs(ABS_MT_TRACKING_ID, 3)).is_none(), "new touch held back during the hold");
    assert!(sim.m.gliding());

    let outs = run_deadlines(&mut sim, 20);
    let evs = synth_events(&outs);
    assert!(!sim.m.gliding());
    let released = index_of(&evs, Ev::abs(ABS_MT_TRACKING_ID, -1)).expect("glide fingers lift");
    let new_touch = index_of(&evs, Ev::abs(ABS_MT_TRACKING_ID, 3)).expect("new touch flushed");
    assert!(released < new_touch, "lift first, then the new touch: {evs:?}");
    let touch_on = evs.iter().rposition(|e| *e == key(BTN_TOUCH_K, 1)).expect("BTN_TOUCH back on");
    assert!(touch_on > released);

    // and it's live from here
    let outs = sim.frame_at(10, &mv(2, 320, 300));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_POSITION_X, 320)));
}

/// A finger that stays down after the other lifts isn't a flick: the
/// glide stops, and the remaining finger is introduced to the clone as a
/// touch of its own (position included).
#[test]
fn finger_left_down_ends_glide_and_becomes_a_touch() {
    let mut sim = momentum_sim();
    let y = two_finger_scroll(&mut sim, 30, 6);
    sim.frame_at(10, &cat(&[&up(0), &[key(BTN_TOOL_DOUBLETAP_K, 0), key(BTN_TOOL_FINGER_K, 1)][..]]));
    assert!(sim.m.gliding());
    let outs = run_deadlines(&mut sim, 300);
    assert!(!sim.m.gliding());
    let evs = synth_events(&outs);
    let released = evs.iter().rposition(|e| *e == Ev::abs(ABS_MT_TRACKING_ID, -1)).unwrap();
    let rest = &evs[released..];
    assert!(rest.contains(&Ev::abs(ABS_MT_TRACKING_ID, 2)), "remaining finger re-introduced: {evs:?}");
    assert!(rest.contains(&Ev::abs(ABS_MT_POSITION_Y, y)), "at its real position: {evs:?}");
    assert!(rest.contains(&key(BTN_TOOL_FINGER_K, 1)));
    let outs = sim.frame_at(10, &mv(1, 710, y));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_POSITION_X, 710)));
}

/// libinput ends an Apple touch whose size falls below 10, which happens
/// in the frames before a lift; during a 2-finger scroll sizes are
/// floored so the scroll survives to the lift. Nothing else is touched.
#[test]
fn two_finger_sizes_are_floored_only_while_scrolling() {
    let mut sim = momentum_sim();
    two_finger_scroll(&mut sim, 30, 2);
    let outs = sim.frame_at(10, &[Ev::abs(ABS_MT_SLOT, 0), Ev::abs(ABS_MT_TOUCH_MAJOR, 5), Ev::abs(ABS_MT_TOUCH_MINOR, 200)]);
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_TOUCH_MAJOR, SIZE_FLOOR)), "{evs:?}");
    assert!(evs.contains(&Ev::abs(ABS_MT_TOUCH_MINOR, 200)));

    let mut sim = momentum_sim();
    sim.frame(&down(0, 1, 100, 100));
    sim.tick(15);
    let outs = sim.frame_at(10, &[Ev::abs(ABS_MT_SLOT, 0), Ev::abs(ABS_MT_TOUCH_MAJOR, 5)]);
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TOUCH_MAJOR, 5)), "1 finger untouched");
}

/// Flick one way, then catch and flick back the other way: the reversal
/// must scroll live and glide too -- not be held behind the stop hold
/// (the bug: it reached the clone as one burst after the hold and was
/// lost, so the glide just braked).
#[test]
fn reverse_flick_during_glide_scrolls_and_glides() {
    let mut sim = momentum_sim();
    two_finger_scroll(&mut sim, 30, 6); // flick +y
    sim.frame_at(10, &lift_both());
    run_deadlines(&mut sim, 80);
    assert!(sim.m.gliding());

    // fingers land again, a few ms apart
    let mut outs = sim.frame(&cat(&[&down(3, 7, 500, 900), &[key(BTN_TOUCH_K, 1), key(BTN_TOOL_FINGER_K, 1)][..]]));
    outs = collect(outs, run_deadlines(&mut sim, 6));
    outs = collect(outs, sim.frame(&cat(&[&down(4, 8, 700, 900), &[key(BTN_TOOL_FINGER_K, 0), key(BTN_TOOL_DOUBLETAP_K, 1)][..]])));
    assert!(!sim.m.gliding(), "a 2-finger touch ends the glide at once");
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)), "glide fingers lift: {evs:?}");

    // and it's an ordinary scroll from here: flushed at the usual
    // debounce, live after that, well inside the old 160 ms hold
    let mut y = 900;
    let mut outs = Vec::new();
    let mut live_at = None;
    for i in 0..8 {
        y -= 30;
        let o = sim.frame_at(10, &cat(&[&mv(3, 500, y), &mv(4, 700, y)]));
        outs = collect(outs, run_deadlines(&mut sim, 0));
        let evs = synth_events(&o);
        if live_at.is_none() && evs.contains(&Ev::abs(ABS_MT_POSITION_Y, y)) {
            live_at = Some(i);
        }
        outs = collect(outs, o);
    }
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, 7)), "new touch reaches the clone");
    assert!(live_at.is_some_and(|i| i <= 5), "reversal relayed live by ~60 ms, got {live_at:?}");

    // the reverse flick glides the other way
    sim.frame_at(10, &cat(&[&up(3), &up(4), &[key(BTN_TOUCH_K, 0), key(BTN_TOOL_DOUBLETAP_K, 0)][..]]));
    assert!(sim.m.gliding(), "the reverse flick glides");
    let ys = ys_for(&synth_events(&run_deadlines(&mut sim, 4000)), 3);
    assert!(ys.len() > 20 && ys.windows(2).all(|w| w[1] <= w[0]), "glides in -y: {:?}", &ys[..ys.len().min(8)]);
    assert!(!sim.m.gliding());
}

// ---- momentum: scroll curve ------------------------------------------------

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol * a.abs().max(b.abs()).max(1e-12)
}

/// Glide frame positions (slot 0, y) after a +y flick of `step` units
/// per 10 ms, plus the y at release.
fn glide_ys(sim: &mut Sim, step: i32) -> (i32, Vec<i32>) {
    let y = two_finger_scroll(sim, step, 6);
    sim.frame_at(10, &lift_both());
    assert!(sim.m.gliding());
    let ys = ys_for(&synth_events(&run_deadlines(sim, 4000)), 0);
    assert!(!sim.m.gliding());
    (y, ys)
}

/// WebKit's MomentumEventDispatcher (Apple's copy of the system curve),
/// re-implemented independently: per 8 ms frame the finger delta is
/// scaled by alpha(|v|), until each axis is at or below the stop speed.
/// Here in mm and ms on a 1-D flick.
fn webkit_glide_mm(v0_mm_per_s: f64) -> (f64, u32) {
    let (tail_v, stop) = (250.0 / 400.0 * 25.4, 30.0 / 400.0 * 25.4); // mm/s
    let mut v = v0_mm_per_s;
    let (mut d, mut frames) = (0.0, 0);
    loop {
        d += v * 0.008;
        let weak = ((tail_v - v) / tail_v).max(0.0);
        v *= 0.975 - (0.975 - 0.91) * weak;
        frames += 1;
        if v <= stop {
            return (d, frames);
        }
    }
}

#[test]
fn mac_constants_match_webkit() {
    let m = Momentum::mac();
    assert!(close(m.tail_speed * 1000.0, 15.875, 1e-12));
    assert!(close(m.stop_speed * 1000.0, 1.905, 1e-12));
    assert_eq!(m.alpha(1.0), 0.975); // fast
    assert!(close(m.alpha(0.0), 0.91, 1e-12)); // at rest
    assert!(close(m.alpha(m.tail_speed / 2.0), (0.975 + 0.91) / 2.0, 1e-12));
}

/// The glide the machine emits is the WebKit replica's, frame for frame
/// in distance and length.
#[test]
fn glide_matches_webkit_replica() {
    for step in [3, 10, 30, 60] {
        let mut sim = momentum_sim();
        let (y, ys) = glide_ys(&mut sim, step); // step/10 units/ms
        let v0 = step as f64 / 10.0 / RES * 1000.0; // mm/s
        let (want_mm, want_frames) = webkit_glide_mm(v0);
        let got_mm = (ys[ys.len() - 1] - y) as f64 / RES;
        assert!(
            (got_mm - want_mm).abs() <= 0.15 + 0.01 * want_mm,
            "{v0} mm/s: {got_mm} mm vs {want_mm}"
        );
        let frames = ys.len() as i64;
        assert!(
            (frames - want_frames as i64).abs() <= 2,
            "{v0} mm/s: {frames} frames vs {want_frames}"
        );
    }
}

/// Slow flicks fade out quickly and glide disproportionately less (the
/// tail decay); there's no long coast that gets cut off.
#[test]
fn slow_flicks_glide_proportionally_less() {
    let (d30, f30) = webkit_glide_mm(30.0);
    let (d300, f300) = webkit_glide_mm(300.0);
    assert!(d300 / d30 > 10.0, "{d30} vs {d300}");
    assert!(f30 < f300);
    // and the machine's glide slows every frame down to the stop
    let mut sim = momentum_sim();
    let (y, ys) = glide_ys(&mut sim, 6); // 60 mm/s
    let mut prev = y;
    let steps: Vec<i32> = ys.iter().map(|&v| { let d = v - prev; prev = v; d }).collect();
    assert!(steps.windows(2).all(|w| w[1] <= w[0] + 1), "{steps:?}"); // rounding jitter
    assert!(*steps.last().unwrap() <= 1);
}

/// The knobs do what they say: a slower fast-decay glides further.
#[test]
fn decay_knob_changes_glide_length() {
    let travel = |fast: f64| {
        let mut sim = momentum_sim_with(Momentum { fast_decay: fast, ..Momentum::mac() });
        let (y, ys) = glide_ys(&mut sim, 30);
        ys[ys.len() - 1] - y
    };
    assert!(travel(0.985) > travel(0.975));
    assert!(travel(0.975) > travel(0.95));
}

// -- three-finger drag switched off --------------------------------------

#[test]
fn three_finger_drag_off_relays_the_touch() {
    let mut sim = Sim::new();
    sim.m.set_timing(Timing {
        three_finger_drag: false,
        ..timing(0)
    });
    let touchdown = cat(&[&down(0, 1, 100, 100), &down(1, 2, 200, 100), &down(2, 3, 300, 100)]);
    let mut outs = sim.frame(&touchdown);
    outs = collect(outs, sim.tick(60));
    outs = collect(outs, sim.frame(&cat(&[&mv(0, 100, 150), &mv(1, 200, 150), &mv(2, 300, 150)])));
    outs = collect(outs, sim.frame(&cat(&[&up(0), &up(1), &up(2)])));
    assert_eq!(mouse_downs(&outs), 0);
    assert_eq!(mouse_ups(&outs), 0);
    assert_eq!(total_move(&outs), (0, 0));
    let synth = synth_events(&outs);
    for id in [1, 2, 3] {
        assert!(synth.contains(&Ev::abs(ABS_MT_TRACKING_ID, id)), "finger {id} not relayed");
    }
    assert_eq!(synth.iter().filter(|e| **e == Ev::abs(ABS_MT_TRACKING_ID, -1)).count(), 3);
}

#[test]
fn three_finger_drag_off_ignores_growth_to_three() {
    let mut sim = Sim::new();
    sim.m.set_timing(Timing {
        three_finger_drag: false,
        ..timing(0)
    });
    let mut outs = sim.frame(&cat(&[&down(0, 1, 100, 100), &down(1, 2, 200, 100)]));
    outs = collect(outs, sim.tick(60));
    outs = collect(outs, sim.frame(&down(2, 3, 300, 100)));
    outs = collect(outs, sim.frame(&mv(2, 300, 160)));
    outs = collect(outs, sim.tick(200));
    assert_eq!(mouse_downs(&outs), 0);
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, 3)));
}

// -- momentum: wheel output ------------------------------------------------

fn wheel_momentum() -> Momentum {
    Momentum {
        output: MomentumOutput::Wheel,
        ..Momentum::mac()
    }
}

fn wheel_sim() -> Sim {
    momentum_sim_with(wheel_momentum())
}

/// Totals of every wheel output: (v120, h120, v, h, outputs).
fn wheel_totals(outs: &[Output]) -> (i64, i64, i64, i64, usize) {
    outs.iter().fold((0, 0, 0, 0, 0), |acc, o| match o {
        Output::Wheel { v120, h120, v, h } => (
            acc.0 + *v120 as i64,
            acc.1 + *h120 as i64,
            acc.2 + *v as i64,
            acc.3 + *h as i64,
            acc.4 + 1,
        ),
        _ => acc,
    })
}

fn wheel_outputs(outs: &[Output]) -> Vec<(i32, i32, i32, i32)> {
    outs.iter()
        .filter_map(|o| match o {
            Output::Wheel { v120, h120, v, h } => Some((*v120, *h120, *v, *h)),
            _ => None,
        })
        .collect()
}

/// Two fingers land and settle, then move by (`sx`, `sy`) units every
/// 10 ms for `frames` frames, then lift together. Returns the lift's
/// outputs.
fn flick_xy(sim: &mut Sim, sx: i32, sy: i32, frames: i32) -> Vec<Output> {
    sim.frame(&cat(&[
        &down(0, 1, 500, 500),
        &down(1, 2, 700, 500),
        &[key(BTN_TOUCH_K, 1), key(BTN_TOOL_DOUBLETAP_K, 1)][..],
    ]));
    sim.tick(50);
    let (mut x, mut y) = (0, 500);
    for _ in 0..frames {
        x += sx;
        y += sy;
        sim.frame_at(10, &cat(&[&mv(0, 500 + x, y), &mv(1, 700 + x, y)]));
    }
    sim.frame_at(10, &lift_both())
}

fn clone_holds_nothing(sim: &Sim) -> bool {
    sim.m.relayed_active.iter().all(|a| !a)
}

/// The point of wheel mode: the lift reaches the clone in the same frame
/// (the app sees the touch end), and the glide follows as wheel events
/// only -- nothing more is written to the clone.
#[test]
fn wheel_flick_relays_lift_at_once_then_scrolls() {
    let mut sim = wheel_sim();
    let outs = flick_xy(&mut sim, 0, 30, 6); // 0.3 mm/ms down
    let evs = synth_events(&outs);
    for ev in [
        Ev::abs(ABS_MT_TRACKING_ID, -1),
        key(BTN_TOUCH_K, 0),
        key(BTN_TOOL_DOUBLETAP_K, 0),
    ] {
        assert!(evs.contains(&ev), "lift must be relayed at once: missing {ev:?} in {evs:?}");
    }
    assert!(clone_holds_nothing(&sim));
    assert!(sim.m.gliding());
    assert!(wheel_outputs(&outs).is_empty(), "first wheel frame comes on the tick");

    let outs = run_deadlines(&mut sim, 4000);
    assert!(synth_events(&outs).is_empty(), "the glide never touches the clone: {outs:?}");
    let wheel = wheel_outputs(&outs);
    assert!(wheel.len() > 50, "a stream of wheel frames, got {}", wheel.len());
    assert!(wheel.iter().all(|w| w.0 > 0 && w.1 == 0 && w.3 == 0), "{wheel:?}");
    let early: i32 = wheel[..5].iter().map(|w| w.0).sum();
    let late: i32 = wheel[wheel.len() - 5..].iter().map(|w| w.0).sum();
    assert!(late < early, "glide slows down ({early} then {late})");
    assert!(!sim.m.gliding());
    assert_eq!(sim.m.next_deadline(), None);
    assert!(clone_holds_nothing(&sim));
}

/// Same decay model as fingers mode, scaled by the gain: the wheel total
/// is the WebKit replica's finger distance x wheel_gain, to the unit.
#[test]
fn wheel_total_matches_model_and_gain() {
    for (step, gain) in [(10, 24.0), (30, 24.0), (60, 24.0), (30, 10.0), (30, 100.0)] {
        let mut sim = momentum_sim_with(Momentum {
            wheel_gain: gain,
            ..wheel_momentum()
        });
        flick_xy(&mut sim, 0, step, 6);
        let (v120, h120, v, h, _) = wheel_totals(&run_deadlines(&mut sim, 4000));
        let v0 = step as f64 / 10.0 / RES * 1000.0; // mm/s
        let want = webkit_glide_mm(v0).0 * gain;
        assert!(
            (v120 as f64 - want).abs() <= 1.0 + 0.001 * want,
            "{v0} mm/s at gain {gain}: {v120} units vs {want:.1}"
        );
        assert_eq!((h120, h), (0, 0));
        // whole notches: everything emitted, less the sub-notch rest
        assert_eq!(v, v120 / 120, "{v0} mm/s at gain {gain}");
    }
    // the default: a 300 mm/s flick glides ~93 mm, ~2240 units, ~18 notches
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    let (v120, _, v, _, _) = wheel_totals(&run_deadlines(&mut sim, 4000));
    assert!((2150..=2330).contains(&v120), "{v120}");
    assert_eq!(v, 18);
}

/// Direction under natural scrolling (the touchpad's KWin setting here):
/// content follows the fingers, so fingers down = wheel up (+v), fingers
/// right = wheel left (-h). momentumWheelInvert flips both.
#[test]
fn wheel_sign_follows_natural_scrolling_and_invert_flips_it() {
    let m = wheel_momentum();
    assert_eq!(m.wheel_units(0.0, 1.0), (-0.0, 24.0));
    assert_eq!(m.wheel_units(1.0, 0.0), (-24.0, 0.0));
    let inv = Momentum { wheel_invert: true, ..m };
    assert_eq!(inv.wheel_units(1.0, -1.0), (24.0, 24.0));

    let totals = |m: Momentum, sx: i32, sy: i32| {
        let mut sim = momentum_sim_with(m);
        flick_xy(&mut sim, sx, sy, 6);
        let (v120, h120, _, _, _) = wheel_totals(&run_deadlines(&mut sim, 4000));
        (v120.signum(), h120.signum())
    };
    assert_eq!(totals(m, 0, 30), (1, 0), "fingers down: wheel up");
    assert_eq!(totals(m, 0, -30), (-1, 0), "fingers up: wheel down");
    assert_eq!(totals(m, 30, 0), (0, -1), "fingers right: wheel left");
    assert_eq!(totals(m, -30, 0), (0, 1), "fingers left: wheel right");
    assert_eq!(totals(m, 20, 20), (1, -1), "diagonal");
    assert_eq!(totals(inv, 0, 30), (-1, 0), "inverted");
    assert_eq!(totals(inv, 30, 0), (0, 1), "inverted");
}

/// Sub-unit and sub-notch remainders carry from frame to frame: at a
/// tiny gain most frames are under one unit and still nothing is lost;
/// at a huge one every notch boundary crossed is emitted exactly once.
#[test]
fn wheel_remainders_carry() {
    for gain in [0.3, 1.0, 7.0, 500.0] {
        let mut sim = momentum_sim_with(Momentum {
            wheel_gain: gain,
            ..wheel_momentum()
        });
        flick_xy(&mut sim, 0, 30, 6);
        let outs = run_deadlines(&mut sim, 4000);
        let wheel = wheel_outputs(&outs);
        assert!(wheel.iter().all(|w| w.0 != 0), "no empty wheel frames: {wheel:?}");
        let want = webkit_glide_mm(300.0).0 * gain;
        let (v120, _, v, _, _) = wheel_totals(&outs);
        assert!(
            (v120 as f64 - want).abs() <= 1.0 + 0.001 * want,
            "gain {gain}: {v120} vs {want:.2}"
        );
        // notches never run ahead of the hi-res total at any point
        let mut hi = 0;
        let mut notches = 0;
        for w in &wheel {
            hi += w.0;
            notches += w.2;
            assert_eq!(notches, hi / 120, "gain {gain}");
        }
        assert_eq!(v as i32, notches);
    }
}

/// A touch stops the glide in that very frame, and there's no stop
/// hold: the touch goes through ordinary classification (a lone finger
/// is live after probe_delay), not after 160 ms.
#[test]
fn wheel_touch_stops_glide_at_once_without_hold() {
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    let outs = run_deadlines(&mut sim, 100);
    assert!(!wheel_outputs(&outs).is_empty());
    assert!(sim.m.gliding());

    let touch = cat(&[&down(2, 3, 300, 300), &[key(BTN_TOUCH_K, 1), key(BTN_TOOL_FINGER_K, 1)][..]]);
    let outs = sim.frame(&touch);
    assert!(!sim.m.gliding(), "a touch stops a wheel glide at once");
    assert!(wheel_outputs(&outs).is_empty());

    let outs = run_deadlines(&mut sim, 20);
    assert!(wheel_outputs(&outs).is_empty(), "nothing after the touch: {outs:?}");
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, 3)), "touch live after probe_delay: {evs:?}");
    assert!(index_of(&evs, Ev::abs(ABS_MT_TRACKING_ID, -1)).is_none(), "no glide fingers to lift");
    let outs = sim.frame_at(10, &mv(2, 320, 300));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_POSITION_X, 320)));
    let outs = sim.frame_at(10, &cat(&[&up(2), &[key(BTN_TOUCH_K, 0), key(BTN_TOOL_FINGER_K, 0)][..]]));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(wheel_outputs(&run_deadlines(&mut sim, 1000)).is_empty());
    assert_eq!(sim.m.next_deadline(), None);
    assert!(clone_holds_nothing(&sim));
}

/// Flick down, catch with two fingers and flick up: the first glide
/// stops at the first contact, the reversal scrolls live and glides the
/// other way.
#[test]
fn wheel_reversal_stops_then_glides_back() {
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    run_deadlines(&mut sim, 80);
    assert!(sim.m.gliding());

    let mut outs = sim.frame(&cat(&[&down(3, 7, 500, 900), &[key(BTN_TOUCH_K, 1), key(BTN_TOOL_FINGER_K, 1)][..]]));
    assert!(!sim.m.gliding(), "first contact stops it");
    outs = collect(outs, run_deadlines(&mut sim, 6));
    outs = collect(outs, sim.frame(&cat(&[&down(4, 8, 700, 900), &[key(BTN_TOOL_FINGER_K, 0), key(BTN_TOOL_DOUBLETAP_K, 1)][..]])));
    let mut y = 900;
    for _ in 0..8 {
        y -= 30;
        outs = collect(outs, sim.frame_at(10, &cat(&[&mv(3, 500, y), &mv(4, 700, y)])));
        outs = collect(outs, run_deadlines(&mut sim, 0));
    }
    assert!(wheel_outputs(&outs).is_empty(), "no wheel while fingers are down");
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, 7)) && evs.contains(&Ev::abs(ABS_MT_POSITION_Y, y)));

    let outs = sim.frame_at(10, &cat(&[&up(3), &up(4), &[key(BTN_TOUCH_K, 0), key(BTN_TOOL_DOUBLETAP_K, 0)][..]]));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(sim.m.gliding(), "the reverse flick glides");
    let wheel = wheel_outputs(&run_deadlines(&mut sim, 4000));
    assert!(wheel.len() > 20 && wheel.iter().all(|w| w.0 < 0), "glides the other way: {wheel:?}");
    assert!(!sim.m.gliding());
    assert!(clone_holds_nothing(&sim));
}

/// Fingers lift 25 ms apart: each lift reaches the clone as it happens
/// (the 1-finger stage included -- that's the real pad), and it is one
/// glide, running from the first lift.
#[test]
fn wheel_staggered_lift_is_relayed_and_one_glide() {
    let mut sim = wheel_sim();
    two_finger_scroll(&mut sim, 30, 6);
    let outs = sim.frame_at(10, &cat(&[&up(0), &[key(BTN_TOOL_DOUBLETAP_K, 0), key(BTN_TOOL_FINGER_K, 1)][..]]));
    let evs = synth_events(&outs);
    assert!(evs.contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)) && evs.contains(&key(BTN_TOOL_FINGER_K, 1)), "{evs:?}");
    assert!(sim.m.gliding());
    let mut outs = run_deadlines(&mut sim, 25);
    outs = collect(outs, sim.frame(&cat(&[&up(1), &[key(BTN_TOUCH_K, 0), key(BTN_TOOL_FINGER_K, 0)][..]])));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(clone_holds_nothing(&sim));
    assert!(sim.m.gliding(), "still one glide");
    outs = collect(outs, run_deadlines(&mut sim, 4000));
    let (v120, ..) = wheel_totals(&outs);
    let want = webkit_glide_mm(300.0).0 * Momentum::DEFAULT_WHEEL_GAIN;
    assert!((v120 as f64 - want).abs() <= 1.0 + 0.001 * want, "{v120} vs {want}");
    assert!(!sim.m.gliding());
    assert_eq!(sim.m.next_deadline(), None);
}

/// A finger that stays down after the other lifts isn't a flick: the
/// glide gives up after the lift window, and that finger, live on the
/// clone all along, carries on as a touch.
#[test]
fn wheel_finger_left_down_stops_glide() {
    let mut sim = wheel_sim();
    let y = two_finger_scroll(&mut sim, 30, 6);
    sim.frame_at(10, &cat(&[&up(0), &[key(BTN_TOOL_DOUBLETAP_K, 0), key(BTN_TOOL_FINGER_K, 1)][..]]));
    assert!(sim.m.gliding());
    run_deadlines(&mut sim, 70);
    assert!(!sim.m.gliding());
    assert!(wheel_outputs(&run_deadlines(&mut sim, 500)).is_empty());
    let outs = sim.frame_at(10, &mv(1, 710, y));
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_POSITION_X, 710)));
    assert_eq!(sim.m.next_deadline(), None);
}

/// A second finger landing while the first lift's glide waits for the
/// other finger also stops it at once.
#[test]
fn wheel_finger_landing_during_lift_window_stops_glide() {
    let mut sim = wheel_sim();
    two_finger_scroll(&mut sim, 30, 6);
    sim.frame_at(10, &up(0));
    assert!(sim.m.gliding());
    sim.frame_at(10, &down(0, 9, 500, 700));
    assert!(!sim.m.gliding());
    assert!(wheel_outputs(&run_deadlines(&mut sim, 500)).is_empty());
}

/// Hot reload mid-glide: a wheel glide finishes as a wheel glide (the
/// new output applies from the next glide), a fingers glide finishes as
/// a fingers glide, turning momentum off stops a wheel glide at once,
/// and none of it leaves a deadline, a glide or a clone touch behind.
#[test]
fn wheel_hot_reload_mid_glide_leaves_no_stuck_state() {
    let fingers_timing = || {
        let mut t = timing(0);
        t.momentum = Some(Momentum::mac());
        t
    };
    let wheel_timing = || {
        let mut t = timing(0);
        t.momentum = Some(wheel_momentum());
        t
    };

    // wheel -> fingers mid-glide: still wheel output to the end
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    run_deadlines(&mut sim, 50);
    sim.m.set_timing(fingers_timing());
    let outs = run_deadlines(&mut sim, 4000);
    assert!(!wheel_outputs(&outs).is_empty() && synth_events(&outs).is_empty());
    assert!(!sim.m.gliding());
    assert_eq!(sim.m.next_deadline(), None);
    // ...and the next flick uses fingers
    let outs = flick_xy(&mut sim, 0, 30, 6);
    assert!(index_of(&synth_events(&outs), Ev::abs(ABS_MT_TRACKING_ID, -1)).is_none());
    let outs = run_deadlines(&mut sim, 4000);
    assert!(wheel_outputs(&outs).is_empty());
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(clone_holds_nothing(&sim));

    // fingers -> wheel mid-glide: the glide fingers still lift at the end
    let mut sim = momentum_sim();
    flick_xy(&mut sim, 0, 30, 6);
    run_deadlines(&mut sim, 50);
    sim.m.set_timing(wheel_timing());
    let outs = run_deadlines(&mut sim, 4000);
    assert!(wheel_outputs(&outs).is_empty());
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(!sim.m.gliding() && clone_holds_nothing(&sim));
    assert_eq!(sim.m.next_deadline(), None);

    // momentum off mid wheel glide: stops on the next frame
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    run_deadlines(&mut sim, 50);
    sim.m.set_timing(timing(0));
    let outs = run_deadlines(&mut sim, 4000);
    assert!(wheel_outputs(&outs).is_empty());
    assert!(!sim.m.gliding());
    assert_eq!(sim.m.next_deadline(), None);
    // and a flick now is a plain relay
    let outs = flick_xy(&mut sim, 0, 30, 6);
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert!(!sim.m.gliding());

    // gain/invert changed mid-glide: the running glide keeps its own
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    sim.m.set_timing(Timing {
        momentum: Some(Momentum { wheel_invert: true, wheel_gain: 1.0, ..wheel_momentum() }),
        ..timing(0)
    });
    let (v120, ..) = wheel_totals(&run_deadlines(&mut sim, 4000));
    assert!(v120 > 2000, "{v120}");
}

/// A dropped-events resync mid-glide just ends a wheel glide: there are
/// no glide fingers to lift.
#[test]
fn wheel_resync_mid_glide_ends_it_cleanly() {
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    run_deadlines(&mut sim, 50);
    let outs = sim.m.on_resync(&[(-1, 0, 0); 16], sim.now);
    assert!(!sim.m.gliding());
    assert!(wheel_outputs(&outs).is_empty() && synth_events(&outs).is_empty(), "{outs:?}");
    assert_eq!(sim.m.next_deadline(), None);
}

/// Fingers mode never emits wheel output (the default is untouched).
#[test]
fn fingers_mode_never_emits_wheel() {
    assert_eq!(Momentum::mac().output, MomentumOutput::Fingers);
    let mut sim = momentum_sim();
    let mut outs = flick_xy(&mut sim, 0, 30, 6);
    outs = collect(outs, run_deadlines(&mut sim, 4000));
    assert!(wheel_outputs(&outs).is_empty());
}

/// If the event loop stalls mid-glide (suspend), the next frame doesn't
/// scroll the whole gap in one burst of notches: the glide just stops.
#[test]
fn wheel_stall_stops_instead_of_jumping() {
    let mut sim = wheel_sim();
    flick_xy(&mut sim, 0, 30, 6);
    run_deadlines(&mut sim, 40);
    assert!(sim.m.gliding());
    let outs = sim.tick(5000);
    assert!(wheel_outputs(&outs).is_empty(), "{outs:?}");
    assert!(!sim.m.gliding());
    assert_eq!(sim.m.next_deadline(), None);
}

/// Turning momentum off mid-glide (a hot reload) ends the glide: its
/// fingers must not stay on the clone.
#[test]
fn momentum_off_mid_glide_ends_it() {
    let mut sim = momentum_sim();
    two_finger_scroll(&mut sim, 30, 6);
    sim.frame_at(10, &lift_both());
    assert!(sim.m.gliding());
    sim.tick(8);
    sim.m.set_timing(timing(0));
    let outs = sim.tick(8);
    assert!(!sim.m.gliding());
    assert!(synth_events(&outs).contains(&Ev::abs(ABS_MT_TRACKING_ID, -1)));
    assert_eq!(sim.m.next_deadline(), None);
}
