// Publishes Codex's review verdict as a commit status, so branch protection
// can gate on something it can actually see.
//
// Codex posts no check run of its own, and its clean pass is only a 👍
// reaction on the PR body — which emits no webhook. So the verdict is both
// invisible to protection rules and undeliverable by event: it has to be
// polled and translated. Without that, auto-merge fires on green CI *before*
// Codex has looked, and a merged-too-early PR is indistinguishable from a
// correctly merged one.
//
// The reaction is the whole verdict, and that is why this file is small.
// Codex's own description of itself: "If Codex has suggestions, it will
// comment; otherwise it will react with 👍." The reaction is therefore present
// only when it has nothing to say — findings in a review body, in a thread, or
// in a top-level comment all mean no reaction, and none of them decides the
// *verdict* here. A submitted review naming the current head is still read,
// but only for the loop's economics: findings mean the next change is a push,
// not a reaction, so the minute clock has nothing to catch until then. Codex
// also revokes the reaction when a new commit lands, so a reaction that is
// present belongs to the head being looked at, and nothing here has to
// compare SHAs to establish that.
//
// An earlier version parsed review bodies for finding badges, timestamped them
// against the head, and ranked findings against passes. All of it re-derived
// what the reaction already says, and two of its bugs pointed the same way:
// approving without a verdict.
//
// Human review threads are deliberately not modeled: GitHub's "require
// conversation resolution" setting does that natively, and better.
//
// Reactions run the other way too. Approval is Codex's 👍 *and* no 👀 and no
// 👎 from the repository owner *and* no owner "@codex review" newer than the
// 👍 — so a hold costs two seconds from a phone, which is the point: without
// it auto-merge can land a PR before someone who wanted a look gets one, and
// there is no other signal that cheap. The nudge case is the same idea one
// step later: asking Codex to look again means the standing verdict is no
// longer the one wanted, so the gate closes until the fresh answer lands.
//
// Only the owner, because this repo is public: anyone can react, and a hold
// deliberately outlives head changes, so an unrestricted one lets a passer-by
// block a PR for as long as they feel like it. Codex's own 👀 counts too —
// it means "still reading", it clears when it reacts 👍, and it is not a
// passer-by.
//
// A hold takes effect within a sweep, NOT immediately, and the difference is
// the whole point of saying so. Reactions emit no webhook, so nothing can
// notice one as it lands; if `success` is already published and another
// required check goes green before the next sweep, auto-merge takes the PR
// with the hold sitting there unread. The interval bounds that and cannot
// close it. **To stop a merge right now, convert the PR to a draft** — GitHub
// disables auto-merge on drafts the moment you do it, and `verdictFor` returns
// null for a draft so nothing here fights you. The reaction is for "don't
// merge this yet", which is the ordinary case; the draft is for "stop".

export const CODEX_BOT = "chatgpt-codex-connector";
export const CONTEXT = "codex";

/**
 * The pending description.
 *
 * `publish` skips the write when state and description both match what is
 * already on the head, and that skip is what keeps the head's earliest
 * `codex` status — its gate marker — where it is. Rewrite the same status
 * each sweep and the marker moves forward every time, until no reaction can
 * ever be newer than it: the gate stalls for good, on every open pull
 * request at once, with nothing failing to say so.
 *
 * It says *approve* rather than *review* because a head Codex has reviewed
 * and left findings on sits here too — nothing is waiting for a review by
 * then, only for the 👍.
 */
export const PENDING = "Waiting for Codex to approve this head";

/**
 * The pending description for a head Codex has reviewed and left findings
 * on. Same state as PENDING — the gate stays closed either way — but the
 * sweep's lean gate treats the two differently: PENDING means the answer is
 * still being written and the next minute matters; FINDINGS means the next
 * change is a push, which restarts the loop by event, so polling for it
 * would only burn the runner.
 */
export const FINDINGS = "Codex left findings on this head";

/**
 * The pending description written over a head the sweep has repeatedly
 * failed to read — see the failure streak in `sweep`. The run going red is
 * not enough on its own: branch protection consumes the *status*, and a
 * `success` published before the failures began would otherwise keep the
 * gate open while a newer hold or re-review request sits unread.
 */
export const UNREADABLE = "Verdict unreadable — failing closed until the sweep recovers";

/**
 * Consecutive failing sweeps before a head's failure stops being treated as
 * transient: minutes past any replication lag, still short enough that a
 * stale published verdict is invalidated and the owner notified promptly.
 */
export const MAX_FAIL_STREAK = 5;

/**
 * Minutes an unanswered head keeps the fast clock before the loop parks it.
 *
 * Codex answers within a few minutes of the push that woke it, or it never
 * started — so polling an unanswered head for an hour buys nothing and is
 * what let a forgotten PR with no verdict keep every loop alive its full 55
 * minutes, restarted by the schedule forever. Past this age the head still
 * gates `pending` (nothing fails open), it just stops counting as awaiting:
 * the retry is a nudge or a push, both events that restart the clock. A 👀
 * is exempt — eyes on means the review genuinely started, and long reads
 * are what the 55-minute loop is for.
 */
export const UNANSWERED_MINUTES = 10;

const stripBot = (login) => String(login ?? "").replace(/\[bot\]$/, "");
export const matchesBot = (login, botLogin = CODEX_BOT) =>
  stripBot(login) === stripBot(botLogin);

/**
 * Normalize a timestamp to a UTC "Z" string, or null.
 *
 * Every time comparison in this file is lexicographic, which is only sound
 * while both sides are UTC strings in the same shape. Today they all are —
 * REST returns UTC "Z", and the GraphQL fields used here (`committedDate`,
 * reaction `createdAt`) are `DateTime`, defined as UTC — so this changes
 * nothing at runtime. It exists because GraphQL also has the
 * offset-preserving `GitTimestamp` (`committer.date`, `author.date`), and
 * one future edit swapping such a field in would make every string
 * comparison silently misorder by up to a day. Applied at each ingestion
 * point, so a value carrying an offset is converted before it can be
 * compared. Already-"Z" strings pass through byte-identical rather than
 * being reformatted: the converted form gains millisecond precision, and
 * reformatting everything would move the head's marker semantics for no
 * gain.
 */
