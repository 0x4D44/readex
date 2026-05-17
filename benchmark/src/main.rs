//! `benchmark` — the mdrcel differential test harness CLI entrypoint.
//!
//! Stage 0+1 (Bootstrap): the workspace skeleton compiles and runs. The full
//! CLI (corpus loading, oracle invocation, scoring, report, regression) is
//! implemented in later stages; the modules below are stubs for now.

mod corpus;
mod crate_run;
mod metrics;
mod oracle;
mod regression;
mod report;
mod score;

fn main() {
    println!("no corpus");
}
