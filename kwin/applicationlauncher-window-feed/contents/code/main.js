var SERVICE = "com.terrydaktal.ApplicationLauncher";
var PATH = "/WindowFeed";
var INTERFACE = "com.terrydaktal.ApplicationLauncher.WindowFeed";

var trackedWindows = {};

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
        minimized: !!window.minimized
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

function trackWindow(window) {
    if (!window || !window.internalId) {
        return;
    }

    var id = String(window.internalId);
    if (trackedWindows[id]) {
        sendUpsert(window);
        return;
    }

    trackedWindows[id] = true;
    sendUpsert(window);

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

for (var i = 0; i < workspace.stackingOrder.length; ++i) {
    trackWindow(workspace.stackingOrder[i]);
}

workspace.windowAdded.connect(function (window) {
    trackWindow(window);
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
        sendUpsert(window);
    }
});