export const utc = (t) => {
  if (!t) return null;
  if (t.endsWith("Z")) return t;
  const ms = Date.parse(t);
  return Number.isNaN(ms) ? null : new Date(ms).toISOString();
};

/** Earliest / latest of some UTC strings, ignoring nulls; null if none. */
export const earlierOf = (...ts) => ts.filter(Boolean).sort()[0] ?? null;
export const laterOf = (...ts) => ts.filter(Boolean).sort().at(-1) ?? null;

/**
 * The newest Codex review of exactly this head in a REST batch, or null.
 *
 * The review's own commit id is the tie to the head, so no time guard is
 * needed (unlike reactions, which outlive pushes until Codex revokes them):
 * a new push changes the head oid and every earlier review stops matching.
 * This also covers standalone inline comments — GitHub wraps every inline
 * comment in a review record (creating a COMMENTED one when none was
 * submitted), so there is no review-comment finding without a review here.
 * The timestamp, not a boolean, because a nudge is only *pending* while it
 * is newer than Codex's last word — see the nudge note in `sweep`.
 */
export function findingsOn(reviews, headRefOid) {
  let at = null;
  for (const r of reviews ?? []) {
    if (!matchesBot(r.user?.login) || r.commit_id !== headRefOid) continue;
    const t = utc(r.submitted_at) ?? "";
    if (at === null || t > at) at = t;
  }
  return at;
}

/**
 * Read every review on the PR; the newest Codex review of this head, or null.
 *
 * REST, paginated to the end, filtered locally by `matchesBot` — not a
 * GraphQL `reviews(author:)` window. The server-side filter needed the
 * bot's login spelled exactly as GitHub stores it, and a mismatch does not
 * error: it returns an empty list forever, which reads as "no findings" and
 * quietly re-opens the always-on polling hole this state exists to close.
 * Local matching cannot miss that way, and full pagination cannot be
 * evicted by later reply-reviews the way a `last:N` window was. Runs only
 * when the answer can still change the verdict, so settled PRs cost no
 * extra calls; the 65-minute job timeout is the backstop against a paging
 * pathology, and an error escaping here ends the run red by design.
 */
export async function codexReviewedAt(api, { owner, name, number, headRefOid }) {
  let at = null;
  for (let page = 1; ; page += 1) {
    const batch = await api.rest(
      `/repos/${owner}/${name}/pulls/${number}/reviews?per_page=100&page=${page}`,
    );
    const t = findingsOn(batch, headRefOid);
    if (t !== null && (at === null || t > at)) at = t;
    if (!batch || batch.length < 100) return at;
  }
}

/**
 * What one comment batch says about this head: Codex's newest word, and the
 * owner's newest "@codex review" nudge, both bounded below by `since` — the
 * head commit's own `committedDate`.
 *
 * Findings arrive in three streams — a submitted review, its inline
 * comments (always wrapped in a review record), and a plain PR comment —
 * and only the first two carry a commit id, so `findingsOn` covers them and
 * `codexAt` covers the third. A comment can only be tied to the head by
 * time, and the bound has to be a moment that provably precedes anything
 * said ABOUT the head. The gate marker is not that: the marker is written
 * by this sweep, so a finding that lands before the first status write sits
 * below the marker forever, reads as "no findings" on every later sweep,
 * and revives the always-on loop for good. The commit's own date precedes
 * the head by construction, so nothing genuine can hide under it.
 *
 * A commit date is forgeable (`--date`, a prebuilt commit), which is why
 * the caller passes the EARLIER of the commit date and the head's first
 * server-stamped status: taking the earlier of two bounds only ever admits
 * more, never hides. The commit date covers the finding that lands before
 * the first status write; the server timestamp covers the commit date
 * being forged into the future — which for the owner's nudge would fail
 * OPEN, since a hidden nudge on an approved head leaves a stale success
 * for auto-merge to take. Forged or honest-but-early dates only admit a
 * previous head's comments, which settles to FINDINGS too eagerly — the
 * gate stays closed and the verdict waits for a nudge, a push, or the
 * schedule. Approval itself never trusts the commit date at all — see
 * `readReactions`.
 *
 * The nudge is owner-only for the same reason holds are: this repo is
 * public, and letting any comment shaped like a nudge hold the loop open
 * would hand passers-by the runner bill.
 */
export function commentSignals(comments, { since, owner }) {
  let codexAt = null;
  let nudgeAt = null;
  const bound = utc(since);
  if (!bound) return { codexAt, nudgeAt };
  for (const c of comments ?? []) {
    const at = utc(c.created_at) ?? "";
    if (matchesBot(c.user?.login)) {
      // Codex's word stays on created_at: its edits do not re-answer, and a
      // later timestamp here could only mask a nudge — the fail-open way.
      if (at <= bound) continue;
      if (codexAt === null || at > codexAt) codexAt = at;
    } else if (
      Boolean(owner) && c.user?.login === owner
      && /@codex review/i.test(c.body ?? "")
    ) {
      // A nudge can be EDITED into an old comment, whose created_at then
      // predates the head or the standing 👍 — dating the ask by the later
      // of creation and edit is what hears it. REST's `since` already
      // filters on updated_at, so the edited comment reaches this walk; the
      // cost is that retouching an old nudge comment re-asks, and erring
      // toward blocking on an owner's ask is this file's stated direction.
      const asked = laterOf(at, utc(c.updated_at));
      if (!asked || asked <= bound) continue;
      if (nudgeAt === null || asked > nudgeAt) nudgeAt = asked;
    }
  }
  return { codexAt, nudgeAt };
}

/**
 * Read the PR's comments since the head was committed, to the end.
 *
 * REST rather than a GraphQL window, because a window can be evicted: a
 * finding pushed past `last:N` by later chatter would read as "no findings",
 * reviving the loop for good. `since` filters server-side, so the normal
 * response is a handful of comments from the current round. No page cap —
 * a cap would re-open the same eviction hole one order of magnitude later;
 * the `< 100` batch check terminates every real walk, and the job timeout
 * backstops a server that pages forever. Only called when the answer can
 * still change the verdict, so it adds no traffic to settled PRs.
 */
