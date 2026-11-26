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

  function appearance(wasm) {
    const roles = ['Dark','Mid','Hi','Contrast','Accent','AccentBright','Error','Confirm','Warning'];
    const textEncoder = new TextEncoder();
    const hex = rgb => rgb.toString(16).padStart(6,'0');
    function normalized(kind, text) {
      if(typeof text!=='string' || text.length>=400000)return undefined;
      const bytes=textEncoder.encode(text);
      const buffer=wasm.appearance_buffer(bytes.length);
      if(!buffer)return undefined;
      new Uint8Array(wasm.memory.buffer,buffer,bytes.length).set(bytes);
      const result=wasm.appearance_normalize(kind,bytes.length);
      return result<0?undefined:result;
    }
    const preset = family => Object.fromEntries(roles.map((role,index)=>[role,wasm.appearance_default(family,index)])
      .filter(([,rgb])=>rgb>=0).map(([role,rgb])=>[role,hex(rgb)]));
    const saved=preset(0);
    return {
      saved,
      manual: dark => preset(dark?1:2),
      swatch: value => {
        const text=String(value||'').replace(/^#/,'');
        return normalized(0,text)===undefined?'':'#'+text;
      },
      palette: (theme, partial=false) => {
        if(!theme || typeof theme!=='object')return null;
        const colors={};
        for(const role of roles){
          const value=theme[role] || (role==='AccentBright' && theme.AccentHi);
          const text=partial?String(value||'').replace(/^#/,''):value;
          if(normalized(0,text)!==undefined)colors[role]=partial?text:text.slice(0,6);
        }
        if(partial)return colors;
        return roles.slice(0,5).every(role=>colors[role])?
          {...saved,...colors,AccentBright:colors.AccentBright||colors.Accent}:null;
      },
      paletteChoice: (follow,pinned,wrist,base,album) => {
        const value=wasm.appearance_selection(follow,pinned,Boolean(wrist),wrist?.album,wrist?.custom,Boolean(base),Boolean(album));
        return {source:value>>8,theme:[null,wrist?.theme,album,base,saved][value&255]};
      },
      colors: theme => {
        const inputs=roles.slice(0,5).map(role=>Number.parseInt(theme[role],16));
        const result=field=>wasm.appearance_colors(...inputs,field);
        const ink=result(0);
        return {ink:'#'+hex(ink),onAccent:'#'+hex(result(1)),dark:Boolean(wasm.appearance_dark(ink)),
          controls:{accent:'#'+hex(result(2)),bed:'#'+hex(result(3)),ink:'#'+hex(result(4))}};
      },
      boolean: text => {
        const value=normalized(1,text);
        return value===undefined?undefined:Boolean(value);
      },
      snap: text => normalized(2,text),
      image: (value,legacy=false) => normalized(legacy?5:4,value)!==undefined,
      mode: value => normalized(3,value),
      background: (value,hasImage,current) => {
        const mode=normalized(3,value)??-1;
        const previous=normalized(3,current)??0;
        return wasm.appearance_background(mode,hasImage,previous)?'custom':'album';
      },
    };
  }

  window.LumControls = core.then(wasm => ({
    appearance: appearance(wasm),
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
