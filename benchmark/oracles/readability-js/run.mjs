// mdrcel Mozilla Readability oracle adapter.
//
// Invocation (HLD 'mdrcel Oracle Adapters' section 3.1):
//
//     node <repo>/benchmark/oracles/readability-js/run.mjs <abs.html> [--base-url <URL>]
//
// Writes EXACTLY ONE JSON object to stdout (single write + flush) and nothing
// else; all logs/warnings/parser noise go to stderr. Output shape is governed
// by ../contract.schema.json; the behavioural contract (the ok/error/exit
// tri-state, same-machine determinism, the well-formed `text` primitive) is
// HLD section 3. Node auto-resolves ./node_modules relative to this script;
// no venv/re-exec is needed (only run.py self-bootstraps). Requires Node >= 20
// for String.prototype.toWellFormed (HLD section 6 / .nvmrc + engines).

import { readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

// Build the COMPLETE object in memory, then a SINGLE write + flush; never
// begin emitting until the object is complete (HLD section 3.3 — no
// partial/interleaved stdout). JSON.stringify already emits non-ASCII raw.
function emit(obj, exitCode) {
  process.stdout.write(JSON.stringify(obj), () => {
    process.exit(exitCode);
  });
}

// The fully field-determined section 3.4 failure envelope. `ok:false` and a
// non-zero exit ALWAYS co-occur (the consumer treats either alone as
// failure). `oracleVersion` is whatever was readable before the failure (or
// null) — the require() of the version happens inside the guarded block so an
// install/resolve failure still yields this envelope.
function emitFailure(message, oracleVersion = null) {
  emit(
    {
      contract_version: 1,
      oracle: "readability-js",
      oracle_version: oracleVersion,
      title: null,
      text: "",
      html: null,
      word_count: null,
      canonical_url: null,
      language: null,
      ok: false,
      error: String(message),
    },
    1,
  );
}

// HLD section 3.3 pinned, never-raising primitive (Node side): replace lone
// surrogates so `text` is valid UTF-8 before serialization (a serde_json
// reject of an otherwise-valid extraction is a Bug-E2 trap). Identity on
// well-formed input (verified incl. astral/BOM/NUL).
function toWellFormed(text) {
  return text.toWellFormed();
}

// Informational only (HLD section 3.2): the consumer recomputes via its
// single tokenizer and ignores this. A simple whitespace split is sufficient
// and deterministic.
function wordCount(text) {
  const t = text.trim();
  return t.length === 0 ? 0 : t.split(/\s+/u).length;
}

// Positional <abs.html> plus optional --base-url <URL>. No CLI library: a
// fixed two-shape CLI.
function parseArgs(argv) {
  let path = null;
  let baseUrl = null;
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--base-url") {
      if (i + 1 >= argv.length) {
        return { err: "--base-url requires a URL argument" };
      }
      baseUrl = argv[i + 1];
      i += 1;
      continue;
    }
    if (path === null) {
      path = a;
      continue;
    }
    return { err: `unexpected extra argument: ${JSON.stringify(a)}` };
  }
  if (path === null) {
    return { err: "missing required <abs.html> argument" };
  }
  return { path, baseUrl };
}

