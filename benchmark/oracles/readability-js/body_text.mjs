// mdrcel Stage-0 parser-equivalence BLOCKER probe (HLD §6.1).
//
// NOT an oracle adapter. This is a clearly-scoped Stage-0 verification aid
// (mdrcel M2 HLD §6.1, supervisor M-1/m-4): it emits the RAW
// `document.body.textContent` produced by **the exact jsdom the oracle uses**
// (jsdom 29.1.1, resolved from this directory's node_modules), so the Rust
// gate (`mdrcel/tests/parser_equivalence_gate.rs`) can prove the
// html5ever+rcdom facade's `text_content` is post-tokenizer token-identical
// to jsdom's BEFORE any extraction logic is built on it. A divergence here is
// a Stage-0 design-decision trigger (rcdom -> kuchikiki), never a downstream
// tuning task.
//
// It deliberately reuses the oracle's jsdom construction (run.mjs:184 —
// `new jsdom.JSDOM(html, { virtualConsole })`, inert: no runScripts, no
// resource loader) so the parse it probes is byte-for-byte the parse
// Readability runs against. It does NOT run Readability — it reads
// `document.body.textContent` directly (the WHATWG Node.textContent the HLD
// §2.1 anchors the port to).
//
// Invocation:  node body_text.mjs <abs.html>
// Output (deliberately NOT JSON — keeps the Rust gate free of any JSON
// dependency and removes all escaping fragility): on success, the RAW UTF-8
// `document.body.textContent` is written to stdout verbatim (single write +
// flush) and the process exits 0; on failure nothing is written to stdout, a
// diagnostic goes to stderr, and the process exits non-zero. The Rust gate
// therefore consumes `String::from_utf8(stdout)` directly and treats a
// non-zero exit as a hard probe failure. All parser noise -> stderr (same
// stdout-hygiene posture as run.mjs).

import { readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

// Write the raw body text (a single write + flush, then exit 0). Mirrors
// run.mjs's "build the complete output, then ONE write" stdout discipline.
function emitText(text) {
  process.stdout.write(text, () => {
    process.exit(0);
  });
}

// Failure: nothing on stdout (so the gate never mistakes a partial/garbled
// write for body text), diagnostic on stderr, non-zero exit.
function emitFailure(message) {
  process.stderr.write(`[body_text.mjs] ${String(message)}\n`);
  process.exit(1);
}

function main() {
  try {
    const snapshotPath = process.argv[2];
    if (!snapshotPath) {
      emitFailure("missing required <abs.html> argument");
      return;
    }

    const jsdom = require("jsdom");

    let st;
    try {
      st = statSync(snapshotPath);
    } catch {
      st = null;
    }
    if (st === null || !st.isFile()) {
      emitFailure(`snapshot not found or not a regular file: ${snapshotPath}`);
      return;
    }

    // Identical byte handling to run.mjs:139 (utf-8 decode of raw bytes,
    // handed to jsdom unmodified — part of the pinned parse).
    const html = readFileSync(snapshotPath, "utf-8");

    // Identical jsdom construction to run.mjs:146-185, MINUS --base-url (we
    // want the raw body text; base URL only affects relative-URL resolution,
    // never textContent). Inert: no runScripts, no resource loader.
    const virtualConsole = new jsdom.VirtualConsole();
    virtualConsole.on("jsdomError", (e) => {
      process.stderr.write(`[jsdomError] ${e && e.message ? e.message : e}\n`);
    });

    const dom = new jsdom.JSDOM(html, { virtualConsole });
    const doc = dom.window.document;

    // The probe target: WHATWG Node.textContent of <body> (HLD §2.1). jsdom
    // always synthesises <body> for a document parse; guard defensively.
    const body = doc.body;
    const bodyText = body ? (body.textContent ?? "") : "";

    emitText(bodyText);
  } catch (exc) {
    const name = exc && exc.constructor ? exc.constructor.name : "Error";
    const msg = exc && exc.message ? exc.message : String(exc);
    emitFailure(`${name}: ${msg}`);
  }
}

main();
