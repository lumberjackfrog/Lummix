/* Desktop and wrist faders share the native Lummix output gain. */
(async () => {
  const url = 'http://127.0.0.1:41838/mix/gains';
  const feedback = document.getElementById('gain-feedback');
  let token = '', timer = 0, retryAt = 0;
  let controls;
  try {
    controls = await window.LumControls;
  } catch {
    if (feedback) feedback.textContent = 'Output controls unavailable';
    return;
  }

  function draw() {
    const values = controls.shown();
    if (Number.isFinite(values.micGain)) get('mic').value = values.micGain;
    if (Number.isFinite(values.musicGain)) get('music').value = values.musicGain;
    if (typeof values.micMuted === 'boolean') state.micMuted = values.micMuted;
    if (typeof values.musicMuted === 'boolean') state.spotifyMuted = values.musicMuted;
    render();
  }

  async function request(patch) {
    const options = {cache:'no-store', credentials:'omit', signal:AbortSignal.timeout(5000)};
    if (patch) Object.assign(options, {
      method:'POST', headers:{'Content-Type':'application/json', 'X-Lum-Mix-Token':token},
      body:JSON.stringify(patch),
    });
    const response = await fetch(url, options);
    if (!response.ok) throw new Error(String(response.status));
    return response.json();
  }

  async function exchange(poll = false) {
    if (Date.now() < retryAt) return;
    let patch = controls.next(Boolean(token), poll);
    while (patch !== undefined) {
      try {
        const response = await request(patch);
        if (typeof response.token !== 'string' || !response.token) throw Error('Missing mixer token');
        token = response.token;
        controls.commit(response);
        if (feedback) feedback.textContent = '';
        draw();
      } catch {
        controls.fail(true);
        token = '';
        if (feedback) feedback.textContent = controls.pending() ?
          'Output host offline · changes pending' : 'Output host unavailable';
        retryAt = Date.now() + 3000;
        clearTimeout(timer);
        timer = setTimeout(() => exchange(true), 3000);
        return;
      }
      patch = controls.next(Boolean(token), false);
    }
  }

  function set(patch) {
    controls.change(patch);
    draw();
    if (Date.now() >= retryAt) {
      clearTimeout(timer);
      timer = setTimeout(exchange, 45);
    }
  }

  for (const [id,key] of [['mic','micGain'], ['music','musicGain']]) {
    get(id).addEventListener('input', () => set({[key]:Number(get(id).value)}));
    get(id).addEventListener('change', () => exchange());
  }
  get('mic-mute').addEventListener('click', () => {
    if (usable()) { set({micMuted:!state.micMuted}); cue(); }
  });
  get('personal-volume').addEventListener('click', () => {
    if (usable()) { set({musicMuted:!state.spotifyMuted}); cue(); }
  });
  window.LumOutputGains = {set};
  exchange(true);
  setInterval(() => { if (!document.hidden) exchange(true); }, 4000);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) exchange(true); });
})();
