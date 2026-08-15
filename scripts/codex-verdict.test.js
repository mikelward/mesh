import { describe, it, expect } from "./vitest-shim.mjs";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import {
  verdictFor,
  runLoop,
  PENDING,
  FINDINGS,
  matchesBot,
  readReactions,
  findingsOn,
  commentSignals,
  sweep,
  sharedHeads,
  CODEX_BOT,
  CONTEXT,
  UNREADABLE,
  MAX_FAIL_STREAK,
  utc,
} from "./codex-verdict.mjs";

describe("verdictFor", () => {
  it("is pending until Codex has reacted", () => {
    // The case auto-merge would otherwise race: CI green, Codex silent.
    expect(verdictFor({ approved: false })).toEqual({
      state: "pending",
      description: PENDING,
    });
  });

  it("is success once it has", () => {
    expect(verdictFor({ approved: true }).state).toBe("success");
  });

  it("refuses a head shared with another open PR, reaction or not", () => {
    // A status belongs to the commit and the reaction to the PR, so one
    // status cannot describe both.
    expect(verdictFor({ approved: true, sharedHead: true })).toEqual({
      state: "failure",
      description: "Head shared with another open PR — verdict is ambiguous",
    });
  });

  it("holds when someone has reacted 👎, even with the verdict in", () => {
    // The cheap way to stop an auto-merge from a phone.
    expect(verdictFor({ approved: true, held: "👎" })).toEqual({
      state: "failure",
      description: "On hold: 👎 on the pull request",
    });
  });

  it("holds on 👀 too", () => {
    expect(verdictFor({ approved: true, held: "👀" }).state).toBe("failure");
  });

  it("stays pending while Codex is reading, even over a 👍", () => {
    // The state that must NOT go `failure`: pending is what the lean gate
    // counts, so this is what keeps the minute loop polling through a
    // review. Codex swaps its 👀 for 👍 with no webhook — a loop already
    // idle would leave that 👍 to the throttled schedule, 10–37 minutes.
    expect(verdictFor({ approved: false, reading: true })).toEqual({
      state: "pending",
      description: PENDING,
    });
    expect(verdictFor({ approved: true, reading: true }).state).toBe("pending");
  });

  it("marks findings as pending for the gate but settled for the clock", () => {
    // Same closed gate, different description: FINDINGS is what the sweep's
    // lean gate reads as "waiting on a push, not on the next minute".
    expect(verdictFor({ findings: true })).toEqual({
      state: "pending",
      description: FINDINGS,
    });
  });

  it("lets a fresh 👍 outrank findings still naming this head", () => {
    // After a fix-and-nudge round the old review still names the head; the
    // fresh reaction is Codex saying it is now satisfied.
    expect(verdictFor({ approved: true, findings: true }).state).toBe("success");
  });

  it("lets reading outrank findings — a re-read is the verdict changing", () => {
    expect(verdictFor({ reading: true, findings: true })).toEqual({
      state: "pending",
      description: PENDING,
    });
  });

  it("reopens the wait when the owner nudges over a standing 👍", () => {
    // Asking Codex to look again means the standing verdict is no longer
    // the one wanted — honoring it would let auto-merge take a PR the owner
    // just asked to have re-reviewed. Codex's fresh answer (a 👍 newer than
    // the nudge) restores success through `approved` on a later sweep.
    expect(verdictFor({ approved: true, nudged: true })).toEqual({
      state: "pending",
      description: PENDING,
    });
  });

  it("skips drafts", () => {
    expect(verdictFor({ isDraft: true, approved: true })).toBeNull();
  });
});

describe("readReactions", () => {
  const HEAD = "2026-08-14T12:00:00Z";
  const CODEX = `${CODEX_BOT}[bot]`;
  // Reactions default to after the head, so each test states only the thing
  // it is about; the staleness cases pass their own timestamp.
  const react = (login, content, createdAt = "2026-08-14T12:05:00Z") =>
    ({ content, createdAt, user: { login } });
  const OWNER = "repo-owner";
  const read = (nodes) => readReactions(nodes, { since: HEAD, owner: OWNER });

  it("ignores a 👍 older than the commit it would approve", () => {
    // Codex revokes on push, but asynchronously — so between a push and its
    // noticing, the PR carries a new head and the previous head's reaction.
    // Reading those together approves a commit nobody reviewed.
    const stale = readReactions([react(CODEX, "THUMBS_UP", "2026-08-14T18:00:00Z")], { since: "2026-08-14T19:00:00Z" });
    expect(stale.approved).toBe(false);
  });

  it("takes a 👍 newer than the commit", () => {
    const fresh = readReactions([react(CODEX, "THUMBS_UP", "2026-08-14T19:30:00Z")], { since: "2026-08-14T19:00:00Z" });
    expect(fresh.approved).toBe(true);
  });

  it("holds on the owner's 👎 from before the head — a new commit is not an answer", () => {
    expect(
      readReactions([react(OWNER, "THUMBS_DOWN", "2026-08-14T18:00:00Z")], {
        since: "2026-08-14T19:00:00Z", owner: OWNER,
      }).held,
    ).toBe("👎");
  });

  it("ignores a hold from anyone but the owner", () => {
    // Public repo: an unrestricted hold outlives every head, so a passer-by
    // could block a PR for as long as they left the reaction there.
    expect(read([react("passer-by", "THUMBS_DOWN")]).held).toBeNull();
    expect(read([react("passer-by", "EYES")]).held).toBeNull();
  });

  it("takes Codex's 👍 as the verdict", () => {
    expect(read([react(CODEX, "THUMBS_UP")])).toEqual({
      approved: true,
      approvedAt: "2026-08-14T12:05:00Z",
      staleApproval: false,
      held: null,
      reading: false,
    });
  });

  it("ignores anyone else's 👍", () => {
    // The reaction is only a verdict because Codex revokes it on push.
    expect(read([react("someone-else", "THUMBS_UP")]).approved).toBe(false);
  });

  it("takes the owner's 👎 as a hold", () => {
    expect(read([react(OWNER, "THUMBS_DOWN")]).held).toBe("👎");
  });

  it("reports Codex's own 👀 as reading, not as a hold", () => {
    // Reading blocks approval like a hold does, but it maps to `pending`,
    // not `failure` — pending is what the lean gate counts, so this is what
    // keeps the minute loop running while the review is in flight. Codex
    // clears its 👀 when it reacts 👍, so a 👀 still present means the
    // answer is still being written.
    expect(read([react(CODEX, "EYES")])).toEqual({
      approved: false,
      approvedAt: null,
      staleApproval: false,
      held: null,
      reading: true,
    });
  });

  it("keeps reading a 👍 that has picked up a 👀 since — a re-read in progress", () => {
    expect(
      read([react(CODEX, "THUMBS_UP"), react(CODEX, "EYES")]),
    ).toEqual({
      approved: true, approvedAt: "2026-08-14T12:05:00Z", staleApproval: false, held: null, reading: true,
    });
  });

  it("prefers 👎 over 👀 when both are present", () => {
    expect(
      read([react(OWNER, "EYES"), react(OWNER, "THUMBS_DOWN")]).held,
    ).toBe("👎");
  });

  it("ignores a reaction it has no rule for", () => {
    expect(read([react(OWNER, "HOORAY")])).toEqual({
      approved: false,
      approvedAt: null,
      staleApproval: false,
      held: null,
      reading: false,
    });
  });
});

describe("findingsOn", () => {
  const at = (commit_id, login, submitted_at = "2026-08-15T08:00:00Z") =>
    [{ user: { login }, commit_id, submitted_at }];

  it("returns the time of Codex's review of exactly this head", () => {
    expect(findingsOn(at("abc", `${CODEX_BOT}[bot]`), "abc")).toBe("2026-08-15T08:00:00Z");
  });

  it("returns the newest when Codex has reviewed the head more than once", () => {
    // The timestamp is what a nudge is compared against, so it must be
    // Codex's LAST word, not its first.
    expect(findingsOn([
      ...at("abc", `${CODEX_BOT}[bot]`, "2026-08-15T08:00:00Z"),
      ...at("abc", `${CODEX_BOT}[bot]`, "2026-08-15T09:00:00Z"),
    ], "abc")).toBe("2026-08-15T09:00:00Z");
  });

  it("ignores a review of a previous head — the commit id is the staleness guard", () => {
    expect(findingsOn(at("old", `${CODEX_BOT}[bot]`), "abc")).toBeNull();
  });

  it("ignores anyone else's review", () => {
    // Owner replies to threads arrive as reviews too; they are not findings.
    expect(findingsOn(at("abc", "repo-owner"), "abc")).toBeNull();
  });

  it("treats missing review data as no findings", () => {
    expect(findingsOn(undefined, "abc")).toBeNull();
    expect(findingsOn([{ user: null, commit_id: null }], "abc")).toBeNull();
  });
});

