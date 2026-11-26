/* The browser owns DOM events and HTTP. The same Rust module owns both control sessions. */
(() => {
  'use strict';
  const url = new URL('controls.wasm', document.currentScript.src);
  const core = WebAssembly.instantiateStreaming(fetch(url)).then(({instance}) => instance.exports);
  const fields = ['micGain', 'musicGain', 'micMuted', 'musicMuted'];
  const keys = {ArrowUp:1, ArrowRight:1, ArrowDown:2, ArrowLeft:2, Home:3, End:4, PageUp:5, PageDown:6};

  const duckStates = ['unavailable', 'off', 'waiting_mic', 'waiting_music', 'mic_muted', 'music_muted', 'ready', 'ducking'];
  const duckFields = ['enabled','depthDb','migrate'];

  function decode(read, names = fields) {
    const values = {};
    for (const [index, key] of names.entries()) {
      const value = read(index);
      if (value >= 0) values[key] = key.endsWith('Muted') || key === 'enabled' || key === 'migrate' ? Boolean(value) : value;
    }
    return values;
  }

  function request(kind, data) {
    if (kind === 0) return undefined;
    return kind === 1 ? null : data();
  }

  const preferenceKeys = ['sounds','followTheme','dark','companion','globalPulse','closeToTray','snap','background','image'];
  const mask = values => preferenceKeys.reduce((bits,key,index) =>
    Object.hasOwn(values,key) ? bits | (1 << index) : bits, 0);
  const keysIn = bits => preferenceKeys.filter((_,index) => bits & (1 << index));

  window.LumControls = core.then(wasm => ({
    slider: (min, max, preference, depth, value, key) =>
      wasm.slider_value(min, max, preference, depth, value, Object.hasOwn(keys, key) ? keys[key] : 0),
    handlesKey: key => Object.hasOwn(keys, key),
    change: patch => fields.forEach((key, index) => {
      if (Object.hasOwn(patch, key)) wasm.gain_change(index, Number(patch[key]));
    }),
    shown: () => decode(wasm.gain_shown),
    pending: () => Boolean(wasm.gain_pending()),
    online: () => Boolean(wasm.gain_online()),
    fail: wasm.gain_failure,
    next: (canWrite, poll) => request(wasm.gain_next(canWrite, poll), () => decode(wasm.gain_writing)),
    commit: result => wasm.gain_success(...fields.map(key =>
      Number.isFinite(result[key]) || typeof result[key] === 'boolean' ? Number(result[key]) : -1)),
    actions: ['spotify','mic','setup'].reduce((channels,name,index) => {
      channels[name] = {
        read: () => wasm.action_read(index),
        current: version => Boolean(wasm.action_current(index,version)),
        reading: () => Boolean(wasm.action_reading(index)),
        finishRead: version => wasm.action_finish_read(index,version),
        cancelRead: () => wasm.action_cancel_read(index),
        write: () => Boolean(wasm.action_write(index)),
        writing: () => Boolean(wasm.action_writing(index)),
        finishWrite: () => wasm.action_finish_write(index),
      };
      return channels;
    },{}),
    preferences: {
      change: values => wasm.preferences_change(mask(values)),
      batch: () => keysIn(wasm.preferences_begin_write()),
      confirm: () => keysIn(wasm.preferences_acknowledge()),
      migration: (legacy,saved) => keysIn(wasm.preferences_migration_keys(mask(legacy),mask(saved))),
      finishMigration: wasm.preferences_finish_migration,
      needsRead: hasToken => Boolean(wasm.preferences_read(hasToken)),
    },
    duck: {
      next: (canWrite, poll) =>
        request(wasm.duck_next(canWrite,poll), () => decode(wasm.duck_writing,duckFields)),
      change: ({enabled,depthDb}) => wasm.duck_change(enabled===undefined?-1:Number(enabled),depthDb??-1),
      migrate: (legacy,configured) => wasm.duck_migrate(Number.isInteger(legacy)?legacy:-1,configured),
      commit: result => wasm.duck_success(Number(result.enabled),result.depthDb,duckStates.indexOf(result.state)),
      online: () => Boolean(wasm.duck_online()),
      enabled: () => Boolean(wasm.duck_enabled()),
      depth: wasm.duck_depth,
      fail: wasm.duck_failure,
      pollMs: wasm.duck_poll_ms,
    },
  }));
})();
