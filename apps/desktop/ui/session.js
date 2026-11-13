"use strict";

// Screen sharing (host) and viewing (viewer) panels.
(() => {
  const { invoke, Channel } = window.__TAURI__.core;
  const $ = (id) => document.getElementById(id);
  const message = (error) => String(error?.message ?? error);

  // ---- Host: share this screen -------------------------------------------

  const pill = $("sharing");
  const shareDisplay = $("share-display");
  const shareToggle = $("share-toggle");
  const shareStats = $("share-stats");
  const shareInput = $("share-input");
  const shareClipboard = $("share-clipboard");
  const shareFiles = $("share-files");

  // ---- Files: shown and driven the same way at both ends -------------------

  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const humanSize = (bytes) => {
    if (bytes < 1024) return `${bytes} B`;
    const units = ["KB", "MB", "GB"];
    let value = bytes / 1024;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
    return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
  };

  const lastTransferSignature = { host: "", viewer: "" };

  /** Shows a transfer list. Everything comes from the other device and is
   *  inserted as text, never as markup. */
  function renderTransfers(role, status) {
    const list = $(`transfer-list-${role}`);
    const items = status ? status.items : [];
    const signature = JSON.stringify(items.map(({ seconds_left, ...rest }) => rest));
    $(`transfer-folder-${role}`).textContent = status
      ? `Received files are saved in ${status.folder}` : "";
    for (const span of list.querySelectorAll("[data-countdown]")) {
      const item = items.find((i) => String(i.id) === span.dataset.countdown && i.direction === "incoming");
      if (item?.seconds_left != null) span.textContent = ` (refused automatically in ${item.seconds_left} s)`;
    }
    if (signature === lastTransferSignature[role]) return;
    lastTransferSignature[role] = signature;

    const answer = (id, accept) => invoke("transfer_answer", { role, id, accept }).catch((e) => {
      $(`transfer-message-${role}`).textContent = message(e);
    });
    const cancel = (item) => invoke("transfer_cancel", { role, direction: item.direction, id: item.id });
    const button = (label, handler, secondary) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = label;
      if (secondary) b.className = "secondary";
      b.addEventListener("click", handler);
      return b;
    };

    list.replaceChildren(...items.slice().reverse().map((item) => {
      const li = document.createElement("li");
      const incoming = item.direction === "incoming";
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = `${incoming ? "↓" : "↑"} ${item.name} (${humanSize(item.size)})`;
      li.append(name);
      const detail = document.createElement("span");
      detail.className = "detail";
      switch (item.state) {
        case "waiting_for_you": {
          li.className = "waiting";
          const countdown = document.createElement("span");
          countdown.dataset.countdown = String(item.id);
          countdown.className = "muted";
          countdown.textContent = ` (refused automatically in ${item.seconds_left ?? 0} s)`;
          li.append(
            document.createTextNode("is being sent to you"),
            button("Accept", () => answer(item.id, true)),
            button("Refuse", () => answer(item.id, false), true),
            countdown,
          );
          break;
        }
        case "waiting_for_peer":
          li.append(document.createTextNode("waiting for the other side to accept…"), button("Cancel", () => cancel(item), true));
          break;
        case "transferring": {
          const bar = document.createElement("progress");
          bar.max = Math.max(item.size, 1);
          bar.value = item.done;
          li.append(bar, button("Cancel", () => cancel(item), true));
          if (item.detail) { detail.textContent = item.detail; li.append(detail); }
          break;
        }
        default:
          detail.textContent = `${item.state.replace("_", " ")}${item.detail ? " — " + item.detail : ""}`;
          li.append(detail);
      }
      return li;
    }));
  }

  /** Offers a file the person picked, waits for the other side to accept,
   *  then sends it in pieces. Only this file's bytes are ever read. */
  async function sendFile(role, file) {
    const note = $(`transfer-message-${role}`);
    note.textContent = `Offering ${file.name}…`;
    let id;
    try {
      id = await invoke("transfer_offer", { role, name: file.name, size: file.size });
      const give_up = Date.now() + 75_000;
      for (;;) {
        const status = await invoke("transfer_status", { role });
        const item = status.items.find((i) => i.direction === "outgoing" && i.id === id);
        if (!item) throw new Error("The transfer disappeared");
        if (item.state === "transferring") break;
        if (["refused", "cancelled", "failed"].includes(item.state)) {
          throw new Error(item.detail ?? "The other side did not accept");
        }
        if (Date.now() > give_up) throw new Error("Nobody accepted the file in time");
        await sleep(300);
      }
      const piece = 256 * 1024;
      for (let offset = 0; offset < file.size; offset += piece) {
        const bytes = new Uint8Array(await file.slice(offset, offset + piece).arrayBuffer());
        await invoke("transfer_chunk", bytes, { headers: { "x-role": role, "x-id": String(id) } });
      }
      await invoke("transfer_finish", { role, id });
      note.textContent = `Sent ${file.name}.`;
    } catch (error) {
      note.textContent = `${file.name}: ${message(error)}`;
      if (id !== undefined) invoke("transfer_cancel", { role, direction: "outgoing", id }).catch(() => {});
    }
  }
  window.__sendFile = sendFile;

  for (const role of ["host", "viewer"]) {
    const input = $(role === "host" ? "host-file" : "view-file");
    input.addEventListener("change", async () => {
      const files = Array.from(input.files);
      input.value = "";
      for (const file of files) await sendFile(role, file);
    });
  }
  let shareTimer = null;

  // ---- Host: a viewer asking to be let in ---------------------------------

  const approval = $("approval");
  let shownRequest = null;

  function showApproval(request) {
    approval.hidden = !request;
    if (!request) {
      shownRequest = null;
      return;
    }
    $("approval-timer").textContent = `Denied automatically in ${request.seconds_left} s`;
    if (shownRequest === request.id) return;
    shownRequest = request.id;
    // Everything here comes from the other device; it is shown as text only.
    $("approval-title").textContent = `"${request.name}" wants to view this screen`;
    $("approval-fingerprint").textContent = request.fingerprint;
    $("approval-known").textContent = request.known_since
      ? `Yes — you allowed this key on ${new Date(request.known_since * 1000).toLocaleDateString()}`
      : "No — this key is new to this device";
    $("approval-code").textContent = request.auth_code;
    $("approval-remember").checked = false;
    approval.scrollIntoView({ block: "nearest" });
  }

  async function answerApproval(allow) {
    if (shownRequest === null) return;
    const remember = allow && $("approval-remember").checked;
    try {
      showShare(await invoke("answer_approval", { id: shownRequest, allow, remember }));
      if (remember) showRemembered();
    } catch (error) {
      shareStats.textContent = message(error);
      shareStats.classList.add("error");
    }
  }

  $("approval-allow").addEventListener("click", () => answerApproval(true));
  $("approval-deny").addEventListener("click", () => answerApproval(false));

  // ---- Remembered devices --------------------------------------------------

  async function showRemembered() {
    const list = $("remembered");
    const devices = await invoke("list_remembered");
    $("remembered-empty").hidden = devices.length > 0;
    list.replaceChildren(...devices.map((device) => {
      const item = document.createElement("li");
      const label = document.createElement("span");
      label.textContent =
        `${device.name} — ${device.fingerprint} — allowed ` +
        new Date(device.approved_at * 1000).toLocaleDateString();
      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "secondary";
      forget.textContent = "Forget";
      forget.addEventListener("click", async () => {
        await invoke("forget_remembered", { fingerprint: device.fingerprint }).catch(() => {});
        showRemembered();
      });
      item.append(label, forget);
      return item;
    }));
  }

  function showShare(status) {
    const live = status.active && status.viewer_connected;
    showApproval(status.pending);
    // Everything the viewer may currently do is named in the badge.
    const allowed = [
      status.input_allowed && "viewer has control",
      status.clipboard_allowed && "clipboard sync",
      status.files_allowed && "file transfer",
    ].filter(Boolean);
    pill.textContent = live
      ? `Sharing screen${allowed.length ? " — " + allowed.join(", ") : ""}`
      : status.pending
        ? "A viewer is waiting for your answer"
        : status.active
          ? status.remote ? "Sharing on — open to the network" : "Sharing on — no viewer"
          : "Not sharing";
    pill.classList.toggle("live", live);
    pill.classList.toggle("armed", status.active && !live);

    shareToggle.textContent = status.active ? "Stop sharing" : "Start sharing";
    shareToggle.dataset.active = String(status.active);
    shareDisplay.disabled = status.active;
    shareInput.disabled = !status.active;
    shareInput.checked = status.input_allowed;
    shareClipboard.disabled = !status.active;
    shareClipboard.checked = status.clipboard_allowed;
    shareFiles.disabled = !status.active;
    shareFiles.checked = status.files_allowed;
    $("transfers-host").hidden = !(status.active && status.viewer_connected && status.transfers);
    $("host-file").disabled = !status.files_allowed;
    renderTransfers("host", status.transfers);

    const sent = `${status.frames} frames, ${(status.bytes / 1e6).toFixed(1)} MB sent` +
      (status.input_events > 0 ? `, ${status.input_events} input events carried out` : "") +
      (describeLink(status.link, "the viewer") ? `. ${describeLink(status.link, "the viewer")}` : "") +
      (status.clipboard_sent + status.clipboard_received > 0
        ? `, clipboard: ${status.clipboard_sent} sent, ${status.clipboard_received} received` : "");
    shareStats.classList.toggle("error", Boolean(status.error));
    if (status.error) {
      shareStats.textContent = status.error;
    } else if (!status.active) {
      shareStats.textContent = status.frames > 0 ? `Stopped. ${sent}.` : "";
    } else if (status.blocked) {
      shareStats.textContent = "Waiting: the lock screen or a system prompt is showing.";
    } else if (live) {
      // Said plainly when the video is being slowed for the connection.
      const pace = status.congestion;
      const slowed = !pace ? ""
        : pace.holding
          ? ` Holding video back: the connection is ${(pace.delay_ms / 1000).toFixed(1)} s behind.`
          : pace.bitrate_kbps < pace.max_kbps
            ? ` Video limited to ${(pace.bitrate_kbps / 1000).toFixed(1)} of ${(pace.max_kbps / 1000).toFixed(1)} Mbit/s for this connection.`
            : "";
      shareStats.textContent = `Port ${status.port}. ${status.width}×${status.height} — ${sent}.${slowed}`;
    } else {
      shareStats.textContent = status.remote
        ? `Port ${status.port}. Waiting for a viewer.`
        : `Port ${status.port}. Waiting for a viewer on this computer.`;
    }

    const remote = $("share-remote");
    remote.hidden = !(status.active && status.remote);
    if (!remote.hidden) {
      const where = status.lan_address ? `${status.lan_address}:${status.port}` : `this computer's address, port ${status.port}`;
      remote.textContent =
        `Other computers on your network can reach this one at ${where}. ` +
        (status.discoverable ? "It also answers \"who is sharing?\" on the local network. " : "") +
        (status.rendezvous
          ? `${status.rendezvous}. Anyone with this device's ID, ${$("fingerprint").textContent}, can ask to connect. `
          : "") +
        "Each viewer still needs your permission.";
    }

    // Handy when testing with a second window on this computer.
    if (status.active && status.port && !viewing && !viewPort.value) {
      viewPort.value = `127.0.0.1:${status.port}`;
    }
    if (status.active && shareTimer === null) {
      // Twice a second, so a viewer's request appears promptly.
      shareTimer = setInterval(async () => showShare(await invoke("share_status")), 500);
    } else if (!status.active && shareTimer !== null) {
      clearInterval(shareTimer);
      shareTimer = null;
    }
  }

  shareToggle.addEventListener("click", async () => {
    shareToggle.disabled = true;
    try {
      const status = shareToggle.dataset.active === "true"
        ? await invoke("stop_share")
        : await invoke("start_share", { display: Number(shareDisplay.value) });
      showShare(status);
    } catch (error) {
      shareStats.textContent = message(error);
      shareStats.classList.add("error");
    }
    shareToggle.disabled = false;
  });

  for (const [box, command] of [[shareClipboard, "set_share_clipboard"], [shareFiles, "set_share_files"]]) {
    box.addEventListener("change", async () => {
      try {
        showShare(await invoke(command, { allowed: box.checked }));
      } catch (error) {
        shareStats.textContent = message(error);
        shareStats.classList.add("error");
      }
    });
  }

  shareInput.addEventListener("change", async () => {
    try {
      showShare(await invoke("set_share_input", { allowed: shareInput.checked }));
    } catch (error) {
      shareStats.textContent = message(error);
      shareStats.classList.add("error");
    }
  });

  // ---- Viewer: watch a shared screen -------------------------------------

  const viewPort = $("view-port");
  const viewToggle = $("view-toggle");
  const viewStats = $("view-stats");
  const canvas = $("view-canvas");
  const context = canvas.getContext("2d");

  const viewControl = $("view-control");
  const viewControlLabel = $("view-control-label");

  const PACKET = { CONFIG: 1, FRAME: 2, ENDED: 3, PERMISSIONS: 4, ACCEPTED: 5, LOST: 6 };
  // H.264 Main profile, level 4.0. With no `description` the decoder expects
  // the Annex B stream the host sends.
  const CODEC = "avc1.4d0028";
  // More frames than this waiting to be decoded means the viewer is falling
  // behind; skip ahead to a fresh keyframe instead of adding delay.
  const MAX_QUEUE = 8;

  let viewing = false;
  let decoder = null;
  let needKeyframe = true;
  let size = { width: 0, height: 0 };
  let statsTimer = null;
  // Running totals; `window.__viewStats` exposes them for inspection.
  const totals = { received: 0, drawn: 0, skipped: 0, decodeErrors: 0, latencyMsSum: 0 };
  let lastTick = { drawn: 0, latencyMsSum: 0, time: 0 };
  window.__viewStats = totals;

  function requestKeyframe() {
    needKeyframe = true;
    invoke("view_request_keyframe").catch(() => {});
  }

  function newDecoder() {
    decoder?.close();
    decoder = new VideoDecoder({
      output(frame) {
        context.drawImage(frame, 0, 0, canvas.width, canvas.height);
        // The frame's timestamp is its capture time on the host's clock.
        totals.latencyMsSum += Date.now() - frame.timestamp / 1000;
        totals.drawn += 1;
        frame.close();
      },
      error(error) {
        totals.decodeErrors += 1;
        viewStats.textContent = `Decoder problem: ${error.message}. Recovering…`;
        decoder = null;
        if (viewing) {
          newDecoder();
          requestKeyframe();
        }
      },
    });
    decoder.configure({ codec: CODEC, optimizeForLatency: true });
    needKeyframe = true;
  }

  function onPacket(payload) {
    const bytes = payload instanceof ArrayBuffer ? new Uint8Array(payload) : Uint8Array.from(payload);
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);

    if (bytes[0] === PACKET.CONFIG) {
      size = { width: view.getUint32(1, true), height: view.getUint32(5, true) };
      canvas.width = size.width;
      canvas.height = size.height;
      canvas.hidden = false;
      newDecoder();
    } else if (bytes[0] === PACKET.FRAME && decoder) {
      totals.received += 1;
      const keyframe = bytes[1] === 1;
      if (needKeyframe && !keyframe) {
        totals.skipped += 1;
        return;
      }
      if (!keyframe && decoder.decodeQueueSize > MAX_QUEUE) {
        totals.skipped += 1;
        requestKeyframe();
        return;
      }
      needKeyframe = false;
      decoder.decode(new EncodedVideoChunk({
        type: keyframe ? "key" : "delta",
        timestamp: Number(view.getBigUint64(2, true)),
        data: bytes.subarray(10),
      }));
    } else if (bytes[0] === PACKET.ACCEPTED) {
      reconnecting = null; // the host said yes: this is the session again
      if (capabilityTimer === null) capabilityTimer = setInterval(showCapabilities, 700);
      viewStats.textContent = "Allowed. Waiting for the first picture…";
      statsTimer = setInterval(showViewStats, 1000);
    } else if (bytes[0] === PACKET.PERMISSIONS) {
      setHostAllowsControl((bytes[1] & 1) === 1);
    } else if (bytes[0] === PACKET.ENDED) {
      const reason = new TextDecoder().decode(bytes.subarray(1));
      endViewing(reason);
      if (reconnecting && Reconnect.transientEnd(reason)) {
        // Part-way through getting a connection back, and the host says "not
        // now" (it may not have noticed the old connection die yet).
        reconnecting.connected = false;
        enterReconnectUi();
        scheduleAttempt();
      } else {
        reconnecting = null;
      }
    } else if (bytes[0] === PACKET.LOST) {
      // The connection broke without a goodbye: try to get it back.
      const reason = new TextDecoder().decode(bytes.subarray(1)).replace(/^connection lost: /i, "");
      endViewing(reason);
      beginReconnect(reason);
    }
  }

  // ---- Viewer: control ----------------------------------------------------
  //
  // Input is sent only while the viewer has ticked "Control" *and* the host
  // allows it. The host enforces its side regardless of what is sent.

  let hostAllowsControl = false;
  const heldKeys = new Set();
  let pendingMove = null;

  function controlling() {
    return viewing && hostAllowsControl && viewControl.checked;
  }

  function send(input) {
    invoke("view_send_input", { input }).catch(() => {});
  }

  function position(event) {
    const box = canvas.getBoundingClientRect();
    return {
      x: (event.clientX - box.left) / box.width,
      y: (event.clientY - box.top) / box.height,
    };
  }

  function releaseHeldKeys() {
    for (const code of heldKeys) send({ kind: "key", code, down: false });
    heldKeys.clear();
  }

  // Mouse buttons held down on the picture, and where the pointer last was.
  // The shared computer keeps a button down until it is told to let go, so
  // every way a press can end without a pointerup has to release it: looking
  // away, switching control off, and the browser cancelling the press.
  const heldButtons = new Set();
  let lastPointer = { x: 0, y: 0 };

  function releaseHeldButtons() {
    for (const button of heldButtons) send({ kind: "button", button, down: false, ...lastPointer });
    heldButtons.clear();
  }

  function releaseHeldInput() {
    releaseHeldKeys();
    releaseHeldButtons();
  }

  // Nothing held in a session that has ended is let go of in the next one;
  // the shared computer lets go of everything itself when a session ends.
  function forgetHeldInput() {
    heldKeys.clear();
    heldButtons.clear();
  }

  function showControl() {
    viewControl.disabled = !(viewing && hostAllowsControl);
    if (viewControl.disabled) viewControl.checked = false;
    viewControlLabel.textContent = !viewing || hostAllowsControl
      ? "Control the shared screen"
      : "Control the shared screen (the host has not allowed it)";
    canvas.classList.toggle("controlling", controlling());
    if (!controlling()) releaseHeldInput();
  }

  function setHostAllowsControl(allowed) {
    hostAllowsControl = allowed;
    showControl();
  }

  viewControl.addEventListener("change", () => {
    showControl();
    if (controlling()) canvas.focus();
  });

  canvas.addEventListener("pointermove", (event) => {
    if (!controlling()) return;
    // Pointer events can arrive far faster than frames; send one per frame.
    const first = pendingMove === null;
    pendingMove = position(event);
    lastPointer = pendingMove;
    if (first) {
      requestAnimationFrame(() => {
        if (controlling()) send({ kind: "move", ...pendingMove });
        pendingMove = null;
      });
    }
  });

  canvas.addEventListener("pointerdown", (event) => {
    if (!controlling()) return;
    event.preventDefault();
    canvas.focus();
    // Keeps the drag alive outside the picture; not essential.
    try { canvas.setPointerCapture(event.pointerId); } catch { /* ignore */ }
    lastPointer = position(event);
    heldButtons.add(event.button);
    send({ kind: "button", button: event.button, down: true, ...lastPointer });
  });

  canvas.addEventListener("pointerup", (event) => {
    // Only a press that is still down. If control was switched off meanwhile
    // the button was let go of then, and this adds nothing.
    if (!heldButtons.has(event.button)) return;
    event.preventDefault();
    lastPointer = position(event);
    heldButtons.delete(event.button);
    send({ kind: "button", button: event.button, down: false, ...lastPointer });
  });

  // A press the browser takes away (touch or pen gestures, a lost capture)
  // never ends in a pointerup. After a normal release these find nothing held.
  canvas.addEventListener("pointercancel", releaseHeldButtons);
  canvas.addEventListener("lostpointercapture", releaseHeldButtons);

  canvas.addEventListener("wheel", (event) => {
    if (!controlling()) return;
    event.preventDefault();
    // The browser reports "down" as positive; Windows counts "up" as positive.
    const notches = (delta) => (event.deltaMode === 0 ? delta / 100 : delta);
    send({ kind: "wheel", dx: notches(event.deltaX), dy: -notches(event.deltaY) });
  }, { passive: false });

  canvas.addEventListener("contextmenu", (event) => {
    if (controlling()) event.preventDefault();
  });

  for (const [type, down] of [["keydown", true], ["keyup", false]]) {
    canvas.addEventListener(type, (event) => {
      if (!controlling()) return;
      event.preventDefault();
      if (down) heldKeys.add(event.code); else heldKeys.delete(event.code);
      send({ kind: "key", code: event.code, down });
    });
  }

  // Keys and buttons must not stay down on the host when the viewer looks away.
  canvas.addEventListener("blur", releaseHeldInput);

  // ---- Viewer: clipboard and files -----------------------------------------

  const viewClipboard = $("view-clipboard");
  let capabilityTimer = null;

  // What the link is doing, from the same poll that shows the capabilities.
  let linkNote = "";
  const ms = (value) => (value < 10 ? value.toFixed(1) : value.toFixed(0));
  // Round trip while the other end answers; a warning once it goes quiet.
  function describeLink(link, who) {
    if (!link) return "";
    if (link.silent_ms > 6000) return `No answer from ${who} for ${Math.round(link.silent_ms / 1000)} s`;
    return link.rtt_ms === null ? "" : `Round trip to ${who}: ${ms(link.rtt_ms)} ms`;
  }

  async function showCapabilities() {
    const status = await invoke("view_capability_status").catch(() => null);
    const available = Boolean(status) && viewing;
    linkNote = available ? describeLink(status.link, "the host") : "";
    viewClipboard.disabled = !(available && status.clipboard_available && status.clipboard_host_allows);
    if (viewClipboard.disabled && !available) viewClipboard.checked = false;
    $("view-clipboard-label").textContent = !available || !status.clipboard_available
      ? "Sync clipboard text with the host"
      : !status.clipboard_host_allows
        ? "Sync clipboard text with the host (the host has not allowed it)"
        : status.clipboard_active
          ? `Syncing clipboard text — ${status.clipboard_sent} sent, ${status.clipboard_received} received`
          : "Sync clipboard text with the host";
    $("transfers-viewer").hidden = !available;
    $("view-file").disabled = !(available && status.transfers.allowed);
    renderTransfers("viewer", available ? status.transfers : null);
    if (!available && capabilityTimer !== null) {
      clearInterval(capabilityTimer);
      capabilityTimer = null;
    }
  }

  viewClipboard.addEventListener("change", () => {
    invoke("set_view_clipboard", { wanted: viewClipboard.checked }).catch((error) => {
      viewClipboard.checked = false;
      viewStats.textContent = message(error);
    });
  });

  function showViewStats() {
    const now = performance.now();
    const drawn = totals.drawn - lastTick.drawn;
    const seconds = (now - lastTick.time) / 1000;
    const latency = drawn > 0 ? (totals.latencyMsSum - lastTick.latencyMsSum) / drawn : null;
    lastTick = { drawn: totals.drawn, latencyMsSum: totals.latencyMsSum, time: now };
    viewStats.classList.remove("error");
    viewStats.textContent =
      `${size.width}×${size.height} — ${(drawn / seconds).toFixed(1)} frames per second` +
      (latency === null ? " (screen not changing)" : `, ${latency.toFixed(0)} ms from capture to display`) +
      (linkNote === "" ? "" : `. ${linkNote}`);
  }

  function endViewing(reason) {
    if (!viewing) return;
    viewing = false;
    forgetHeldInput();
    linkNote = "";
    clearInterval(statsTimer);
    statsTimer = null;
    decoder?.close();
    decoder = null;
    viewToggle.textContent = "Connect";
    viewPort.disabled = false;
    $("view-host").hidden = true;
    setHostAllowsControl(false);
    viewClipboard.checked = false;
    showCapabilities();
    viewStats.classList.remove("error");
    viewStats.textContent = `Ended: ${reason}. ${totals.drawn} frames shown.`;
  }

  // The key a discovery result announced; a typed address has none. The
  // connection is refused if the device at the address holds another.
  let expectedKey = null;
  viewPort.addEventListener("input", () => { expectedKey = null; });

  const findButton = $("find-devices");
  const findStatus = $("find-status");

  findButton.addEventListener("click", async () => {
    findButton.disabled = true;
    findStatus.textContent = "Looking…";
    try {
      const devices = await invoke("discover_devices");
      findStatus.textContent = devices.length === 0
        ? "Nothing found. The other device must be sharing with 'Let other computers connect' on, and be on this network."
        : `${devices.length} found. Names are what each device says about itself; compare the key.`;
      $("found").replaceChildren(...devices.map((device) => {
        const item = document.createElement("li");
        const label = document.createElement("span");
        const name = document.createElement("strong");
        name.textContent = device.name + (device.this_device ? " (this device)" : "");
        const detail = document.createElement("div");
        detail.className = "detail";
        detail.textContent = `${device.address} — key ${device.fingerprint}`;
        label.append(name, detail);
        const use = document.createElement("button");
        use.type = "button";
        use.textContent = "Connect";
        use.addEventListener("click", () => {
          viewPort.value = device.address;
          expectedKey = device.public_key;
          if (!viewing) viewToggle.click();
        });
        item.append(label, use);
        return item;
      }));
    } catch (error) {
      findStatus.textContent = message(error);
    }
    findButton.disabled = false;
  });

  // Connects, and shows who answered. Throws if the connection cannot be made.
  async function connectNow(address) {
    forgetHeldInput();
    Object.keys(totals).forEach((key) => { totals[key] = 0; });
    lastTick = { drawn: 0, latencyMsSum: 0, time: performance.now() };

    const packets = new Channel();
    packets.onmessage = onPacket;
    viewing = true;
    let host;
    try {
      host = await invoke("view_connect", { address, expectedKey, packets });
    } catch (error) {
      viewing = false;
      throw error;
    }
    viewToggle.textContent = "Disconnect";
    viewPort.disabled = true;
    // Shown as text only: the name is whatever the other device says.
    $("view-host-name").textContent = host.name;
    $("view-host-fingerprint").textContent = host.fingerprint;
    $("view-host-route").textContent = host.route;
    $("view-host-code").textContent = host.auth_code;
    $("view-host").hidden = false;
    viewStats.classList.remove("error");
    // The host may already have answered while this call returned.
    if (viewing && statsTimer === null) {
      viewStats.textContent =
        "Waiting for the person at the other device to allow it. " +
        "Their screen must show the same code.";
    }
  }

  // ---- Viewer: getting a lost connection back -----------------------------
  //
  // Only a connection that broke without a goodbye is retried. Every attempt
  // is a brand new session: the host asks its user again each time, and the
  // device must still hold the key that was asked for. Reconnect.js decides
  // what is retried and for how long.

  let reconnecting = null;

  function enterReconnectUi() {
    viewToggle.textContent = "Stop reconnecting";
    viewToggle.disabled = false;
    viewPort.disabled = true;
  }

  function beginReconnect(reason) {
    // Lost again part-way through getting it back: the clock keeps running.
    const state = reconnecting ?? {
      address: viewPort.value.trim(), lost: Date.now(), attempt: 0, timer: null, cancelled: false,
    };
    state.reason = reason;
    state.connected = false;
    reconnecting = state;
    enterReconnectUi();
    scheduleAttempt();
  }

  function showReconnect(text) {
    viewStats.classList.add("error");
    viewStats.textContent = text;
  }

  function scheduleAttempt() {
    const state = reconnecting;
    state.attempt += 1;
    const elapsed = (Date.now() - state.lost) / 1000;
    if (!Reconnect.mayTryAgain(elapsed, state.attempt)) {
      return stopReconnecting(`Connection lost: ${state.reason}. Could not get it back; gave up.`);
    }
    let wait = Reconnect.delaySeconds(state.attempt);
    const tick = () => {
      if (state.cancelled) return;
      if (wait > 0) {
        showReconnect(`Connection lost: ${state.reason}. Trying again in ${wait} s (attempt ${state.attempt}).`);
        wait -= 1;
        state.timer = setTimeout(tick, 1000);
      } else {
        attempt();
      }
    };
    tick();
  }

  async function attempt() {
    const state = reconnecting;
    showReconnect(`Connection lost: ${state.reason}. Reconnecting (attempt ${state.attempt})…`);
    try {
      await connectNow(state.address);
    } catch (error) {
      if (state.cancelled) return;
      if (!Reconnect.retryable(error)) {
        return stopReconnecting(`Connection lost: ${state.reason}. Not trying again: ${message(error)}`);
      }
      return scheduleAttempt();
    }
    if (state.cancelled) {
      // Cancelled while the attempt was in flight.
      await invoke("view_disconnect").catch(() => {});
      return;
    }
    // Connected, but the host has not said yes yet. `reconnecting` stays
    // until it does (ACCEPTED) or ends the attempt.
    state.connected = true;
  }

  function stopReconnecting(text) {
    if (reconnecting) {
      reconnecting.cancelled = true;
      clearTimeout(reconnecting.timer);
      reconnecting = null;
    }
    viewToggle.textContent = "Connect";
    viewPort.disabled = false;
    showReconnect(text);
  }

  viewToggle.addEventListener("click", async () => {
    if (reconnecting && !reconnecting.connected) {
      stopReconnecting("Reconnecting stopped.");
      viewStats.classList.remove("error");
      return;
    }
    viewToggle.disabled = true;
    try {
      if (viewing) {
        await invoke("view_disconnect");
      } else {
        const address = viewPort.value.trim();
        if (!address) throw new Error("Enter the device ID or address shown by the sharing device, or find it on the network");
        if (!("VideoDecoder" in window)) throw new Error("This system's web view cannot decode video");
        await connectNow(address);
      }
    } catch (error) {
      viewStats.textContent = message(error);
      viewStats.classList.add("error");
    }
    viewToggle.disabled = false;
  });

  // ---- Start-up ----------------------------------------------------------

  (async () => {
    try {
      for (const display of await invoke("list_displays")) {
        shareDisplay.add(new Option(display.label, display.index));
      }
      shareToggle.disabled = shareDisplay.options.length === 0;
      showShare(await invoke("share_status"));
      await showRemembered();
    } catch (error) {
      shareStats.textContent = message(error);
      shareStats.classList.add("error");
    }
  })();
})();