describe("commentSignals", () => {
  const MARK = "2026-08-15T07:00:00Z";
  const OWNER = "repo-owner";
  const c = (login, created_at, body = "") => ({ user: { login }, created_at, body });
  const read = (batch) => commentSignals(batch, { since: MARK, owner: OWNER });

  it("reports a Codex comment newer than the head's own commit", () => {
    expect(read([c(`${CODEX_BOT}[bot]`, "2026-08-15T08:00:00Z")]).codexAt).toBe("2026-08-15T08:00:00Z");
  });

  it("ignores a comment from before this head existed — it belongs to a previous head", () => {
    // REST's `since` filters on update time, so an edited old comment can
    // arrive in the batch; the created_at guard is what keeps it stale.
    expect(read([c(`${CODEX_BOT}[bot]`, "2026-08-15T06:00:00Z")]).codexAt).toBeNull();
  });

  it("reports the owner's newest @codex review nudge", () => {
    const { nudgeAt } = read([
      c(OWNER, "2026-08-15T08:00:00Z", "@codex review"),
      c(OWNER, "2026-08-15T09:00:00Z", "please @codex review again"),
    ]);
    expect(nudgeAt).toBe("2026-08-15T09:00:00Z");
  });

  it("ignores a nudge-shaped comment from anyone but the owner", () => {
    // Public repo: a passer-by's nudge must not hold the loop open, for
    // the same reason their reactions cannot hold the gate.
    expect(read([c("passer-by", "2026-08-15T08:00:00Z", "@codex review")]).nudgeAt).toBeNull();
  });

  it("does not read an ordinary owner comment as a nudge", () => {
    expect(read([c(OWNER, "2026-08-15T08:00:00Z", "looks good")]).nudgeAt).toBeNull();
  });

  it("reports nothing when the head's commit date is missing", () => {
    // Defensive: with no bound nothing can be tied to this head, and
    // nothing is the safe direction — the loop keeps polling for minutes,
    // where a wrong "findings" strands the verdict on the throttled
    // schedule.
    expect(commentSignals([c(`${CODEX_BOT}[bot]`, "2026-08-15T08:00:00Z")], { since: undefined, owner: OWNER }))
      .toEqual({ codexAt: null, nudgeAt: null });
  });
});

describe("utc", () => {
  it("passes a UTC string through byte-identical", () => {
    // Reformatting would add millisecond precision and quietly change what
    // an identical-status comparison sees.
    expect(utc("2026-08-15T10:00:00Z")).toBe("2026-08-15T10:00:00Z");
  });

  it("converts an offset timestamp to UTC", () => {
    expect(utc("2026-08-15T05:00:00-07:00")).toBe("2026-08-15T12:00:00.000Z");
  });

  it("returns null for nothing and for garbage", () => {
    expect(utc(null)).toBeNull();
    expect(utc(undefined)).toBeNull();
    expect(utc("not a time")).toBeNull();
  });

  it("keeps a fresh 👍 fresh when its timestamp carries an offset", () => {
    // The comparison that goes wrong without normalization: 05:05-07:00 IS
    // 12:05Z — after the head — but as a raw string it sorts before
    // "...T12:00:00Z" and would read as stale, sticking the gate at
    // pending. Today GitHub sends UTC everywhere this file reads; this
    // pins the behavior if a future field swap brings offsets in.
    const seen = readReactions(
      [{ content: "THUMBS_UP", createdAt: "2026-08-14T05:05:00-07:00", user: { login: `${CODEX_BOT}[bot]` } }],
      { since: "2026-08-14T12:00:00Z" },
    );
    expect(seen.approved).toBe(true);
    expect(seen.approvedAt).toBe("2026-08-14T12:05:00.000Z");
  });
});

describe("matchesBot", () => {
  it("matches with or without the [bot] suffix", () => {
    expect(matchesBot(`${CODEX_BOT}[bot]`)).toBe(true);
    expect(matchesBot(CODEX_BOT)).toBe(true);
  });

  it("rejects anyone else, and a missing login", () => {
    // The reaction is only a verdict because Codex revokes it on push;
    // a human's 👍 carries no such guarantee.
    expect(matchesBot("someone-else")).toBe(false);
    expect(matchesBot(undefined)).toBe(false);
  });
});

