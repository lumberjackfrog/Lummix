/* The backdrop is a user preference. Spotify continues to own the cover thumbnail. */
(async () => {
  const host = document.getElementById('lum-visualizer');
  const choice = document.getElementById('idle-style');
  const picker = document.getElementById('background-picker');
  const file = document.getElementById('idle-image');
  const status = document.getElementById('art-message');
  const wrist = document.getElementById('native-wrist');
  const wristOrigin = 'http://127.0.0.1:41836';
  const prefix = 'data:image/jpeg;base64,';
  let appearance;
  try{({appearance}=await window.LumControls);}catch{return;}
  const valid = value => appearance.image(value);
  let image = '', mode = 'album', upload = 0;
  try {
    const saved = localStorage.getItem('lum-idle-image');
    if (valid(saved)) image = saved;
    mode = appearance.background(localStorage.getItem('lum-idle-style'), Boolean(image), mode);
  } catch{}

  function share() {
    wrist.contentWindow?.postMessage({
      type:'lum-idle-art', mode, jpeg:mode === 'custom' ? image.slice(prefix.length) : ''
    }, wristOrigin);
  }
  function apply() {
    const custom = mode === 'custom' && Boolean(image);
    document.documentElement.dataset.background = custom ? 'custom' : 'album';
    host.dataset.customArt = String(custom);
    if (custom) host.style.setProperty('--lum-idle-art', 'url("' + image + '")');
    else host.style.removeProperty('--lum-idle-art');
    picker.hidden = choice.value !== 'custom';
    share();
    document.dispatchEvent(new Event('lum-background'));
  }
  function saveChoice(withImage=false) {
    try { localStorage.setItem('lum-idle-style', mode); } catch {}
    const settings=withImage ? {background:mode,image} : {background:mode};
    document.dispatchEvent(new CustomEvent('lum-background-chosen',{detail:settings}));
  }
  document.addEventListener('lum-preferences-applied',({detail})=>{
    const savedImage=valid(detail.image)?detail.image:image;
    const savedMode=appearance.background(detail.background,Boolean(savedImage),mode);
    if(savedMode===mode && savedImage===image) return;
    image=savedImage;mode=savedMode;choice.value=mode;apply();
  });
  choice.value = mode;
  choice.addEventListener('change', () => {
    upload++;
    status.textContent = '';
    if (choice.value === 'custom' && !image) {
      picker.hidden = false;
      status.textContent = 'Choose an image to use as your background.';
      file.click();
      return;
    }
    mode = choice.value;
    apply();saveChoice();
  });
  file.addEventListener('cancel', () => {
    if (!image) { choice.value = mode; status.textContent = ''; apply(); }
  });
  file.addEventListener('change', async () => {
    const source = file.files?.[0];
    if (!source) return;
    const current = ++upload;
    let bitmap;
    try {
      if (!['image/png','image/jpeg','image/webp'].includes(source.type) || source.size > 12 * 1024 * 1024) throw Error();
      bitmap = await createImageBitmap(source);
      if (current !== upload) return;
      const scale = Math.min(1, 1440 / Math.max(bitmap.width, bitmap.height));
      const canvas = document.createElement('canvas');
      canvas.width = Math.max(1, Math.round(bitmap.width * scale));
      canvas.height = Math.max(1, Math.round(bitmap.height * scale));
      const ctx = canvas.getContext('2d');
      ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
      let encoded = canvas.toDataURL('image/jpeg', .82);
      if (!valid(encoded)) {
        const reduced = document.createElement('canvas');
        reduced.width = Math.max(1, Math.round(canvas.width / 2));
        reduced.height = Math.max(1, Math.round(canvas.height / 2));
        reduced.getContext('2d').drawImage(canvas, 0, 0, reduced.width, reduced.height);
        encoded = reduced.toDataURL('image/jpeg', .76);
      }
      if (!valid(encoded)) throw Error();
      image = encoded; mode = 'custom'; choice.value = mode;
      apply();saveChoice(true);
      status.textContent = 'Image applied.';
      try { localStorage.setItem('lum-idle-image', image); }
      catch { status.textContent = 'Image applied for this session.'; }
    } catch {
      if (current === upload) status.textContent = 'Choose a PNG, JPEG or WebP under 12 MB.';
    } finally {
      bitmap?.close();
      if (current === upload) file.value = '';
    }
  });
  wrist.addEventListener('load', share);
  window.addEventListener('message', event => {
    if (event.source === wrist.contentWindow && event.origin === wristOrigin && event.data?.type === 'lum-wrist-ready') share();
  });
  apply();
})();
