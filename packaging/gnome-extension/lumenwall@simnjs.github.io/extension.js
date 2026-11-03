// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Lumenwall desktop state: publishes, for each monitor, whether a fullscreen or a maximised window is shown on
// it, so the Lumenwall wallpaper engine can pause the wallpaper behind it.
//
// On GNOME, Lumenwall draws the wallpaper in XWayland desktop windows and cannot see native Wayland windows.
// It notices a fullscreen window because the compositor stops painting the desktop, but a maximised window
// leaves the strip under the top bar visible, so nothing tells it that the desktop is covered. This extension
// tells it, on the session bus:
//
//   name       io.github.simnjs.Lumenwall.Shell
//   object     /io/github/simnjs/Lumenwall/Shell
//   interface  io.github.simnjs.Lumenwall.Shell1
//     GetMonitors() -> a(siiiiu)          connector, x, y, width, height, flags (1 fullscreen, 2 maximised)
//     signal MonitorsChanged(a(siiiiu))   sent only when something changed
//
// Install: copy this folder to ~/.local/share/gnome-shell/extensions/, log out and back in (Wayland sessions
// only discover new extensions at login), then `gnome-extensions enable lumenwall@simnjs.github.io`.
// Lumenwall finds it by itself. The extension does nothing else: no UI, no settings, no network, no polling.

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Meta from 'gi://Meta';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'io.github.simnjs.Lumenwall.Shell';
const OBJECT_PATH = '/io/github/simnjs/Lumenwall/Shell';
const INTERFACE_XML = `<node>
  <interface name="io.github.simnjs.Lumenwall.Shell1">
    <method name="GetMonitors">
      <arg name="monitors" type="a(siiiiu)" direction="out"/>
    </method>
    <signal name="MonitorsChanged">
      <arg name="monitors" type="a(siiiiu)"/>
    </signal>
    <property name="Version" type="u" access="read"/>
  </interface>
</node>`;

const FULLSCREEN = 1;
const MAXIMIZED = 2;

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
        // Managed windows and their handler ids; a closing window leaves the map before its actor goes away.
        this._windows = new Map();

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
    }

    // D-Bus property.
    get Version() {
        return 1;
    }

    // D-Bus method.
    GetMonitors() {
        return this._state;
    }

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
        for (let i = 0; i < count; i++) {
            const g = display.get_monitor_geometry(i);
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
