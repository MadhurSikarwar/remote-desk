// Finding 4: a mouse button pressed on the picture must not stay down on the
// shared computer when the viewer looks away, switches control off, or the
// browser cancels the press.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { createPage, packet, PACKET } = require("./fake_page.js");

const press = (page, button = 0, extra = {}) =>
  page.fire("view-canvas", "pointerdown", { button, pointerId: 1, clientX: 10, clientY: 20, ...extra });
const release = (page, button = 0, extra = {}) =>
  page.fire("view-canvas", "pointerup", { button, pointerId: 1, clientX: 10, clientY: 20, ...extra });
const buttons = (inputs) => inputs.filter((i) => i.kind === "button").map((i) => `${i.button}:${i.down ? "down" : "up"}`);

test("a press and a release send exactly a down and an up", async () => {
  const page = await createPage();
  await page.connect();
  press(page);
  release(page);
  await page.sleep(10);
  assert.deepEqual(buttons(page.inputs()), ["0:down", "0:up"]);
});

test("losing focus while a button is down lets it go, where the pointer was", async () => {
  const page = await createPage();
  await page.connect();
  press(page, 0, { clientX: 30, clientY: 40 });
  page.fire("view-canvas", "blur");
  await page.sleep(10);
  const sent = page.inputs().filter((i) => i.kind === "button");
  assert.deepEqual(buttons(sent), ["0:down", "0:up"]);
  assert.equal(sent[1].x, 0.3);
  assert.equal(sent[1].y, 0.4);
  // And only once: a later blur has nothing left to release.
  page.calls.length = 0;
  page.fire("view-canvas", "blur");
  await page.sleep(10);
  assert.deepEqual(page.inputs(), []);
});

test("switching control off in the middle of a press lets the button go at once", async () => {
  const page = await createPage();
  await page.connect();
  press(page, 2);
  page.calls.length = 0;
  page.el("view-control").checked = false;
  page.fire("view-control", "change");
  await page.sleep(10);
  assert.deepEqual(buttons(page.inputs()), ["2:up"]);
  // The browser's own pointerup afterwards adds nothing.
  page.calls.length = 0;
  release(page, 2);
  await page.sleep(10);
  assert.deepEqual(page.inputs(), []);
});

test("a cancelled press, and a pointer lost without a pointerup, let go", async () => {
  for (const type of ["pointercancel", "lostpointercapture"]) {
    const page = await createPage();
    await page.connect();
    press(page);
    page.calls.length = 0;
    page.fire("view-canvas", type, { pointerId: 1 });
    await page.sleep(10);
    assert.deepEqual(buttons(page.inputs()), ["0:up"], type);
  }
});

test("the lostpointercapture that follows a normal release sends nothing", async () => {
  const page = await createPage();
  await page.connect();
  press(page);
  release(page);
  page.calls.length = 0;
  page.fire("view-canvas", "lostpointercapture", { pointerId: 1 });
  await page.sleep(10);
  assert.deepEqual(page.inputs(), []);
});

test("several buttons held at once are all let go", async () => {
  const page = await createPage();
  await page.connect();
  press(page, 0);
  press(page, 2);
  page.calls.length = 0;
  page.fire("view-canvas", "blur");
  await page.sleep(10);
  assert.deepEqual(buttons(page.inputs()).sort(), ["0:up", "2:up"]);
});

test("when the session ends nothing is sent, and nothing stale reaches the next session", async () => {
  const page = await createPage();
  await page.connect();
  press(page);
  page.calls.length = 0;
  page.deliver(packet(PACKET.ENDED, "sharing stopped"));
  await page.sleep(20);
  assert.deepEqual(page.inputs(), [], "the ended session was sent a release");

  // A new session starts; looking away there must not release the old press.
  await page.connect();
  page.fire("view-canvas", "blur");
  await page.sleep(10);
  assert.deepEqual(page.inputs(), []);
});
