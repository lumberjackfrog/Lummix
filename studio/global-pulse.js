(() => {
  const toggle = document.getElementById('global-pulse');
  const wrist = document.getElementById('native-wrist');
  const mixer = document.querySelector('.frame .mixer');
  const layer = document.createElement('span');
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  const origin = 'http://127.0.0.1:41836';
  const key = 'lum-pref-global-pulse';
  let player, envelope = 0, timer, request, wristOpen = false;
  layer.className = 'lum-pulse-layer';
  layer.setAttribute('aria-hidden', 'true');
  mixer.prepend(layer);
  try { toggle.checked = localStorage.getItem(key) === 'true'; } catch {}

  function eligible() {
    const view = document.documentElement.dataset.boardView;
    return toggle.checked && !document.hidden && !reduced.matches && player?.playing &&
      !!player.token && (view === 'wrist' ? wristOpen : mixer.getClientRects().length > 0);
  }

  function paint(value) {
    layer.style.opacity = value;
    const view = document.documentElement.dataset.boardView;
    wrist.contentWindow?.postMessage({type:'lum-global-pulse', value:view !== 'desktop' && wristOpen ? value : 0}, origin);
  }

  async function read() {
    if (!eligible() || request) return;
    const controller = new AbortController();
    request = controller;
    let delay = 250;
    try {
      const response = await fetch('http://127.0.0.1:41838/spotify/peak', {
        headers: {'X-Lum-Spotify-Token': player.token},
        cache:'no-store', credentials:'omit',
        signal:AbortSignal.any([controller.signal, AbortSignal.timeout(1500)])
      });
      const sample = response.ok ? await response.json() : null;
      if (!eligible() || request !== controller) return;
      if (sample?.state === 'ready' && Number.isFinite(sample.peak) && sample.peak >= 0 && sample.peak <= 1) {
        const target = sample.peak < .015 ? 0 : Math.sqrt(sample.peak) * .45;
        envelope += (target - envelope) * (target > envelope ? .45 : .22);
        paint(envelope < .005 ? 0 : Number(envelope.toFixed(3)));
      } else {
        envelope = 0;
        paint(0);
        delay = 1800;
      }
    } catch {
      if (request === controller) { envelope = 0; paint(0); }
      delay = 2200;
    } finally {
      if (request === controller) {
        request = null;
        if (eligible()) timer = setTimeout(run, delay);
      }
    }
  }

  function reconcile() {
    if (!eligible()) {
      clearTimeout(timer);
      timer = null;
      request?.abort();
      request = null;
      envelope = 0;
      paint(0);
    } else if (!timer && !request) {
      timer = setTimeout(run, 0);
    }
  }
  function run() { timer = null; read(); }
  // The delay wrapper releases the timer slot before checking current visibility.
  toggle.addEventListener('change', () => {
    try { localStorage.setItem(key, String(toggle.checked)); } catch {}
    reconcile();
  });
  reduced.addEventListener('change', reconcile);
  document.addEventListener('lum-preferences-applied',reconcile);
  document.addEventListener('visibilitychange', reconcile);
  document.addEventListener('lum-view', reconcile);
  new MutationObserver(reconcile).observe(mixer, {attributes:true, attributeFilter:['hidden']});
  wrist.addEventListener('load', () => { wristOpen = false; reconcile(); });
  window.addEventListener('message', event => {
    if (event.source !== wrist.contentWindow || event.origin !== origin) return;
    if (event.data?.type === 'lum-wrist-music-drawer') {
      wristOpen = event.data.open === true;
      if (!wristOpen) wrist.contentWindow?.postMessage({type:'lum-global-pulse',value:0}, origin);
      reconcile();
    }
  });
  window.LumGlobalPulse = {
    bind(info) {
      player = info?.state === 'ready' ? info : null;
      reconcile();
    }
  };
})();
