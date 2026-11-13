// Finding 10: after a connection to a typed address is lost, the automatic
// reconnect must insist on the device whose key was proven the first time. The
// host's own approval prompt and code comparison still apply to every attempt;
// this only stops a different device at the same address from being shown to
// the viewer as if nothing had changed.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { createPage, packet, PACKET, HOST } = require("./fake_page.js");


const connects = (page) => page.calls.filter((c) => c.command === "view_connect").map((c) => c.args);

test("the automatic reconnect to a typed address asks for the same key", async () => {
  const page = await createPage();
  page.handlers.view_connect = async () => HOST;
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  assert.equal(connects(page)[0].expectedKey ?? null, null, "the first connection pinned something");

  page.deliver(packet(PACKET.LOST, "connection lost: no answer"));
  await page.sleep(2600); // the first retry waits 2 s
  const all = connects(page);
  assert.equal(all.length >= 2, true, "no reconnect attempt was made");
  assert.equal(all[1].expectedKey, HOST.public_key);
});

test("a reconnect that finds another device stops and does not try again", async () => {
  const page = await createPage();
  let attempts = 0;
  page.handlers.view_connect = async () => {
    attempts += 1;
    if (attempts === 1) return HOST;
    throw new Error("The device at 192.168.1.20:5000 is not the one you picked: it has key AA but BB was expected");
  };
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  page.deliver(packet(PACKET.LOST, "connection lost: no answer"));
  await page.sleep(2800);

  assert.equal(attempts, 2);
  assert.match(page.el("view-stats").textContent, /Not trying again/);
  assert.equal(page.el("view-toggle").textContent, "Connect", "it is still reconnecting");
});

test("editing the address forgets the pinned key", async () => {
  const page = await createPage();
  page.handlers.view_connect = async () => HOST;
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  page.deliver(packet(PACKET.ENDED, "sharing stopped"));
  await page.sleep(20);

  page.el("view-port").value = "192.168.1.99:5000";
  page.fire("view-port", "input");
  page.calls.length = 0;
  page.fire("view-toggle", "click");
  await page.sleep(30);
  assert.equal(connects(page)[0].expectedKey ?? null, null);
});

test("a new connection chosen by the person is a new decision, not bound to the last key", async () => {
  const page = await createPage();
  page.handlers.view_connect = async () => HOST;
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  page.deliver(packet(PACKET.ENDED, "sharing stopped")); // ended on purpose
  await page.sleep(20);

  page.calls.length = 0;
  page.fire("view-toggle", "click"); // Connect again, same address
  await page.sleep(30);
  assert.equal(connects(page)[0].expectedKey ?? null, null);
});
