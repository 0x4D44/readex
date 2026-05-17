// Committed dev-time self-test for the Readability-JS oracle adapter.
//
// One command (see README). NOT a runtime dependency of run.mjs. Uses the
// dev-time-only `ajv` validator from devDependencies (HLD section 6); not in
// the adapter's pinned runtime closure.
//
// Implements the HLD section 7 five-step contract proof against
// ../contract.schema.json, plus the "found nothing != error" Bug-E2 guard.
// The 'oracle of the oracles' for the Readability side: schema validity, the
// ok/error/exit tri-state, non-empty text on a content page, stdout-hygiene
// under the fixture's malformed CSS, the section 3.3 well-formed primitive,
// and same-machine run-twice byte-identity.
//
// Exit 0 iff every step passes; non-zero with a stderr reason otherwise.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const RUN_MJS = join(HERE, "run.mjs");
const SCHEMA = join(HERE, "..", "contract.schema.json");
const FIXTURES = join(HERE, "..", "fixtures");
const ARTICLE = join(FIXTURES, "article.html");
const EMPTY = join(FIXTURES, "empty.html");
const NONEXISTENT = join(FIXTURES, "this-path-does-not-exist.html");

function fail(step, msg) {
  process.stderr.write(`[selftest.mjs] FAIL (${step}): ${msg}\n`);
  process.exit(1);
}

// Invoke run.mjs exactly as the harness does: bare `node` + script + abs path,
// plus any extra CLI args verbatim (e.g. ["--base-url", "..."]).
function invoke(path, ...extraArgs) {
  const r = spawnSync(process.execPath, [RUN_MJS, path, ...extraArgs], {
    encoding: "buffer",
  });
  return {
    code: r.status,
    stdout: r.stdout,
    stderr: r.stderr ? r.stderr.toString("utf-8") : "",
  };
}

function loadValidator() {
  const Ajv = require("ajv");
  const schema = JSON.parse(readFileSync(SCHEMA, "utf-8"));
  const ajv = new Ajv({ allErrors: true, strict: false });
  return ajv.compile(schema);
}

function parseStdout(step, stdoutBuf) {
  let txt;
  try {
    txt = stdoutBuf.toString("utf-8");
  } catch (e) {
    fail(step, `stdout is not valid UTF-8: ${e}`);
  }
  try {
    return JSON.parse(txt);
  } catch (e) {
    fail(step, `stdout is not one valid JSON object: ${e} :: ${txt.slice(0, 200)}`);
  }
  return undefined;
}

function validate(step, validateFn, obj) {
  if (!validateFn(obj)) {
    fail(
      step,
      `schema validation failed: ${JSON.stringify(validateFn.errors)}`,
    );
  }
}

function hasSurrogate(s) {
  for (const ch of s) {
    const c = ch.codePointAt(0);
    if (c >= 0xd800 && c <= 0xdfff) return true;
  }
  return false;
}

const validateFn = loadValidator();

// --- Step 1: article.html -> schema-valid, ok:true, exit 0, substantive
//     text, no <head>/<script>/<style> leak, ONE clean object despite the
//     fixture's deliberately malformed CSS (exercises section 5 stderr-only
//     virtual console). ----------------------------------------------------
{
  const r = invoke(ARTICLE);
  if (r.code !== 0) fail("step1", `expected exit 0, got ${r.code}`);
  const obj = parseStdout("step1", r.stdout);
  validate("step1", validateFn, obj);
  if (obj.ok !== true || obj.error !== null) {
    fail("step1", `expected ok:true/error:null, got ${JSON.stringify(obj)}`);
  }
  if (typeof obj.text !== "string" || obj.text.trim().length < 80) {
    fail("step1", "expected substantive `text` on the article fixture");
  }
  if (obj.text.includes("SCRIPT_SHOULD_NOT_APPEAR_IN_TEXT")) {
    fail("step1", "<script> content leaked into extracted `text`");
  }
  if (obj.text.includes("color: #123456") || obj.text.includes("@media screen")) {
    fail("step1", "<style> content leaked into extracted `text`");
  }
  if (obj.contract_version !== 1) {
    fail("step1", `contract_version must be 1, got ${obj.contract_version}`);
  }
  // stdout must be exactly ONE object: parseStdout above already proved the
  // whole stdout is a single JSON value (no trailing parser noise).
}

