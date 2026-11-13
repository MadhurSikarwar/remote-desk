// When to try again after a connection is lost, and when not to. Plain
// functions with no page dependencies so they can be tested with node.
(function (root) {
  "use strict";

  // Seconds to wait before attempts 1, 2, 3, ...; the last value repeats.
  const DELAYS = [2, 4, 8, 15];
  // Stop trying this long after the connection was lost.
  const GIVE_UP_AFTER = 120;

  function delaySeconds(attempt) {
    return DELAYS[Math.min(Math.max(attempt, 1), DELAYS.length) - 1];
  }

  // Whether another attempt may still be made `elapsed` seconds after the
  // loss, given the wait that would come before it.
  function mayTryAgain(elapsed, attempt) {
    return elapsed + delaySeconds(attempt) <= GIVE_UP_AFTER;
  }

  // Failures that mean "could not reach it, maybe later". Everything else ends
  // the attempts, in particular anything saying the device answering is not
  // the one that was asked for: that is never retried, and neither is a
  // device that does not speak the secure protocol.
  const REACH_FAILURES = [
    "cannot connect to",
    "could not reach the device",
    "cannot reach the server",
    "the server did not answer",
    "the server closed the connection",
    "lost the server",
  ];

  // A device that accepts the connection but is too slow to finish the secure
  // handshake (frozen, overloaded, asleep) is "not reachable yet"; one that
  // finishes it badly is not.
  const SLOW_ANSWER = ["timed out", "did not answer", "os error 10060"];

  function retryable(error) {
    const text = String(error).toLowerCase();
    if (text.includes("not the one")) return false;
    if (REACH_FAILURES.some((prefix) => text.includes(prefix))) return true;
    return text.includes("could not set up a secure connection") &&
      SLOW_ANSWER.some((phrase) => text.includes(phrase));
  }

  // The host's reasons for ending a session it only just began that mean
  // "not now" rather than "no": its old session for this viewer may not have
  // been noticed as dead yet, or the person there has just answered someone
  // else. A refusal ("declined", "did not answer") is final.
  const TRANSIENT_ENDS = ["busy with another viewer", "try again in a moment"];

  function transientEnd(reason) {
    const text = String(reason).toLowerCase();
    return TRANSIENT_ENDS.some((phrase) => text.includes(phrase));
  }

  const api = { delaySeconds, mayTryAgain, retryable, transientEnd, GIVE_UP_AFTER };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.Reconnect = api;
})(typeof window !== "undefined" ? window : globalThis);
