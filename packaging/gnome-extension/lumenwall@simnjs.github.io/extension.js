// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Lumenwall desktop state: publishes, for each monitor, whether a fullscreen or a maximised window is shown on
// it, so the Lumenwall wallpaper engine can pause the wallpaper behind it; and, while Lumenwall asks for it, the
// pointer position and the clicks that land on the desktop, so wallpapers can follow the cursor and react to
// clicks (a desktop-icons window such as Desktop Icons NG covers the desktop and takes the clicks).
//
// On GNOME, Lumenwall draws the wallpaper in XWayland desktop windows and cannot see native Wayland windows nor
// the pointer above them. It notices a fullscreen window because the compositor stops painting the desktop, but
// a maximised window leaves the strip under the top bar visible, so nothing tells it that the desktop is
// covered. This extension tells it, on the session bus:
//
//   name       io.github.simnjs.Lumenwall.Shell
//   object     /io/github/simnjs/Lumenwall/Shell
//   interface  io.github.simnjs.Lumenwall.Shell1
//     GetMonitors() -> a(siiiiu)          connector, x, y, width, height, flags (1 fullscreen, 2 maximised)
//     signal MonitorsChanged(a(siiiiu))   sent only when something changed
//     WatchPointer(u flags, u max_rate)   (version 2) flags: 1 position, 2 desktop clicks; 0 stops. max_rate in
//                                         Hz (0: once per frame). Per caller, dropped when the caller leaves.
//     signal PointerMotion(d x, d y, s connector)
//         sent to the caller only: global logical position, at most once per frame and max_rate, only when
//         the pointer moved; the current position right after WatchPointer.
//     signal PointerButton(d x, d y, s connector, u button, b pressed)
//         sent to the caller only: 1 left, 2 middle, 3 right (right clicks too: Wallpaper Engine seldom uses
//         them, they also open the desktop menu). A press is sent when it lands on the desktop: no window under
//         the pointer, or a desktop-type window (Desktop Icons NG, icons included, like Wallpaper Engine on
//         Windows); never on application windows, the top bar, docks, menus, the overview or while a Shell
//         grab is active. The release of a press that was sent always follows.
//     property Version (u)
//
// Input is observed, never consumed: GNOME Shell extensions cannot see the events that Mutter hands to Wayland
// clients, so the pointer state is read when Mutter's idle monitor reports user activity (at the first input
// event, then once per frame while input continues, then at the next event again) and compared with the
// previous one. Nothing runs while nobody watches the pointer or while the input is idle. A press and release
// that both fall between two looks of a moving pointer (within one frame) are not seen.
//
// Install: copy this folder to ~/.local/share/gnome-shell/extensions/, log out and back in (Wayland sessions
// only discover new or updated extensions at login), then `gnome-extensions enable lumenwall@simnjs.github.io`.
// Lumenwall finds it by itself. The extension does nothing else: no UI, no settings, no network, no polling.

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Meta from 'gi://Meta';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const BUS_NAME = 'io.github.simnjs.Lumenwall.Shell';
const OBJECT_PATH = '/io/github/simnjs/Lumenwall/Shell';
const INTERFACE = 'io.github.simnjs.Lumenwall.Shell1';
const INTERFACE_XML = `<node>
  <interface name="${INTERFACE}">
    <method name="GetMonitors">
      <arg name="monitors" type="a(siiiiu)" direction="out"/>
    </method>
    <signal name="MonitorsChanged">
      <arg name="monitors" type="a(siiiiu)"/>
    </signal>
    <method name="WatchPointer">
      <arg name="flags" type="u" direction="in"/>
      <arg name="max_rate" type="u" direction="in"/>
    </method>
    <signal name="PointerMotion">
      <arg name="x" type="d"/>
      <arg name="y" type="d"/>
      <arg name="connector" type="s"/>
    </signal>
    <signal name="PointerButton">
      <arg name="x" type="d"/>
      <arg name="y" type="d"/>
      <arg name="connector" type="s"/>
      <arg name="button" type="u"/>
      <arg name="pressed" type="b"/>
    </signal>
    <property name="Version" type="u" access="read"/>
  </interface>
</node>`;

const FULLSCREEN = 1;
const MAXIMIZED = 2;

// WatchPointer flags.
const WATCH_MOTION = 1;
const WATCH_BUTTONS = 2;

// Pointer buttons reported, with their modifier bit. Mutter's native backend orders the button bits like the
// evdev codes (left, right, middle), not like Clutter's button numbers (measured on GNOME 50.1: the button that
// opens the desktop menu sets BUTTON2_MASK).
const BUTTONS = [
    [Clutter.ModifierType.BUTTON1_MASK, 1],
    [Clutter.ModifierType.BUTTON2_MASK, 3],
    [Clutter.ModifierType.BUTTON3_MASK, 2],
];
const BUTTON_MASK = BUTTONS.reduce((all, [mask]) => all | mask, 0);

