// Installed in the existing XSOverlay Wrist.html by the optional wrist integration.
(() => {
  'use strict';
  const loopback = ['127.0.0.1', 'localhost', '[::1]'].includes(location.hostname);
  if ((loopback && location.port === '41836') || !location.pathname.toLowerCase().endsWith('/wrist.html')) return;
  if (document.getElementById('lummix-live-wrist')) return;

  const origin = 'http://127.0.0.1:41839';
  const frame = document.createElement('iframe');
  frame.id = 'lummix-live-wrist';
  frame.title = 'Lummix microphone and music';
  frame.referrerPolicy = 'no-referrer';
  frame.style.cssText = 'position:absolute;border:0;background:transparent;overflow:hidden;display:none;pointer-events:none;z-index:2';
  let readyAt = 0, nextAttempt = 0, lastTheme = '', pending = false;
  const swatch=document.createElement('canvas');
  swatch.width=swatch.height=1;
  const paint=swatch.getContext('2d',{willReadFrequently:true});
  function color(value) {
    if (/^(#|rgba?\()/i.test(value) || !paint || !CSS.supports('color',value)) return value;
    // Chromium can serialize an album tint as color(srgb ...). Send a concrete
    // sRGB color across the frame so CSS custom properties never leak through.
    paint.clearRect(0,0,1,1);
    paint.fillStyle=value;
    paint.fillRect(0,0,1,1);
    const [r,g,b,a]=paint.getImageData(0,0,1,1).data;
    return 'rgba('+[r,g,b,a/255].join(',')+')';
  }
  const hide = () => {
    frame.style.display = 'none';
    frame.style.pointerEvents = 'none';
  };

  function vacant(x, y, width, height, wrist) {
    // The native toolbar may also have its own open layouts or another add-on.
    // A covered slot stays empty. Ignore only our own frame and layout ancestors.
    for (const dx of [.05,.5,.95]) {
      for (const dy of [.05,.5,.95]) {
        if (document.elementsFromPoint(x+width*dx,y+height*dy).some(node =>
          node !== frame && !node.contains(wrist))) return false;
      }
    }
    return true;
  }

  function update() {
    pending = false;
    const container = document.querySelector('#uiContainer');
    const wrist = container?.querySelector('.wrist-container');
    const toolbar = container?.querySelector(':scope > .toolbar');
    const clock = wrist?.querySelector('.clock-container');
    const body = wrist?.querySelector('.rounded-background');
    const drawer = wrist?.querySelector('.media-widget');
    const track = drawer?.querySelector('.media-widget-track');
    const perf = container?.querySelector('.performance-background');
    if (!clock || !toolbar || !body || !track || !perf ||
        drawer.style.animationName !== 'show-media-player' ||
        Number(getComputedStyle(perf).opacity) > .01 || perf.style.pointerEvents === 'auto') return hide();

    const bounds = body.getBoundingClientRect(), rootBox = container.getBoundingClientRect();
    const toolbarBox = toolbar.getBoundingClientRect();
    const scale = toolbarBox.width / toolbar.offsetWidth;
    if (!(scale > 0) || getComputedStyle(drawer).visibility === 'hidden') return hide();
    const gap = 7*scale;
    const leftEdge = Math.max(0,rootBox.left);
    const rightEdge = Math.min(bounds.left-gap,innerWidth-gap);
    const width = Math.min(108*scale,rightEdge-leftEdge-gap);
    const x = rightEdge-width;
    const y = Math.max(bounds.top,toolbarBox.bottom+gap,gap);
    const height = Math.min(180*scale,bounds.bottom-y,innerHeight-gap-y);
    if (width<94*scale || height<132*scale || !vacant(x,y,width,height,wrist)) return hide();

    if (frame.parentNode !== container) container.append(frame);
    const now = Date.now();
    if (now-readyAt > 30000) {
      hide();
      if (now >= nextAttempt) {
        nextAttempt = now+30000;
        frame.src = origin+'/wrist';
      }
      return;
    }
    Object.assign(frame.style, {
      left: (x-rootBox.left)/scale+'px', top: (y-rootBox.top)/scale+'px',
      width: width/scale+'px', height: height/scale+'px',
      display: 'block', pointerEvents: 'auto',
      borderRadius: getComputedStyle(drawer).borderTopLeftRadius
    });
    const palette = {
      type:'lummix-wrist-theme', surface:color(getComputedStyle(drawer).backgroundColor),
      text:color(getComputedStyle(track).color),
      accent:color(getComputedStyle(drawer).getPropertyValue('--theme-accent').trim())
    };
    const key = JSON.stringify(palette);
    if (key !== lastTheme) {
      frame.contentWindow.postMessage(palette,origin);
      lastTheme = key;
    }
  }
  function schedule() {
    if (!pending) { pending=true; requestAnimationFrame(update); }
  }
  window.addEventListener('message',event => {
    if (event.origin !== origin || event.source !== frame.contentWindow) return;
    if (event.data?.type === 'lummix-wrist-ready') {
      if (!readyAt || Date.now()-readyAt > 30000) lastTheme='';
      readyAt=Date.now();
      schedule();
    } else if (event.data?.type === 'lummix-wrist-unavailable') {
      readyAt=0; lastTheme=''; hide();
    }
  });
  const observer = new MutationObserver(records => {
    if (records.some(change => change.target !== frame)) schedule();
  });
  observer.observe(document.documentElement,{childList:true,subtree:true,attributes:true,attributeFilter:['style','class','hidden']});
  for (const event of ['resize','animationend','transitionend']) window.addEventListener(event,schedule,true);
  const heartbeat=setInterval(schedule,5000);
  window.addEventListener('pagehide',() => {observer.disconnect();clearInterval(heartbeat);},{once:true});
  schedule();
})();
