// @vitest-environment node
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from './vitest-shim.mjs';

// Runs the real script against real shallow clones. Its failure mode is a
// false pass — a script that silently does nothing looks identical to one that
// worked — so these assert the resulting repository, never the script's text.

const SCRIPT = new URL('./unshallow.sh', import.meta.url).pathname;
const git = (cwd, ...args) => execFileSync('git', args, { cwd, encoding: 'utf8' }).trim();
const run = (cwd) => execFileSync('sh', [SCRIPT], { cwd, encoding: 'utf8' });

const roots = [];
afterEach(() => {
  for (const r of roots.splice(0)) rmSync(r, { recursive: true, force: true });
});

/** An origin with `commits` commits, and a clone of it at `depth`. */
function fixture({ commits = 4, depth = 1 } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'unshallow-'));
  roots.push(root);
  const origin = join(root, 'origin');
  const clone = join(root, 'clone');

  execFileSync('git', ['init', '--quiet', '--initial-branch=main', origin]);
  git(origin, 'config', 'user.email', 'test@example.com');
  git(origin, 'config', 'user.name', 'Test');
  for (let i = 0; i < commits; i += 1) {
    git(origin, 'commit', '--quiet', '--allow-empty', '-m', `commit ${i}`);
  }
  execFileSync('git', ['clone', '--quiet', '--depth', String(depth), `file://${origin}`, clone]);
  return { root, origin, clone };
}

const isShallow = (cwd) => git(cwd, 'rev-parse', '--is-shallow-repository') === 'true';

const which = (tool) => execFileSync('which', [tool], { encoding: 'utf8' }).trim();

// Null rather than a throw when a tool is absent, so the suite can skip a
// branch-specific test on hosts the script itself supports without that
// tool — a BusyBox image has `timeout` but no perl.
const toolPath = (tool) => {
  try {
    return which(tool);
  } catch {
    return null;
  }
};
const PERL = toolPath('perl');

const HOLD = '/bin/sleep 60';
// `trap '' TERM` then `exec`: an ignored disposition survives the exec, so
// this is a transport that shrugs off SIGTERM — which a real one can, and
// which plain `sleep` never does, so nothing else here can show the
// difference between signaling and killing.
const HOLD_IGNORING_TERM = `/bin/sh -c "trap '' TERM; exec /bin/sleep 60"`;

/**
 * A shallow clone whose `git` hangs on `fetch` and delegates everything else,
 * plus the `tried` path it touches first — which is what tells a fetch that
 * was bounded and killed apart from one that was never attempted.
 */
function hangingGit({ commits = 3, ignoresTerm = false } = {}) {
  const made = fixture({ commits, depth: 1 });
  const bin = join(made.root, 'bin');
  const tried = join(made.root, 'tried');
  execFileSync('mkdir', ['-p', bin]);
  writeFileSync(
    join(bin, 'git'),
    // Models what `git fetch` actually does: spawn a transport (ssh,
    // git-remote-https) that inherits our stdout, and wait on it. Killing
    // only the parent leaves that child holding the pipe, so a caller
    // capturing output blocks for the full sleep however promptly the
    // deadline fired — which is the bug this fixture has to be able to show.
    // An earlier version `exec`ed the sleep, making parent and child one
    // process, and could not distinguish killing a process from killing a
    // process group.
    //
    // `/bin/sleep` by absolute path because these tests narrow PATH to hide
    // `timeout`, which hides `sleep` with it.
    '#!/bin/sh\n'
    + `for a in "$@"; do [ "$a" = fetch ] && { : > ${tried}; ${ignoresTerm ? HOLD_IGNORING_TERM : HOLD} & wait; exit 0; }; done\n`
    + 'exec "$REAL_GIT" "$@"\n',
    { mode: 0o755 },
  );
  return { ...made, bin, tried };
}

const runWith = (cwd, path) => execFileSync('/bin/sh', [SCRIPT], {
  cwd,
  encoding: 'utf8',
  stdio: ['pipe', 'pipe', 'pipe'],
  env: { ...process.env, PATH: path, REAL_GIT: which('git'), UNSHALLOW_TIMEOUT: '1' },
});