// Window changes that can start or end "maximised on monitor N".
const WINDOW_SIGNALS = [
    'notify::maximized-horizontally',
    'notify::maximized-vertically',
    'notify::minimized',
    'notify::fullscreen',
    'notify::skip-taskbar',
    'workspace-changed',
];

export default class LumenwallStateExtension extends Extension {
    enable() {
        this._state = [];
        this._key = '';
        this._signals = [];
        // Logical monitors: geometry and first connector, for the pointer signals.
        this._monitors = [];
        // Managed windows and their handler ids; a closing window leaves the map before its actor goes away.
        this._windows = new Map();
        // WatchPointer callers: unique name -> {flags, interval (µs), watch (name watcher id)}.
        this._watchers = new Map();
        this._pointer = null;

        this._dbus = Gio.DBusExportedObject.wrapJSObject(INTERFACE_XML, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);

        const display = global.display;
        this._connect(display, 'window-created', (_display, window) => {
            this._watch(window);
            this._update();
        });
        // Updates run right away (a few dozen windows to look at): Lumenwall should hear of a fullscreen window
        // before the compositor stops painting the desktop, while its renderer can still be told to stop.
        for (const name of ['in-fullscreen-changed', 'restacked', 'window-entered-monitor',
            'window-left-monitor', 'workareas-changed'])
            this._connect(display, name, () => this._update());
        this._connect(global.workspace_manager, 'active-workspace-changed', () => this._update());
        const monitorManager = global.backend?.get_monitor_manager?.();
        if (monitorManager)
            this._connect(monitorManager, 'monitors-changed', () => this._update());

        for (const actor of global.get_window_actors())
            this._watch(actor.meta_window);
        this._update();

        // Owned last: Lumenwall asks for the state as soon as the name appears.
        this._name = Gio.bus_own_name_on_connection(Gio.DBus.session, BUS_NAME,
            Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        if (this._name) {
            Gio.bus_unown_name(this._name);
            this._name = 0;
        }
        for (const watcher of this._watchers.values())
            Gio.bus_unwatch_name(watcher.watch);
        this._watchers = null;
        this._stopPointer();
        for (const [object, id] of this._signals)
            object.disconnect(id);
        this._signals = null;
        for (const [window, ids] of this._windows) {
            for (const id of ids)
                window.disconnect(id);
        }
        this._windows = null;
        this._dbus?.unexport();
        this._dbus = null;
        this._state = null;
        this._monitors = null;
    }

    // D-Bus property.
    get Version() {
        return 2;
    }

    // D-Bus method.
    GetMonitors() {
        return this._state;
    }

    // D-Bus method: (flags, max_rate) for the calling connection.
    WatchPointerAsync([flags, maxRate], invocation) {
        const sender = invocation.get_sender();
        if (!sender) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.InvalidArgs', 'no sender');
            return;
        }
        flags &= WATCH_MOTION | WATCH_BUTTONS;
        const old = this._watchers.get(sender);
        if (flags === 0) {
            if (old) {
                Gio.bus_unwatch_name(old.watch);
                this._watchers.delete(sender);
            }
        } else {
            const interval = maxRate > 0 ? Math.ceil(1e6 / maxRate) : 0;
            const watch = old?.watch ?? Gio.bus_watch_name_on_connection(Gio.DBus.session, sender,
                Gio.BusNameWatcherFlags.NONE, null, () => this._forget(sender));
            this._watchers.set(sender, {flags, interval, watch});
        }
        invocation.return_value(null);
        this._pointerWatchersChanged();
        // A new watcher starts from the current position, whether the pointer moves or not.
        if (flags & WATCH_MOTION && !(old && old.flags & WATCH_MOTION) && this._pointer) {
            const [point] = this._pointer.tracker.get_pointer();
            this._emitTo(sender, 'PointerMotion',
                new GLib.Variant('(dds)', [point.x, point.y, this._connectorAt(point.x, point.y)]));
        }
    }

    // The caller left the bus without saying so.
    _forget(sender) {
        const watcher = this._watchers?.get(sender);
        if (!watcher)
            return;
        Gio.bus_unwatch_name(watcher.watch);
        this._watchers.delete(sender);
        this._pointerWatchersChanged();
    }

    // ------------------------------------------------------------------------------------------------------
    // Pointer.

