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
  let shareTimer = null;

  function showShare(status) {
    const live = status.active && status.viewer_connected;
    pill.textContent = live
      ? "Sharing screen"
      : status.active
        ? "Sharing on — no viewer"
        : "Not sharing";
    pill.classList.toggle("live", live);
    pill.classList.toggle("armed", status.active && !live);

    shareToggle.textContent = status.active ? "Stop sharing" : "Start sharing";
    shareToggle.dataset.active = String(status.active);
    shareDisplay.disabled = status.active;

    const sent = `${status.frames} frames, ${(status.bytes / 1e6).toFixed(1)} MB sent`;
    shareStats.classList.toggle("error", Boolean(status.error));
    if (status.error) {
      shareStats.textContent = status.error;
    } else if (!status.active) {
      shareStats.textContent = status.frames > 0 ? `Stopped. ${sent}.` : "";
    } else if (status.blocked) {
      shareStats.textContent = "Waiting: the lock screen or a system prompt is showing.";
    } else if (live) {
      shareStats.textContent = `Port ${status.port}. ${status.width}×${status.height} — ${sent}.`;
    } else {
      shareStats.textContent = `Port ${status.port}. Waiting for a viewer on this computer.`;
    }

    if (status.active && status.port && !viewing) {
      viewPort.value = status.port;
    }
    if (status.active && shareTimer === null) {
      shareTimer = setInterval(async () => showShare(await invoke("share_status")), 1000);
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

  // ---- Viewer: watch a shared screen -------------------------------------

  const viewPort = $("view-port");
  const viewToggle = $("view-toggle");
  const viewStats = $("view-stats");
  const canvas = $("view-canvas");
  const context = canvas.getContext("2d");

  const PACKET = { CONFIG: 1, FRAME: 2, ENDED: 3 };
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
    } else if (bytes[0] === PACKET.ENDED) {
      endViewing(new TextDecoder().decode(bytes.subarray(1)));
    }
  }

  function showViewStats() {
    const now = performance.now();
    const drawn = totals.drawn - lastTick.drawn;
    const seconds = (now - lastTick.time) / 1000;
    const latency = drawn > 0 ? (totals.latencyMsSum - lastTick.latencyMsSum) / drawn : null;
    lastTick = { drawn: totals.drawn, latencyMsSum: totals.latencyMsSum, time: now };
    viewStats.classList.remove("error");
    viewStats.textContent =
      `${size.width}×${size.height} — ${(drawn / seconds).toFixed(1)} frames per second` +
      (latency === null ? " (screen not changing)" : `, ${latency.toFixed(0)} ms from capture to display`);
  }

  function endViewing(reason) {
    if (!viewing) return;
    viewing = false;
    clearInterval(statsTimer);
    decoder?.close();
    decoder = null;
    viewToggle.textContent = "Connect";
    viewPort.disabled = false;
    viewStats.classList.remove("error");
    viewStats.textContent = `Ended: ${reason}. ${totals.drawn} frames shown.`;
  }

  viewToggle.addEventListener("click", async () => {
    viewToggle.disabled = true;
    try {
      if (viewing) {
        await invoke("view_disconnect");
      } else {
        const port = Number.parseInt(viewPort.value, 10);
        if (!(port >= 1 && port <= 65535)) throw new Error("Enter the port shown by the sharing device");
        if (!("VideoDecoder" in window)) throw new Error("This system's web view cannot decode video");
        Object.keys(totals).forEach((key) => { totals[key] = 0; });
        lastTick = { drawn: 0, latencyMsSum: 0, time: performance.now() };

        const packets = new Channel();
        packets.onmessage = onPacket;
        viewing = true;
        try {
          await invoke("view_connect", { port, packets });
        } catch (error) {
          viewing = false;
          throw error;
        }
        viewToggle.textContent = "Disconnect";
        viewPort.disabled = true;
        viewStats.classList.remove("error");
        viewStats.textContent = "Connected. Waiting for the first picture…";
        statsTimer = setInterval(showViewStats, 1000);
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
    } catch (error) {
      shareStats.textContent = message(error);
      shareStats.classList.add("error");
    }
  })();
})();
