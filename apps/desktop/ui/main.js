"use strict";

const { invoke } = window.__TAURI__.core;

const form = document.getElementById("settings");
const saveButton = document.getElementById("save");
const saveResult = document.getElementById("save-result");

/** The configuration as last loaded or saved; the form edits a copy of it. */
let current = null;

const LIST_FIELDS = new Set(["network.rendezvous_servers", "network.relay_servers"]);

function fields() {
  return Array.from(form.elements).filter((el) => el.name);
}

function getPath(object, path) {
  return path.split(".").reduce((value, key) => value[key], object);
}

function setPath(object, path, value) {
  const keys = path.split(".");
  const last = keys.pop();
  keys.reduce((value, key) => value[key], object)[last] = value;
}

function showConfig(config) {
  for (const el of fields()) {
    const value = getPath(config, el.name);
    if (el.type === "checkbox") {
      el.checked = value;
    } else if (LIST_FIELDS.has(el.name)) {
      el.value = value.join("\n");
    } else {
      el.value = value;
    }
  }
}

function readConfig() {
  const config = structuredClone(current);
  for (const el of fields()) {
    let value;
    if (el.type === "checkbox") {
      value = el.checked;
    } else if (el.type === "number") {
      value = Number.parseInt(el.value, 10);
      if (!Number.isInteger(value) || value < 0) {
        throw new Error(`${el.labels[0].firstChild.textContent.trim()}: enter a whole number`);
      }
    } else if (LIST_FIELDS.has(el.name)) {
      value = el.value.split("\n").map((line) => line.trim()).filter(Boolean);
    } else {
      value = el.value;
    }
    setPath(config, el.name, value);
  }
  return config;
}

function showResult(message, isError) {
  saveResult.textContent = message;
  saveResult.classList.toggle("error", isError);
}

async function showStatus() {
  const status = await invoke("get_status");
  document.getElementById("device-name").textContent = status.device_name;
  document.getElementById("version").textContent = status.version;
  document.getElementById("config-path").textContent = status.config_path ?? "unavailable";
  document.getElementById("log-dir").textContent = status.log_dir ?? "unavailable";

  const banner = document.getElementById("config-error");
  banner.hidden = !status.config_error;
  banner.textContent = status.config_error
    ? `Settings could not be loaded and are read-only. ${status.config_error}`
    : "";
  return status;
}

form.addEventListener("input", () => {
  saveButton.disabled = false;
  showResult("", false);
});

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  saveButton.disabled = true;
  try {
    current = await invoke("save_config", { config: readConfig() });
    showConfig(current);
    await showStatus();
    showResult("Settings saved.", false);
  } catch (error) {
    showResult(String(error.message ?? error), true);
    saveButton.disabled = false;
  }
});

const sharing = document.getElementById("sharing");
const captureDisplay = document.getElementById("capture-display");
const captureToggle = document.getElementById("capture-toggle");
const captureStats = document.getElementById("capture-stats");
let captureTimer = null;

function showCapture(status) {
  sharing.textContent = status.active ? "Capturing screen" : "Not sharing";
  sharing.classList.toggle("live", status.active);
  captureToggle.textContent = status.active ? "Stop capture" : "Start capture";
  captureToggle.dataset.active = String(status.active);
  captureDisplay.disabled = status.active;

  captureStats.classList.toggle("error", Boolean(status.error));
  if (status.error) {
    captureStats.textContent = status.error;
  } else if (status.active && status.blocked) {
    captureStats.textContent = "Waiting: the lock screen or a system prompt is showing.";
  } else if (status.frames > 0 || status.active) {
    const rate = status.seconds > 0 ? (status.frames / status.seconds).toFixed(1) : "0.0";
    captureStats.textContent =
      `${status.active ? "" : "Stopped. "}` +
      `${status.width}×${status.height} — ${status.frames} frames in ` +
      `${status.seconds.toFixed(0)} s (${rate} per second)`;
  }

  if (status.active && captureTimer === null) {
    captureTimer = setInterval(async () => showCapture(await invoke("capture_status")), 1000);
  } else if (!status.active && captureTimer !== null) {
    clearInterval(captureTimer);
    captureTimer = null;
  }
}

captureToggle.addEventListener("click", async () => {
  captureToggle.disabled = true;
  try {
    const status = captureToggle.dataset.active === "true"
      ? await invoke("stop_capture")
      : await invoke("start_capture", { display: Number(captureDisplay.value) });
    showCapture(status);
  } catch (error) {
    captureStats.textContent = String(error.message ?? error);
    captureStats.classList.add("error");
  }
  captureToggle.disabled = false;
});

async function startCapturePanel() {
  try {
    for (const display of await invoke("list_displays")) {
      captureDisplay.add(new Option(display.label, display.index));
    }
    captureToggle.disabled = captureDisplay.options.length === 0;
    showCapture(await invoke("capture_status"));
  } catch (error) {
    captureStats.textContent = String(error.message ?? error);
    captureStats.classList.add("error");
  }
}

async function start() {
  startCapturePanel();
  try {
    const status = await showStatus();
    current = await invoke("get_config");
    showConfig(current);
    if (status.config_error) {
      for (const el of fields()) el.disabled = true;
    }
  } catch (error) {
    showResult(String(error.message ?? error), true);
  }
}

start();