    _pointerWatchersChanged() {
        let interval = Infinity;
        for (const w of this._watchers.values()) {
            if (w.flags & WATCH_MOTION)
                interval = Math.min(interval, w.interval);
        }
        if (this._watchers.size === 0) {
            this._stopPointer();
            return;
        }
        if (!this._pointer) {
            const tracker = global.backend.get_cursor_tracker();
            const [, mods] = tracker.get_pointer();
            this._pointer = {
                tracker,
                idle: global.backend.get_core_idle_monitor(),
                laters: global.compositor.get_laters(),
                activeId: 0,
                laterId: 0,
                rateId: 0,
                // Buttons already held when the watch starts are never reported.
                buttons: mods & BUTTON_MASK,
                // Presses that were sent: their release follows wherever it happens.
                sent: 0,
                // Last position sent, last position looked at.
                x: NaN,
                y: NaN,
                seenX: NaN,
                seenY: NaN,
                lastEmit: 0,
                // Whether anyone wants the position, and how often at most (µs between signals).
                motion: false,
                interval: 0,
            };
            this._armActivity();
        }
        this._pointer.motion = Number.isFinite(interval);
        this._pointer.interval = this._pointer.motion ? interval : 0;
    }

    _stopPointer() {
        const p = this._pointer;
        if (!p)
            return;
        if (p.activeId)
            p.idle.remove_watch(p.activeId);
        if (p.laterId)
            p.laters.remove(p.laterId);
        if (p.rateId)
            GLib.source_remove(p.rateId);
        this._pointer = null;
    }

    // One callback at the next input event of any kind (Mutter resets the idle time on each of them).
    _armActivity() {
        const p = this._pointer;
        if (p && !p.activeId)
            p.activeId = p.idle.add_user_active_watch(() => this._onActivity());
    }

    _onActivity() {
        const p = this._pointer;
        if (!p)
            return;
        p.activeId = 0;
        // Buttons right away (before a menu opened by this click can take a grab), the position at the frame.
        this._checkButtons();
        this._lookAtNextFrame();
    }

    _lookAtNextFrame() {
        const p = this._pointer;
        if (p.laterId)
            return;
        p.laterId = p.laters.add(Meta.LaterType.BEFORE_REDRAW, () => {
            p.laterId = 0;
            if (this._pointer !== p)
                return GLib.SOURCE_REMOVE;
            this._checkButtons();
            // While the pointer moves it is looked at once per frame, and once more after it stopped (the last
            // events of a burst can be applied after a look); then the activity watch takes over again.
            if (this._checkMotion())
                this._lookAtNextFrame();
            else
                this._armActivity();
            return GLib.SOURCE_REMOVE;
        });
    }

    _checkButtons() {
        const p = this._pointer;
        const [point, mods] = p.tracker.get_pointer();
        const now = mods & BUTTON_MASK;
        const changed = now ^ p.buttons;
        if (!changed)
            return;
        p.buttons = now;
        for (const [mask, button] of BUTTONS) {
            if (!(changed & mask))
                continue;
            const pressed = (now & mask) !== 0;
            if (pressed) {
                if (!this._onDesktop(point.x, point.y))
                    continue;
                p.sent |= mask;
            } else if (p.sent & mask) {
                p.sent &= ~mask;
            } else {
                continue;
            }
            this._emitButton(point.x, point.y, button, pressed);
        }
    }

