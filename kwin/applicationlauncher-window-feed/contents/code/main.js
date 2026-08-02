var SERVICE = "com.terrydaktal.ApplicationLauncher";
var PATH = "/WindowFeed";
var INTERFACE = "com.terrydaktal.ApplicationLauncher.WindowFeed";
var TRACKER_PATH = "/Tracker";
var TRACKER_INTERFACE = "com.terrydaktal.ApplicationLauncher.Tracker1";

var trackedWindows = {};

function activeWindowIsBrowser() {
    var window = workspace.activeWindow;
    if (!window) {
        return false;
    }
    var identity = (windowClass(window) + " " +
        (window.desktopFileName ? String(window.desktopFileName) : "")).toLowerCase();
    return identity.indexOf("chrome") !== -1 ||
        identity.indexOf("chromium") !== -1 ||
        identity.indexOf("firefox") !== -1;
}

function reopenLatestClosedWindow() {
    if (activeWindowIsBrowser()) {
        return;
    }
    callDBus(
        SERVICE,
        TRACKER_PATH,
        TRACKER_INTERFACE,
        "ReopenLatestHistory"
    );
}

function windowClass(window) {
    if (!window) {
        return "";
    }
    if (window.windowClass) {
        return String(window.windowClass);
    }
    if (window.resourceClass) {
        return String(window.resourceClass);
    }
    if (window.desktopFileName) {
        return String(window.desktopFileName);
    }
    return "";
}

function serializeWindow(window) {
    if (!window || !window.internalId) {
        return null;
    }

    var geometry = window.frameGeometry;

    var desktop = 0;
    if (typeof window.x11DesktopNumber === "number") {
        desktop = window.x11DesktopNumber;
    } else if (window.desktops && window.desktops.length > 0 &&
               typeof window.desktops[0].x11DesktopNumber === "number") {
        desktop = window.desktops[0].x11DesktopNumber;
    } else if (window.desktops && window.desktops.length > 0 && workspace.desktops) {
        for (var desktopIndex = 0; desktopIndex < workspace.desktops.length; ++desktopIndex) {
            if (workspace.desktops[desktopIndex] === window.desktops[0] ||
                (workspace.desktops[desktopIndex].id && window.desktops[0].id &&
                 workspace.desktops[desktopIndex].id === window.desktops[0].id)) {
                desktop = desktopIndex + 1;
                break;
            }
        }
    }
    var outputName = "";
    if (window.output && window.output.name) {
        outputName = String(window.output.name);
    }

    return {
        id: String(window.internalId),
        title: window.caption ? String(window.caption) : "",
        class: windowClass(window),
        desktopFileName: window.desktopFileName ? String(window.desktopFileName) : "",
        pid: typeof window.pid === "number" ? window.pid : 0,
        x: geometry ? Math.round(geometry.x) : 0,
        y: geometry ? Math.round(geometry.y) : 0,
        width: geometry ? Math.round(geometry.width) : 0,
        height: geometry ? Math.round(geometry.height) : 0,
        minimized: !!window.minimized,
        maximized: !!window.maximized ||
            (!!window.maximizedHorizontally && !!window.maximizedVertically),
        fullscreen: !!window.fullScreen,
        demandsAttention: !!window.demandsAttention,
        active: !!window.active,
        desktop: desktop,
        onAllDesktops: !!window.onAllDesktops,
        output: outputName
    };
}

function sendUpsert(window) {
    var payload = serializeWindow(window);
    if (!payload) {
        return;
    }
    callDBus(
        SERVICE,
        PATH,
        INTERFACE,
        "UpsertWindow",
        JSON.stringify(payload)
    );
}

function sendRemove(windowOrId) {
    var id = "";
    if (typeof windowOrId === "string") {
        id = windowOrId;
    } else if (windowOrId && windowOrId.internalId) {
        id = String(windowOrId.internalId);
    }
    if (!id) {
        return;
    }
    callDBus(SERVICE, PATH, INTERFACE, "RemoveWindow", id);
}

registerShortcut(
    "applicationlauncher-reopen-latest",
    "Reopen recently closed window",
    "Ctrl+Shift+T",
    reopenLatestClosedWindow
);

function trackWindow(window, deferInitialUpsert) {
    if (!window || !window.internalId) {
        return;
    }

    var id = String(window.internalId);
    if (trackedWindows[id]) {
        sendUpsert(window);
        return;
    }

    trackedWindows[id] = true;
    if (!deferInitialUpsert) {
        sendUpsert(window);
    }

    if (window.captionChanged) {
        window.captionChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.windowClassChanged) {
        window.windowClassChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.frameGeometryChanged) {
        window.frameGeometryChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.desktopsChanged) {
        window.desktopsChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.minimizedChanged) {
        window.minimizedChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.activeChanged) {
        window.activeChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.demandsAttentionChanged) {
        window.demandsAttentionChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.outputChanged) {
        window.outputChanged.connect(function () {
            sendUpsert(window);
        });
    }
    if (window.closed) {
        window.closed.connect(function () {
            delete trackedWindows[id];
            sendRemove(id);
        });
    }
}

var initialWindows = [];
for (var i = 0; i < workspace.stackingOrder.length; ++i) {
    var initialPayload = serializeWindow(workspace.stackingOrder[i]);
    if (initialPayload) {
        initialWindows.push(initialPayload);
    }
    trackWindow(workspace.stackingOrder[i], true);
}
callDBus(
    SERVICE,
    PATH,
    INTERFACE,
    "ReplaceSnapshot",
    JSON.stringify(initialWindows)
);
if (workspace.activeWindow) {
    var activePayload = serializeWindow(workspace.activeWindow);
    if (activePayload) {
        callDBus(
            SERVICE,
            PATH,
            INTERFACE,
            "WindowActivated",
            JSON.stringify(activePayload)
        );
    }
}

workspace.windowAdded.connect(function (window) {
    trackWindow(window, false);
});

workspace.windowRemoved.connect(function (window) {
    if (window && window.internalId) {
        var id = String(window.internalId);
        delete trackedWindows[id];
        sendRemove(id);
    }
});

workspace.windowActivated.connect(function (window) {
    if (window) {
        var payload = serializeWindow(window);
        if (payload) {
            callDBus(
                SERVICE,
                PATH,
                INTERFACE,
                "WindowActivated",
                JSON.stringify(payload)
            );
        }
    }
});
