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

  document.getElementById("fingerprint").textContent = status.fingerprint ?? "unavailable";
  const securityBanner = document.getElementById("security-error");
  securityBanner.hidden = !status.security_error;
  securityBanner.textContent = status.security_error
    ? `This device's key could not be loaded, so it cannot share or view. ${status.security_error}`
    : "";

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

async function start() {
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