export async function codexCommentSignals(api, { owner, name, number, since }) {
  let codexAt = null;
  let nudgeAt = null;
  if (!since) return { codexAt, nudgeAt };
  // Both comment streams: top-level (`issues/…/comments`) and inline
  // review-thread replies (`pulls/…/comments`). A rebuttal-plus-nudge is
  // most naturally typed as a thread reply, and since the sweep is the sole
  // source of nudge state, missing that stream would settle FINDINGS over
  // a nudge that was plainly made.
  for (const stream of ["issues", "pulls"]) {
    for (let page = 1; ; page += 1) {
      const batch = await api.rest(
        `/repos/${owner}/${name}/${stream}/${number}/comments?since=${encodeURIComponent(since)}&per_page=100&page=${page}`,
      );
      const seen = commentSignals(batch, { since, owner });
      if (seen.codexAt !== null && (codexAt === null || seen.codexAt > codexAt)) codexAt = seen.codexAt;
      if (seen.nudgeAt !== null && (nudgeAt === null || seen.nudgeAt > nudgeAt)) nudgeAt = seen.nudgeAt;
      if (!batch || batch.length < 100) break;
    }
  }
  return { codexAt, nudgeAt };
}

/**
 * Decide the commit status for one pull request.
 * Returns null for a draft — nothing to gate until it is ready for review.
 */
export function verdictFor({
  isDraft, approved, sharedHead, held, reading, findings, nudged,
}) {
  if (isDraft) return null;

  // A status belongs to the commit; the reaction belongs to the PR. Two open
  // PRs on one head cannot both be described by one status, so approve
  // neither — blocking asks a human to look, where the alternative is a merge
  // justified by another PR's review.
  if (sharedHead) {
    return {
      state: "failure",
      description: "Head shared with another open PR — verdict is ambiguous",
    };
  }

  // Someone asked for a look. Blocking rather than pending, so it reads as a
  // deliberate hold rather than something still on its way.
  if (held) {
    return { state: "failure", description: `On hold: ${held} on the pull request` };
  }

  // Codex still reading is the answer being written, not a hold: `pending`,
  // even over a 👍 (a re-read in progress revokes the old verdict's meaning
  // before it revokes the reaction). Pending is also what keeps the minute
  // loop running through the review, which is the loop's whole point — a
  // `failure` here would idle the loop precisely while the next minute could
  // change the answer.
  if (reading) return { state: "pending", description: PENDING };

  // The owner asked for another look, and the ask is newer than Codex's
  // last word — including a standing 👍. Honoring the old approval while a
  // re-review is pending is a merge nobody wants anymore, so the gate
  // closes until the fresh answer lands; a new 👍 postdating the nudge
  // reopens it through `approved` on a later sweep.
  if (nudged) return { state: "pending", description: PENDING };

  if (approved) {
    return { state: "success", description: "Codex reviewed this head, no findings" };
  }

  // Approval outranks findings on purpose: after a fix-and-nudge round the
  // old review still names this head, and the fresh 👍 is Codex saying it is
  // satisfied. Reading outranks both — a re-read is the verdict changing.
  if (findings) return { state: "pending", description: FINDINGS };

  return { state: "pending", description: PENDING };
}

const PAGE = `pageInfo { hasNextPage endCursor }`;

const OPEN_PRS = `
query($owner:String!, $name:String!, $after:String) {
  repository(owner:$owner, name:$name) {
    pullRequests(states:OPEN, first:50, after:$after) {
      ${PAGE}
      nodes {
        number
        isDraft
        headRefOid
        headRefName
        isCrossRepository
        updatedAt
        commits(last:1) { nodes { commit { committedDate } } }
        timelineItems(itemTypes:[HEAD_REF_FORCE_PUSHED_EVENT], last:1) {
          nodes { ... on HeadRefForcePushedEvent { createdAt } }
        }
        reactions(first:100) { ${PAGE} nodes { content createdAt user { login } } }
      }
    }
  }
}`;

const MORE_REACTIONS = `
query($owner:String!, $name:String!, $number:Int!, $after:String!) {
  repository(owner:$owner, name:$name) {
    pullRequest(number:$number) {
      reactions(first:100, after:$after) { ${PAGE} nodes { content createdAt user { login } } }
    }
  }
}`;

/** Thin GitHub client, so the sweep can be driven by a fake `fetch` in tests. */
export function createApi({ token, fetchImpl = fetch }) {
  async function rest(path, { method = "GET", body } = {}) {
    const res = await fetchImpl(`https://api.github.com${path}`, {
      method,
      headers: {
        authorization: `Bearer ${token}`,
        accept: "application/vnd.github+json",
        "content-type": "application/json",
      },
      body: body ? JSON.stringify(body) : undefined,
    });
    // Name the failed call and its status; never the token or the raw body.
    if (!res.ok) throw new Error(`${method} ${path} failed: ${res.status}`);
    return res.status === 204 ? null : res.json();
  }

  async function graphql(query, variables) {
    const out = await rest("/graphql", { method: "POST", body: { query, variables } });
    if (out.errors?.length) throw new Error(`graphql: ${out.errors[0].message}`);
    return out.data;
  }

  return { rest, graphql };
}