// --- Step 2: empty.html -> schema-valid, ok:true, text:"", exit 0 (the Bug
//     E2 'found nothing' guard). example.com-style empties legitimately
//     return empty for a correct Readability adapter. ----------------------
{
  const r = invoke(EMPTY);
  if (r.code !== 0) fail("step2", `expected exit 0, got ${r.code}`);
  const obj = parseStdout("step2", r.stdout);
  validate("step2", validateFn, obj);
  if (obj.ok !== true || obj.error !== null) {
    fail("step2", `'found nothing' must be ok:true, got ${JSON.stringify(obj)}`);
  }
  if (obj.text !== "") {
    fail("step2", `expected text:'' on empty.html, got ${JSON.stringify(obj.text)}`);
  }
}

// --- Step 3: nonexistent path -> schema-valid failure envelope, fully
//     field-determined per section 3.4: ok:false, error set, exit != 0 (the
//     Bug E2 'blew up — catchable' guard). ---------------------------------
{
  const r = invoke(NONEXISTENT);
  if (r.code === 0) fail("step3", "expected non-zero exit on a nonexistent path");
  const obj = parseStdout("step3", r.stdout);
  validate("step3", validateFn, obj);
  if (obj.ok !== false || !obj.error) {
    fail("step3", `expected ok:false + error set, got ${JSON.stringify(obj)}`);
  }
  if (obj.text !== "" || obj.word_count !== null) {
    fail("step3", "failure envelope must be fully field-determined");
  }
  if (obj.contract_version !== 1 || obj.oracle !== "readability-js") {
    fail("step3", "failure envelope must set contract_version & oracle");
  }
}

// --- Step 4: section 3.3 primitive guard. Lone surrogate via a SOURCE-LEVEL
//     escape (not a UTF-8 fixture file); assert the post-primitive text is
//     schema-valid and contains NO code point in U+D800..U+DFFF. -----------
{
  const lone = "before\uD800after"; // lone high surrogate via source escape.
  if (!hasSurrogate(lone)) fail("step4", "test setup error: no lone surrogate");
  const cleaned = lone.toWellFormed(); // run.mjs's pinned primitive exactly.
  if (hasSurrogate(cleaned)) {
    fail("step4", "post-primitive text still contains a lone surrogate");
  }
  const probe = {
    contract_version: 1,
    oracle: "readability-js",
    oracle_version: "0.6.0",
    title: null,
    text: cleaned,
    html: null,
    word_count: cleaned.split(/\s+/u).length,
    canonical_url: null,
    language: null,
    ok: true,
    error: null,
  };
  validate("step4", validateFn, probe);
  JSON.parse(JSON.stringify(probe)); // round-trips without raising.
}

// --- Step 5: re-run step 1 and diff — byte-identical (same-machine
//     determinism, section 3.5). -------------------------------------------
{
  const a = invoke(ARTICLE);
  const b = invoke(ARTICLE);
  if (Buffer.compare(a.stdout, b.stdout) !== 0) {
    fail("step5", "two runs on article.html were NOT byte-identical");
  }
}