    // Sends the position when it changed and the rate allows it. Returns whether it changed since the last look.
    _checkMotion() {
        const p = this._pointer;
        if (!p.motion)
            return false;
        const [point] = p.tracker.get_pointer();
        const moved = point.x !== p.seenX || point.y !== p.seenY;
        p.seenX = point.x;
        p.seenY = point.y;
        if (p.rateId || (point.x === p.x && point.y === p.y))
            return moved;
        const now = GLib.get_monotonic_time();
        const wait = p.lastEmit + p.interval - now;
        if (wait > 0) {
            // Rate cap: the latest position goes out when the interval is over.
            p.rateId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, Math.ceil(wait / 1000), () => {
                p.rateId = 0;
                if (this._pointer === p)
                    this._checkMotion();
                return GLib.SOURCE_REMOVE;
            });
            return moved;
        }
        p.x = point.x;
        p.y = point.y;
        p.lastEmit = now;
        const value = new GLib.Variant('(dds)', [point.x, point.y, this._connectorAt(point.x, point.y)]);
        for (const sender of this._watchersWith(WATCH_MOTION))
            this._emitTo(sender, 'PointerMotion', value);
        return moved;
    }

    _emitButton(x, y, button, pressed) {
        const value = new GLib.Variant('(ddsub)', [x, y, this._connectorAt(x, y), button, pressed]);
        for (const sender of this._watchersWith(WATCH_BUTTONS))
            this._emitTo(sender, 'PointerButton', value);
    }

    * _watchersWith(flag) {
        for (const [sender, w] of this._watchers) {
            if (w.flags & flag)
                yield sender;
        }
    }

    _emitTo(sender, name, value) {
        try {
            Gio.DBus.session.emit_signal(sender, OBJECT_PATH, INTERFACE, name, value);
        } catch (e) {
            logError(e, 'Lumenwall: pointer signal');
        }
    }

    // Whether a click at (x, y) lands on the desktop.
    _onDesktop(x, y) {
        // Shell menus, popups of applications, modal dialogs, the screenshot UI and the overview hold a grab:
        // the click closes or acts on them.
        if (global.stage.get_grab_actor() !== null || Main.overview.visible)
            return false;
        const actor = global.stage.get_actor_at_pos(Clutter.PickMode.REACTIVE, x, y);
        for (let a = actor; a; a = a.get_parent()) {
            if (a instanceof Meta.WindowActor) {
                // Desktop-type windows: Lumenwall's own, Desktop Icons NG.
                return a.get_meta_window()?.get_window_type() === Meta.WindowType.DESKTOP;
            }
            // The background of the window group, with no window above it.
            if (a === global.window_group)
                return true;
        }
        // Nothing reactive at all is the bare stage; anything else is Shell chrome (panel, dock, notifications).
        return actor === null || actor === global.stage;
    }

    _connectorAt(x, y) {
        for (const m of this._monitors) {
            if (x >= m.x && y >= m.y && x < m.x + m.width && y < m.y + m.height)
                return m.connector;
        }
        return '';
    }

    // ------------------------------------------------------------------------------------------------------
    // Monitors.

    _connect(object, name, callback) {
        this._signals.push([object, object.connect(name, callback)]);
    }

    _watch(window) {
        if (!window || this._windows.has(window))
            return;
        const ids = WINDOW_SIGNALS.map(name => window.connect(name, () => this._update()));
        const forget = () => {
            for (const id of this._windows.get(window) ?? [])
                window.disconnect(id);
            if (this._windows.delete(window))
                this._update();
        };
        for (const name of ['unmanaging', 'unmanaged']) {
            try {
                ids.push(window.connect(name, forget));
            } catch {
                // 'unmanaging' is missing on older GNOME versions; 'unmanaged' is enough there.
            }
        }
        this._windows.set(window, ids);
    }

    _update() {
        const state = this._compute();
        const key = JSON.stringify(state);
        if (key === this._key)
            return;
        this._key = key;
        this._state = state;
        this._dbus.emit_signal('MonitorsChanged', new GLib.Variant('(a(siiiiu))', [state]));
    }

    _compute() {
        const display = global.display;
        const workspace = global.workspace_manager.get_active_workspace();
        const count = display.get_n_monitors();
        const flags = new Array(count).fill(0);
        for (let i = 0; i < count; i++) {
            if (display.get_monitor_in_fullscreen(i))
                flags[i] |= FULLSCREEN;
        }
        for (const window of this._windows.keys()) {
            if (window.minimized || window.skip_taskbar ||
                window.get_window_type() !== Meta.WindowType.NORMAL)
                continue;
            if (!window.maximized_horizontally || !window.maximized_vertically)
                continue;
            if (!window.located_on_workspace(workspace) || !window.showing_on_its_workspace())
                continue;
            const monitor = window.get_monitor();
            if (monitor >= 0 && monitor < count)
                flags[monitor] |= MAXIMIZED;
        }

        const connectors = this._connectors(count);
        const state = [];
        this._monitors = [];
        for (let i = 0; i < count; i++) {
            const g = display.get_monitor_geometry(i);
            this._monitors.push({x: g.x, y: g.y, width: g.width, height: g.height, connector: connectors[i][0]});
            // Mirrored monitors share one logical monitor: one entry per connector.
            for (const connector of connectors[i])
                state.push([connector, g.x, g.y, g.width, g.height, flags[i]]);
        }
        return state;
    }

    // Connector names per logical monitor index; [''] when this GNOME version does not expose them
    // (Lumenwall then matches the geometry).
    _connectors(count) {
        const names = Array.from({length: count}, () => []);
        const logical = global.backend?.get_monitor_manager?.()?.get_logical_monitors?.() ?? [];
        for (const lm of logical) {
            const index = lm.get_number?.();
            if (index === undefined || index < 0 || index >= count)
                continue;
            for (const monitor of lm.get_monitors?.() ?? []) {
                const connector = monitor.get_connector?.();
                if (connector)
                    names[index].push(connector);
            }
        }
        return names.map(n => (n.length > 0 ? n : ['']));
    }
}
