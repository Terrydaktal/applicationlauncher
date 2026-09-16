// Loaded only in a disposable test installation, never by the real installer.
(() => {
  const env = Components.classes["@mozilla.org/process/environment;1"].getService(Components.interfaces.nsIEnvironment);
  const root = env.get("APPLICATIONLAUNCHER_FIREFOX_TEST_ROOT");
  if (!root || !root.startsWith("/tmp/al-firefox-integration-")) throw new Error("not a fixture");
  Components.utils.importGlobalProperties(["IOUtils", "PathUtils"]);
  const { setInterval } = ChromeUtils.importESModule("resource://gre/modules/Timer.sys.mjs");
  let last = "";
  let busy = false;
  setInterval(async () => {
    if (busy) return;
    busy = true;
    try {
      const command = await IOUtils.readUTF8(PathUtils.join(root, "command"));
      if (command === last) return;
      const windows = [...Services.wm.getEnumerator("navigator:browser")].filter(win => win.gBrowser);
      if (!windows.length) return;
      const win = windows[0];
      if (command === "start") {
        const html = "<title>Fixture Music</title><script>window.ctx=new AudioContext();window.tone=ctx.createOscillator();tone.connect(ctx.destination);tone.start();ctx.resume();</script>";
        win.gBrowser.selectedBrowser.loadURI(Services.io.newURI("data:text/html," + encodeURIComponent(html)),
          { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() });
      } else if (command === "background") {
        win.gBrowser.selectedTab = win.gBrowser.addTab("about:blank", {triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal()});
      } else if (command === "second-window") {
        win.OpenBrowserWindow();
      } else if (command === "mute" || command === "unmute") {
        for (const owner of windows) {
          for (const tab of owner.gBrowser.tabs) {
            if (tab.label === "Fixture Music" && tab.muted !== (command === "mute")) tab.toggleMuteAudio();
          }
        }
      } else if (command === "stop") {
        for (const owner of windows) {
          for (const tab of owner.gBrowser.tabs) {
            if (tab.label === "Fixture Music") tab.linkedBrowser.loadURI(Services.io.newURI("about:blank"),
              { triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal() });
          }
        }
      } else {
        throw new Error("unknown test command");
      }
      last = command;
    } catch (error) {
      if (error.name !== "NotFoundError") Components.utils.reportError("Audio bridge fixture: " + error);
    } finally {
      busy = false;
    }
  }, 100);
})();
