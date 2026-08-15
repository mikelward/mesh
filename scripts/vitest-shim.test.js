// Tests for the vitest shim the two ported suites run on.
//
// A test helper's failure mode is a false pass: a matcher that forgets to
// assert reports every suite green forever, and nothing else in the tree would
// notice. So every case below is a pair -- the value that must pass, and one
// that must fail -- and the failing half is what earns the file. `describe` and
// `it` come from `node:test` directly rather than from the shim, so the thing
// under test is never also the thing running the test.
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { expect, it as shimIt } from "./vitest-shim.mjs";

/** Asserts that `fn` throws an AssertionError -- i.e. that the matcher bit. */
const fails = (fn) => assert.throws(fn, { name: "AssertionError" });

describe("expect", () => {
  it("compares by identity with toBe", () => {
    expect(1).toBe(1);
    expect("a").toBe("a");
    fails(() => expect(1).toBe(2));
    // The trap a `==` implementation would fall into.
    fails(() => expect("1").toBe(1));
  });

  it("distinguishes null from undefined with toBeNull", () => {
    expect(null).toBeNull();
    fails(() => expect(undefined).toBeNull());
    fails(() => expect(0).toBeNull());
    fails(() => expect({}).toBeNull());
  });

  it("compares structurally with toEqual", () => {
    expect({ a: [1, 2] }).toEqual({ a: [1, 2] });
    fails(() => expect({ a: 1 }).toEqual({ a: 2 }));
    // Deep-strict, so an extra key is a difference.
    fails(() => expect({ a: 1, b: 2 }).toEqual({ a: 1 }));
  });

  it("matches and refuses to match a pattern", () => {
    expect("hello").toMatch(/ell/);
    expect("hello").not.toMatch(/xyz/);
    fails(() => expect("hello").toMatch(/xyz/));
    fails(() => expect("hello").not.toMatch(/ell/));
  });

  it("checks containment with toContain", () => {
    expect("abcdef").toContain("cde");
    expect([1, 2, 3]).toContain(2);
    fails(() => expect("abcdef").toContain("xyz"));
    fails(() => expect([1, 2, 3]).toContain(9));
  });

  it("checks length with toHaveLength", () => {
    expect([1, 2]).toHaveLength(2);
    fails(() => expect([1, 2]).toHaveLength(3));
    // `String.match` with no hit returns null, which is the case this matcher
    // exists to catch in the workflow tests -- it must fail, not throw a
    // TypeError.
    fails(() => expect(null).toHaveLength(2));
  });

  it("compares numerically with toBeLessThan", () => {
    expect(1).toBeLessThan(2);
    fails(() => expect(2).toBeLessThan(2));
    fails(() => expect(3).toBeLessThan(2));
  });

  it("matches named fields only with toMatchObject", () => {
    expect({ a: 1, b: 2 }).toMatchObject({ a: 1 });
    expect({ a: { b: 1, c: 2 } }).toMatchObject({ a: { b: 1 } });
    fails(() => expect({ a: 1 }).toMatchObject({ a: 2 }));
    // A key the actual object does not have is a mismatch, not a field to
    // skip -- the whole point of the matcher is the fields you named.
    fails(() => expect({ a: 1 }).toMatchObject({ b: 1 }));
  });

  it("takes a message that replaces the default", () => {
    assert.throws(() => expect(1, "the count").toBe(2), /the count/);
  });

  describe("rejects.toThrow", () => {
    it("accepts a promise or a function returning one", async () => {
      await expect(Promise.reject(new Error("boom"))).rejects.toThrow(/boom/);
      await expect(() => Promise.reject(new Error("boom"))).rejects.toThrow("boom");
    });

    it("fails when the promise resolves instead", async () => {
      await assert.rejects(
        () => expect(Promise.resolve("fine")).rejects.toThrow(/boom/),
        { name: "AssertionError" },
      );
    });

    it("fails when the rejection says something else", async () => {
      await assert.rejects(
        () => expect(Promise.reject(new Error("boom"))).rejects.toThrow(/other/),
        { name: "AssertionError" },
      );
      await assert.rejects(
        () => expect(Promise.reject(new Error("boom"))).rejects.toThrow("other"),
        { name: "AssertionError" },
      );
    });
  });
});

describe("it", () => {
  // Both of these would be silently skipped if `skipIf` had its condition
  // inverted, so each body asserts something that would then never run --
  // node:test reports a skipped test, and the pair below reads as one
  // running and one skipped in the output.
  shimIt.skipIf(false)("runs when the condition is false", () => {
    expect(1).toBe(1);
  });

  shimIt.skipIf(true)("is skipped when the condition is true", () => {
    assert.fail("a skipIf(true) body must not run");
  });

  shimIt("takes vitest's trailing timeout argument", () => {
    expect(1).toBe(1);
  }, 5_000);
});
