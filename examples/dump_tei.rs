//! Throwaway M8 investigation helper: dump mdrcel's `extract_to_tei` output
//! for a fixture path so the teiHeader can be diffed against the Python oracle.
//! Usage: cargo run --example dump_tei -- <path.html>
use std::fs;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_tei <path.html>");
    let bytes = fs::read(&path).expect("read fixture");
    let html = String::from_utf8_lossy(&bytes);
    match mdrcel::extract_to_tei(&html, None, &mdrcel::Options::default()) {
        Ok(s) => print!("{s}"),
        Err(e) => {
            eprintln!("extract_to_tei Err: {e:?}");
            std::process::exit(1);
        }
    }
}