// --- Step 6: --base-url contract surface. --base-url is AUXILIARY
//     (canonical/relative-link resolution only), NOT essential to main-content
//     extraction; jsdom rejects an invalid `url` with `TypeError: Invalid
//     URL`. Without this guard a malformed --base-url would silently remove
//     THIS oracle from the differential for every malformed-URL page while the
//     Trafilatura oracle keeps participating (asymmetric instrument bias).
//     Assert all three contract cases: (a) VALID --base-url -> ok:true,
//     byte-deterministic; (b) ABSENT --base-url -> ok:true (the existing
//     no-base behaviour, unchanged); (c) STRUCTURALLY-INVALID --base-url ->
//     ok:true WITH substantive extracted text (graceful degrade, NOT a hard
//     failure) AND byte-deterministic. (b) and (c) must be byte-identical:
//     a malformed base must collapse to exactly the no-base path. -----------
{
  // (a) valid --base-url.
  const va = invoke(ARTICLE, "--base-url", "https://example.com/page");
  if (va.code !== 0) {
    fail("step6", `valid --base-url: expected exit 0, got ${va.code}`);
  }
  const vObj = parseStdout("step6", va.stdout);
  validate("step6", validateFn, vObj);
  if (vObj.ok !== true || vObj.error !== null) {
    fail(
      "step6",
      `valid --base-url: expected ok:true/error:null, got ${JSON.stringify(
        vObj,
      )}`,
    );
  }
  if (typeof vObj.text !== "string" || vObj.text.trim().length < 80) {
    fail("step6", "valid --base-url: expected substantive `text`");
  }
  const vb = invoke(ARTICLE, "--base-url", "https://example.com/page");
  if (Buffer.compare(va.stdout, vb.stdout) !== 0) {
    fail("step6", "valid --base-url: two runs were NOT byte-identical");
  }

  // (b) absent --base-url (unchanged no-base behaviour).
  const ab = invoke(ARTICLE);
  if (ab.code !== 0) {
    fail("step6", `absent --base-url: expected exit 0, got ${ab.code}`);
  }
  const abObj = parseStdout("step6", ab.stdout);
  validate("step6", validateFn, abObj);
  if (abObj.ok !== true || abObj.error !== null) {
    fail(
      "step6",
      `absent --base-url: expected ok:true/error:null, got ${JSON.stringify(
        abObj,
      )}`,
    );
  }

  // (c) structurally-invalid --base-url: MUST still extract (ok:true, text
  //     present), MUST NOT escalate to the ok:false/exit!=0 failure envelope,
  //     and MUST be byte-deterministic. Several malformed shapes.
  for (const bad of [
    "//x",
    "//proto-relative",
    "http://[invalid",
    "::::not a url::::",
    "http://a b c/x",
  ]) {
    const ia = invoke(ARTICLE, "--base-url", bad);
    if (ia.code !== 0) {
      fail(
        "step6",
        `invalid --base-url ${JSON.stringify(bad)}: expected exit 0 ` +
          `(graceful degrade), got ${ia.code} :: ${ia.stderr.slice(0, 200)}`,
      );
    }
    const iObj = parseStdout("step6", ia.stdout);
    validate("step6", validateFn, iObj);
    if (iObj.ok !== true || iObj.error !== null) {
      fail(
        "step6",
        `invalid --base-url ${JSON.stringify(bad)}: a malformed base is ` +
          `AUXILIARY and must degrade gracefully, not hard-fail; got ` +
          JSON.stringify(iObj),
      );
    }
    if (typeof iObj.text !== "string" || iObj.text.trim().length < 80) {
      fail(
        "step6",
        `invalid --base-url ${JSON.stringify(bad)}: expected substantive ` +
          "`text` (extraction must still occur)",
      );
    }
    const ib = invoke(ARTICLE, "--base-url", bad);
    if (Buffer.compare(ia.stdout, ib.stdout) !== 0) {
      fail(
        "step6",
        `invalid --base-url ${JSON.stringify(bad)}: two runs were NOT ` +
          "byte-identical",
      );
    }
    // A malformed base must collapse to EXACTLY the no-base path.
    if (Buffer.compare(ia.stdout, ab.stdout) !== 0) {
      fail(
        "step6",
        `invalid --base-url ${JSON.stringify(bad)}: output not ` +
          "byte-identical to the absent-base path (must be symmetric)",
      );
    }
  }
}

process.stderr.write(
  "[selftest.mjs] PASS — all 6 steps (schema-valid, tri-state, non-empty " +
    "text, section 3.3 primitive, byte-identical re-run, --base-url " +
    "valid/absent/invalid graceful-degrade).\n",
);
process.exit(0);
