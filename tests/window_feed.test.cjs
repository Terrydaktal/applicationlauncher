const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname,
  "../kwin/applicationlauncher-window-feed/contents/code/main.js"), "utf8");

function signal() {
  const callbacks = [];
  return {
    connect(callback) { callbacks.push(callback); },
    emit(...args) { for (const callback of [...callbacks]) callback(...args); },
  };
}

function window(id, caption = "Dental Anatomy Atlas - Google Chrome") {
  return {
    internalId: id, caption, resourceClass: "Google-chrome", desktopFileName: "google-chrome",
    pid: 100, frameGeometry: { x: 100, y: 200, width: 1200, height: 800 },
    captionChanged: signal(), frameGeometryChanged: signal(), minimizedChanged: signal(),
    interactiveMoveResizeFinished: signal(), closed: signal(),
  };
}

function feed(initial = []) {
  const calls = [];
  const workspace = {
    stackingOrder: [...initial], activeWindow: null,
    windowAdded: signal(), windowRemoved: signal(), windowActivated: signal(),
    screensChanged: signal(),
  };
  vm.runInNewContext(source, {
    workspace,
    registerShortcut() {},
    callDBus(service, objectPath, iface, method, payload) {
      calls.push({ method, payload: ["ReplaceSnapshot", "UpsertWindow", "WindowActivated"].includes(method)
        ? JSON.parse(payload) : payload });
    },
  }, { filename: "applicationlauncher-window-feed/main.js", timeout: 1000 });
  return { workspace, calls };
}

for (const closePath of ["closed", "windowRemoved", "both"]) {
  test(`late teardown signals cannot resurrect a window removed by ${closePath}`, () => {
    const { workspace, calls } = feed();
    const chrome = window("detached");
    workspace.windowAdded.emit(chrome);
    assert.equal(calls.at(-1).method, "UpsertWindow");
    if (closePath !== "windowRemoved") chrome.closed.emit();
    if (closePath !== "closed") workspace.windowRemoved.emit(chrome);
    assert.equal(calls.at(-1).method, "RemoveWindow");
    const beforeLateSignals = calls.length;
    chrome.captionChanged.emit();
    chrome.frameGeometryChanged.emit();
    chrome.interactiveMoveResizeFinished.emit();
    chrome.minimizedChanged.emit();
    assert.equal(calls.length, beforeLateSignals, "removed window was published again");
  });
}

test("late activation cannot resurrect a closed window", () => {
  const { workspace, calls } = feed();
  const chrome = window("detached");
  workspace.windowAdded.emit(chrome);
  chrome.closed.emit();
  const count = calls.length;
  workspace.windowActivated.emit(chrome);
  assert.equal(calls.length, count);
});

test("reattaching one tab does not hide another real window with the same title", () => {
  const { workspace, calls } = feed();
  const original = window("original");
  const detached = window("detached");
  workspace.windowAdded.emit(original);
  workspace.windowAdded.emit(detached);
  detached.closed.emit();
  workspace.windowRemoved.emit(detached);
  const afterClose = calls.length;
  detached.frameGeometryChanged.emit();
  detached.captionChanged.emit();
  original.captionChanged.emit();
  const updates = calls.slice(afterClose);
  assert.equal(updates.length, 1);
  assert.equal(updates[0].payload.id, "original");
  assert.equal(updates[0].method, "UpsertWindow");
});

test("initial snapshots and legitimate live updates remain unchanged", () => {
  const chrome = window("original");
  const { workspace, calls } = feed([chrome]);
  const initial = calls.find(call => call.method === "ReplaceSnapshot");
  assert.equal(initial.payload.length, 1);
  assert.equal(initial.payload[0].id, "original");
  chrome.caption = "Another selected tab - Google Chrome";
  chrome.captionChanged.emit();
  assert.equal(calls.at(-1).payload.title, chrome.caption);
  workspace.windowActivated.emit(chrome);
  assert.equal(calls.at(-1).method, "WindowActivated");
  assert.equal(calls.at(-1).payload.id, "original");
});

test("repeated tab detach and reattach leaves no resurrected feed records", () => {
  const { workspace, calls } = feed();
  const original = window("original");
  workspace.windowAdded.emit(original);
  for (let index = 0; index < 100; index++) {
    const detached = window(`detached-${index}`);
    workspace.windowAdded.emit(detached);
    detached.closed.emit();
    workspace.windowRemoved.emit(detached);
    detached.frameGeometryChanged.emit();
    detached.captionChanged.emit();
    original.captionChanged.emit();
  }
  const live = new Map();
  for (const call of calls) {
    if (call.method === "ReplaceSnapshot") {
      live.clear();
      for (const payload of call.payload) live.set(payload.id, payload);
    } else if (call.method === "UpsertWindow" || call.method === "WindowActivated") {
      live.set(call.payload.id, call.payload);
    } else if (call.method === "RemoveWindow") {
      live.delete(call.payload);
    }
  }
  assert.deepEqual([...live.keys()], ["original"]);
});
