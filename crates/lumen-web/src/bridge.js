// Lumen web bridge: the public Wallpaper Engine web wallpaper API.
// Injected into the main frame before any page script runs.
//
// Host -> page: the host evaluates `window.__lumenWE(message)`.
// Page -> host: `window.__lumenHostPost(json)` (native function installed by the host right before
// this script; captured here, then removed from the global object).
(function () {
  'use strict';
  var g = typeof window !== 'undefined' ? window : globalThis;
  if (g.__lumenWE) {
    return;
  }

  var hostPost = g.__lumenHostPost;
  try {
    delete g.__lumenHostPost;
  } catch (e) {
    g.__lumenHostPost = undefined;
  }
  if (typeof hostPost !== 'function') {
    hostPost = function () {};
  }

  function send(message) {
    try {
      hostPost(JSON.stringify(message));
    } catch (e) {
      report(e);
    }
  }

  function report(error) {
    if (typeof console !== 'undefined' && console && typeof console.error === 'function') {
      console.error(error);
    }
  }

  function invoke(fn, self, args) {
    try {
      fn.apply(self, args);
    } catch (e) {
      report(e);
    }
  }

  function later(fn) {
    if (typeof g.queueMicrotask === 'function') {
      g.queueMicrotask(fn);
    } else if (typeof Promise === 'function') {
      Promise.resolve().then(fn);
    } else {
      fn();
    }
  }

  // ---- window.wallpaperPropertyListener -------------------------------------------------------
  // Events are queued until the page's listener (and the relevant method) exists, so a page that
  // assigns its listener late still receives the initial properties.
  var listener;
  var pendingUser = null;
  var pendingGeneral = null;
  var pendingPaused = false;
  var pendingDirs = [];

  function merge(target, source) {
    for (var key in source) {
      if (Object.prototype.hasOwnProperty.call(source, key)) {
        target[key] = source[key];
      }
    }
    return target;
  }

  function method(name) {
    var l = listener;
    if (l === null || (typeof l !== 'object' && typeof l !== 'function')) {
      return null;
    }
    var fn = l[name];
    return typeof fn === 'function' ? fn : null;
  }

  function flush() {
    var fn;
    if (pendingUser !== null && (fn = method('applyUserProperties'))) {
      var user = pendingUser;
      pendingUser = null;
      invoke(fn, listener, [user]);
    }
    if (pendingGeneral !== null && (fn = method('applyGeneralProperties'))) {
      var general = pendingGeneral;
      pendingGeneral = null;
      invoke(fn, listener, [general]);
    }
    if (pendingDirs.length > 0) {
      var rest = [];
      for (var i = 0; i < pendingDirs.length; i++) {
        var entry = pendingDirs[i];
        if ((fn = method(entry[0]))) {
          invoke(fn, listener, [entry[1], entry[2]]);
        } else {
          rest.push(entry);
        }
      }
      pendingDirs = rest;
    }
    if (pendingPaused && (fn = method('setPaused'))) {
      pendingPaused = false;
      invoke(fn, listener, [true]);
    }
  }

  var existing = g.wallpaperPropertyListener;
  Object.defineProperty(g, 'wallpaperPropertyListener', {
    configurable: true,
    enumerable: true,
    get: function () {
      return listener;
    },
    set: function (value) {
      listener = value;
      // Deferred so `listener = {}; listener.applyUserProperties = ...` also works.
      later(flush);
    }
  });
  if (existing !== undefined) {
    listener = existing;
  }

  if (typeof g.addEventListener === 'function') {
    g.addEventListener('DOMContentLoaded', flush);
    g.addEventListener('load', flush);
  }

  // ---- Audio ------------------------------------------------------------------------------------
  var audioListener = null;

  g.wallpaperRegisterAudioListener = function (callback) {
    audioListener = typeof callback === 'function' ? callback : null;
    send({ t: 'listen', k: 'audio', on: audioListener !== null });
  };

  // ---- Media integration ------------------------------------------------------------------------
  var mediaListeners = {
    status: null,
    properties: null,
    thumbnail: null,
    playback: null,
    timeline: null
  };

  function mediaRegistration(kind) {
    return function (callback) {
      mediaListeners[kind] = typeof callback === 'function' ? callback : null;
      send({ t: 'listen', k: kind, on: mediaListeners[kind] !== null });
    };
  }

  g.wallpaperRegisterMediaStatusListener = mediaRegistration('status');
  g.wallpaperRegisterMediaPropertiesListener = mediaRegistration('properties');
  g.wallpaperRegisterMediaThumbnailListener = mediaRegistration('thumbnail');
  g.wallpaperRegisterMediaPlaybackListener = mediaRegistration('playback');
  g.wallpaperRegisterMediaTimelineListener = mediaRegistration('timeline');

  // Both spellings appear in the public documentation.
  var PLAYBACK_STOPPED = 0;
  var PLAYBACK_PLAYING = 1;
  var PLAYBACK_PAUSED = 2;
  g.wallpaperMediaIntegration = {
    PLAYBACK_STOPPED: PLAYBACK_STOPPED,
    PLAYBACK_PLAYING: PLAYBACK_PLAYING,
    PLAYBACK_PAUSED: PLAYBACK_PAUSED,
    playback: {
      STOPPED: PLAYBACK_STOPPED,
      PLAYING: PLAYBACK_PLAYING,
      PAUSED: PLAYBACK_PAUSED
    }
  };

  // ---- Directory properties ("ondemand") ---------------------------------------------------------
  var randomRequests = {};
  var nextRequest = 1;

  g.wallpaperRequestRandomFileForProperty = function (propertyName, callback) {
    var id = nextRequest++;
    if (typeof callback === 'function') {
      randomRequests[id] = callback;
    }
    send({ t: 'randomFile', id: id, n: String(propertyName) });
  };

  // ---- Host -> page -------------------------------------------------------------------------------
  function dispatch(m) {
    if (m === null || typeof m !== 'object') {
      return;
    }
    var fn;
    switch (m.t) {
      case 'audio':
        if (audioListener !== null) {
          invoke(audioListener, g, [m.d]);
        }
        break;
      case 'user':
        pendingUser = merge(pendingUser === null ? {} : pendingUser, m.p);
        flush();
        break;
      case 'general':
        pendingGeneral = merge(pendingGeneral === null ? {} : pendingGeneral, m.p);
        flush();
        break;
      case 'paused':
        if ((fn = method('setPaused'))) {
          pendingPaused = false;
          invoke(fn, listener, [m.v === true]);
        } else {
          pendingPaused = m.v === true;
        }
        break;
      case 'dirChanged':
        pendingDirs.push(['userDirectoryFilesAddedOrChanged', m.n, m.f]);
        flush();
        break;
      case 'dirRemoved':
        pendingDirs.push(['userDirectoryFilesRemoved', m.n, m.f]);
        flush();
        break;
      case 'media':
        if ((fn = mediaListeners[m.k])) {
          invoke(fn, g, [m.e]);
        }
        break;
      case 'randomFile':
        fn = randomRequests[m.id];
        delete randomRequests[m.id];
        if (fn && typeof m.f === 'string' && m.f.length > 0) {
          invoke(fn, g, [m.n, m.f]);
        }
        break;
      default:
        break;
    }
  }

  Object.defineProperty(g, '__lumenWE', {
    value: dispatch,
    configurable: false,
    enumerable: false,
    writable: false
  });

  send({ t: 'ready' });
})();
