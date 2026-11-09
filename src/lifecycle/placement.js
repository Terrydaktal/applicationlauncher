/* One bounded compositor operation for the daemon's terminal replacement job. */
(function () {
    "use strict";
    function rect(value) {
        return value ? {x: Math.round(value.x), y: Math.round(value.y),
            width: Math.round(value.width), height: Math.round(value.height)} : null;
    }
    function output(value) {
        if (!value || !value.name || !value.geometry) { return null; }
        var result = rect(value.geometry);
        result.name = String(value.name);
        result.scale = Math.round((value.devicePixelRatio || 1) * 1000);
        return result;
    }
    function terminal(window) {
        var name = String(window.resourceClass).toLowerCase();
        return !window.deleted && !window.dialog &&
            (name.indexOf("xfce4-terminal") !== -1 || name === "org.xfce.terminal");
    }
    function snapshot(window) {
        return {id: String(window.internalId), pid: window.pid, title: String(window.caption),
            geometry: rect(window.frameGeometry), output: output(window.output),
            normal: rect(window.fullScreen ? window.fullscreenGeometryRestore : window.maximizeGeometryRestore),
            maximized: typeof window.maximizeMode === "number" ? window.maximizeMode :
                ((window.maximizedVertically ? 1 : 0) | (window.maximizedHorizontally ? 2 : 0)),
            fullscreen: !!window.fullScreen, minimized: !!window.minimized, active: !!window.active,
            desktops: (window.desktops || []).map(function (desktop) { return String(desktop.id); }),
            activities: window.activities || [], keepAbove: !!window.keepAbove, keepBelow: !!window.keepBelow,
            noBorder: !!window.noBorder, shaded: !!window.shade, skipTaskbar: !!window.skipTaskbar,
            skipPager: !!window.skipPager, skipSwitcher: !!window.skipSwitcher,
            above: workspace.stackingOrder.filter(function (other) {
                return other !== window && other.stackingOrder > window.stackingOrder;
            }).map(function (other) { return String(other.internalId); })};
    }
    try {
        var windows = workspace.windowList().filter(function (window) {
            return terminal(window) && window.pid === request.pid;
        });
        var result;
        if (request.action === "list") {
            result = {windows: windows.map(snapshot)};
        } else {
            if (request.id) {
                windows = windows.filter(function (window) { return String(window.internalId) === request.id; });
            }
            if (!windows.length && request.allowMissing && request.id) {
                result = {missing: true};
            } else {
                if (windows.length !== 1) { throw new Error("The exact terminal window is missing or ambiguous"); }
                var window = windows[0];
                if (request.action === "close") {
                    window.closeWindow();
                    result = {closeRequested: true};
                } else {
                    if (request.action !== "capture") {
                        var saved = request.saved;
                        var screens = workspace.screens.filter(function (screen) {
                            return String(screen.name) === saved.output.name;
                        });
                        if (screens.length !== 1 || JSON.stringify(output(screens[0])) !== JSON.stringify(saved.output)) {
                            throw new Error("Monitor geometry or scale changed");
                        }
                        if (request.action === "place") {
                            window.minimized = true;
                            window.fullScreen = false;
                            window.setMaximize(false, false);
                            ["noBorder", "keepAbove", "keepBelow", "skipTaskbar", "skipPager", "skipSwitcher"].forEach(function (key) {
                                window[key] = saved[key];
                            });
                            var desktops = workspace.desktops.filter(function (desktop) {
                                return saved.desktops.indexOf(String(desktop.id)) !== -1;
                            });
                            if (desktops.length !== saved.desktops.length) { throw new Error("A saved desktop disappeared"); }
                            window.desktops = desktops;
                            window.activities = saved.activities;
                            window.frameGeometry = (saved.maximized || saved.fullscreen) ? saved.normal : saved.geometry;
                            window.setMaximize(!!(saved.maximized & 1), !!(saved.maximized & 2));
                            window.fullScreen = saved.fullscreen;
                            window.shade = saved.shaded;
                        } else if (request.action === "present") {
                            window.minimized = saved.minimized;
                            if (saved.active && !saved.minimized) { workspace.activeWindow = window; }
                            else if (workspace.restackWindowUnder) {
                                var above = workspace.windowList().filter(function (other) {
                                    return saved.above.indexOf(String(other.internalId)) !== -1;
                                });
                                if (above.length) { workspace.restackWindowUnder(window, above[0]); }
                            }
                        } else { throw new Error("Unknown placement action"); }
                    }
                    result = snapshot(window);
                }
            }
        }
        callDBus(request.service, request.path, "com.terrydaktal.ApplicationLauncher.Placement1",
            "Result", request.nonce, JSON.stringify({ok: result}));
    } catch (error) {
        callDBus(request.service, request.path, "com.terrydaktal.ApplicationLauncher.Placement1",
            "Result", request.nonce, JSON.stringify({error: String(error)}));
    }
}());
