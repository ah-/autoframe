//! Prints framing behaviour for each simulated scene (mean over 10 noise seeds).
//!     cargo run --release --example sim

use autoframe::framer::Feel;
use autoframe::sim;

const SEEDS: u64 = 10;

fn main() {
    let (params, feel) = (sim::default_params(), Feel::default());
    println!(
        "{:<12} {:>5} {:>8} {:>7} {:>6} {:>5} {:>7} {:>7} {:>5}   expectation",
        "scene", "moves", "path px", "zoom Δ", "max v", "cut", "1st mv", "settled", "zoom"
    );
    let start: Vec<_> =
        (0..SEEDS).map(|s| sim::run(params, feel, sim::sit_still, s).acquired_at.unwrap_or(f64::NAN)).collect();
    println!(
        "initial framing settles after {:.2}s (mean of {SEEDS} seeds)\n",
        start.iter().sum::<f64>() / SEEDS as f64
    );
    for (name, scene, expect) in sim::scenes() {
        let runs: Vec<_> = (0..SEEDS).map(|s| sim::run(params, feel, scene, s)).collect();
        let mean = |f: &dyn Fn(&sim::Stats) -> f64| runs.iter().map(f).sum::<f64>() / SEEDS as f64;
        let fmt = |o: Option<f64>| o.map(|v| format!("{v:.1}s")).unwrap_or("-".into());
        println!(
            "{:<12} {:>5.1} {:>8.0} {:>7.3} {:>6.0} {:>5.1} {:>7} {:>7} {:>4.2}x   {}",
            name,
            mean(&|s| s.moves as f64),
            mean(&|s| s.path_px),
            mean(&|s| s.zoom_path),
            runs.iter().map(|s| s.max_v).fold(0.0, f64::max),
            mean(&|s| s.cut_frames as f64),
            fmt(runs[0].first_move),
            fmt(runs[0].settled),
            runs[0].final_zoom,
            expect
        );
    }
}
