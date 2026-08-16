// Tests for `.github/workflows/codex-review.yml`.
//
// The sweep it runs lives in mikelward/codex-review, tested there. What stays
// here is the boundary around it: which events may start a job holding
// `statuses: write`, what scope that job gets, and that nothing else in this
// tree can write the status it publishes. Those are decisions about *this*
// repository, so they are reviewed here.
//
// Every failure below is silent in the same way. Nothing errors when the
// trigger list gains a hole or the concurrency setting flips -- the gate just
// stops meaning what it claims, and the first sign is a PR that merged without
// a verdict, by which point the evidence is gone.
//
// Two rules hold this file together, and both were learned the same way --
// by review finding a YAML notation the checks did not see.
//
// 1. NO NEGATIVE ASSERTIONS. Absence is unbounded, so a `not.toMatch` only
//    rejects the spellings someone anticipated; `write-all`, a `.yaml`
//    filename, `statuses: "write"` and `"pull_request":` each sailed past one.
//    Compare whole sets and whole blocks instead.
// 2. THE WORKFLOW ITSELF IS PINNED AS TEXT. Rule 1 still leaves the extractors
//    -- trigger keys, permissions blocks, step counts -- as regexes over YAML,
//    and `"permissions":` and a bare sequence dash sailed past those. The
//    first test below compares the file's directive lines against an expected
//    list, so nothing about the file under test is extracted or approximated.
//
// Everything after that first test explains WHY each line is what it is. Those
// checks can be fooled; the pin cannot, so a mutation that slips past one
// still fails the other. Keep it that way.
import { describe, it, expect } from "./vitest-shim.mjs";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const WORKFLOWS = ".github/workflows";

/**
 * Both extensions GitHub accepts for a workflow.
 *
 * Filtering to `.yml` alone would let a `.yaml` file request
 * `statuses: write` while the sole-writer check below still passed -- the
 * invariant bypassed by a filename, with the test reporting green.
 */
const isWorkflow = (file) => /\.ya?ml$/.test(file);
const WORKFLOW = "codex-review.yml";

/**
 * A workflow's YAML with its comment lines removed.
 *
 * These files carry more prose than YAML, and every phrase this suite looks
 * for -- `statuses: write`, `workflow_dispatch` -- is also something the
 * comments have to be able to *discuss*. Matching the raw text conflates the
 * two in the direction that hurts: the sole-writer check below went red
 * because ci.yml explains, in a comment, which token this workflow holds.
 * Reading only the directives keeps the prose free to say anything.
 */
