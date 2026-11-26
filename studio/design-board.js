'use strict';
const byId = id => document.getElementById(id);
const canvas = byId('canvas');
const previewFile = 'Mixer%20preview.html';
const storeKey = 'lum-mix-board-v1';
const installed = new URLSearchParams(location.search).get('app') === '1' && !!window.chrome?.webview;
if (installed) {
  document.querySelector('body > header').style.display='none';
  const grips = byId('resize-grips');
  const bridge = window.chrome.webview;
  grips.addEventListener('pointerdown', event => {
    if (!event.isTrusted || !event.isPrimary || event.button !== 0) return;
    event.preventDefault();
    bridge.postMessage('resize-' + event.target.dataset.edge);
  });
  bridge.addEventListener('message', ({data}) => {
    if (data?.type === 'lummix-window-state') grips.hidden = data.maximized || data.fullscreen;
  });
}
const baseConfig = { view: 'desktop', size: '100%', palette: 'xso', swatch: '#7a0aa6' };
let config = { ...baseConfig };
if(!installed) try { Object.assign(config, JSON.parse(localStorage.getItem(storeKey))); } catch {}
const validView = value => value === 'wrist' ? 'wrist' : 'desktop';
config.view = validView(new URLSearchParams(location.search).get('view') ?? config.view);
let lastSource = '', socket = null, retryAt = 0, palette = {}, lastAttempt = 0;
let appearance;
const readyAppearance=window.LumControls.then(controls=>{
  appearance=controls.appearance;
  paint();
  return appearance;
}).catch(()=>null);
const themes = new Map();

function sendTheme(){for(const theme of themes.values()) canvas.contentWindow?.postMessage({type:'lum-board-theme',theme},location.origin);}
function persist(){if(!installed)try{ localStorage.setItem(storeKey, JSON.stringify(config)); } catch{}}
function paint(){
  const doc = canvas.contentDocument;
  if(!doc?.body) return;
  const root = doc.documentElement;
  if(root.dataset.boardView!==config.view){root.dataset.boardView=config.view;doc.dispatchEvent(new Event('lum-view'));}
  let skin = doc.getElementById('board-design-overrides');
  if(!skin){skin = doc.createElement('style');skin.id = 'board-design-overrides';doc.head.append(skin);}
  const rules = [];
  if(config.palette === 'custom' && appearance?.swatch(config.swatch)) rules.push('--mint:' + config.swatch);
  skin.textContent = (rules.length ? '.mixer{--lum-accent:'+config.swatch+';' + rules.join(';') + '}' : '') +
    (config.view === 'wrist' ? '.frame{max-width:750px!important}.layout{display:block!important}.mixer{display:none!important}.wrist-area{display:block!important;width:min(100%,620px)!important;max-width:none!important;margin:0 auto!important}' : '');
  canvas.style.maxWidth = config.size;
  canvas.style.marginInline = 'auto';
  byId('palette').value = config.palette;byId('size').value = config.size;byId('swatch').value = config.swatch;
  byId('swatch-row').style.display = config.palette === 'custom' ? 'flex' : 'none';
  byId('sync').textContent = Object.keys(palette).length ? 'Wrist colours supplied by XSOverlay.' : 'Wrist uses the saved theme reference. Waiting for XSOverlay.';
}
function updateNavigation(){
  document.querySelectorAll('[data-view]').forEach(el=>el.setAttribute('aria-pressed',String(el.dataset.view===config.view)));
  const url = new URL(location.href);
  if(url.searchParams.has('view') && url.searchParams.get('view') !== config.view){
    url.searchParams.set('view',config.view);
    history.replaceState(null,'',url);
  }
}
function selectView(view){
  const doc=canvas.contentDocument;
  config.view = validView(view);
  updateNavigation();
  if(config.view==='wrist'){const wrist=doc?.getElementById('companion');if(wrist && !wrist.checked) wrist.click();}
  paint();persist();
}
function listen(){
  if(socket || Date.now() < retryAt) return;
  retryAt = Date.now() + 15000;
  const ws = new WebSocket('ws://127.0.0.1:42070/?client=lum-mix-design');
  socket = ws;
  ws.onopen = () => ws.send(JSON.stringify({sender:'lum-mix-design',target:'xsoverlay',command:'SubscribeToEvents',jsonData:JSON.stringify(['Theme']),rawData:null}));
  ws.onmessage = async event => {
    try{
      const msg=JSON.parse(event.data);
      if(msg.command !== 'UpdateTheme') return;
      const data=typeof msg.jsonData==='string'?JSON.parse(msg.jsonData):msg.jsonData;
      if(!data || !data.ThemeType) return;
      const engine=appearance||await readyAppearance;
      if(socket!==ws || !engine)return;
      const clean=engine.palette(data,true);
      if(clean.Accent || clean.Mid){
        palette=clean;
        clean.ThemeType=Number(data.ThemeType);clean.UseMediaTheme=Boolean(data.UseMediaTheme);
        themes.set(clean.ThemeType,clean);paint();sendTheme();
      }
    } catch {}
  };
  ws.onclose = () => {if(socket===ws){socket=null;palette={};themes.clear();canvas.contentWindow?.postMessage({type:"lum-board-offline"},location.origin);paint();}};
}
for(const button of document.querySelectorAll('[data-view]')) button.addEventListener('click',()=>selectView(button.dataset.view));
for(const id of ['palette','swatch','size']) byId(id).addEventListener('input',()=>{config[id]=byId(id).value;listen();paint();persist();});
byId('reset').onclick=()=>{config={...baseConfig};listen();selectView('desktop');};
canvas.addEventListener('load',()=>{ selectView(config.view);sendTheme();canvas.contentDocument?.querySelectorAll('iframe').forEach(frame=>frame.addEventListener('load',sendTheme)); });
async function refresh(){
  if(document.hidden || Date.now()-lastAttempt < 750) return;
  lastAttempt=Date.now();
  try{
    const response=await fetch(previewFile,{cache:'no-store'});
    if(!response.ok) throw new Error();
    const source=await response.text();
    if(source!==lastSource && source.includes('</html>')){lastSource=source;canvas.srcdoc=source;}
    byId('updates').textContent='Live updates on';
  }catch{byId('updates').textContent='Reconnecting…';}
  listen();
}
updateNavigation();persist();refresh();
setInterval(refresh,1800);
document.addEventListener('visibilitychange',refresh);
