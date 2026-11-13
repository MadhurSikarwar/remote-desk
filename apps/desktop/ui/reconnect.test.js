const test = require("node:test");
const assert = require("node:assert/strict");
const { delaySeconds, mayTryAgain, retryable, transientEnd, GIVE_UP_AFTER } = require("./reconnect.js");

test("waits grow, then stay at the last value", () => {
  assert.deepEqual([1, 2, 3, 4, 5, 20].map(delaySeconds), [2, 4, 8, 15, 15, 15]);
  assert.equal(delaySeconds(0), 2);
});

test("attempts stop after the limit", () => {
  assert.equal(mayTryAgain(0, 1), true);
  assert.equal(mayTryAgain(GIVE_UP_AFTER - 15, 9), true);
  assert.equal(mayTryAgain(GIVE_UP_AFTER - 14, 9), false);
  assert.equal(mayTryAgain(GIVE_UP_AFTER, 1), false);
});

test("an unreachable device is retried", () => {
  for (const error of [
    "cannot connect to 127.0.0.1:5000: connection refused",
    "Could not reach the device (directly to 10.0.0.2:51000: timed out; through the server: the server did not answer)",
    "cannot reach the server at 1.2.3.4:443: timed out",
    "lost the server: reset",
    new Error("cannot connect to host: x"),
    "could not set up a secure connection: the other device did not answer in time",
    "could not set up a secure connection: A connection attempt failed (os error 10060)",
  ]) {
    assert.equal(retryable(error), true, String(error));
  }
});

test("a different device, or anything else, is never retried", () => {
  for (const error of [
    "The device at 10.0.0.2:51000 is not the one you picked: it has key AA but BB was expected",
    "The device reached through the server is not the one with that ID: it has key AA",
    "Could not reach the device (directly to x: The device reached x is not the one with that ID: y)",
    "could not set up a secure connection: bad handshake",
    "Already viewing a screen",
    "Enter the device ID or address shown by the sharing device",
    "",
    undefined,
  ]) {
    assert.equal(retryable(error), false, String(error));
  }
});

test("a host that is only busy is tried again; a refusal is final", () => {
  assert.equal(transientEnd("the host is busy with another viewer"), true);
  assert.equal(transientEnd("try again in a moment"), true);
  for (const reason of [
    "the host declined the request",
    "the host did not answer",
    "sharing stopped",
    "disconnected",
    "viewer disconnected",
    "",
    undefined,
  ]) {
    assert.equal(transientEnd(reason), false, String(reason));
  }
});