const directives = (file) =>
  readFileSync(join(WORKFLOWS, file), "utf8")
    .split("\n")
    .filter((line) => !/^\s*#/.test(line))
    .join("\n");

const yml = directives(WORKFLOW);

/**
 * The exact top-level `permissions:` block every *other* workflow may declare.
 *
 * Adding a workflow, or changing one's permissions, fails the sole-writer
 * check until its entry here is updated -- which is the intended friction:
 * this is the one invariant in the tree whose breach is silent.
 */
const ALLOWED_PERMISSIONS = {
  "ci.yml": ["permissions:", "  contents: read", "  pull-requests: read"],
  "release.yml": ["permissions:", "  contents: write"],
};

/**
 * Every `permissions:` block in a workflow, top-level or per-job, as trimmed
 * lines.
 *
 * Deliberately finds per-job blocks too: the sole-writer check requires there
 * to be exactly one, so a job-level grant cannot hide under a top-level block
 * that disclaims it. A block runs until a line indented no deeper than its
 * own key, so a one-line flow mapping (`permissions: {statuses: write}`)
 * comes back as itself and fails the comparison rather than parsing to
 * nothing.
 */
const permissionBlocks = (text) => {
  const lines = text.split("\n");
  const blocks = [];
  lines.forEach((line, i) => {
    const opener = /^(\s*)permissions:/.exec(line);
    if (!opener) return;
    const depth = opener[1].length;
    const block = [line.trimEnd()];
    for (let j = i + 1; j < lines.length; j += 1) {
      const indent = lines[j].search(/\S/);
      if (indent === -1 || indent <= depth) break;
      block.push(lines[j].trimEnd());
    }
    blocks.push(block);
  });
  return blocks;
};

/**
 * The top-level keys of the `on:` mapping, unquoted.
 *
 * Compared as a SET below rather than probed for forbidden names. `"pull_request":`
 * is valid YAML naming the same trigger, and a `not.toMatch(/pull_request:/)`
 * sails past it -- the fifth review finding of exactly that shape. A set
 * comparison has nothing to spell: anything added, removed or renamed fails,
 * in whatever notation.
 */
const triggerKeys = () => {
  const lines = yml.split("\n");
  const start = lines.findIndex((line) => /^on:/.test(line));
  const keys = [];
  let depth = null;
  for (let i = start + 1; i < lines.length; i += 1) {
    const indent = lines[i].search(/\S/);
    if (indent === -1) continue;
    if (indent === 0) break;
    if (depth === null) depth = indent;
    if (indent !== depth) continue;
    const key = /^\s*["']?([A-Za-z_][\w-]*)["']?\s*:/.exec(lines[i]);
    if (key) keys.push(key[1]);
  }
  return keys.sort();
};

/** The trigger block, for the `types:` completeness checks below. */
const triggers = yml.slice(yml.indexOf("\non:"), yml.indexOf("permissions:"));

describe("the codex-review workflow", () => {
  it("is exactly this, line for line", () => {
    // The gate the rest of this file only describes.
    //
    // Every check below extracts something -- the trigger keys, a permissions
    // block, the step count -- with a regex, and a regex over YAML is an
    // approximation of YAML. Six review rounds found six forms the
    // approximations did not see: `write-all`, a `.yaml` filename,
    // `statuses: "write"`, `"pull_request":`, `"permissions":`, and a sequence
    // dash alone on its line. Patching the seventh buys the eighth.
    //
    // So the security-critical file is pinned as text. There is nothing to
    // extract and nothing to approximate: any edit in any notation changes
    // these lines and fails here, and has to be re-approved by editing this
    // list. Comments and blank lines are excluded, so the prose above each
    // stanza stays free to change -- it is the directives that decide what
    // runs.
    //
    // The checks below are kept because they say WHY each line is what it is,
    // and their names are what a failure prints. They are documentation with
    // assertions attached; this is the one that cannot be fooled.
    const directiveLines = readFileSync(join(WORKFLOWS, WORKFLOW), "utf8")
      .split("\n")
      .filter((line) => !/^\s*#/.test(line) && line.trim() !== "")
      .map((line) => line.trimEnd());

    expect(directiveLines).toEqual([
      "name: codex-review",
      "on:",
      "  schedule:",
      "    - cron: '23 * * * *'",
      "  pull_request_target:",
      "    types: [opened, reopened, ready_for_review, synchronize, closed]",
      "  issue_comment:",
      "    types: [created, edited]",
      "  pull_request_review_comment:",
      "    types: [created, edited]",
      "permissions:",
      "  contents: read",
      "  pull-requests: read",
      "  checks: read",
      "  statuses: write",
      "concurrency:",
      "  group: codex-verdict",
      "  cancel-in-progress: false",
      "jobs:",
      "  sweep:",
      "    runs-on: ubuntu-latest",
      "    timeout-minutes: 65",
      "    steps:",
      "      - uses: mikelward/codex-review@main",
    ]);
  });

  it("starts on exactly these events and no others", () => {
    // The security-critical one. `workflow_dispatch` takes a ref and GitHub
    // runs the workflow file from it, so a branch could supply its own steps
    // and keep the write token; `pull_request` (without `_target`) has the
    // same hole, since the definition comes from the merge ref. Pinning the
    // action version does not help when the branch supplies the job around it.
    //
    // Asserted as the whole set rather than as forbidden names: a denylist has
    // to know every spelling in advance and YAML has more than one for
    // everything.
    expect(triggerKeys()).toEqual([
      "issue_comment",
      "pull_request_review_comment",
      "pull_request_target",
      "schedule",
    ]);
  });

  it("starts the loop on push events, so a fresh head is never left to the throttled schedule", () => {
    // `pull_request_target` takes the workflow definition from the base ref,
    // so unlike dispatch a PR cannot bring its own sweep -- and it is the only
    // event-driven start available, since reactions emit no webhook at all.
    // Without it the first push after a quiet spell waits 10-37 measured
    // minutes for a scheduled fire before minute-polling begins.
    expect(triggers).toMatch(/pull_request_target:/);
    expect(triggers).toMatch(/types:.*\bsynchronize\b/);
    expect(triggers).toMatch(/types:.*\bopened\b/);
    // `reopened` reuses an unchanged SHA, which may still carry an earlier
    // `codex: success`. Without the event nothing fail-closes it until the
    // hourly schedule, and the gate stays open on a stale verdict meanwhile.
    expect(triggers).toMatch(/types:.*\breopened\b/);
    expect(triggers).toMatch(/types:.*\bready_for_review\b/);
    // `closed` clears a shared-head failure the moment the duplicate PR goes
    // away -- a webhook-capable, merge-enabling transition that would
    // otherwise wait on the throttled schedule.
    expect(triggers).toMatch(/types:.*\bclosed\b/);
  });

  it("starts the loop on comment events, for the round that has no push", () => {
    // A rebuttal plus an "@codex review" nudge changes the verdict with no
    // pull_request event anywhere, and the reactions that follow emit
    // nothing -- these are what keep that round on the minute clock.
    expect(triggers).toMatch(/issue_comment:/);
    expect(triggers).toMatch(/pull_request_review_comment:/);
    // Both streams also fire on `edited`: a nudge edited into an existing
    // comment is dated by its edit, and without the event that re-read waits
    // on the throttled schedule.
    expect(triggers.match(/types: \[created, edited\]/g)).toHaveLength(2);
  });

  it("keeps the schedule shorter than the action's loop", () => {
    // The schedule only has to keep a run alive; the run is the clock. Hourly,
    // and off the hour to dodge the :00 stampede. Everything the schedule
    // alone catches fails closed and any comment clears it on demand.
    expect(yml).toMatch(/cron:\s*'23 \* \* \* \*'/);
  });

  it("bounds the job so a hung loop cannot hold the concurrency queue", () => {
    // Without a timeout a wedged API call keeps the runner for the 6-hour
    // default -- and the queued successor waits behind it, stalling the gate.
    // Above the action's own 55-minute loop, or a healthy run gets killed.
    const minutes = Number(yml.match(/timeout-minutes:\s*(\d+)/)?.[1]);
    expect(minutes > 55).toBe(true);
  });

  it("keeps the successor queued rather than canceling into a gap", () => {
    expect(yml).toMatch(/cancel-in-progress:\s*false/);
  });

  it("keeps the concurrency group named for the workflow it replaced", () => {
    // Deliberately not `codex-review`, and the mismatch with the file name is
    // what makes it look like an oversight worth tidying. A concurrency group
    // is a repo-wide namespace, so this one name is what serializes a still-
    // in-flight `codex-verdict` run against this file -- deleting a workflow
    // does not cancel a run of it, and a run lasts up to 65 minutes. Rename it
    // and the two poll concurrently: two unordered writers of one status,
    // where a stale `success` landing after a newer `failure` opens the gate
    // on findings nobody answered. Renaming later reopens the same window, so
    // it never gets renamed.
    expect(yml).toMatch(/group:\s*codex-verdict\b/);
  });

  it("holds exactly the scope it needs and no more", () => {
    // Exact, and exactly one block: a per-job grant must not be able to add to
    // what the top-level one declares, and widening any line here has to be a
    // deliberate edit to this list rather than something a reader skims past.
    expect(permissionBlocks(yml)).toEqual([[
      "permissions:",
      "  contents: read",
      "  pull-requests: read",
      "  checks: read",
      "  statuses: write",
    ]]);
  });

  it("runs the shared action, as its only step", () => {
    // One step, and it is the action. Counted rather than asserted absent: a
    // checkout would put this repo's code in the same job as the write token
    // for no reason, and "no checkout" is a denylist of one where "one step"
    // is the property actually wanted.
    const steps = yml.slice(yml.indexOf("steps:")).match(/^\s+-\s\S/gm) ?? [];
    expect(steps).toHaveLength(1);
    expect(yml).toMatch(/uses:\s*mikelward\/codex-review@main\b/);
  });

  it("is the only workflow that can write commit statuses", () => {
    // The sweep's correctness leans on being the sole writer: a second writer
    // is an unordered write, and one delayed past the loop's exit overwrites a
    // just-published success with nothing left to notice. A regression here is
    // silent, so pin the absence.
    //
    // Pinned by ALLOWLIST rather than by forbidden spellings, and that is the
    // whole design. Four rounds of review each found another way to write the
    // same grant -- `write-all`, a `.yaml` filename, `statuses: "write"`,
    // `"statuses": write` -- because YAML has unboundedly many notations for
    // one mapping, and a denylist has to know them all while an attacker or an
    // honest refactor needs only one. Comparing each other workflow's
    // permissions against the exact block it is known to need inverts that: any
    // change, in any notation, fails here and has to be re-approved.
    const all = readdirSync(WORKFLOWS).filter(isWorkflow);
    const others = all.filter((f) => f !== WORKFLOW);
    // Two guards on the guard. An empty listing would make the loop below
    // vacuous -- this repo has ci.yml and release.yml -- and a `WORKFLOW` that
    // no longer names a real file would quietly move the workflow under test
    // into `others`, where it fails on its own `statuses: write`.
    expect(others.length > 0).toBe(true);
    expect(all.includes(WORKFLOW)).toBe(true);

    for (const file of others) {
      const allowed = ALLOWED_PERMISSIONS[file];
      // A workflow nobody has vetted is not a workflow that grants nothing.
      expect(
        Boolean(allowed),
        `${file} has no entry in ALLOWED_PERMISSIONS — add one deliberately, ` +
          "after checking it cannot write commit statuses",
      ).toBe(true);

      const blocks = permissionBlocks(directives(file));
      // Exactly one, so a per-job block cannot grant what the top-level one
      // disclaims -- and so that declaring none, which inherits the repository
      // default this tree cannot read, is rejected too.
      expect(
        blocks.length,
        `${file} must declare exactly one permissions block, at the top level`,
      ).toBe(1);
      expect(blocks[0], `${file}'s permissions changed — re-approve it`).toEqual(allowed);
    }
  });
});