/**
 * What the PR-body reactions say: Codex's verdict, and any hold on it.
 *
 * `since` is when GitHub first saw a commit status on this head — the
 * earliest status of ANY context, not just ours. Codex revokes its 👍 when
 * a new commit lands, but *asynchronously* — it has to notice the push
 * first — so for a few seconds or minutes the PR carries a new head and the
 * previous head's reaction, and reading those together approves a commit
 * nobody reviewed. A reaction newer than `since` cannot be that: any status
 * is server-stamped proof the head already existed.
 *
 * Any context, because our own first write can be LATE — a delayed first
 * sweep — and a 👍 that arrived before it would then sit below the bound
 * forever, unrevivable, with the gate stuck at pending. A third party's
 * status (a deploy, classic CI) lands seconds after the push, well before
 * Codex's earliest possible reaction, so in practice the bound predates
 * every genuine 👍; where that still dates the head too late, `judge`
 * retries with the head's check suites (see `earliestCheckSuite`), and
 * floors everything at the last force-push so a recycled commit's old
 * records cannot resurrect a previous life's approval. The bound is a server timestamp rather than the commit's
 * `committedDate` because a commit date is set by whoever makes the commit
 * (`--date`, or a prebuilt commit), and forged early it would make a stale
 * 👍 read fresh — the one failure that opens the gate. The comment walk
 * makes the opposite choice, and `commentSignals` says why the safe bound
 * differs by direction. No statuses at all means nothing dates the head,
 * and the answer to that is `pending` — this sweep writes the first status,
 * and a fresh reaction after it can approve on a later sweep.
 *
 * Holds come only from the repository owner, plus Codex's own 👀. On a public
 * repo any account can react, and a hold deliberately survives head changes,
 * so an unrestricted hold lets a passer-by block a PR indefinitely. Codex's
 * 👀 is included because it means "still reading" — it clears that when it
 * reacts 👍 — and it is not a passer-by.
 *
 * Holds are deliberately NOT filtered by time: a 👎 left on an earlier head is
 * still someone saying don't merge this, and a new commit is not an answer to
 * it. That errs toward blocking, which is the safe direction here.
 */
export function readReactions(nodes, { since, owner } = {}) {
  let approved = false;
  let approvedAt = null;
  let staleApproval = false;
  let held = null;
  let reading = false;
  const bound = utc(since);
  for (const r of nodes ?? []) {
    const login = r.user?.login;
    const codex = matchesBot(login);
    const at = utc(r.createdAt) ?? "";
    // A missing bound or reaction time both mean "cannot show this reaction
    // is about this head", and the answer to that is `pending` rather than a
    // merge — an unexpected result must not be what opens the gate.
    const fresh = Boolean(bound) && at > bound;
    // The 👍 must be Codex's: it is only a verdict because Codex revokes it
    // on push, and nobody else's does that. Its time is kept because a
    // clean pass leaves no review or comment — the 👍 IS Codex's last word,
    // and a nudge is only pending while it is newer than that word.
    if (r.content === "THUMBS_UP" && codex) {
      if (fresh) {
        approved = true;
        if (approvedAt === null || at > approvedAt) approvedAt = at;
      } else {
        // A Codex 👍 rejected only for freshness. The caller uses this to
        // decide whether a better birth record (a check suite) could
        // change the answer — see `judge` — so the expensive lookup is
        // paid only when it could matter.
        staleApproval = true;
      }
    }
    const mayHold = Boolean(owner) && login === owner;
    if (r.content === "THUMBS_DOWN" && mayHold) held = "👎";
    if (r.content === "EYES" && mayHold && held === null) held = "👀";
    // Codex's own 👀 is the review in flight, not a hold: it blocks approval
    // the same way, but as `pending` rather than `failure`, because pending
    // is what keeps the minute loop polling — Codex swaps 👀 for 👍 with no
    // webhook, and a loop that had already gone idle would leave that 👍
    // waiting on the throttled schedule.
    if (r.content === "EYES" && codex) reading = true;
  }
  return { approved, approvedAt, staleApproval, held, reading };
}

/**
 * Read every page of PR-body reactions.
 *
 * No short-circuit on finding the thumbs-up: a hold can be on a later page,
 * and stopping early would approve over it. Missing the thumbs-up entirely
 * leaves the status `pending` — safe, but it never clears on its own, and
 * every later sweep refetches the same truncated page.
 */
export async function reactionState(api, base, { owner, name, number, since }) {
  let page = base;
  let approved = false;
  let approvedAt = null;
  let staleApproval = false;
  let held = null;
  let reading = false;
  for (;;) {
    const seen = readReactions(page.nodes, { since, owner });
    approved = approved || seen.approved;
    if (seen.approvedAt !== null && (approvedAt === null || seen.approvedAt > approvedAt)) {
      approvedAt = seen.approvedAt;
    }
    staleApproval = staleApproval || seen.staleApproval;
    held = held ?? seen.held;
    reading = reading || seen.reading;
    if (!page.pageInfo.hasNextPage) {
      return { approved, approvedAt, staleApproval, held, reading };
    }
    const data = await api.graphql(MORE_REACTIONS, {
      owner, name, number, after: page.pageInfo.endCursor,
    });
    page = data.repository.pullRequest.reactions;
  }
}

/** Head SHAs carried by more than one open PR. */
export function sharedHeads(prs) {
  const seen = new Map();
  for (const pr of prs) seen.set(pr.headRefOid, (seen.get(pr.headRefOid) ?? 0) + 1);
  return new Set([...seen].filter(([, n]) => n > 1).map(([oid]) => oid));
}

/**
 * A head's status history: every `codex` status newest first, plus the
 * created time of the earliest status of ANY context.
 *
 * The newest `codex` entry is what this sweep compares against so it does
 * not rewrite an identical status. `firstSeen` is the reaction-freshness
 * bound — see `readReactions` for why it spans every context: any status is
 * server-stamped proof the head existed by then, and a third party's lands
 * seconds after the push, before our own first write can (a delayed first
 * sweep would otherwise date the head too late and invalidate a 👍 that
 * arrived first, permanently).
 *
 * Paged to the end, because the endpoint returns *every* context, so a page
 * can be full of statuses that are not ours while older ones sit behind it.
 * Missing the tail is not a harmless truncation: a too-late bound rejects
 * an existing 👍 on every later sweep. The cap is a backstop against an
 * endless loop, not an expected limit.
 */
