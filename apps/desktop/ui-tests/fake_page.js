// A fake page for ui/session.js, so its behaviour can be tested without a
// browser or a desktop: a small stand-in for the document and for the calls
// the window makes into the program (`invoke`), with the packets the program
// sends back (`channel`). It is not a browser: layout, real events and real
// timing are not tested here.
//
// Kept outside ui/ on purpose: everything in ui/ is built into the program.
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const SESSION = path.join(__dirname, "..", "ui", "session.js");
const RECONNECT = path.join(__dirname, "..", "ui", "reconnect.js");

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const PACKET = { CONFIG: 1, FRAME: 2, ENDED: 3, PERMISSIONS: 4, ACCEPTED: 5, LOST: 6 };
const packet = (type, text = "") => Uint8Array.from([type, ...Buffer.from(text)]);
const HOST = {
  name: "Host", fingerprint: "AAAA-BBBB-CCCC-DDDD", public_key: "ab".repeat(32),
  route: "directly", auth_code: "123 456",
};

function createPage() {
  const elements = {};

  function makeElement(id) {
    const store = {
      id, _listeners: {}, textContent: "", hidden: false, disabled: false, checked: false,
      value: "", classList: { add() {}, remove() {}, toggle() {} }, dataset: {}, style: {},
      options: [], children: [],
    };
    return new Proxy(store, {
      get(target, key) {
        if (key in target) return target[key];
        if (key === "addEventListener") {
          return (type, fn) => { (target._listeners[type] ||= []).push(fn); };
        }
        if (key === "getBoundingClientRect") return () => ({ left: 0, top: 0, width: 100, height: 100 });
        if (key === "getContext") return () => new Proxy({}, { get: () => () => {} });
        if (key === "querySelectorAll") return () => [];
        if (key === "then") return undefined;
        return () => {};
      },
      set(target, key, value) { target[key] = value; return true; },
    });
  }
  const el = (id) => (elements[id] ||= makeElement(id));
  const fire = (id, type, event = {}) =>
    (el(id)._listeners[type] || []).forEach((fn) => fn({ preventDefault() {}, ...event }));

  const page = { calls: [], handlers: {}, channels: [], el, fire, sleep, packet, PACKET, HOST };

  class Channel {
    set onmessage(fn) { this._fn = fn; page.channels.push(this); }
  }
  const invoke = async (command, args, options) => {
    page.calls.push({ command, args });
    if (page.handlers[command]) return page.handlers[command](args, options);
    if (command === "list_displays" || command === "list_remembered") return [];
    if (command === "share_status") return { active: false, items: [], transfers: null };
    return undefined;
  };

  class VideoDecoder {
    constructor() { this.decodeQueueSize = 0; }
    configure() {} decode() {} close() {}
  }
  const sandbox = {
    document: {
      getElementById: el, querySelector: el, querySelectorAll: () => [],
      createElement: () => makeElement("created"), addEventListener() {}, body: makeElement("body"),
    },
    VideoDecoder, EncodedVideoChunk: class {}, Channel,
    requestAnimationFrame: (fn) => setTimeout(fn, 0),
    setTimeout, clearTimeout,
    // The window's polling timers are not run: tests drive the calls they care about.
    setInterval: () => 0, clearInterval() {},
    performance, TextDecoder, TextEncoder, console, Date,
  };
  sandbox.window = sandbox;
  sandbox.__TAURI__ = { core: { invoke, Channel } };
  vm.createContext(sandbox);
  // The reconnect policy is a script of its own, loaded before session.js.
  vm.runInContext(fs.readFileSync(RECONNECT, "utf8"), sandbox, { filename: "reconnect.js" });
  vm.runInContext(fs.readFileSync(SESSION, "utf8"), sandbox, { filename: "session.js" });

  /** Inputs the window has sent to the program so far. */
  page.inputs = () => page.calls.filter((c) => c.command === "view_send_input").map((c) => c.args.input);
  /** The packets of the newest connection come through here. */
  page.deliver = (bytes) => page.channels[page.channels.length - 1]._fn(bytes);
  /** Connects to `HOST` and has it allow the session and control. */
  page.connect = async ({ allowControl = true } = {}) => {
    page.handlers.view_connect = async () => HOST;
    el("view-port").value = "127.0.0.1:5000";
    fire("view-toggle", "click");
    await sleep(30);
    page.deliver(packet(PACKET.ACCEPTED));
    if (allowControl) {
      page.deliver(Uint8Array.from([PACKET.PERMISSIONS, 1]));
      el("view-control").checked = true;
      fire("view-control", "change");
    }
    await sleep(10);
    page.calls.length = 0;
  };
  return sleep(30).then(() => page);
}

module.exports = { createPage, packet, PACKET, HOST, sleep };