function main() {
  // Everything that can fail at the tool/resolve layer is inside this guard
  // so a resolve/parse failure still emits the section 3.4 envelope (the
  // Bug-E2 'adapter blew up — catchable' guard).
  let oracleVersion = null;
  try {
    const args = parseArgs(process.argv.slice(2));
    if (args.err) {
      emitFailure(args.err);
      return;
    }
    const { path: snapshotPath, baseUrl } = args;

    // Resolve deps + version read INSIDE the guard (HLD section 3.4). The
    // pinned @mozilla/readability 0.6.x ships NO `exports` map so the
    // package.json subpath resolves (pin-bump checklist, README).
    const { Readability } = require("@mozilla/readability");
    const jsdom = require("jsdom");
    try {
      oracleVersion = require("@mozilla/readability/package.json").version;
    } catch {
      oracleVersion = null;
    }

    let st;
    try {
      st = statSync(snapshotPath);
    } catch {
      st = null;
    }
    if (st === null || !st.isFile()) {
      emitFailure(
        `snapshot file not found or not a regular file: ${JSON.stringify(
          snapshotPath,
        )}`,
        oracleVersion,
      );
      return;
    }

    // Bytes read RAW (utf-8 decode of the raw bytes) and handed to jsdom
    // unmodified; jsdom does whatever parsing it does — part of the pinned
    // algorithm (HLD section 3.1, honest framing).
    const html = readFileSync(snapshotPath, "utf-8");

    // stdout hygiene (HLD section 5): a bare VirtualConsole already installs a
    // no-op `error` listener in the pinned jsdom (verified, README pin-bump
    // checklist) and forwards nowhere. Attach ONLY a jsdomError -> stderr
    // listener (diagnostics). Do NOT call forwardTo/sendTo (that is what would
    // route parser noise to stdout).
    const virtualConsole = new jsdom.VirtualConsole();
    virtualConsole.on("jsdomError", (e) => {
      process.stderr.write(`[jsdomError] ${e && e.message ? e.message : e}\n`);
    });

    // --base-url is AUXILIARY (canonical-URL / relative-link resolution only),
    // NOT essential to main-content extraction. A malformed value must degrade
    // gracefully — be IGNORED — not escalate to a hard extraction failure
    // (jsdom rejects an invalid `url` with `TypeError: Invalid URL`, which
    // would silently remove this oracle from the differential for every
    // malformed-URL page while the Trafilatura oracle keeps participating: an
    // asymmetric measuring-instrument bias). So validate it here; an absent OR
    // structurally-invalid base URL collapses to the same null effective base,
    // and extraction proceeds exactly as the no-base path does. A valid base
    // URL is unchanged (zero behavioural difference). This mirrors
    // Trafilatura's run.py, which tolerates a malformed url= and still
    // extracts the article.
    let effectiveBaseUrl = null;
    if (baseUrl) {
      try {
        // eslint-disable-next-line no-new
        new URL(baseUrl);
        effectiveBaseUrl = baseUrl;
      } catch {
        effectiveBaseUrl = null; // malformed -> ignore (same as absent).
      }
    }

    // jsdom inert & offline (HLD section 5): do NOT enable runScripts and do
    // NOT install a resources loader — both off by default; that IS the
    // no-network / no-JS / deterministic-parse guarantee. Add nothing more.
    // `url` only set when a VALID --base-url was given (jsdom DOM base for
    // relative resolution); with none/invalid, jsdom's default about:blank
    // applies.
    const jsdomOpts = { virtualConsole };
    if (effectiveBaseUrl) {
      jsdomOpts.url = effectiveBaseUrl;
    }
    const dom = new jsdom.JSDOM(html, jsdomOpts);
    const doc = dom.window.document;

    const article = new Readability(doc).parse();

    if (article === null) {
      // `.parse()` -> null is 'found nothing', NOT an error (HLD section 5 /
      // section 3.4) -> ok:true, text:"", exit 0. The exact distinction Bug
      // E2 collapsed.
      emit(
        {
          contract_version: 1,
          oracle: "readability-js",
          oracle_version: oracleVersion,
          title: null,
          text: "",
          html: null,
          word_count: 0,
          canonical_url: null,
          language: null,
          ok: true,
          error: null,
        },
        0,
      );
      return;
    }

    const text = toWellFormed(article.textContent ?? "");

    // canonical_url: <link rel="canonical"> href, emitted only if absolute or
    // a VALID --base-url was given; with no (or an ignored, malformed) base
    // URL a relative canonical is null (do NOT resolve against about:blank —
    // HLD section 5). Keyed off effectiveBaseUrl so the malformed-base path is
    // byte-symmetric with the absent-base path.
    let canonicalUrl = null;
    const linkEl = doc.querySelector('link[rel~="canonical" i]');
    if (linkEl) {
      const rawHref = linkEl.getAttribute("href");
      if (rawHref) {
        if (effectiveBaseUrl) {
          // jsdom resolved .href against the supplied base URL.
          canonicalUrl = linkEl.href || rawHref;
        } else {
          try {
            // Absolute on its own?
            // eslint-disable-next-line no-new
            new URL(rawHref);
            canonicalUrl = rawHref;
          } catch {
            canonicalUrl = null; // relative + no base -> null.
          }
        }
      }
    }

    const lang = article.lang; // Readability's returned lang; do NOT re-read
    //                             the DOM (HLD section 5).
    emit(
      {
        contract_version: 1,
        oracle: "readability-js",
        oracle_version: oracleVersion,
        title: article.title ? article.title : null,
        text,
        html: article.content ?? null, // free, populated; never scored.
        word_count: wordCount(text),
        canonical_url: canonicalUrl,
        language: lang ? lang : null,
        ok: true,
        error: null,
      },
      0,
    );
  } catch (exc) {
    // Any catchable tool/runtime error still emits the section 3.4 envelope.
    const name = exc && exc.constructor ? exc.constructor.name : "Error";
    const msg = exc && exc.message ? exc.message : String(exc);
    emitFailure(`${name}: ${msg}`, oracleVersion);
  }
}

main();
