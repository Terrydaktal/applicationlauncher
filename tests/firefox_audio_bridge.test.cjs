const assert = require("node:assert/strict");
const test = require("node:test");
const { collectWindows, createPublisher } = require("../firefox/audio-bridge.js");

function tab(name, options = {}) {
  return { label: name, soundPlaying: true, hasAttribute: key => key === "soundplaying-scheduledremoval" && !!options.stopping,
    linkedBrowser: { contentTitle: name, audioMuted: !!options.audioMuted }, ...options };
}
function window(id, tabs, title = id) {
  return { id, document: { title }, gBrowser: { tabs } };
}
const collect = windows => collectWindows(windows, win => win.id);

test("background playing tab belongs only to its own window", () => {
  assert.deepEqual(collect([
    window("one", [tab("Silent tab", { soundPlaying: false })], "Work - Mozilla Firefox"),
    window("two", [tab("Unrelated selected tab", { soundPlaying: false }), tab("Music - YouTube")], "Selected page - Mozilla Firefox"),
  ]).map(win => win.audible), [[], ["Music - YouTube"]]);
});

test("pause clears ownership before Firefox's delayed audio icon disappears", () => {
  const song = tab("Music");
  assert.deepEqual(collect([window("one", [song])])[0].audible, ["Music"]);
  song.hasAttribute = name => name === "soundplaying-scheduledremoval";
  assert.deepEqual(collect([window("one", [song])])[0].audible, []);
  song.hasAttribute = () => false;
  assert.deepEqual(collect([window("one", [song])])[0].audible, ["Music"]);
});

test("mute, closure and discarded tabs are not playback", () => {
  assert.deepEqual(collect([window("one", [tab("a", { muted: true }), tab("b", { audioMuted: true }),
    tab("c", { closing: true }), tab("d", { soundPlaying: false })])])[0].audible, []);
});

test("moving a playing tab transfers ownership without selecting it", () => {
  const one = window("one", [tab("Music")]);
  const two = window("two", []);
  two.gBrowser.tabs.push(one.gBrowser.tabs.pop());
  assert.deepEqual(collect([one, two]).map(win => win.audible), [[], ["Music"]]);
});

test("several playing windows, duplicate titles, and unnamed streams are retained", () => {
  assert.deepEqual(collect([window("one", [tab("same")], "same"), window("two", [tab("")], "same")])
    .map(win => [win.id, win.audible]), [["one", ["same"]], ["two", [""]]]);
});

test("bounds invalidate the whole snapshot instead of reporting partial ownership", () => {
  assert.equal(collect(Array.from({ length: 129 }, (_, n) => window(String(n), []))), null);
  assert.equal(collect([window("one", Array.from({ length: 8193 }, () => tab("", { soundPlaying: false })))]), null);
  assert.equal(collect([window("one", [], "x".repeat(1025))]), null);
});

test("no URLs, page contents or browsing histories enter the wire payload", () => {
  const song = tab("Music");
  song.linkedBrowser.currentURI = { spec: "https://private.example/secret" };
  const json = JSON.stringify(collect([window("one", [song])]));
  assert(!json.includes("private.example"));
  assert.deepEqual(Object.keys(JSON.parse(json)[0]).sort(), ["audible", "id", "title"]);
});

test("writes coalesce, do not overlap, and stop cannot resurrect the publisher", async () => {
  let scheduled;
  let pending;
  let count = 0;
  const publisher = createPublisher({ collect: () => [], identity: { pid: 1, start_ticks: "2" }, now: () => 3,
    write: () => { count++; return new Promise(resolve => { pending = resolve; }); },
    later: callback => { assert(!scheduled); scheduled = callback; return 1; },
    cancel: () => { scheduled = null; }, report: assert.fail });
  for (let i = 0; i < 1000; i++) publisher.schedule();
  const run = scheduled; scheduled = null;
  const promise = run();
  for (let i = 0; i < 1000; i++) publisher.schedule();
  assert.equal(count, 1);
  pending(); await promise;
  assert(scheduled);
  publisher.stop();
  publisher.schedule();
  assert.equal(scheduled, null);
});