export async function codexStatuses(api, { owner, name, sha }) {
  const mine = [];
  let firstSeen = null;
  for (let page = 1; page <= 10; page += 1) {
    const batch = await api.rest(`/repos/${owner}/${name}/statuses/${sha}?per_page=100&page=${page}`);
    for (const s of batch ?? []) {
      if (s.context === CONTEXT) mine.push(s);
      const t = utc(s.created_at);
      if (t && (firstSeen === null || t < firstSeen)) firstSeen = t;
    }
    if (!batch || batch.length < 100) break;
  }
  return { mine, firstSeen };
}

/**
 * The head's check-suite birth records: `any` is the created time of its
 * earliest suite from any branch, `forBranch` the earliest born on the
 * given branch. Either is null when no such suite exists.
 *
 * The other server-stamped birth records, and they cut both ways. Suites
 * are created when the commit is pushed, by Actions or any checks app, so
 * `any` dates a head on repos whose CI never writes a commit status — and
 * it can predate a status that merely landed late, rescuing a genuine 👍
 * (gating that rescue on "were all the statuses ours" was tried and is
 * wrong — a slow deploy status arriving AFTER the 👍 suppressed the lookup
 * while still dating the head too late, sticking the gate at pending
 * forever). `forBranch` dates the moment the commit reached THIS branch:
 * a fast-forward onto a pre-existing commit leaves no timeline event, so
 * the suite its arrival triggers is the only server-stamped record of the
 * transition — the floor that keeps the previous head's lingering 👍 from
 * approving a commit nobody reviewed. Fetched only when a 👍 is in play
 * (see `judge`), so an ordinary sweep never pays the call. `head_branch`
 * is a bare branch name — GitHub reports null for fork heads, so a fork
 * head never yields a `forBranch` and `judge` fails it closed: its 👍
 * cannot open the gate, and fork contributions merge by admin override or
 * a same-repo re-push.
 */
export async function checkSuiteBirths(api, { owner, name, sha, branch, since }) {
  let any = null;
  let forBranch = null;
  const floor = utc(since);
  for (let page = 1; page <= 10; page += 1) {
    const batch = await api.rest(`/repos/${owner}/${name}/commits/${sha}/check-suites?per_page=100&page=${page}`);
    const suites = batch?.check_suites ?? [];
    for (const s of suites) {
      const t = utc(s.created_at);
      if (!t) continue;
      // Suites from before the last force-push belong to a previous life of
      // the branch. The subtle revisit: rewind to an ancestor (a force-push,
      // leaving the event), earn a 👍 there, then fast-forward BACK to the
      // original SHA — the return leaves no event and the SHA already has a
      // branch-born suite from its first tenure, which would date the
      // arrival too early and let the rewound head's 👍 approve it. Any
      // same-branch revisit necessarily implies a force-push somewhere
      // between the tenures, so suites after the last one are exactly the
      // current life's; the re-arrival's own workflow runs mint fresh
      // suites within seconds, and until one exists the head is undatable
      // and fails closed.
      if (floor && t < floor) continue;
      if (any === null || t < any) any = t;
      if (branch && s.head_branch === branch && (forBranch === null || t < forBranch)) {
        forBranch = t;
      }
    }
    if (suites.length < 100) break;
  }
  return { any, forBranch };
}

/** Write the status unless an identical one is already on the head. */
export async function publish(api, { owner, name, pr, verdict, current, log }) {
  // Every write shows up in the PR's check list, and a five-minute cadence
  // would otherwise bury it. It also keeps the marker still: rewriting the
  // same status would move the head's earliest-gated timestamp forward.
  if (current?.state === verdict.state && current?.description === verdict.description) {
    log(`#${pr.number}: ${verdict.state} (unchanged)`);
    return false;
  }
  await api.rest(`/repos/${owner}/${name}/statuses/${pr.headRefOid}`, {
    method: "POST",
    body: { context: CONTEXT, state: verdict.state, description: verdict.description },
  });
  log(`#${pr.number}: ${verdict.state} — ${verdict.description}`);
  return true;
}

