// Finding 5: the window must not say a file was sent until the receiving end
// has confirmed that it arrived intact, and must say what really happened when
// it did not.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { createPage } = require("./fake_page.js");

const FILE = { name: "report.pdf", size: 1000, slice: () => ({ arrayBuffer: async () => new ArrayBuffer(1000) }) };

/** A transfer engine whose answer about the file changes as the test says. */
function engine(page) {
  const world = { state: "transferring", detail: null, finished: false };
  page.handlers.transfer_offer = async () => 7;
  page.handlers.transfer_chunk = async () => undefined;
  page.handlers.transfer_finish = async () => { world.finished = true; world.state = "awaiting_confirmation"; };
  page.handlers.transfer_cancel = async () => undefined;
  page.handlers.transfer_status = async () => ({
    items: [{ direction: "outgoing", id: 7, state: world.state, detail: world.detail }],
  });
  return world;
}

const note = (page) => page.el("transfer-message-viewer").textContent;

test("not 'Sent' while the receiver has not confirmed", async () => {
  const page = await createPage();
  const world = engine(page);
  const sending = window_sendFile(page);
  await page.sleep(500);
  assert.equal(world.finished, true, "the end of the file was never sent");
  assert.doesNotMatch(note(page), /^Sent /, `told the person it was sent too early: ${note(page)}`);
  assert.match(note(page), /confirm/i);

  world.state = "finished";
  world.detail = "delivered";
  await sending;
  assert.match(note(page), /confirmed/i);
});

test("a file the receiver reports damaged is not reported as sent", async () => {
  const page = await createPage();
  const world = engine(page);
  const sending = window_sendFile(page);
  await page.sleep(400);
  world.state = "cancelled";
  world.detail = "the file arrived damaged and was deleted";
  await sending;
  assert.match(note(page), /damaged/);
  assert.doesNotMatch(note(page), /confirmed|^Sent /);
});

test("a confirmation that never comes ends as a failure with the engine's reason", async () => {
  const page = await createPage();
  const world = engine(page);
  const sending = window_sendFile(page);
  await page.sleep(400);
  world.state = "failed";
  world.detail = "the other side did not confirm in time";
  await sending;
  assert.match(note(page), /did not confirm/);
});

function window_sendFile(page) {
  // The window exposes the function it uses for the file picker.
  return page.window.__sendFile("viewer", FILE);
}
