// Finding 6: the program can report that a session ended (a refusal, a
// disconnect) before the call that started it has returned. The window must
// not then bring back the Disconnect button and the host panel for a session
// that is over, whether the connect was a first one or a reconnect attempt.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { createPage, packet, PACKET, HOST } = require("./fake_page.js");

/** A view_connect the test finishes by hand. */
function pendingConnect(page) {
  const waiting = [];
  page.handlers.view_connect = () => new Promise((resolve, reject) => waiting.push({ resolve, reject }));
  return waiting;
}

const consistentlyEnded = (page) => {
  assert.equal(page.el("view-toggle").textContent, "Connect");
  assert.equal(page.el("view-host").hidden, true, "the host panel is shown for a session that is over");
  assert.equal(page.el("view-port").disabled, false);
};

test("a refusal that arrives before the connect call returns leaves a consistent window", async () => {
  const page = await createPage();
  const waiting = pendingConnect(page);
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(20);

  page.deliver(packet(PACKET.ENDED, "the host is busy with another viewer"));
  await page.sleep(10);
  waiting[0].resolve(HOST);
  await page.sleep(30);

  assert.match(page.el("view-stats").textContent, /Ended: the host is busy/);
  consistentlyEnded(page);
});

test("disconnecting while the connect call is still out leaves a consistent window", async () => {
  const page = await createPage();
  const waiting = pendingConnect(page);
  page.handlers.view_disconnect = async () => { page.deliver(packet(PACKET.ENDED, "disconnected")); };
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(20);

  page.fire("view-toggle", "click"); // Disconnect while connecting
  await page.sleep(20);
  waiting[0].resolve(HOST);
  await page.sleep(30);

  consistentlyEnded(page);
});

test("packets of a connection that has been replaced are ignored", async () => {
  const page = await createPage();
  page.handlers.view_connect = async () => HOST;
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  const first = page.channels[0];
  first._fn(packet(PACKET.ENDED, "sharing stopped"));
  await page.sleep(10);

  page.fire("view-toggle", "click"); // connect again
  await page.sleep(30);
  assert.equal(page.channels.length, 2);
  // A late packet of the first connection must not end the second.
  first._fn(packet(PACKET.ENDED, "late news from the old connection"));
  await page.sleep(10);
  assert.equal(page.el("view-toggle").textContent, "Disconnect");
  assert.doesNotMatch(page.el("view-stats").textContent, /late news/);
});

test("a busy host during a reconnect attempt means trying again, with the window saying so", async () => {
  const page = await createPage();
  page.handlers.view_connect = async () => HOST;
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  page.deliver(packet(PACKET.LOST, "connection lost: no answer"));

  const waiting = pendingConnect(page);
  await page.sleep(2400); // the first retry starts after 2 s
  assert.equal(waiting.length, 1, "no reconnect attempt was made");
  page.deliver(packet(PACKET.ENDED, "the host is busy with another viewer"));
  await page.sleep(10);
  waiting[0].resolve(HOST);
  await page.sleep(30);

  assert.equal(page.el("view-toggle").textContent, "Stop reconnecting");
  assert.match(page.el("view-stats").textContent, /Trying again in/);
  assert.equal(page.el("view-host").hidden, true);
});

test("stopping the reconnect while an attempt is out ends that attempt and keeps the message", async () => {
  const page = await createPage();
  page.handlers.view_connect = async () => HOST;
  page.handlers.view_disconnect = async () => undefined;
  page.el("view-port").value = "192.168.1.20:5000";
  page.fire("view-toggle", "click");
  await page.sleep(30);
  page.deliver(packet(PACKET.LOST, "connection lost: no answer"));

  const waiting = pendingConnect(page);
  await page.sleep(2400);
  assert.equal(waiting.length, 1);
  page.fire("view-toggle", "click"); // Stop reconnecting
  await page.sleep(10);
  page.calls.length = 0;
  waiting[0].resolve(HOST);
  await page.sleep(40);

  assert.equal(page.el("view-stats").textContent, "Reconnecting stopped.");
  consistentlyEnded(page);
  assert.equal(page.calls.filter((c) => c.command === "view_disconnect").length, 1,
    "the connection that was made after the person stopped was left open");
});