export async function sweep({
  owner, name, token, fetchImpl = fetch, log = console.log,
  streaks = new Map(), cadence = new Map(), revisitEvery = 5, now = Date.now,
}) {
  const api = createApi({ token, fetchImpl });
  const written = [];
  const failed = [];
  const open = [];
  let after = null;

  // Collect every open PR before judging any: whether a head is shared is a
  // fact about the set, not about one PR.
  for (;;) {
    const data = await api.graphql(OPEN_PRS, { owner, name, after });
    const { nodes, pageInfo } = data.repository.pullRequests;
    open.push(...nodes);
    if (!pageInfo.hasNextPage) break;
    after = pageInfo.endCursor;
  }

  const shared = sharedHeads(open);
  let awaiting = 0;

  // Judge one head; returns 1 if it is still awaiting Codex's answer.
  async function judge(node) {
    const sharedHead = shared.has(node.headRefOid);
    // Read the head's status history first: its earliest entry, whatever
    // the context, is what a reaction has to be newer than — so this has to
    // happen before the reactions are judged rather than at write time.
    const { mine, firstSeen } = await codexStatuses(api, { owner, name, sha: node.headRefOid });
    // When this SHA became this PR's head. A push can move the PR onto a
    // commit that already existed — whose statuses and check suites date
    // from its FIRST life, before the previous head's 👍. A force-push
    // leaves a server-stamped timeline event, so every birth bound below is
    // floored at the last one; but a FAST-FORWARD to a pre-existing commit
    // (a stacked branch graduating, say) leaves no timeline event at all,
    // and for that transition the floor comes from the head's check suites
    // instead: the suite born on THIS PR's own branch is created when the
    // commit reaches the branch, so it dates the transition where the
    // timeline cannot. The force-push floor can never hide a genuine
    // signal — a 👍 about this head postdates its arrival by construction.
    // The suite floor is an UPPER bound of the arrival, and that is a
    // deliberate trade: if suite creation is delayed past a genuine 👍
    // (Actions minutes behind on the very push Codex answered in minutes —
    // incident territory, since both fan out from the same event), the 👍
    // reads stale and the gate holds at pending until a nudge or push
    // refreshes the verdict. Visible and recoverable, where the
    // alternative — trusting a 👍 older than every record of the arrival —
    // is the silent merge of a commit nobody reviewed. No record GitHub
    // keeps is guaranteed to precede a fast-forward arrival, so there is
    // no sound earlier floor to prefer.
    const movedAt = utc(node.timelineItems?.nodes?.[0]?.createdAt);
    let bound = laterOf(firstSeen, movedAt);
    // Don't read reactions when the shared head has already decided.
    let seen = sharedHead
      ? { approved: false, approvedAt: null, staleApproval: false, held: null, reading: false }
      : await reactionState(api, node.reactions, {
        owner, name, number: node.number, since: bound,
      });
    // A Codex 👍 in play — fresh-looking or stale — is the case where a
    // check-suite birth record can change the answer, in either direction:
    // a suite born on this branch AFTER the 👍 proves the head arrived by
    // fast-forward later than the statuses admit (the 👍 belongs to the
    // previous head, and trusting it merges a commit nobody reviewed), and
    // a suite born BEFORE a slow status rescues a genuine 👍 the statuses
    // date too late. Suites are fetched only in those two cases, so an
    // ordinary sweep never pays the call.
    let births = null;
    if (!sharedHead && (seen.approved || seen.staleApproval)) {
      births = await checkSuiteBirths(api, {
        owner, name, sha: node.headRefOid, branch: node.headRefName, since: movedAt,
      });
      // A same-repo head always earns a suite on its own branch within
      // seconds — this workflow's own pull_request_target run creates one
      // on the head SHA even where no other CI does — so a missing branch
      // suite means the transition cannot be dated yet: the signature of a
      // fast-forward onto a pre-stamped commit, judged in the gap before
      // its first branch suite. Whether the commit's OLD records are suites
      // or only statuses changes nothing, so no suites at all is the same
      // undatable gap, not a pass. Fail closed and let the next sweep read
      // the suite that is about to exist. Fork heads are undatable FOREVER
      // by this test — GitHub reports no head_branch for their suites — so
      // a fork PR's 👍 never opens this gate: their fast-forward transition
      // has no server-stamped record at all, and an earlier exemption that
      // fell back to the status bound was a fail-open hole wearing a
      // compatibility excuse. A fork contribution merges by an admin
      // override or by the owner re-pushing it to a same-repo branch,
      // where every floor applies.
      const undatable = births.forBranch === null;
      if (seen.approved) {
        if (births.forBranch !== null) {
          const confirmed = laterOf(bound, births.forBranch);
          if (confirmed !== bound) {
            bound = confirmed;
            seen = await reactionState(api, node.reactions, {
              owner, name, number: node.number, since: bound,
            });
          }
        } else if (undatable) {
          // The 👍's own time goes too: it may be the previous head's last
          // word, and treating it as an answer would read this head as
          // settled findings instead of an unanswered wait.
          seen = { ...seen, approved: false, approvedAt: null, staleApproval: true };
        }
      }
      // The rescue: a 👍 rejected purely for freshness may be genuine, with
      // the head merely dated too late by a slow external status. An
      // earlier birth record — the earliest suite, floored at both the
      // force-push event and the branch-born suite so a recycled commit's
      // old records cannot resurrect a previous life's approval — can lower
      // the bound and revive it. An undatable head gets no rescue: lowering
      // its bound with a foreign branch's suite is the same hole again.
      if (!undatable && !seen.approved && seen.staleApproval) {
        const better = laterOf(
          earlierOf(firstSeen, births.any),
          laterOf(movedAt, births.forBranch),
        );
        if (better !== null && (bound === null || better < bound)) {
          bound = better;
          seen = await reactionState(api, node.reactions, {
            owner, name, number: node.number, since: bound,
          });
        }
      }
    }
    const { approved, approvedAt, held, reading } = seen;
    // The finding and nudge streams are fetched whenever they could change
    // the answer. That includes an APPROVED head: an owner nudge newer than
    // the 👍 must reopen the wait, or auto-merge honors a verdict the owner
    // has already asked to be redone. Only a shared head, a hold, and a
    // read in progress settle without the walk; the `since` bound keeps it
    // to a page in practice.
    const undecided = !sharedHead && !held && !reading;
    let findings = false;
    let nudged = false;
    let nudgeAt = null;
    let reviewedAt = null;
    if (undecided) {
      reviewedAt = await codexReviewedAt(api, {
        owner, name, number: node.number, headRefOid: node.headRefOid,
      });
      // The comment walk is bounded by the EARLIER of the head commit's own
      // date and the head's first server-stamped status — then floored at
      // the head-moved time above. The commit date covers a finding that
      // lands before the first status write; the server timestamp covers a
      // commit date forged into the future, which would otherwise hide a
      // later owner nudge — on an approved head, that is a stale success
      // left open for auto-merge. Taking the earlier of the two only ever
      // admits more — see `commentSignals`.
      const bornAt = utc(node.commits?.nodes?.[0]?.commit?.committedDate);
      const walkSince = laterOf(earlierOf(bornAt, firstSeen), movedAt);
      const signals = await codexCommentSignals(api, {
        owner, name, number: node.number, since: walkSince,
      });
      const codexAt = signals.codexAt;
      nudgeAt = signals.nudgeAt;
      // Codex's last word on this head, wherever it was said: a review, a
      // comment, or — for a clean pass, which leaves neither — the 👍.
      const answeredAt = laterOf(reviewedAt, codexAt, approvedAt);
      // A nudge newer than Codex's last word reopens the wait: the answer
      // is due again, exactly as during a read — so the head counts as
      // awaiting and the clock runs. Deriving this from the comments on
      // EVERY sweep is what makes it robust: the state survives however
      // the run was started, including a nudge run replaced in the
      // concurrency queue by a grace-less successor. A TIE reopens it too:
      // GitHub stamps to the second, so a nudge in the same second as the
      // standing 👍 is an unresolved ordering, and the ambiguity must not
      // be what leaves a success open — only an answer strictly newer than
      // the ask settles it.
      nudged = Boolean(nudgeAt) && (answeredAt === null || nudgeAt >= answeredAt);
      findings = !approved && Boolean(answeredAt) && !nudged;
    }
    const verdict = verdictFor({
      isDraft: false, approved, sharedHead, held, reading, findings, nudged,
    });
    const changed = await publish(api, { owner, name, pr: node, verdict, current: mine[0], log });
    if (changed) written.push({ number: node.number, ...verdict });
    if (verdict.state !== "pending") return 0;
    // Eyes on means the review genuinely started; long reads are what the
    // loop is for, so a reading head never decays off the clock.
    if (reading) return 1;
    if (verdict.description === FINDINGS && reviewedAt) return 0;
    // Everything else pending is a wait for an answer that arrives within
    // minutes of the event that asked for it — the push for a fresh head,
    // the nudge for a re-review, the push for the 👍 a comment-only finding
    // may still be followed by. Past UNANSWERED_MINUTES with no answer,
    // Codex is not coming on its own: park the head (still `pending` for
    // the gate — nothing fails open) and let the next event or scheduled
    // sweep be the retry, instead of a forgotten PR keeping every loop
    // alive to its cap forever.
    //
    // The age is server-anchored on the NEWEST of the head-birth bound, the
    // owner's last nudge, and our own latest status write. The last one is
    // what keeps a just-arrived head with ancient records — the fast-forward
    // shapes above, whose bound predates the arrival by days — from being
    // parked on sight: its first sweep writes `pending`, and the write both
    // keeps this sweep's clock (a changed status is a state that just
    // moved) and anchors the next sweeps' age, so every head gets a full
    // UNANSWERED_MINUTES from the moment this loop first gated it. The
    // identical-write skip in `publish` means nothing refreshes the anchor
    // while the state stands still, so the window cannot self-extend.
    if (changed) return 1;
    let waitedSince = laterOf(bound, nudgeAt, utc(mine[0]?.created_at));
    if (!waitedSince) return 1;
    let ms = Date.parse(waitedSince);
    if (Number.isNaN(ms)) return 1;
    if (now() - ms < UNANSWERED_MINUTES * 60_000) return 1;
    // Looks expired — but a fast-forward can land a head already carrying
    // an identical old PENDING from a previous life, and then nothing
    // above refreshed the anchor: no reaction means the suites were never
    // read, publish skipped the identical write, and the head would park
    // on its first sweep, before Codex's pickup window even opens. One
    // suites call, paid only on this would-park path, re-anchors the age
    // on the branch-born suite — the arrival's own record.
    if (births === null && node.headRefName) {
      births = await checkSuiteBirths(api, {
        owner, name, sha: node.headRefOid, branch: node.headRefName, since: movedAt,
      });
    }
    waitedSince = laterOf(waitedSince, births?.forBranch);
    ms = Date.parse(waitedSince);
    return now() - ms >= UNANSWERED_MINUTES * 60_000 ? 0 : 1;
  }

  for (const node of open) {
    if (node.isDraft) {
      log(`#${node.number}: draft, skipped`);
      continue;
    }
    // Settled heads are re-read every `revisitEvery` sweeps, not every
    // minute. `cadence` is shared across the run's sweeps like `streaks`,
    // and this is the REST budget holding: judging a head costs several
    // calls, and a repo can hold many settled PRs beside the one the loop
    // is actually waiting on — rescanning all of them every 60s is what
    // would blow the 1,000-requests/hour GITHUB_TOKEN ceiling and turn
    // healthy heads unreadable.
    //
    // Three things bypass the slow path, because each is a change the very
    // next sweep must see: a new head (the SHA check), a still-awaiting or
    // failing head, and ANY activity on the PR — `updatedAt` moved. The
    // last one is what keeps a nudge honest while a loop is already
    // running: the nudge's own event run only QUEUES behind the concurrency
    // group, so the active loop is the one that has to notice, and an
    // approved head skipped for four more sweeps would leave a stale
    // success mergeable for minutes after the owner asked for a re-read. A
    // nudge is a comment, comments move `updatedAt`, and `updatedAt` rides
    // the PR list query already paid for — so noticing costs nothing. What
    // remains on the slow path is only the change that moves no timestamp
    // anywhere: a reaction added or removed, which was on the 15-minute
    // trickle before this PR existed and now waits at most `revisitEvery`
    // sweeps.
    const seen = cadence.get(node.number);
    const touched = utc(node.updatedAt);
    // Shared-head membership is topology on OTHER PRs: a duplicate closing
    // or moving away changes this head's verdict while its own SHA and
    // `updatedAt` stand still, and the event-triggered successor only
    // queues behind this run. The set is computed fresh each sweep from the
    // list query already paid for, so rejudging on a flip costs nothing —
    // without it a survivor sits on a stale shared-head failure for up to
    // `revisitEvery` sweeps.
    const sharedNow = shared.has(node.headRefOid);
    if (seen && seen.sha === node.headRefOid && !seen.awaiting && seen.updatedAt === touched
      && seen.shared === sharedNow) {
      seen.age += 1;
      if (seen.age < revisitEvery) continue;
      seen.age = 0;
    }
    // One head's failure must not end the run. The canonical case is real:
    // right after a force-push the statuses API can 422 ("no commit found")
    // until the new SHA replicates, and a run that dies on it goes red —
    // which notifies the owner — for something the next sweep repairs on
    // its own. So the failure is logged (the message carries the method,
    // path and code from `createApi`, never a token or a body), the head is
    // counted as awaiting so the minute loop itself is the retry, and the
    // other heads still get judged.
    //
    // Containment is not forgiveness. `streaks` is shared across the run's
    // sweeps (main passes one map to every call), and a head still failing
    // after MAX_FAIL_STREAK consecutive sweeps is a real error wearing a
    // transient's clothes — worse, its published status may be stale: a
    // `success` earned before a hold or a re-review request landed keeps
    // the gate OPEN while the failures shield it, because branch protection
    // consumes the status, not this job's color. So persistence does two
    // things, in order: best-effort, it writes UNREADABLE `pending` over
    // the head — failing closed; the write path is often alive when the
    // reads are not, and when the write is what is broken there was no
    // fresh success being published anyway — and then it throws, making the
    // run red so the owner is notified once and the queued successor takes
    // over.
    // Streaks are keyed by number AND head: a force-push resets the count,
    // so the expected transient 422 on the brand-new SHA is not read as the
    // old head's fifth consecutive failure.
    const streakKey = `${node.number}:${node.headRefOid}`;
    try {
      const a = await judge(node);
      awaiting += a;
      cadence.set(node.number, {
        sha: node.headRefOid, awaiting: a === 1, age: 0, updatedAt: touched, shared: sharedNow,
      });
      streaks.delete(streakKey);
    } catch (err) {
      const run = (streaks.get(streakKey) ?? 0) + 1;
      streaks.set(streakKey, run);
      // A failing head is retried every sweep, never put on the slow path.
      cadence.set(node.number, {
        sha: node.headRefOid, awaiting: true, age: 0, updatedAt: touched, shared: sharedNow,
      });
      if (run >= MAX_FAIL_STREAK) {
        try {
          await api.rest(`/repos/${owner}/${name}/statuses/${node.headRefOid}`, {
            method: "POST",
            body: { context: CONTEXT, state: "pending", description: UNREADABLE },
          });
          log(`#${node.number}: failed ${run} sweeps straight — status failed closed`);
        } catch (writeErr) {
          // The write path is down too, so there is no stale success being
          // refreshed either; the red run below is all that is left to say.
          log(`#${node.number}: could not fail the status closed (${writeErr.message})`);
        }
        throw new Error(
          `#${node.number}: still failing after ${run} consecutive sweeps (${err.message})`,
          { cause: err },
        );
      }
      log(`#${node.number}: sweep failed (${err.message}) — retrying next sweep`);
      awaiting += 1;
      failed.push(node.number);
    }
  }

  // `awaiting` is what the loop runs on: the fast clock only matters while
  // Codex's answer is still due. A PR at `success` merges by auto-merge with
  // nothing to poll for, and one held at `failure` changes only by the owner
  // removing the reaction — the throttled schedule covers both. A pending
  // verdict is the one state where the next minute can change the answer.
  return { written, awaiting, failed };
}

