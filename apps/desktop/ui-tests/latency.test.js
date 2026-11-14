// Finding 11: the window used to show "capture to display" by subtracting the
// host's capture time from the viewer's own clock. Across two computers that
// is wrong by however far apart the clocks are. It must show only what can be
// measured honestly: the viewer's own decode time, how much later pictures
// arrive than they recently did (the clock offset cancels out), and, as a
// separate thing, the round trip.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { createPage, PACKET, packet } = require("./fake_page.js");

/** Feeds `count` pictures, one every `gapMs`, captured at (now + skewMs - lagMs). */
async function feed(page, { count, gapMs = 20, skewMs = 0, lag = () => 0 }) {
  for (let i = 0; i < count; i++) {
    page.deliver(page.frame(Date.now() + skewMs - lag(i), i === 0));
    await page.sleep(gapMs);
  }
}

async function started(skewMs = 0, decodeDelayMs = 0) {
  const page = await createPage();
  page.decodeDelayMs = decodeDelayMs;
  await page.connect({ allowControl: false });
  page.deliver(page.config());
  page.deliver(packet(PACKET.ACCEPTED));
  await page.sleep(10);
  return page;
}

const stats = (page) => {
  page.window.__showViewStats();
  return page.el("view-stats").textContent;
};

test("the window no longer claims a capture-to-display time", async () => {
  const page = await started();
  await feed(page, { count: 10 });
  assert.doesNotMatch(stats(page), /capture to display/i);
});

test("a host clock minutes off does not show up as latency", async () => {
  for (const skewMs of [-600_000, 0, +600_000]) {
    const page = await started(skewMs);
    page.window.__showViewStats(); // starts the interval
    await feed(page, { count: 40, skewMs });
    const text = stats(page);
    const numbers = [...text.matchAll(/(-?\d+(?:\.\d+)?) ms/g)].map((m) => Number(m[1]));
    assert.ok(numbers.every((n) => n >= 0 && n < 1000), `skew ${skewMs} ms gave: ${text}`);
  }
});

test("pictures arriving later than they recently did are shown, whatever the clock offset", async () => {
  for (const skewMs of [-600_000, +600_000]) {
    const page = await started(skewMs);
    // Steady for a while, then each picture is 300 ms later than before. The
    // figure averages the last second, so the late spell must last longer
    // than that (by the clock, not by a picture count: timers differ).
    await feed(page, { count: 25, skewMs });
    page.window.__showViewStats();
    const until = Date.now() + 1300;
    while (Date.now() < until) await feed(page, { count: 1, skewMs, lag: () => 300 });
    const text = stats(page);
    const extra = Number(/(\d+) ms later than/.exec(text)?.[1]);
    assert.ok(extra >= 200 && extra <= 400, `skew ${skewMs} ms: expected about 300 ms later, got: ${text}`);
  }
});

test("decode time is measured on the viewer's own clock", async () => {
  const page = await started(0, 40);
  page.window.__showViewStats();
  await feed(page, { count: 30 });
  await page.sleep(80);
  const text = stats(page);
  const decode = Number(/decoded in (\d+) ms/.exec(text)?.[1]);
  assert.ok(decode >= 30 && decode <= 120, `expected about 40 ms, got: ${text}`);
});

test("the round trip is a separate figure from the picture delays", async () => {
  const page = await started();
  page.handlers.view_capability_status = async () => ({
    clipboard_host_allows: false, clipboard_available: false, clipboard_active: false,
    clipboard_sent: 0, clipboard_received: 0, transfers: { items: [], allowed: false },
    link: { rtt_ms: 42, silent_ms: 10 },
  });
  await page.window.__showCapabilities();
  await feed(page, { count: 10 });
  const text = stats(page);
  assert.match(text, /Round trip to the host: 42 ms/);
  // Two figures in two sentences: the delays of the pictures, and the round trip.
  const [pictures, link] = text.split(". ");
  assert.match(pictures, /later than/);
  assert.doesNotMatch(pictures, /round trip/i);
  assert.match(link, /Round trip/);
  assert.doesNotMatch(link, /later than|decoded/);
});