describe('unshallow', () => {
  it('deepens a shallow clone to its full history', () => {
    const { clone } = fixture({ commits: 4, depth: 1 });
    expect(isShallow(clone)).toBe(true);
    expect(git(clone, 'rev-list', '--count', 'HEAD')).toBe('1');

    const out = run(clone);

    expect(isShallow(clone)).toBe(false);
    expect(git(clone, 'rev-list', '--count', 'HEAD')).toBe('4');
    expect(out).toMatch(/deepened to 4 commits/);
  });

  it('is a no-op on a complete clone', () => {
    const { origin } = fixture();
    expect(run(origin)).toMatch(/already complete/);
    expect(isShallow(origin)).toBe(false);
  });

  it('gives up on a fetch that hangs, rather than holding the session open', () => {
    // The failure this guards is a session that never starts: at session
    // start a fetch with no deadline waits on an unreachable remote (or a
    // credential prompt nobody can answer) and `|| true` is only reached
    // once it returns. The deadline is what ends it.
    const { clone, bin } = hangingGit({ commits: 3 });

    const started = Date.now();
    const out = runWith(clone, `${bin}:${process.env.PATH}`);

    expect(Date.now() - started).toBeLessThan(20_000);
    expect(out).toBe('');
    expect(isShallow(clone)).toBe(true);
  }, 25_000);

  // Skipped, not failed, where perl is genuinely absent (a BusyBox image):
  // that host exercises the timeout branch above instead, and a suite that
  // cannot run there would fail on an environment the script supports.
  it.skipIf(!PERL)('bounds the fetch with perl where timeout is absent — a default macOS', () => {
    // The fallback has to keep the guard, not just the feature: a bare fetch
    // on a host without coreutils is exactly the unbounded hang this exists
    // to prevent. PATH is narrowed to the stub and perl, so `timeout` and
    // `gtimeout` are genuinely unreachable.
    const { clone, root, bin, tried } = hangingGit({ commits: 3 });
    const perlOnly = join(root, 'perlonly');
    execFileSync('mkdir', ['-p', perlOnly]);
    execFileSync('ln', ['-s', PERL, join(perlOnly, 'perl')]);

    const started = Date.now();
    const out = runWith(clone, `${bin}:${perlOnly}`);

    expect(Date.now() - started).toBeLessThan(20_000);
    expect(out).toBe('');
    expect(isShallow(clone)).toBe(true);
    // The distinction that matters: attempted and cut short, not skipped.
    expect(existsSync(tried)).toBe(true);
  }, 25_000);

  it('escalates to KILL for a transport that ignores TERM', () => {
    // `timeout` signals once and waits; a child that ignores the signal keeps
    // the pipe and the session stays blocked past the deadline anyway. Only a
    // TERM-proof child can tell that apart from a working deadline.
    const { clone, bin } = hangingGit({ commits: 3, ignoresTerm: true });

    const started = Date.now();
    const out = runWith(clone, `${bin}:${process.env.PATH}`);

    expect(Date.now() - started).toBeLessThan(30_000);
    expect(out).toBe('');
    expect(isShallow(clone)).toBe(true);
  }, 35_000);

  it('skips the fetch entirely when nothing can bound it', () => {
    // Better a shallow clone with a warning — the state this script already
    // documents — than a session that never starts.
    const { clone, bin, tried } = hangingGit({ commits: 3 });

    const started = Date.now();
    const out = runWith(clone, bin);

    expect(Date.now() - started).toBeLessThan(20_000);
    expect(out).toBe('');
    expect(isShallow(clone)).toBe(true);
    expect(existsSync(tried)).toBe(false);
  });

  it('passes every auth-related variable through untouched', () => {
    // Six review rounds each found another configuration route a "fill in a
    // safe default" branch would override — GIT_SSH_COMMAND, GIT_SSH,
    // core.sshCommand, core.askPass, SSH_ASKPASS. The class only closes by
    // the script modifying nothing auth-related, which is what this asserts.
    const { clone, root } = fixture({ commits: 3, depth: 1 });
    const bin = join(root, 'bin');
    const seen = join(root, 'seen');
    execFileSync('mkdir', ['-p', bin]);
    writeFileSync(
      join(bin, 'git'),
      '#!/bin/sh\n'
      + 'for a in "$@"; do\n'
      + `  [ "$a" = fetch ] && { echo "$GIT_ASKPASS|$GIT_SSH_COMMAND|$GIT_SSH|$SSH_ASKPASS" > ${seen}; exit 0; }\n`
      + 'done\n'
      + 'exec "$REAL_GIT" "$@"\n',
      { mode: 0o755 },
    );

    execFileSync('sh', [SCRIPT], {
      cwd: clone,
      encoding: 'utf8',
      stdio: ['pipe', 'pipe', 'pipe'],
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        REAL_GIT: which('git'),
        GIT_ASKPASS: '/my/helper',
        GIT_SSH_COMMAND: 'env SSH_AUTH_SOCK=/s ssh -i /my/key',
        GIT_SSH: '/my/wrapper',
        SSH_ASKPASS: '/my/ssh-askpass',
      },
    });

    expect(readFileSync(seen, 'utf8').trim())
      .toBe('/my/helper|env SSH_AUTH_SOCK=/s ssh -i /my/key|/my/wrapper|/my/ssh-askpass');
  });

  it('sets nothing auth-related when the checkout has nothing, only the no-prompt switch', () => {
    // The stock case gets no "safe defaults" either: an unset variable that
    // stays unset is what guarantees no configured route — env, git config,
    // or ssh's own — can be shadowed. GIT_TERMINAL_PROMPT is the one export,
    // and it disables the interactive prompt without displacing any helper.
    const { clone, root } = fixture({ commits: 3, depth: 1 });
    const bin = join(root, 'bin');
    const seen = join(root, 'seen');
    execFileSync('mkdir', ['-p', bin]);
    writeFileSync(
      join(bin, 'git'),
      '#!/bin/sh\n'
      + 'for a in "$@"; do\n'
      + `  [ "$a" = fetch ] && { echo "$GIT_ASKPASS|$GIT_SSH_COMMAND|$GIT_SSH|$GIT_TERMINAL_PROMPT" > ${seen}; exit 0; }\n`
      + 'done\n'
      + 'exec "$REAL_GIT" "$@"\n',
      { mode: 0o755 },
    );

    const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, REAL_GIT: which('git') };
    delete env.GIT_ASKPASS;
    delete env.GIT_SSH_COMMAND;
    delete env.GIT_SSH;
    execFileSync('sh', [SCRIPT], { cwd: clone, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'], env });

    expect(readFileSync(seen, 'utf8').trim()).toBe('|||0');
  });

  it('keeps the fetch log owner-only while it exists', () => {
    // `.git` is commonly 0755 and a redirect creates under the process
    // umask, so without the owner-only creation the log's fetch diagnostics
    // are world-readable for the fetch's duration on a multi-user host. The
    // stub observes the mode *during* the fetch — the trap deletes the log
    // on exit, so afterwards there is nothing for Node to stat (the test
    // process is blocked in execFileSync the whole time). The two stat
    // dialects are tried in failure-cleanliness order: GNU/BusyBox `-c`
    // first because it fails hard on BSD (illegal option), BSD `-f` second
    // because on GNU that flag is *valid with a different meaning* — put
    // the ambiguous form last and it never gets a vote.
    const { clone, root } = fixture({ commits: 3, depth: 1 });
    const bin = join(root, 'bin');
    const seen = join(root, 'seen');
    execFileSync('mkdir', ['-p', bin]);
    writeFileSync(
      join(bin, 'git'),
      '#!/bin/sh\n'
      + 'for a in "$@"; do\n'
      + '  [ "$a" = fetch ] && {\n'
      + '    for f in .git/unshallow-log.*; do\n'
      + `      { stat -c %a "$f" 2>/dev/null || stat -f %Lp "$f"; } > ${seen}\n`
      + '    done\n'
      + '    exit 0\n'
      + '  }\n'
      + 'done\n'
      + 'exec "$REAL_GIT" "$@"\n',
      { mode: 0o755 },
    );

    const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, REAL_GIT: which('git') };
    execFileSync('sh', [SCRIPT], { cwd: clone, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'], env });

    expect(readFileSync(seen, 'utf8').trim()).toBe('600');
  });

  it('warns and exits 0 when the remote is gone, rather than failing the session', () => {
    // The session must still start; a wrong commit count is the thing to
    // shout about, not a reason to refuse to work.
    const { origin, clone } = fixture({ commits: 3, depth: 1 });
    rmSync(origin, { recursive: true, force: true });

    const out = execFileSync('sh', [SCRIPT], { cwd: clone, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });

    expect(out).toBe('');
    expect(isShallow(clone)).toBe(true);
  });
});
