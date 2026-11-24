/* Static desktop album surface. The Spotify media bridge owns device and playback state. */
(() => {
  const state = { suspended: false, enabled: true };
  const maxArtworkLength = 6_000_000;
  let host, cover, artworkStatus, title, artist;
  let requestedArtwork = '';
  let media = window.LumSpotifyMedia;

  function safeArtwork(value) {
    return typeof value === 'string' &&
      value.length <= maxArtworkLength &&
      /^data:image\/(?:png|jpeg);base64,[a-z0-9+/]+={0,2}$/i.test(value) ? value : '';
  }

  function updateArtwork(candidate) {
    if (candidate === requestedArtwork) return;
    requestedArtwork = candidate;
    host.removeAttribute('data-has-art');
    host.style.removeProperty('--lum-album-art');
    cover.hidden = true;
    if (candidate) cover.src = candidate;
    else cover.removeAttribute('src');
  }

  function updateMedia(detail) {
    if (!detail || typeof detail !== 'object') return;
    media = detail;
    if (!host) return;
    const track = typeof detail.title === 'string' ? detail.title.trim().slice(0, 180) : '';
    const performer = typeof detail.artist === 'string' ? detail.artist.trim().slice(0, 160) : '';
    const art = safeArtwork(detail.artwork);
    host.dataset.hasTrack = String(Boolean(track));
    title.textContent = track || 'Spotify artwork';
    artist.textContent = track ? performer : 'Cover art appears when Spotify shares a track.';
    artist.hidden = Boolean(track && !performer);
    const link = host.querySelector('.lum-cover-link');
    if (/^https:\/\/open\.spotify\.com\/(album|track|episode)\/[a-zA-Z0-9]{22}$/.test(detail.itemUrl || '')) {
      link.href = detail.itemUrl;
      link.setAttribute('aria-label', 'Open in Spotify');
    } else {
      link.removeAttribute('href');
      link.removeAttribute('aria-label');
    }
    artworkStatus.textContent = track && detail.state === 'ready' ? detail.playing ? 'Playing' : 'Paused' : 'Spotify';
    updateArtwork(art);
  }

  function setState(next) {
    if (!next || typeof next !== 'object') return;
    for (const key of ['suspended', 'enabled']) {
      if (key in next) state[key] = Boolean(next[key]);
    }
    if (host) host.hidden = !state.enabled || state.suspended;
  }

  function mount() {
    host = document.getElementById('lum-visualizer');
    if (!host) return;
    host.setAttribute('aria-label', 'Spotify cover art');
    host.innerHTML = '<div class="lum-viz-body">'+
      '<div class="lum-cover-ambient" aria-hidden="true"></div>'+
      '<div class="lum-cover-content">'+
      '<div class="lum-cover-plate"><div class="lum-cover-empty" aria-hidden="true"></div><a class="lum-cover-link" target="_blank" rel="noopener noreferrer"><img class="lum-cover-image" alt="Album cover" hidden></a></div>'+
      '<div class="lum-cover-meta"><span class="lum-cover-status">Spotify</span><strong class="lum-cover-title">Spotify artwork</strong>'+
      '<span class="lum-cover-artist">Cover art appears when Spotify shares a track.</span></div>'+
      '</div></div>';
    cover = host.querySelector('.lum-cover-image');
    artworkStatus = host.querySelector('.lum-cover-status');
    title = host.querySelector('.lum-cover-title');
    artist = host.querySelector('.lum-cover-artist');

    cover.addEventListener('load', () => {
      if (!requestedArtwork || cover.getAttribute('src') !== requestedArtwork || !cover.naturalWidth) return;
      host.style.setProperty('--lum-album-art', 'url("' + requestedArtwork + '")');
      host.dataset.hasArt = 'true';
      cover.hidden = false;
    });
    cover.addEventListener('error', () => {
      if (cover.getAttribute('src') !== requestedArtwork || cover.naturalWidth) return;
      host.removeAttribute('data-has-art');
      host.style.removeProperty('--lum-album-art');
      cover.hidden = true;
    });
    setState(state);
    updateMedia(media || window.LumSpotifyMedia || {});
  }

  window.LumVisualizer = { setState };
  document.addEventListener('lum-spotify-media', event => updateMedia(event.detail));
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount, { once: true });
  else mount();
})();
