// Privileged Firefox AutoConfig. No page content, URLs, audio samples or commands.
(() => {
  "use strict";
  const MAX_WINDOWS = 128;
  const MAX_TABS = 8192;
  const MAX_TITLES = 512;

  function collectWindows(windows, identify) {
    const result = [];
    let count = 0;
    let titles = 0;
    for (const win of windows) {
      if (win.closed) continue;
      if (!win.gBrowser || result.length >= MAX_WINDOWS) return null;
      const title = win.document.title;
      if (typeof title !== "string" || title.length > 1024) return null;
      const audible = new Set();
      for (const tab of win.gBrowser.tabs) {
        if (++count > MAX_TABS) return null;
        if (tab.closing || !tab.soundPlaying || tab.muted ||
            tab.linkedBrowser?.audioMuted || tab.hasAttribute("soundplaying-scheduledremoval")) {
          continue;
        }
        // contentTitle is also used for named audio streams. Keep the UI label
        // as an exact alias, never a fuzzy match or a URL-derived guess.
        for (const name of [tab.linkedBrowser?.contentTitle, tab.label]) {
          if (typeof name !== "string" || !name) continue;
          if (name.length > 1024 || ++titles > MAX_TITLES) return null;
          audible.add(name);
        }
        // An unnamed WebAudio stream still establishes that this window plays.
        if (audible.size === 0) audible.add("");
      }
      result.push({ id: identify(win), title, audible: [...audible] });
    }
    return result;
  }

  function createPublisher({ collect, write, now, later, cancel, report, identity }) {
    let timer = null;
    let writing = false;
    let dirty = false;
    let stopped = false;
    async function flush() {
      timer = null;
      if (stopped || writing) return;
      writing = true;
      dirty = false;
      try {
        const windows = collect();
        await write({ schema: 1, ...identity, written_at_ms: now(),
          complete: windows !== null, windows: windows || [] });
      } catch (error) {
        report(error);
      } finally {
        writing = false;
        // One replaceable pending update, not one queued write per browser event.
        if (dirty && !stopped) schedule();
      }
    }
    function schedule() {
      if (stopped) return;
      dirty = true;
      if (!writing && timer === null) timer = later(flush, 40);
    }
    return { schedule, stop() { stopped = true; if (timer !== null) cancel(timer); } };
  }

  if (typeof module !== "undefined" && module.exports) {
    module.exports = { collectWindows, createPublisher };
    return;
  }

  const { classes: Cc, interfaces: Ci, utils: Cu } = Components;
  const observers = Cc["@mozilla.org/observer-service;1"].getService(Ci.nsIObserverService);
  const mediator = Cc["@mozilla.org/appshell/window-mediator;1"].getService(Ci.nsIWindowMediator);
  const runtime = Cc["@mozilla.org/xre/app-info;1"].getService(Ci.nsIXULRuntime);
  const env = Cc["@mozilla.org/process/environment;1"].getService(Ci.nsIEnvironment);
  const runtimeDir = env.get("XDG_RUNTIME_DIR");
  if (!runtimeDir || !runtimeDir.startsWith("/")) return;
  Cu.importGlobalProperties(["IOUtils", "PathUtils"]);
  const { setTimeout, clearTimeout, setInterval, clearInterval } =
    ChromeUtils.importESModule("resource://gre/modules/Timer.sys.mjs");
  const directory = PathUtils.join(runtimeDir, "applicationlauncher-firefox-audio");
  const path = PathUtils.join(directory, `firefox-${runtime.processID}.json`);
  const ids = new WeakMap();
  const attached = new Map();
  let nextId = 0;
  let publisher;
  let heartbeat;
  let closed = false;
  let lastError = 0;
  const report = error => {
    if (Date.now() - lastError > 60000) {
      lastError = Date.now();
      Cu.reportError(`Application Launcher audio bridge: ${error}`);
    }
  };
  const changed = () => publisher?.schedule();
  const events = ["DOMAudioPlaybackStarted", "DOMAudioPlaybackStopped", "TabAttrModified",
    "TabOpen", "TabClose", "TabSelect", "TabRemotenessChange", "DOMTitleChanged"];

  function identify(win) {
    if (!ids.has(win)) ids.set(win, String(++nextId));
    return ids.get(win);
  }
  function attach(win) {
    if (!win?.gBrowser || win.closed || attached.has(win)) return;
    for (const name of events) win.addEventListener(name, changed);
    const mutation = new win.MutationObserver(changed);
    mutation.observe(win.document.documentElement, { attributes: true, attributeFilter: ["title"] });
    attached.set(win, mutation);
    changed();
  }
  function detach(win) {
    if (!attached.has(win)) return;
    attached.get(win).disconnect();
    attached.delete(win);
    for (const name of events) win.removeEventListener(name, changed);
    changed();
  }
  const topics = ["browser-delayed-startup-finished", "domwindowclosed", "quit-application-granted"];
  const observer = {
    observe(subject, topic) {
      if (topic === "browser-delayed-startup-finished") attach(subject);
      else if (topic === "domwindowclosed") detach(subject);
      else {
        closed = true;
        publisher?.stop();
        if (heartbeat) clearInterval(heartbeat);
        for (const win of [...attached.keys()]) detach(win);
        for (const name of topics) observers.removeObserver(observer, name);
        // If a write is already in flight, its process identity/TTL still makes
        // any late file harmless. The launcher never trusts files after exit.
        IOUtils.remove(path, { ignoreAbsent: true }).catch(report);
      }
    },
  };
  for (const topic of topics) observers.addObserver(observer, topic);
  function processStartTicks() {
    // procfs reports size zero: IOUtils.readUTF8 treats it as an empty file.
    // Read this one bounded local pseudo-file through a streaming decoder instead.
    const file = Cc["@mozilla.org/file/local;1"].createInstance(Ci.nsIFile);
    file.initWithPath("/proc/self/stat");
    const input = Cc["@mozilla.org/network/file-input-stream;1"].createInstance(Ci.nsIFileInputStream);
    input.init(file, 0x01, 0, 0);
    const decoder = Cc["@mozilla.org/intl/converter-input-stream;1"].createInstance(Ci.nsIConverterInputStream);
    try {
      decoder.init(input, "UTF-8", 4096, 0);
      const value = {};
      decoder.readString(4096, value);
      return value.value.slice(value.value.lastIndexOf(")") + 2).trim().split(/\s+/)[19];
    } finally {
      decoder.close();
      input.close();
    }
  }
  async function start() {
    try {
      const startTicks = processStartTicks();
      if (!/^\d+$/.test(startTicks)) throw new Error("process identity unavailable");
      await IOUtils.makeDirectory(directory, { permissions: 0o700, ignoreExisting: true });
      const directoryFile = Cc["@mozilla.org/file/local;1"].createInstance(Ci.nsIFile);
      directoryFile.initWithPath(directory);
      if (directoryFile.isSymlink()) throw new Error("bridge directory must not be a symlink");
      await IOUtils.setPermissions(directory, 0o700);
      if (closed) return;
      publisher = createPublisher({
        collect: () => {
          const windows = [...mediator.getEnumerator("navigator:browser")];
          for (const win of windows) attach(win);
          return collectWindows(windows, identify);
        },
        identity: { pid: runtime.processID, start_ticks: startTicks },
        write: async data => {
          // writeJSON has no permissions option. Make the staged file private
          // before the atomic rename so readers never see a public snapshot.
          const staged = path + ".tmp";
          await IOUtils.writeJSON(staged, data);
          await IOUtils.setPermissions(staged, 0o600);
          await IOUtils.move(staged, path, { noOverwrite: false });
        },
        now: Date.now, later: setTimeout, cancel: clearTimeout, report,
      });
      publisher.schedule();
      // A bounded heartbeat expires ownership after a browser/bridge failure.
      heartbeat = setInterval(changed, 5000);
    } catch (error) {
      report(error);
    }
  }
  start();
})();
