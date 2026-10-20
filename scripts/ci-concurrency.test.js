// @vitest-environment node
import { readFileSync } from 'node:fs';
import { describe, expect, it } from './vitest-shim.mjs';

// Pins ci.yml's workflow-level concurrency group. Its failure is silent: a
// group that cancels the wrong run leaves a required check canceled or
// missing on a head, and one that cancels nothing quietly doubles every
// push's runs; neither turns any job red. Read as text, after asserting the
// block is there at all, so a renamed key can't pass by matching nothing.

const workflow = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8');
const block = workflow.match(/^concurrency:\n((?: {2}.*\n)+)/m);

describe('ci.yml concurrency', () => {
  it('declares one workflow-level concurrency group', () => {
    expect(block === null).toBe(false);
    expect(workflow.match(/^concurrency:/gm).length).toBe(1);
  });

  it('groups a pull request by its number, every dispatch by the PR it names, and a push to main by commit', () => {
    expect(block[1]).toContain(
      "  group: ci-${{ github.event_name == 'push' && format('main-{0}', github.sha) || github.event.pull_request.number || inputs.pr || github.ref }}\n",
    );
  });

  it('cancels on a push, open or reopen, never on a push to main or an edit', () => {
    // An `edited` run carries the head as of the edit, and GitHub doesn't
    // promise to start runs in event order, so a canceling edit could cancel
    // the only run on a newer head; it queues instead.
    expect(block[1]).toContain("  cancel-in-progress: ${{ github.event_name != 'push' && github.event.action != 'edited' }}\n");
  });

  it('still runs on an edit and a push, so a retarget re-runs the lane', () => {
    expect(workflow).toMatch(/types: \[opened, synchronize, reopened, edited\]/);
  });
});