/**
 * Sweep repeatedly until `minutes` have elapsed, `intervalSeconds` apart.
 *
 * The scheduled trigger cannot be the clock: GitHub throttles it, and eight
 * measured hours delivered a median gap of 14 minutes against the 5 asked
 * for. So the job itself is the clock — one run polls every minute for most
 * of an hour, and the schedule only has to keep a run alive, which even a
 * throttled schedule does. The workflow's concurrency group holds the next
 * run queued while one loops, so the chain hands over without a gap and the
 * verdict lands within about a poll interval of Codex's reaction.
 *
 * Always sweeps at least once, so `minutes: 0` is the single pass a one-shot
 * invocation wants. An error escaping a sweep is deliberately NOT caught
 * here — the queued successor starts the moment this run dies, so the chain
 * self-heals in the same motion that reports the failure. But by then it is
 * a real failure: `sweep` contains per-head errors itself (a transient 422
 * on one write must not go red and notify the owner) and escalates only a
 * MAX_FAIL_STREAK-sweep persistence, after failing the head's status
 * closed — so what escapes is that, or a run that could not even list the
 * open PRs.
 */
export async function runLoop({
  minutes, intervalSeconds, sweepOnce, sleep, now = Date.now,
  shouldContinue = () => true,
}) {
  const start = now();
  const until = start + minutes * 60_000;
  for (;;) {
    const result = await sweepOnce();
    // The lean gate: the fast clock runs only while a verdict is still due,
    // so runner time is proportional to pushes (each opens a pending window
    // of a few minutes) rather than to how long PRs sit open — a PR waiting
    // overnight on a human costs nothing. Everything else changes only by
    // human action, and the hourly schedule's trickle covers it. Two
    // accepted costs, both bounded: the first verdict after a quiet stretch
    // waits for the schedule to start a run once, and a PR whose review
    // never arrives keeps the loop warm for UNANSWERED_MINUTES, once per
    // run, before parking to wait for a nudge.
    //
    // There is deliberately no settling grace here. Every wait state is
    // derived from data the sweep itself reads — Codex's 👀, an unanswered
    // head, a nudge newer than Codex's last word — so it holds on every
    // sweep however the run was started, and the sweep is the only writer
    // of the `codex` status, so there is no unordered write to outwait. An
    // earlier version graced event-triggered runs to bridge both gaps, and
    // the grace kept failing the same way: trigger-level state does not
    // survive replacement in the concurrency queue, and no fixed window
    // bounds a delayed Actions run.
    if (now() >= until || !shouldContinue(result)) return;
    await sleep(intervalSeconds * 1000);
  }
}

async function main() {
  const token = process.env.GITHUB_TOKEN;
  const slug = process.env.GITHUB_REPOSITORY;
  if (!token || !slug) throw new Error("GITHUB_TOKEN and GITHUB_REPOSITORY are required");
  const [owner, name] = slug.split("/");
  // One streak map and one cadence map for the whole run: a head failing
  // sweep after sweep accumulates toward MAX_FAIL_STREAK instead of
  // resetting every minute, and settled heads keep their revisit age.
  const streaks = new Map();
  const cadence = new Map();
  await runLoop({
    minutes: Number(process.env.SWEEP_LOOP_MINUTES ?? 0),
    intervalSeconds: Number(process.env.SWEEP_INTERVAL_SECONDS ?? 60),
    sweepOnce: () => sweep({ owner, name, token, streaks, cadence }),
    shouldContinue: ({ awaiting }) => awaiting > 0,
    sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  });
}

// Only run the sweep when invoked as a script, so the tests can import the
// pieces without making a single network call.
if (process.argv[1] && import.meta.url === `file://${process.argv[1]}`) {
  main().catch((err) => {
    console.error(err.message);
    process.exit(1);
  });
}
