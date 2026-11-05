//! Framing regression tests: each scene runs over several detector-noise seeds and must keep the
//! behaviour we tuned for (still when it should be still, one clean move when it should move).

use autoframe::framer::Feel;
use autoframe::sim::{self, Scene, Stats};

const SEEDS: u64 = 10;

fn runs(scene: Scene) -> Vec<Stats> {
    (0..SEEDS).map(|s| sim::run(sim::default_params(), Feel::default(), scene, s)).collect()
}

fn assert_still(name: &str, scene: Scene) {
    for (seed, s) in runs(scene).iter().enumerate() {
        assert_eq!(s.moves, 0, "{name} seed {seed}: expected no movement, got {s:?}");
        assert_eq!(s.cut_frames, 0, "{name} seed {seed}: face cut off: {s:?}");
    }
}

#[test]
fn sitting_still_never_moves() {
    assert_still("sit still", sim::sit_still);
}

#[test]
fn talking_and_fidgeting_never_moves() {
    assert_still("fidget", sim::fidget);
}

#[test]
fn nodding_never_moves() {
    assert_still("nodding", sim::nodding);
}

#[test]
fn short_glance_never_moves() {
    assert_still("glance", sim::glance);
}

#[test]
fn settles_on_a_zoomed_shot() {
    for s in runs(sim::sit_still) {
        assert!((1.15..=1.35).contains(&s.final_zoom), "zoom {}", s.final_zoom);
    }
}

#[test]
fn initial_framing_is_quick() {
    for (seed, s) in runs(sim::sit_still).iter().enumerate() {
        let t = s.acquired_at.expect("never framed");
        assert!(t < 1.8, "seed {seed}: initial framing took {t:.2}s");
    }
}

#[test]
fn leaning_gives_one_smooth_pan() {
    for (seed, s) in runs(sim::lean_right).iter().enumerate() {
        assert_eq!(s.moves, 1, "seed {seed}: {s:?}");
        assert!(s.path_px > 120.0, "seed {seed}: barely moved: {s:?}");
        // Calm: no faster than ~0.3 frame widths per second.
        assert!(s.max_v < 600.0, "seed {seed}: too fast: {s:?}");
        let dur = s.settled.unwrap() - s.first_move.unwrap();
        assert!((0.4..2.0).contains(&dur), "seed {seed}: pan took {dur:.2}s");
        assert_eq!(s.cut_frames, 0);
    }
}

#[test]
fn leaning_back_gives_one_slow_zoom_in() {
    for (seed, s) in runs(sim::lean_back).iter().enumerate() {
        assert_eq!(s.moves, 1, "seed {seed}: {s:?}");
        assert!(s.zoom_path > 0.1, "seed {seed}: no zoom change: {s:?}");
        assert_eq!(s.cut_frames, 0);
    }
}

#[test]
fn walking_off_is_followed_then_goes_wide() {
    for (seed, s) in runs(sim::walk_off).iter().enumerate() {
        // Fast exit (790 px in 1 s): a few cut frames at the very edge are unavoidable.
        assert!(s.cut_frames <= 10, "seed {seed}: face cut for too long: {s:?}");
        assert!(s.moves <= 3, "seed {seed}: too many moves: {s:?}");
        assert!((s.final_zoom - 1.0).abs() < 0.01, "seed {seed}: did not return to wide: {s:?}");
    }
}