describe("sharedHeads", () => {
  it("names only the SHAs carried by more than one PR", () => {
    expect([...sharedHeads([{ headRefOid: "a" }, { headRefOid: "a" }, { headRefOid: "b" }])]).toEqual(["a"]);
  });

  it("is empty when every head is distinct", () => {
    expect(sharedHeads([{ headRefOid: "a" }, { headRefOid: "b" }]).size).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Sweep, driven by a fake fetch. Every request it makes is recorded, so these
// assert what it asked for as well as what it concluded.

const page = (nodes, hasNextPage = false, endCursor = null) => ({
  nodes,
  pageInfo: { hasNextPage, endCursor },
});

const GATED_AT = "2026-08-14T12:00:00Z";
const AFTER = "2026-08-14T12:05:00Z";
const BEFORE = "2026-08-14T11:00:00Z";
// When the head commit was made — the comment walk's lower bound, always
// earlier than the marker but later than anything about a previous head.
const BORN_AT = "2026-08-14T11:30:00Z";
const OWNER = "o";

// The marker: the earliest `codex` status on the head, saying when it was
// first gated. A reaction has to be newer than this to count as approval.
const gate = (created_at = GATED_AT, state = "pending") =>
  ({ context: CONTEXT, state, description: PENDING, created_at });

// The suite born on the PR's own branch — what dates a same-repo head's
// arrival. Approval fixtures need one: a head with no branch-born suite is
// undatable and fails closed by design.
const bornSuite = (created_at = "2026-08-14T11:59:00Z", head_branch = "claude/topic") =>
  ({ created_at, head_branch });

const prNode = (over = {}) => ({
  number: 1,
  isDraft: false,
  headRefOid: "abc1234",
  headRefName: "claude/topic",
  isCrossRepository: false,
  updatedAt: GATED_AT,
  commits: { nodes: [{ commit: { committedDate: BORN_AT } }] },
  reactions: page([]),
  ...over,
});

const review = (commit_id = "abc1234", login = `${CODEX_BOT}[bot]`, submitted_at = AFTER) => ({
  user: { login },
  commit_id,
  submitted_at,
});

const thumbs = (login = `${CODEX_BOT}[bot]`, createdAt = AFTER) => ({
  content: "THUMBS_UP",
  createdAt,
  user: { login },
});
const hold = (content = "THUMBS_DOWN", login = OWNER) => ({
  content,
  createdAt: AFTER,
  user: { login },
});

// `statuses` maps a SHA to its `codex` entries, newest first, as GitHub
// returns them. A GET on the statuses path reads them; a POST writes one.
// `issueComments` maps a PR number to REST-shaped comments; the fake honors
// paging but not `since` — the code under test re-filters by created_at.
function fakeFetch({
  graphqlResponses = [], statuses = {}, issueComments = {}, prReviews = {},
  reviewThreadComments = {}, checkSuites = {}, failStatusWrite = false,
  failComments = false,
} = {}) {
  const calls = [];
  const queue = [...graphqlResponses];
  const impl = async (url, opts = {}) => {
    const path = url.replace("https://api.github.com", "");
    const method = opts.method ?? "GET";
    calls.push({ path, method, body: opts.body ? JSON.parse(opts.body) : null });

    if (path === "/graphql") {
      const data = queue.shift();
      if (!data) throw new Error("unexpected extra graphql call");
      return { ok: true, status: 200, json: async () => (data.errors ? data : { data }) };
    }
    if (path.includes("/check-suites")) {
      const sha = path.split("/commits/")[1].split("/")[0];
      const page = Number(new URLSearchParams(path.split("?")[1] ?? "").get("page") ?? 1);
      const all = checkSuites[sha] ?? [];
      return {
        ok: true,
        status: 200,
        json: async () => ({ check_suites: all.slice((page - 1) * 100, page * 100) }),
      };
    }
    if (path.includes("/comments")) {
      if (failComments) return { ok: false, status: 500, json: async () => ({}) };
      const kind = path.includes("/issues/") ? "/issues/" : "/pulls/";
      const number = path.split(kind)[1].split("/")[0];
      const page = Number(new URLSearchParams(path.split("?")[1] ?? "").get("page") ?? 1);
      const all = (kind === "/issues/" ? issueComments : reviewThreadComments)[number] ?? [];
      return { ok: true, status: 200, json: async () => all.slice((page - 1) * 100, page * 100) };
    }
    if (path.includes("/reviews")) {
      const number = path.split("/pulls/")[1].split("/")[0];
      const page = Number(new URLSearchParams(path.split("?")[1] ?? "").get("page") ?? 1);
      const all = prReviews[number] ?? [];
      return { ok: true, status: 200, json: async () => all.slice((page - 1) * 100, page * 100) };
    }
    if (path.includes("/statuses/")) {
      if (method === "GET") {
        const sha = path.split("/statuses/")[1].split("?")[0];
        const page = Number(new URLSearchParams(path.split("?")[1] ?? "").get("page") ?? 1);
        const all = statuses[sha] ?? [];
        return { ok: true, status: 200, json: async () => all.slice((page - 1) * 100, page * 100) };
      }
      if (failStatusWrite) return { ok: false, status: 422, json: async () => ({}) };
      return { ok: true, status: 201, json: async () => ({}) };
    }
    throw new Error(`unexpected path ${path}`);
  };
  return { impl, calls };
}

const repoPRs = (nodes, hasNextPage = false, endCursor = null) => ({
  repository: { pullRequests: page(nodes, hasNextPage, endCursor) },
});

// Most tests only care about what was written; `active` has its own cases.
// A fixed clock near the fixtures' own times, so the unanswered-head decay
// judges ages the way a live sweep would minutes after the events — not
// against the wall clock, which is years past every fixture.
const TEST_NOW = () => Date.parse("2026-08-14T12:06:00Z");
const runFull = (fake) => sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: () => {}, now: TEST_NOW });
const run = async (fake) => (await runFull(fake)).written;

describe("sweep", () => {
  it("publishes pending for a PR Codex has not reacted to", async () => {
    // No `codex` status yet: this sweep is what first gates the head, and
    // writing that marker is what a later sweep judges a reaction against.
    const fake = fakeFetch({ graphqlResponses: [repoPRs([prNode()])] });
    expect(await run(fake)).toEqual([
      { number: 1, state: "pending", description: PENDING },
    ]);
    const write = fake.calls.find((c) => c.method === "POST" && c.path.includes("/statuses/"));
    expect(write.body).toMatchObject({ context: CONTEXT, state: "pending" });
    expect(write.path).toContain("abc1234");
  });

  it("publishes success once Codex has reacted", async () => {
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      checkSuites: { abc1234: [bornSuite()] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs()]) })])],
    });
    expect((await run(fake))[0].state).toBe("success");
  });

  it("ignores a thumbs-up from anyone else", async () => {
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs("someone-else")]) })])],
    });
    expect((await run(fake))[0].state).toBe("pending");
  });

  it("does not rewrite an identical status", async () => {
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([prNode()])],
      statuses: { abc1234: [gate()] },
    });
    expect(await run(fake)).toEqual([]);
    // The GET shares this path, so only a POST counts as a rewrite.
    expect(fake.calls.some((c) => c.method === "POST" && c.path.includes("/statuses/"))).toBe(false);
  });

  it("rewrites when the state has moved on", async () => {
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs()]) })])],
      statuses: { abc1234: [{ ...gate(), description: "old" }] },
      checkSuites: { abc1234: [bornSuite()] },
    });
    expect((await run(fake))[0].state).toBe("success");
  });

  it("skips drafts without touching the status API", async () => {
    const fake = fakeFetch({ statuses: { abc1234: [gate()] }, graphqlResponses: [repoPRs([prNode({ isDraft: true })])] });
    expect(await run(fake)).toEqual([]);
    expect(fake.calls.filter((c) => c.path !== "/graphql")).toEqual([]);
  });

  it("follows reactor pages before declaring pending", async () => {
    // Codex's reaction past the first page would otherwise stick the status
    // on pending forever, refetching the same truncated page each sweep.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      checkSuites: { abc1234: [bornSuite()] },
      graphqlResponses: [
        repoPRs([prNode({ reactions: page([thumbs("someone-else")], true, "cursor9") })]),
        { repository: { pullRequest: { reactions: page([thumbs()]) } } },
      ],
    });
    expect((await run(fake))[0].state).toBe("success");
  });

  it("stays pending on a 👍 that predates the head being gated", async () => {
    // The push-vs-revoke race, end to end: new head, previous head's 👍.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, BEFORE)]) })])],
    });
    // The head is already marked pending, so a correct sweep writes nothing;
    // what would show here is the 👍 flipping it to success.
    expect(await run(fake)).toEqual([]);
    expect(fake.calls.some((c) => c.method === "POST" && c.path.includes("/statuses/"))).toBe(false);
  });

  it("stays pending on a head that has never been gated", async () => {
    // No marker means nothing to date the reaction against, and an
    // unexpected result must not be readable as approval.
    const fake = fakeFetch({
      graphqlResponses: [
        repoPRs([prNode({ reactions: page([thumbs()]) })]),
      ],
    });
    expect((await run(fake))[0].state).toBe("pending");
  });

  it("pages past a head's other statuses to find the gate marker", async () => {
    // The endpoint returns every context, so a full page of Vercel statuses
    // can hide the marker. Losing it is not a harmless truncation: the sweep
    // would write a replacement, and the existing 👍 would be older than it
    // and rejected by every later sweep.
    const filler = Array.from({ length: 100 }, () => ({ context: "vercel", state: "success", created_at: AFTER }));
    const fake = fakeFetch({
      statuses: { abc1234: [...filler, gate()] },
      checkSuites: { abc1234: [bornSuite()] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs()]) })])],
    });
    expect((await run(fake))[0].state).toBe("success");
  });

  it("withholds approval while someone is holding the PR", async () => {
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(), hold()]) })])],
    });
    expect((await run(fake))[0]).toEqual({
      number: 1,
      state: "failure",
      description: "On hold: 👎 on the pull request",
    });
  });

  it("finds a hold on a later reaction page rather than approving over it", async () => {
    // Why the page walk has no short-circuit on the thumbs-up: stopping at
    // the verdict would merge past a hold nobody got to see.
    const fake = fakeFetch({
      graphqlResponses: [
        repoPRs([prNode({ reactions: page([thumbs()], true, "cursor9") })]),
        { repository: { pullRequest: { reactions: page([hold()]) } } },
      ],
    });
    expect((await run(fake))[0].state).toBe("failure");
  });

  it("counts only pending verdicts toward the loop's lean gate", async () => {
    // One PR waiting on Codex, one already approved, one draft: only the
    // first should keep the fast clock running.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()], bbb: [gate()] },
      checkSuites: { bbb: [bornSuite()] },
      graphqlResponses: [repoPRs([
        prNode(),
        prNode({ number: 2, headRefOid: "bbb", reactions: page([thumbs()]) }),
        prNode({ number: 3, isDraft: true, headRefOid: "d1" }),
      ])],
    });
    expect((await runFull(fake)).awaiting).toBe(1);
  });

  it("keeps the loop running while Codex is reading", async () => {
    // Codex's 👀 is the review in flight. It must land as `pending` and
    // count toward `awaiting`: the swap from 👀 to 👍 emits no webhook, so a
    // loop that treated reading as a hold would go idle at the exact moment
    // the next minute could change the answer, leaving the 👍 to the
    // throttled schedule.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      graphqlResponses: [repoPRs([
        prNode({ reactions: page([hold("EYES", `${CODEX_BOT}[bot]`)]) }),
      ])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    // Unchanged from the marker's pending, so nothing is rewritten either.
    expect(out.written).toEqual([]);
  });

  it("stops the clock once Codex has returned findings for the head", async () => {
    // A review naming the current head with no fresh 👍 means the next
    // change is a push — which restarts the loop by event — so polling for
    // it would only burn the runner. The status flips to the FINDINGS
    // wording so the check line says what is actually being waited for.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [review()] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(0);
    expect(out.written).toEqual([
      { number: 1, state: "pending", description: FINDINGS },
    ]);
  });

  it("closes the gate on a top-level comment but keeps the clock running", async () => {
    // The third finding stream: no submitted review, just a PR comment —
    // tied to the head by being newer than the head commit's own date,
    // since comments carry no commit id. That missing commit id is also why
    // the clock keeps running: Codex finishing the PREVIOUS head after a
    // push posts exactly this shape, and the clean 👍 it may still give the
    // current head emits no webhook — settling the loop here would leave
    // that 👍 to the throttled schedule. Only a review matched to this head
    // stops the clock.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      issueComments: { 1: [{ user: { login: `${CODEX_BOT}[bot]` }, created_at: AFTER }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].description).toBe(FINDINGS);
  });

  it("hears a comment finding that landed before the head was first gated", async () => {
    // The no-marker ordering: Codex's comment arrives before any `codex`
    // status exists (a delayed first sweep), and the sweep's own first
    // write would then be a marker NEWER than the finding. Bounding the
    // comment walk by that marker filtered the finding out of every later
    // sweep, forever — an always-on loop with nothing left that could end
    // it. The bound is the head commit's own date, which precedes anything
    // said about the head by construction.
    const fake = fakeFetch({
      issueComments: { 1: [{ user: { login: `${CODEX_BOT}[bot]` }, created_at: AFTER }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].description).toBe(FINDINGS);
  });

  it("ignores a comment about a previous head — older than this head's commit", async () => {
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      issueComments: { 1: [{ user: { login: `${CODEX_BOT}[bot]` }, created_at: BEFORE }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written).toEqual([]);
  });

  it("pages the comment stream to the end rather than trusting any window", async () => {
    // The eviction hazard: a finding buried behind ANY number of pages of
    // later chatter must still be found, or the loop revives for good — a
    // fixed page cap is the same hole one order of magnitude later, so the
    // finding here sits past what a ten-page cap would have read.
    const filler = Array.from({ length: 1000 }, () => ({ user: { login: "someone-else" }, created_at: AFTER }));
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      issueComments: { 1: [...filler, { user: { login: `${CODEX_BOT}[bot]` }, created_at: AFTER }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].description).toBe(FINDINGS);
  });

  it("pages the review stream to the end too", async () => {
    // Same eviction logic as comments: a Codex review of the current head
    // behind a full page of reply-reviews still stops the clock.
    const filler = Array.from({ length: 100 }, () => review("oldhead", "repo-owner"));
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [...filler, review()] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(0);
    expect(out.written[0].description).toBe(FINDINGS);
  });

  it("asks for no comments on a PR a hold already settles", async () => {
    // The streams are fetched only when they could change the answer. A
    // hold outranks everything a comment could say, so it costs no extra
    // call — but an APPROVED head still gets the walk: a newer owner nudge
    // must be able to reopen the wait (see below).
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([prNode({ reactions: page([hold()]) })])],
    });
    await runFull(fake);
    expect(fake.calls.some((c) => c.path.includes("/comments"))).toBe(false);
  });

  it("reopens the wait when the owner nudges over a standing 👍", async () => {
    // A re-review request on an approved head: honoring the old 👍 while
    // the answer is due again would let auto-merge take a PR the owner just
    // asked to have re-read. The nudge is newer than the 👍 — Codex's last
    // word on a clean pass — so the gate closes and the head counts as
    // awaiting until the fresh answer lands.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      issueComments: { 1: [{ user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "@codex review" }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs()]) })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    // The marker already says PENDING, so nothing is rewritten either.
    expect(out.written).toEqual([]);
  });

  it("restores success once a fresh 👍 postdates the nudge", async () => {
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      checkSuites: { abc1234: [bornSuite()] },
      issueComments: { 1: [{ user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "@codex review" }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, "2026-08-14T12:15:00Z")]) })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(0);
    expect(out.written[0].state).toBe("success");
  });

  it("honors a 👍 that arrived before the sweep's own first status", async () => {
    // The delayed-first-sweep ordering: Codex approves before any `codex`
    // status exists, then the first write would date the head AFTER the 👍
    // and stale it forever. The freshness bound is the earliest status of
    // ANY context — a third party's (deploy, CI) lands seconds after the
    // push, before any genuine reaction can — so the 👍 stays fresh.
    const fake = fakeFetch({
      statuses: { abc1234: [
        gate("2026-08-14T12:00:00Z"),
        { context: "Vercel", state: "success", created_at: "2026-08-14T11:58:00Z" },
      ] },
      checkSuites: { abc1234: [{ created_at: "2026-08-14T11:58:30Z", head_branch: "claude/topic" }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, "2026-08-14T11:59:00Z")]) })])],
    });
    const out = await runFull(fake);
    expect(out.written[0].state).toBe("success");
    // A fresh-looking 👍 pays one suites call to confirm the head did not
    // arrive by fast-forward onto a pre-stamped commit — the branch-born
    // suite predates the 👍 here, so the approval holds.
    expect(fake.calls.some((c) => c.path.includes("/check-suites"))).toBe(true);
  });

  it("fails closed when a same-repo head has no suites at all yet", async () => {
    // A fast-forwarded head can carry old STATUSES and no suites — judged
    // in the gap before this workflow's own run creates the branch suite.
    // Old records being statuses rather than suites changes nothing: with
    // no branch-born suite the transition cannot be dated, and trusting
    // the status bound is the same fail-open hole.
    const fake = fakeFetch({
      statuses: { abc1234: [{ context: "Vercel", state: "success", created_at: "2026-08-10T09:00:00Z" }] },
      graphqlResponses: [repoPRs([prNode({
        reactions: page([thumbs(undefined, "2026-08-10T09:05:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].state).toBe("pending");
  });

  it("hears a nudge edited into an old comment", async () => {
    // An owner can ask for a re-review by editing "@codex review" into an
    // existing comment; its created_at then predates the head or the
    // standing 👍 and the ask would be filtered out or read as answered.
    // The ask is dated by the later of creation and edit — erring toward
    // blocking on an owner's ask, the file's stated direction.
    const fake = fakeFetch({
      statuses: { abc1234: [
        gate("2026-08-14T12:00:00Z"),
        { context: "Vercel", state: "success", created_at: "2026-08-14T11:58:00Z" },
      ] },
      checkSuites: { abc1234: [{ created_at: "2026-08-14T11:58:30Z", head_branch: "claude/topic" }] },
      issueComments: { 1: [{
        user: { login: OWNER },
        created_at: "2026-08-13T09:00:00Z",
        updated_at: "2026-08-14T12:30:00Z",
        body: "@codex review",
      }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, "2026-08-14T12:15:00Z")]) })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    // The head already carries `pending`; the point is that the edited-in
    // ask keeps the 👍 from flipping it to success.
    expect(out.written).toEqual([]);
  });

  it("ignores a previous life's records on a recycled head", async () => {
    // A force-push can move the PR onto a commit that already existed —
    // whose statuses, suites, and even a 👍 date from its first life. The
    // birth bound is floored at the force-push event (server-stamped), so
    // nothing from before the commit became THIS PR's head can approve it.
    const era = "2026-08-10T09:00:00Z";
    const fake = fakeFetch({
      statuses: { abc1234: [{ context: "Vercel", state: "success", created_at: era }] },
      checkSuites: { abc1234: [{ created_at: era, head_branch: "claude/topic" }] },
      graphqlResponses: [repoPRs([prNode({
        timelineItems: { nodes: [{ createdAt: "2026-08-14T12:00:00Z" }] },
        reactions: page([thumbs(undefined, "2026-08-10T09:05:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].state).toBe("pending");
  });

  it("rescues a 👍 dated too late only by a slow external status", async () => {
    // The gap the all-statuses-ours gate left open: a deploy status landing
    // AFTER the 👍 suppressed the suite lookup while still dating the head
    // too late. The lookup now runs whenever a Codex 👍 was rejected purely
    // for freshness — the one case an earlier bound could change the answer.
    const fake = fakeFetch({
      statuses: { abc1234: [
        gate("2026-08-14T12:00:00Z"),
        { context: "Vercel", state: "success", created_at: "2026-08-14T12:10:00Z" },
      ] },
      checkSuites: { abc1234: [{ created_at: "2026-08-14T11:58:00Z", head_branch: "claude/topic" }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, "2026-08-14T11:59:00Z")]) })])],
    });
    const out = await runFull(fake);
    expect(out.written[0].state).toBe("success");
  });

  it("rejects a lingering 👍 when the head arrived by fast-forward", async () => {
    // A fast-forward onto a pre-existing commit — a stacked branch
    // graduating — leaves NO force-push event, and the commit's old
    // statuses and suites predate the previous head's 👍, which Codex has
    // not yet revoked. The suite born on this PR's own branch is the only
    // server-stamped record of the transition; a 👍 older than it belongs
    // to a head nobody is looking at anymore.
    const era = "2026-08-10T09:00:00Z";
    const fake = fakeFetch({
      statuses: { abc1234: [{ context: "Vercel", state: "success", created_at: era }] },
      checkSuites: { abc1234: [
        { created_at: era, head_branch: "stack/upper" },
        { created_at: "2026-08-14T12:00:00Z", head_branch: "claude/topic" },
      ] },
      graphqlResponses: [repoPRs([prNode({
        reactions: page([thumbs(undefined, "2026-08-10T09:05:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].state).toBe("pending");
  });

  it("fails closed when a same-repo head's suites name only other branches", async () => {
    // The gap before the fast-forwarded head's first branch suite exists:
    // old foreign-branch suites prove the commit predates its arrival here,
    // and nothing yet dates the arrival. Trusting the status bound in that
    // window is exactly the fast-forward hole, so the head waits the sweep
    // out — CI runs on every same-repo push, so the dating suite is seconds
    // away.
    const era = "2026-08-10T09:00:00Z";
    const fake = fakeFetch({
      statuses: { abc1234: [{ context: "Vercel", state: "success", created_at: era }] },
      checkSuites: { abc1234: [{ created_at: era, head_branch: "stack/upper" }] },
      graphqlResponses: [repoPRs([prNode({
        reactions: page([thumbs(undefined, "2026-08-10T09:05:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].state).toBe("pending");
  });

  it("fails closed for a fork head, whose suites never name a branch", async () => {
    // GitHub reports head_branch: null for suites on cross-repository
    // heads, so nothing ever dates a fork head's transition — and an
    // exemption that fell back to the status bound let a fast-forwarded
    // fork head inherit the previous head's 👍. Fork contributions merge
    // by admin override or a same-repo re-push; this gate stays closed.
    const fake = fakeFetch({
      statuses: { abc1234: [
        gate("2026-08-14T12:00:00Z"),
        { context: "Vercel", state: "success", created_at: "2026-08-14T11:58:00Z" },
      ] },
      checkSuites: { abc1234: [{ created_at: "2026-08-14T11:58:30Z", head_branch: null }] },
      graphqlResponses: [repoPRs([prNode({
        isCrossRepository: true,
        reactions: page([thumbs(undefined, "2026-08-14T11:59:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written).toEqual([]);
  });

  it("reopens the wait on a nudge tied with the 👍 to the second", async () => {
    // GitHub stamps to the second; a nudge in the same second as the
    // standing 👍 is an unresolved ordering, and ambiguity must not be
    // what leaves a success open for auto-merge.
    const fake = fakeFetch({
      statuses: { abc1234: [gate("2026-08-14T12:00:00Z")] },
      checkSuites: { abc1234: [bornSuite()] },
      issueComments: { 1: [{ user: { login: OWNER }, created_at: AFTER, body: "@codex review" }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, AFTER)]) })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written).toEqual([]);
  });

  it("re-anchors a would-park head on its branch-born suite", async () => {
    // A fast-forward can land a head already wearing an identical old
    // PENDING: no reaction, so the suites were never read; no write, so
    // nothing refreshed the anchor — and the head would park on its first
    // sweep, before Codex's pickup window opens. The would-park path pays
    // one suites call and finds the arrival's own record.
    const at = (suiteAt, t) => sweep({
      owner: OWNER, name: "r", token: "t",
      fetchImpl: fakeFetch({
        statuses: { abc1234: [gate("2026-08-14T11:40:00Z")] },
        checkSuites: { abc1234: [{ created_at: suiteAt, head_branch: "claude/topic" }] },
        graphqlResponses: [repoPRs([prNode()])],
      }).impl,
      log: () => {}, now: () => Date.parse(t),
    });
    // The suite says the head arrived two minutes ago: keep the clock.
    expect((await at("2026-08-14T12:04:00Z", "2026-08-14T12:06:00Z")).awaiting).toBe(1);
    // An old suite corroborates the old marker: park.
    expect((await at("2026-08-14T11:39:00Z", "2026-08-14T12:06:00Z")).awaiting).toBe(0);
  });

  it("keeps the rescue from lowering the bound past the branch-born suite", async () => {
    // The rescue exists to lower a too-late bound with an earlier birth
    // record — but on a fast-forwarded head the earlier records belong to
    // the commit's previous life. Floored at the branch-born suite, the
    // rescue can no longer revive the previous head's 👍 through the side
    // door the confirm path just closed.
    const fake = fakeFetch({
      statuses: { abc1234: [gate("2026-08-14T12:00:00Z")] },
      checkSuites: { abc1234: [
        { created_at: "2026-08-14T11:50:00Z", head_branch: "stack/upper" },
        { created_at: "2026-08-14T12:05:00Z", head_branch: "claude/topic" },
      ] },
      graphqlResponses: [repoPRs([prNode({
        reactions: page([thumbs(undefined, "2026-08-14T11:59:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    // The head already carries `pending`; the point is that no success
    // replaces it.
    expect(out.written).toEqual([]);
  });

  it("dates the head by its check suites when no one writes statuses", async () => {
    // A repo whose CI reports check runs, not commit statuses: with only
    // our own writes on the head, the bound would rest on the very marker a
    // delayed first sweep writes too late — the self-poisoning shape. Check
    // suites are created at push, server-stamped, so a 👍 that beat our
    // first write still approves.
    const fake = fakeFetch({
      statuses: { abc1234: [gate("2026-08-14T12:00:00Z")] },
      checkSuites: { abc1234: [{ created_at: "2026-08-14T11:58:00Z", head_branch: "claude/topic" }] },
      graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs(undefined, "2026-08-14T11:59:00Z")]) })])],
    });
    const out = await runFull(fake);
    expect(out.written[0].state).toBe("success");
  });

  it("hears a nudge hidden behind a future-dated commit", async () => {
    // The commit date is author-controlled; forged past the owner's
    // "@codex review" it would filter the nudge out of the walk, and on an
    // approved head that fails OPEN — a stale success left for auto-merge.
    // The walk bound is the EARLIER of the commit date and the head's first
    // server-stamped status, so the forged date only ever loses.
    const future = { nodes: [{ commit: { committedDate: "2026-08-14T13:00:00Z" } }] };
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      issueComments: { 1: [{ user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "@codex review" }] },
      graphqlResponses: [repoPRs([prNode({ commits: future, reactions: page([thumbs()]) })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written).toEqual([]);
  });

  it("keeps the clock running while an owner nudge awaits Codex's answer", async () => {
    // A nudge newer than Codex's last word means the answer is due again —
    // the head counts as awaiting on EVERY sweep, however the run was
    // started. This is what makes the nudge race immune to queue
    // replacement: no trigger-level grace has to survive anything.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [review()] },
      issueComments: { 1: [{ user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "@codex review" }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    // PENDING, not FINDINGS — the marker already says PENDING, so no write.
    expect(out.written).toEqual([]);
  });

  it("settles again once Codex answers the nudge", async () => {
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [review()] },
      issueComments: { 1: [
        { user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "@codex review" },
        { user: { login: `${CODEX_BOT}[bot]` }, created_at: "2026-08-14T12:15:00Z", body: "still the same finding" },
      ] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(0);
    expect(out.written[0].description).toBe(FINDINGS);
  });

  it("hears a nudge typed as an inline review-thread reply", async () => {
    // The rebuttal-plus-nudge round most naturally happens ON the finding's
    // thread, which is the pulls comment stream, not the issues one — and
    // the sweep is the sole source of nudge state, so missing that stream
    // would settle FINDINGS over a nudge that was plainly made.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [review()] },
      reviewThreadComments: { 1: [{ user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "not a real issue — @codex review" }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    expect((await runFull(fake)).awaiting).toBe(1);
  });

  it("keeps the clock running when the findings belong to a previous head", async () => {
    // The review's own commit id is the staleness guard: a push moves the
    // head oid and every earlier review stops matching, so a fresh head with
    // an old review is simply awaiting a verdict again.
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [review("oldhead")] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written).toEqual([]);
  });

  it("keeps the clock running through a re-read of a head with findings", async () => {
    const fake = fakeFetch({
      statuses: { abc1234: [gate()] },
      prReviews: { 1: [review()] },
      graphqlResponses: [repoPRs([
        prNode({ reactions: page([hold("EYES", `${CODEX_BOT}[bot]`)]) }),
      ])],
    });
    expect((await runFull(fake)).awaiting).toBe(1);
  });

  it("holds on the owner's 👀 without keeping the fast clock running", async () => {
    // The owner's 👀 is a deliberate hold: `failure`, cleared only by human
    // action, which the throttled schedule notices on its own.
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([
        prNode({ reactions: page([hold("EYES")]) }),
      ])],
    });
    const out = await runFull(fake);
    expect(out.written[0].state).toBe("failure");
    expect(out.awaiting).toBe(0);
  });

  it("refuses two open PRs sharing a head, and asks nothing further", async () => {
    const fake = fakeFetch({
      graphqlResponses: [
        repoPRs([prNode({ number: 1, reactions: page([thumbs()]) }), prNode({ number: 2 })]),
      ],
    });
    const out = await run(fake);
    expect(out.map((w) => w.state)).toEqual(["failure", "failure"]);
    // The set settled it, so no reaction lookup was needed.
    expect(fake.calls.filter((c) => c.path === "/graphql")).toHaveLength(1);
  });

  it("still judges PRs whose heads are distinct", async () => {
    const fake = fakeFetch({
      // bbb has no marker yet, so its first gating is this sweep.
      statuses: { aaa: [gate()] },
      checkSuites: { aaa: [bornSuite()] },
      graphqlResponses: [
        repoPRs([
          prNode({ number: 1, headRefOid: "aaa", reactions: page([thumbs()]) }),
          prNode({ number: 2, headRefOid: "bbb" }),
        ]),
      ],
    });
    expect((await run(fake)).map((w) => w.state)).toEqual(["success", "pending"]);
  });

  it("follows pull-request pages", async () => {
    const fake = fakeFetch({
      graphqlResponses: [
        repoPRs([prNode({ number: 1 })], true, "prCursor"),
        repoPRs([prNode({ number: 2, headRefOid: "def5678" })]),
      ],
    });
    expect((await run(fake)).map((w) => w.number)).toEqual([1, 2]);
  });

  it("surfaces a GraphQL error instead of publishing anything", async () => {
    const fake = fakeFetch({ statuses: { abc1234: [gate()] }, graphqlResponses: [{ errors: [{ message: "bad query" }] }] });
    await expect(run(fake)).rejects.toThrow(/bad query/);
    expect(fake.calls.some((c) => c.path.includes("/statuses/"))).toBe(false);
  });

  it("contains a failed status write instead of going red", async () => {
    // The canonical case: a 422 moments after a force-push, before the new
    // SHA replicates. A run that died here went red — which notifies the
    // owner every time — for something the next sweep repairs by itself.
    // The head counts as awaiting, so the minute loop IS the retry; the
    // log names the call and its code, never the token or the body.
    const lines = [];
    const fake = fakeFetch({ graphqlResponses: [repoPRs([prNode()])], failStatusWrite: true });
    const out = await sweep({
      owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: (l) => lines.push(l),
    });
    expect(out.awaiting).toBe(1);
    expect(out.written).toEqual([]);
    // Contained is not forgiven: the head is reported so runLoop can turn a
    // persistent streak into a red run.
    expect(out.failed).toEqual([1]);
    expect(lines.join("\n")).toMatch(/#1: sweep failed \(POST \/repos\/o\/r\/statuses\/abc1234 failed: 422\)/);
  });

  it("still judges the other heads after one head's failure", async () => {
    const fake = fakeFetch({
      graphqlResponses: [repoPRs([
        prNode({ number: 1, headRefOid: "aaa" }),
        prNode({ number: 2, headRefOid: "bbb" }),
      ])],
      failStatusWrite: true,
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(2);
    expect(out.failed).toEqual([1, 2]);
    // Both writes were attempted — the first failure did not end the loop.
    const posts = fake.calls.filter((c) => c.method === "POST" && c.path.includes("/statuses/"));
    expect(posts.map((c) => c.path)).toEqual([
      "/repos/o/r/statuses/aaa",
      "/repos/o/r/statuses/bbb",
    ]);
  });

  it("fails the status closed and goes red once a head stays unreadable", async () => {
    // Branch protection consumes the STATUS, not this job's color: an
    // approved head whose reads then start failing may be carrying a
    // success earned before a hold or re-review request landed, and a red
    // run alone leaves that gate open. At the streak threshold the sweep
    // writes UNREADABLE pending over the head — the write path here is
    // alive even though the reads are not — and then throws.
    const streaks = new Map();
    const runOnce = () => {
      const fake = fakeFetch({
        statuses: { abc1234: [gate()] },
        graphqlResponses: [repoPRs([prNode({ reactions: page([thumbs()]) })])],
        failComments: true,
      });
      return { fake, run: sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: () => {}, streaks }) };
    };
    for (let i = 1; i < MAX_FAIL_STREAK; i += 1) {
      const { run } = runOnce();
      const out = await run;
      expect(out.failed).toEqual([1]);
    }
    const { fake, run } = runOnce();
    await expect(run).rejects.toThrow(/#1: still failing after 5 consecutive sweeps/);
    const post = fake.calls.find((c) => c.method === "POST" && c.path.includes("/statuses/"));
    expect(post.body).toMatchObject({ context: CONTEXT, state: "pending", description: UNREADABLE });
  });

  it("still goes red when the fail-closed write fails too", async () => {
    // Nothing fresh was being published through a broken write path, so
    // there is no stale success to invalidate — the red run is all that is
    // left to say, and it must still be said.
    const streaks = new Map();
    const runOnce = () => {
      const fake = fakeFetch({ graphqlResponses: [repoPRs([prNode()])], failStatusWrite: true });
      return sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: () => {}, streaks });
    };
    for (let i = 1; i < MAX_FAIL_STREAK; i += 1) await runOnce();
    await expect(runOnce()).rejects.toThrow(/#1: still failing after 5 consecutive sweeps/);
  });

  it("forgives a failure streak that ends", async () => {
    const streaks = new Map();
    const failing = fakeFetch({ graphqlResponses: [repoPRs([prNode()])], failStatusWrite: true });
    await sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: failing.impl, log: () => {}, streaks });
    expect(streaks.get("1:abc1234")).toBe(1);
    const healthy = fakeFetch({ graphqlResponses: [repoPRs([prNode()])] });
    await sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: healthy.impl, log: () => {}, streaks });
    expect(streaks.has("1:abc1234")).toBe(false);
  });

  it("resets the failure streak when the head changes", async () => {
    // A force-push right at the threshold: the expected transient 422 on
    // the brand-new SHA must not be read as the old head's fifth
    // consecutive failure — the streak is keyed by head, not PR number.
    const streaks = new Map();
    const failOnce = (sha) => {
      const fake = fakeFetch({
        graphqlResponses: [repoPRs([prNode({ headRefOid: sha })])],
        failStatusWrite: true,
      });
      return sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: () => {}, streaks });
    };
    for (let i = 1; i < MAX_FAIL_STREAK; i += 1) await failOnce("abc1234");
    const out = await failOnce("def5678");
    expect(out.failed).toEqual([1]);
    expect(streaks.get("1:def5678")).toBe(1);
  });

  it("revisits settled heads on the slow cadence, awaiting heads every sweep", async () => {
    // The REST budget holding: a settled head costs several calls to judge,
    // and rescanning every settled PR each minute is what would blow the
    // 1,000/hour token ceiling. Awaiting heads and new SHAs stay on the
    // minute clock; a fresh event run starts with an empty cadence map, so
    // human actions that emit webhooks are still seen in seconds.
    const cadence = new Map();
    const settledSweep = (revisitEvery = 5) => {
      const fake = fakeFetch({
        statuses: { abc1234: [gate()] },
        prReviews: { 1: [review()] },
        graphqlResponses: [repoPRs([prNode()])],
      });
      return sweep({
        owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: () => {}, cadence, revisitEvery,
      }).then((out) => ({ out, fake }));
    };
    // First sweep judges and settles (review findings → awaiting 0).
    const first = await settledSweep();
    expect(first.out.awaiting).toBe(0);
    expect(first.fake.calls.some((c) => c.path.includes("/statuses/"))).toBe(true);
    // Sweeps 2–5 skip the head entirely: no status reads, no walks.
    for (let i = 0; i < 4; i += 1) {
      const skipped = await settledSweep();
      expect(skipped.fake.calls.filter((c) => c.path.includes("/statuses/"))).toEqual([]);
      expect(skipped.out.awaiting).toBe(0);
    }
    // Sweep 6 is the revisit: the head is read again.
    const revisit = await settledSweep();
    expect(revisit.fake.calls.some((c) => c.path.includes("/statuses/"))).toBe(true);
    // A new head on the same PR is judged immediately, cadence or not.
    const moved = fakeFetch({
      graphqlResponses: [repoPRs([prNode({ headRefOid: "def5678" })])],
    });
    await sweep({ owner: OWNER, name: "r", token: "t", fetchImpl: moved.impl, log: () => {}, cadence });
    expect(moved.calls.some((c) => c.path.includes("/statuses/def5678"))).toBe(true);
  });

  it("rejudges a survivor the moment its duplicate closes", async () => {
    // Shared-head membership is topology on OTHER PRs: the duplicate
    // closing changes this head's verdict while its own SHA and updatedAt
    // stand still, and the closed-event successor only queues behind the
    // active run — so the active loop must notice the flip itself.
    const cadence = new Map();
    const both = fakeFetch({
      graphqlResponses: [repoPRs([
        prNode(),
        prNode({ number: 2, headRefName: "claude/other" }),
      ])],
    });
    const first = await sweep({
      owner: OWNER, name: "r", token: "t", fetchImpl: both.impl, log: () => {}, cadence,
    });
    expect(first.written.map((w) => w.state)).toEqual(["failure", "failure"]);
    // Same survivor, same SHA, same updatedAt — but the duplicate is gone,
    // and the very next sweep must rejudge rather than sit on the failure.
    const alone = fakeFetch({
      statuses: { abc1234: [] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const second = await sweep({
      owner: OWNER, name: "r", token: "t", fetchImpl: alone.impl, log: () => {}, cadence,
    });
    expect(second.written.map((w) => w.state)).toEqual(["pending"]);
  });

  it("keeps the rewound head's 👍 from approving a fast-forward return", async () => {
    // Rewind to an ancestor (a force-push, leaving the event), earn a 👍
    // there, fast-forward BACK to the original SHA: the return leaves no
    // event, and the SHA's branch-born suite from its first tenure would
    // date the arrival too early. Suites before the last force-push belong
    // to a previous life, so the floor leaves only the re-arrival's fresh
    // suite — which the rewound head's 👍 predates.
    const fake = fakeFetch({
      statuses: { abc1234: [{ context: "Vercel", state: "success", created_at: "2026-08-14T10:05:00Z" }] },
      checkSuites: { abc1234: [
        { created_at: "2026-08-14T10:00:00Z", head_branch: "claude/topic" },
        { created_at: "2026-08-14T12:20:00Z", head_branch: "claude/topic" },
      ] },
      graphqlResponses: [repoPRs([prNode({
        timelineItems: { nodes: [{ createdAt: "2026-08-14T12:00:00Z" }] },
        reactions: page([thumbs(undefined, "2026-08-14T12:10:00Z")]),
      })])],
    });
    const out = await runFull(fake);
    expect(out.awaiting).toBe(1);
    expect(out.written[0].state).toBe("pending");
  });

  it("parks an unanswered head after UNANSWERED_MINUTES", async () => {
    // Codex answers within minutes of the event that woke it or it never
    // started — a forgotten PR with no verdict must not keep every loop
    // alive to its cap forever. The head still gates pending; only the
    // clock lets go, and a nudge or push (both events) is the retry.
    const at = (t) => sweep({
      owner: OWNER, name: "r", token: "t",
      fetchImpl: fakeFetch({
        statuses: { abc1234: [gate("2026-08-14T12:00:00Z")] },
        graphqlResponses: [repoPRs([prNode()])],
      }).impl,
      log: () => {}, now: () => Date.parse(t),
    });
    // Inside the window the head keeps the fast clock…
    expect((await at("2026-08-14T12:09:00Z")).awaiting).toBe(1);
    // …and past it the loop parks, with nothing written over the gate.
    const parked = await at("2026-08-14T12:11:00Z");
    expect(parked.awaiting).toBe(0);
    expect(parked.written).toEqual([]);
  });

  it("keeps a reading head on the clock past the window", async () => {
    // 👀 means the review genuinely started; the swap to 👍 emits no
    // webhook, so a parked reading head would strand the verdict on the
    // hourly schedule at the exact moment the next minute matters.
    const fake = fakeFetch({
      statuses: { abc1234: [gate("2026-08-14T12:00:00Z")] },
      graphqlResponses: [repoPRs([prNode({
        reactions: page([{ content: "EYES", createdAt: "2026-08-14T12:00:30Z", user: { login: `${CODEX_BOT}[bot]` } }]),
      })])],
    });
    const out = await sweep({
      owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl,
      log: () => {}, now: () => Date.parse("2026-08-14T12:45:00Z"),
    });
    expect(out.awaiting).toBe(1);
  });

  it("parks a nudged head UNANSWERED_MINUTES after the ask", async () => {
    // The nudge restarts the window from its own time — and when Codex
    // ignores it too, the next nudge is another event, not a loop held to
    // its cap.
    const at = (t) => sweep({
      owner: OWNER, name: "r", token: "t",
      fetchImpl: fakeFetch({
        statuses: { abc1234: [gate("2026-08-14T12:00:00Z")] },
        issueComments: { 1: [{ user: { login: OWNER }, created_at: "2026-08-14T12:10:00Z", body: "@codex review" }] },
        graphqlResponses: [repoPRs([prNode()])],
      }).impl,
      log: () => {}, now: () => Date.parse(t),
    });
    expect((await at("2026-08-14T12:15:00Z")).awaiting).toBe(1);
    expect((await at("2026-08-14T12:21:00Z")).awaiting).toBe(0);
  });

  it("gives a just-gated head the full window whatever its records say", async () => {
    // The fast-forward shapes arrive carrying records days old; anchored
    // only on the bound they would be parked on sight, and the run their
    // own push started would exit before the dating suite exists. The
    // first sweep's own `pending` write keeps that sweep's clock and
    // anchors the next ones, so every head gets UNANSWERED_MINUTES from
    // its first gating.
    const era = "2026-08-10T09:00:00Z";
    const fake = fakeFetch({
      statuses: { abc1234: [{ context: "Vercel", state: "success", created_at: era }] },
      graphqlResponses: [repoPRs([prNode()])],
    });
    const out = await sweep({
      owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl,
      log: () => {}, now: () => Date.parse("2026-08-14T12:06:00Z"),
    });
    expect(out.written[0].state).toBe("pending");
    expect(out.awaiting).toBe(1);
  });

  it("revisits a settled head at once when the PR is touched", async () => {
    // The queued-nudge hole: while a loop is already running, a nudge's own
    // event run only queues behind the concurrency group — the ACTIVE loop
    // has to notice, or an approved head skipped for four more sweeps
    // leaves a stale success mergeable for minutes after the owner asked
    // for a re-read. A nudge is a comment and comments move `updatedAt`,
    // which rides the PR list query the sweep already pays for.
    const cadence = new Map();
    const sweepAt = (updatedAt) => {
      const fake = fakeFetch({
        statuses: { abc1234: [gate()] },
        prReviews: { 1: [review()] },
        graphqlResponses: [repoPRs([prNode({ updatedAt })])],
      });
      return sweep({
        owner: OWNER, name: "r", token: "t", fetchImpl: fake.impl, log: () => {}, cadence,
      }).then(() => fake);
    };
    await sweepAt(GATED_AT);
    // Untouched: skipped.
    const quiet = await sweepAt(GATED_AT);
    expect(quiet.calls.filter((c) => c.path.includes("/statuses/"))).toEqual([]);
    // Touched (a comment landed): judged on the very next sweep.
    const touched = await sweepAt("2026-08-14T12:20:00Z");
    expect(touched.calls.some((c) => c.path.includes("/statuses/"))).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// The workflow holds `statuses: write`, and its safety is largely the
// *absence* of something — a checkout, a branch-selectable trigger, a second
// writer — which nothing else would notice going missing.

describe("runLoop", () => {
  // Fake clock: sleeping is what advances time, so the deadline arithmetic
  // is exercised without a single real wait.
  function harness({ minutes, failOn = Infinity }) {
    let clock = 0;
    let sweeps = 0;
    const sleeps = [];
    const run = runLoop({
      minutes,
      intervalSeconds: 60,
      now: () => clock,
      sleep: async (ms) => { sleeps.push(ms); clock += ms; },
      sweepOnce: async () => { sweeps += 1; if (sweeps === failOn) throw new Error("boom"); },
    });
    return { run, count: () => sweeps, sleeps };
  }

  it("stops after one sweep when no verdict is due — the lean gate", async () => {
    // Runner time must be proportional to pushes, not to how long PRs sit
    // open: an always-on loop is what makes this unaffordable on a private
    // repo, and a PR waiting overnight on a human is not a reason to poll.
    // Wired exactly as main() wires it.
    // The clock advances through sleep so a broken gate runs to the deadline
    // and fails on the count, rather than starving the runner forever on an
    // instantly-resolving fake sleep.
    let clock = 0;
    let sweeps = 0;
    await runLoop({
      minutes: 55,
      intervalSeconds: 60,
      now: () => clock,
      sleep: async (ms) => { clock += ms; },
      sweepOnce: async () => { sweeps += 1; return { written: [], awaiting: 0 }; },
      shouldContinue: ({ awaiting }) => awaiting > 0,
    });
    expect(sweeps).toBe(1);
  });

  it("keeps looping while verdicts are due, and stops when the last settles", async () => {
    let clock = 0;
    let sweeps = 0;
    const awaitingBySweep = [2, 1, 0];
    await runLoop({
      minutes: 55,
      intervalSeconds: 60,
      now: () => clock,
      sleep: async (ms) => { clock += ms; },
      sweepOnce: async () => { sweeps += 1; return { written: [], awaiting: awaitingBySweep.shift() }; },
      shouldContinue: ({ awaiting }) => awaiting > 0,
    });
    expect(sweeps).toBe(3);
  });

  it("sweeps once when minutes is 0 — the one-shot invocation", async () => {
    const h = harness({ minutes: 0 });
    await h.run;
    expect(h.count()).toBe(1);
    expect(h.sleeps).toEqual([]);
  });

  it("keeps sweeping a poll interval apart until the deadline", async () => {
    const h = harness({ minutes: 3 });
    await h.run;
    // t=0,60s,120s,180s — the sweep at the deadline still runs.
    expect(h.count()).toBe(4);
    expect(h.sleeps).toEqual([60_000, 60_000, 60_000]);
  });

  it("lets an error escape rather than looping over a broken sweep", async () => {
    // A red job is visible and the queued successor starts immediately; a
    // loop that swallows failures looks healthy while the gate stalls.
    const h = harness({ minutes: 60, failOn: 2 });
    await expect(h.run).rejects.toThrow("boom");
    expect(h.count()).toBe(2);
  });

});

describe("codex-verdict workflow", () => {
  const yml = readFileSync(".github/workflows/codex-verdict.yml", "utf8");

  it("has no branch-selectable trigger", () => {
    // `workflow_dispatch` takes a ref and GitHub runs the workflow file from
    // it, so a branch could supply its own steps and keep the write token.
    // Scoped to the trigger block: the comment above it names what it removed.
    // `pull_request` (without `_target`) has the same hole — the workflow
    // definition comes from the merge ref, which contains the PR's changes.
    const on = yml.slice(yml.indexOf("\non:"), yml.indexOf("permissions:"));
    expect(on).not.toMatch(/workflow_dispatch/);
    expect(on).not.toMatch(/pull_request:/);
    expect(on).toMatch(/schedule:/);
  });

  it("starts the loop on push events, so a fresh head is never left to the throttled schedule", () => {
    // `pull_request_target` takes the workflow definition from the base ref,
    // so unlike dispatch a PR cannot bring its own sweep — and it is the only
    // event-driven start available, since reactions emit no webhook at all.
    // Without it, the first push after a quiet spell waits 10–37 measured
    // minutes for a scheduled fire before minute-polling begins.
    const on = yml.slice(yml.indexOf("\non:"), yml.indexOf("permissions:"));
    expect(on).toMatch(/pull_request_target:/);
    expect(on).toMatch(/types:.*\bsynchronize\b/);
    expect(on).toMatch(/types:.*\bopened\b/);
    expect(on).toMatch(/types:.*\bready_for_review\b/);
    // `closed` clears a shared-head failure the moment the duplicate PR
    // goes away — a webhook-capable, merge-enabling transition that would
    // otherwise wait on the throttled schedule.
    expect(on).toMatch(/types:.*\bclosed\b/);
  });

  it("starts the loop on comment events, for the round that has no push", () => {
    // A rebuttal plus an "@codex review" nudge changes the verdict with no
    // pull_request event anywhere, and the reactions that follow emit
    // nothing — these are what keep that round on the minute clock. Both
    // run the workflow definition from the default branch.
    const on = yml.slice(yml.indexOf("\non:"), yml.indexOf("permissions:"));
    expect(on).toMatch(/issue_comment:/);
    expect(on).toMatch(/pull_request_review_comment:/);
    // Both streams also fire on `edited`: a nudge edited into an existing
    // comment is dated by its edit, and without the event that re-read
    // waits on the throttled schedule.
    expect(on.match(/types: \[created, edited\]/g)).toHaveLength(2);
  });

  it("loops inside the job, with the schedule as keep-alive", () => {
    // The loop is the clock — GitHub throttles the schedule to a measured
    // median of 14 minutes, so without SWEEP_LOOP_MINUTES the gate lags a
    // quarter hour behind Codex. All three settings are load-bearing: the
    // loop length, an interval that keeps ~60 sweeps/hour inside the token's
    // rate budget, and a queue-feeding schedule always shorter than the loop.
    expect(yml).toMatch(/SWEEP_LOOP_MINUTES:\s*"55"/);
    expect(yml).toMatch(/SWEEP_INTERVAL_SECONDS:\s*"60"/);
    // Hourly, deliberately: everything the schedule alone catches fails
    // closed and any comment clears it on demand, and on a private repo
    // each idle fire bills a full minute — hourly is ~720/month against
    // `*/15`'s ~2,900. Off the hour, dodging the :00 stampede.
    expect(yml).toMatch(/cron:\s*'23 \* \* \* \*'/);
  });

  it("is the only workflow that can write commit statuses", () => {
    // The sweep's correctness leans on being the sole writer of the `codex`
    // status: a second writer is an unordered write, and one delayed past
    // this loop's exit overwrites a just-published success with nothing
    // left to notice — the race class that got the old reset workflow
    // deleted. A regression here is silent, so pin the absence.
    const dir = ".github/workflows";
    for (const file of readdirSync(dir).filter((f) => f.endsWith(".yml"))) {
      if (file === "codex-verdict.yml") continue;
      const text = readFileSync(join(dir, file), "utf8");
      expect(text, `${file} must not hold statuses: write`).not.toMatch(/statuses:\s*write/);
    }
  });

  it("says approve rather than review in the pending wording", () => {
    // A head Codex has reviewed and left findings on sits at pending too, so
    // "waiting to review" would name the one thing that already happened.
    expect(PENDING).toMatch(/approve/);
  });

  it("bounds the job so a hung loop cannot hold the concurrency queue", () => {
    // Without a timeout a wedged API call keeps the runner for the 6-hour
    // default — and the queued successor waits behind it, stalling the gate.
    expect(yml).toMatch(/timeout-minutes:\s*65/);
  });

  it("keeps the successor queued rather than canceling into a gap", () => {
    expect(yml).toMatch(/cancel-in-progress:\s*false/);
  });

  it("still checks out the default branch", () => {
    expect(yml).toMatch(/ref:\s*\$\{\{\s*github\.event\.repository\.default_branch\s*\}\}/);
  });

  it("asks for no write scope beyond statuses", () => {
    const perms = yml.slice(yml.indexOf("permissions:"), yml.indexOf("concurrency:"));
    expect(perms).toMatch(/statuses:\s*write/);
    expect(perms).not.toMatch(/contents:\s*write/);
    expect(perms).not.toMatch(/actions:\s*write/);
  });

  it("runs on the same Node major CI tests it on", () => {
    // This repo is a Rust workspace with no `.nvmrc` and no package.json, so
    // nothing else forces the two to agree — and a split is the quiet kind:
    // the suite goes green on one runtime while the gate that decides merges
    // runs another. Both are bare majors, so this compares the numbers rather
    // than the strings.
    const major = (text) => text.match(/node-version:\s*'(\d+)'/)?.[1];
    const ci = readFileSync(".github/workflows/ci.yml", "utf8");
    expect(major(yml)).toBe(major(ci));
    expect(major(yml)).toMatch(/^\d+$/);
  });

  it("is tested by CI at all", () => {
    // The sweep's suite is not a cargo test, so it runs only because a step
    // names it. Without this, deleting that step leaves the file here looking
    // like coverage while nothing executes it.
    const ci = readFileSync(".github/workflows/ci.yml", "utf8");
    expect(ci).toMatch(/node --test scripts\/\*\.test\.js/);
  });
});
