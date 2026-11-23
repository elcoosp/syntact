//! Example: fail-fast gated drives (`drive_sink_until` + [`MaxErrors`]).
//!
//! A stream with three deferred diagnostics drains fully through
//! [`drive_sink`]; behind an error budget the drive halts before the
//! budget-exceeding diagnostic reaches the sink, and a closure gate can
//! stop before any diagnostic at all. The [`DriveStats`] record reports
//! how far each run got.
//!
//! Run: `cargo run -p runtime-core --example fail_fast`

use std::ops::ControlFlow;

use runtime_core::events::Output;
use runtime_core::sink::{
    DriveStats, FilterStep, MaxErrors, ValidationSink, drive_sink, drive_sink_until,
};

fn main() {
    // Hand-packed stream: `10 { tok err tok err tok err }` over an
    // 8-byte source — three diagnostics inside one node.
    let output = Output {
        stream: vec![
            Output::pack_enter(10),
            Output::pack_token(1, 1),
            Output::pack_error(0),
            Output::pack_token(2, 1),
            Output::pack_error(1),
            Output::pack_token(3, 1),
            Output::pack_error(2),
            Output::pack_exit(),
        ],
    };
    let ranges = [0..2_u32, 3..5, 6..8];

    // Full drain: every diagnostic lands (the drive_sink baseline).
    let mut full = ValidationSink::default();
    let _ = drive_sink(&output, &ranges, &mut full);
    println!("full drive:      {:?}", full.summary());

    // Error budget: MaxErrors::new(1) lets the first diagnostic through
    // and halts the drive before the second one reaches the sink.
    let mut sink = ValidationSink::default();
    let mut gate = MaxErrors::new(1);
    let stats: DriveStats = drive_sink_until(&output, &ranges, &mut gate, &mut sink);
    println!(
        "budget of one:   {stats:?}, errors {}, budget tripped {}",
        sink.errors,
        gate.tripped()
    );

    // A closure gate: stop before ANY diagnostic step is delivered.
    let mut sink = ValidationSink::default();
    let mut gate = |step: &FilterStep<'_>| {
        if matches!(step, FilterStep::Error { .. }) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let stats = drive_sink_until(&output, &ranges, &mut gate, &mut sink);
    println!("fail-fast gate:  {stats:?}, errors {}", sink.errors);
}
