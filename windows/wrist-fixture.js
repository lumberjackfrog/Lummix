const parentOrigin = 'http://127.0.0.1:41835';
const accent = Object.values(savedSettings.AccentColor || {r:.48,g:.04,b:.65}).slice(0,3)
  .map(value=>Math.round(value*255).toString(16).padStart(2,'0')).join('');
const paletteKeys = ['Hi','Mid','Dark','Contrast','Accent','AccentBright','Confirm','Warning','Error'];
const fixtureTheme = {
  ThemeType:1, UseMediaTheme:!!savedSettings.MediaThemeing,
  Hi:savedSettings.UseDarkTheme?'555555':'eeeeee', Mid:savedSettings.UseDarkTheme?'444444':'dddddd',
  Dark:savedSettings.UseDarkTheme?'333333':'cccccc', Contrast:savedSettings.UseDarkTheme?'ffffff':'111111',
  Accent:accent, AccentBright:accent, Confirm:'73da79', Warning:'ffcc76', Error:'ff7474'
};
let socket, currentTheme=fixtureTheme, parentTarget=parentOrigin, playing=true, selected=0;
const tracks=['Moonlight Bus','Blue Hour Loop','Night Ferry'];
const embedded = window.parent!==window;
let mediaDrawerOpen = false;
let performanceOpen = false;
const performanceFixture = {
  Framerate:89,MaxFramerate:90,GpuFrametime:8.6,CpuFrametime:4.2,TotalFrametime:9.4,MaxFrametime:11.1,
  GPU:{Load:62,MemLoad:7100,MemSize:16000,Temperature:63},
  CPU:{Load:27,MemLoad:0,MemSize:0,Temperature:55},
  Memory:{Load:43,MemLoad:13600,MemSize:32000,Temperature:0},
};
function trackNativeMediaSkin() {
  const tile=document.querySelector('.lum-companion');
  const media=document.querySelector('.media-widget');
  const controls=document.querySelector('.media-widget-controls');
  const track=document.querySelector('.media-widget-track');
  const battery=document.querySelector('.battery-widget');
  if(!tile || !media || !controls || !track || !battery)return;

  let scheduled=false;
  const synchronize=()=>{
    scheduled=false;
    const panel=getComputedStyle(media);
    const transport=getComputedStyle(controls);
    const isGlass=media.classList.contains('glass-backdrop');
    // Reuse the installed CSS rim and read the native rendered role values.
    tile.classList.toggle('glass-backdrop',isGlass);
    const values={
      surface:panel.backgroundColor,
      transport:transport.backgroundColor,
      filter:panel.backdropFilter,
      shadow:panel.boxShadow,
      text:getComputedStyle(track).color,
      tint:isGlass?getComputedStyle(battery).backgroundColor:'transparent'
    };
    for(const [role,value] of Object.entries(values))tile.style.setProperty('--lum-native-'+role,value);
  };
  const schedule=()=>{
    if(scheduled)return;
    scheduled=true;
    requestAnimationFrame(synchronize);
  };
  const observer=new MutationObserver(schedule);
  for(const node of [media,controls,track,battery,document.documentElement]){
    observer.observe(node,{attributes:true,attributeFilter:['class','style']});
  }
  schedule();
}

function reportMediaDrawer() {
  const drawer = document.querySelector('.lum-companion');
  if (drawer) drawer.hidden = !mediaDrawerOpen || performanceOpen;
  if(window.parent!==window) window.parent.postMessage({type:'lum-wrist-music-drawer',open:mediaDrawerOpen},parentTarget);
}
function updatePerformanceVisibility() {
  document.documentElement.toggleAttribute('data-lum-native-performance',performanceOpen);
  const panel = document.querySelector('.performance-background');
  if (panel) {
    panel.setAttribute('aria-label','Example performance readings');
    panel.style.marginTop='32px';
    let note=document.querySelector('.lum-fixture-perf-label');
    if(!note){
      note=document.createElement('div');
      note.className='lum-fixture-perf-label';
      note.textContent='Fixture stats';
      note.style.cssText='position:absolute;left:521px;top:350px;z-index:100;color:var(--theme-contrasting);background:var(--theme-dark);padding:4px 7px;border-radius:5px;font:13px Segoe UI,sans-serif';
      document.body.append(note);
    }
    note.hidden=!performanceOpen;
  }
  const tile=document.querySelector('.lum-companion');
  if(tile)tile.style.visibility=performanceOpen?'hidden':'';
  reportMediaDrawer();
}
let effectiveAlbum=null;
let lastAppearance='';

function nativeHex(value) {
  if(typeof value!=='string')return '';
  const hex=value.trim().replace(/^#/,'');
  if(/^[0-9a-f]{6}(?:[0-9a-f]{2})?$/i.test(hex))return hex.slice(0,6);
  const rgb=/^rgb\(\s*([.\d]+)[, ]+([.\d]+)[, ]+([.\d]+)\s*\)$/i.exec(value.trim());
  return rgb ? rgb.slice(1).map(x=>Math.max(0,Math.min(255,Math.round(Number(x)))).toString(16).padStart(2,'0')).join('') : '';
}
function reportEffectivePalette(){
  if(!embedded)return;
  const fields={Hi:'hi',Mid:'mid',Dark:'dark',Contrast:'contrasting',Accent:'accent',AccentBright:'accent-bright'};
  const computed=getComputedStyle(document.documentElement),base={};
  for(const [key,suffix] of Object.entries(fields)){
    base[key]=nativeHex(currentTheme[key])||nativeHex(computed.getPropertyValue('--theme-'+suffix));
    if(!base[key])return;
  }
  const active=effectiveAlbum!==null && Boolean(document.querySelector('.media-widget.glass-backdrop'));
  const custom=backdropMode==='custom' && Boolean(pinnedTheme);
  const theme=custom?{...base,...pinnedTheme}:active?{...base,...effectiveAlbum}:base;
  const signature=JSON.stringify([theme,active,custom]);
  if(signature===lastAppearance)return;
  lastAppearance=signature;
  window.parent.postMessage({type:'lum-wrist-effective',theme,album:active&&!custom,custom},parentTarget);
}
function observeVendorTheme(data){
  let theme;
  try{theme=JSON.parse(data);}catch{return;}
  if(theme?.ThemeType===2 && theme.UseMediaTheme===true){
    const result={};
    for(const key of ['Hi','Mid','Dark','Contrast','Accent']){
      result[key]=nativeHex(theme[key]);
      if(!result[key])return;
    }
    result.AccentBright=result.Accent;
    effectiveAlbum=result;
  }else if(theme?.UseMediaTheme===false){
    effectiveAlbum=null;
  }
  queueMicrotask(reportEffectivePalette);
}

let boardAppearance='combined';
let drawerPreference=true;
let changingDrawerForAppearance=false;
function checkBoardDrawer(){
  if(!embedded || !mediaInitialized)return;
  const desired=boardAppearance==='desktop' || drawerPreference;
  if(desired===mediaDrawerOpen)return;
  changingDrawerForAppearance=true;
  document.querySelector('button:has(.bi-music-note-list)')?.click();
}
function installSparsePaletteFallback(){
  if(!window.Vibrant?.from)return;
  const original=window.Vibrant.from.bind(window.Vibrant);
  window.Vibrant.from=(...args)=>{
    const pinnedAddress=backdropAddress();
    const paletteBuilder=original(...args);
    const getPalette=paletteBuilder.getPalette.bind(paletteBuilder);
    paletteBuilder.getPalette=callback=>{
      if(typeof callback!=='function')return getPalette();
      return getPalette((error,output)=>{
        const palette={...(output||{})};
        const fromBase=key=>({_rgb:(nativeHex(currentTheme[key])||fixtureTheme[key]).match(/../g).map(x=>parseInt(x,16))});
        const swatches=Object.values(palette).filter(s=>Array.isArray(s?._rgb));
        // Only supply swatches where the native callback would dereference null.
        if(!palette.DarkMuted && !palette.LightVibrant)
          palette.DarkMuted=palette.DarkVibrant||palette.Muted||swatches[0]||fromBase('Mid');
        if(!palette.Vibrant && !palette.Muted)
          palette.Vibrant=palette.LightVibrant||swatches[0]||fromBase('Accent');
        callback(error,palette);
        const image=args[0];
        if(embedded && backdropMode==='custom' && image?.matches?.('.wrist-container img.background-image')
          && image.getAttribute('src')===pinnedAddress) adoptPinnedPalette(palette);

      });
    };
    return paletteBuilder;
  };
}
const emit=(command,jsonData=null,rawData=null)=>socket?.emit(command,jsonData,rawData);
let idleImage='';
let backdropMode='album',pinnedTheme=null;
const nativeBackdrops=new Map();
let liveMedia=null;
let wristLinked=false;
let vendorMediaSignature='';
let mediaInitialized=false;
const maxArtworkLength=6_000_000;

function nativeText(value,limit) {
  return typeof value==='string'
    ? value.trim().slice(0,limit).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c])
    : '';
}
function liveArtwork(value) {
  if(typeof value!=='string' || value.length>maxArtworkLength)return null;
  const match=/^data:image\/(png|jpeg);base64,([a-z0-9+/]+={0,2})$/i.exec(value);
  if(!match)return null;
  const [,kind,bytes]=match;
  return (kind.toLowerCase()==='png'?/^iVBORw0KGgo/.test(bytes):/^\/9j\//.test(bytes)) ? bytes : null;
}
function canTransport() {
  return wristLinked && liveMedia?.state==='ready' && Boolean(liveMedia.title) &&
    (liveMedia.playing ? liveMedia.canPause===true : liveMedia.canPlay===true);
}
function appearanceTransport() {
  if(!embedded)return;
  const controls=document.querySelector('.media-widget-controls');
  if(!controls)return;
  const [previous,toggle,next]=controls.querySelectorAll('button');
  if(!next)return;
  for(const [button,label,cap] of [[previous,'Previous track','canPrevious'],[next,'Next track','canNext']]){
    button.disabled=!(wristLinked && liveMedia?.state==='ready' && liveMedia[cap]);
    button.setAttribute('aria-label',button.disabled?label+' unavailable':label);
    button.title=button.getAttribute('aria-label');
  }
  toggle.disabled=!canTransport();
  const label=liveMedia?.playing?'Pause Spotify':'Play Spotify';
  toggle.setAttribute('aria-label',toggle.disabled?label+' unavailable':label);
  toggle.title=toggle.getAttribute('aria-label');
}
function backdropAddress(){
  return backdropMode==='custom'&&idleImage?'data:image/jpeg;base64,'+idleImage:'';
}
function applyPinnedTheme(){
  if(pinnedTheme)emit('UpdateTheme',{...currentTheme,...pinnedTheme,ThemeType:2,UseMediaTheme:true});
  reportEffectivePalette();
}
function adoptPinnedPalette(palette){
  const dark=(palette.DarkMuted||palette.LightVibrant)?._rgb;
  const vibrant=(palette.Vibrant||palette.Muted)?._rgb;
  if(!dark || !vibrant)return;
  const hex=values=>values.map(value=>Math.round(Math.max(0,Math.min(255,value))).toString(16).padStart(2,'0')).join('');
  // These are the installed toolbar's own media role mixes, with its Vibrant swatches.
  pinnedTheme={
    Hi:hex(dark.map(value=>value*.9+25.5)),Mid:hex(dark),Dark:hex(dark.map(value=>value*.65)),
    Accent:hex(vibrant),AccentBright:hex(vibrant),Contrast:currentTheme.Contrast
  };
  applyPinnedTheme();
}
function syncNativeBackdrop(){
  for(const [image,source] of nativeBackdrops)image.src=source;
  document.documentElement.toggleAttribute('data-lum-pinned-art',Boolean(backdropAddress()));
}
function keepNativeCoverStable() {
  if(!embedded)return;
  const nativeSource=Object.getOwnPropertyDescriptor(HTMLImageElement.prototype,'src');
  for(const image of document.querySelectorAll('.media-widget-icon, img.background-image, img.layouts-widget-background')){
    if(Object.hasOwn(image,'src'))continue;
    const backdrop=!image.matches('.media-widget-icon');
    if(backdrop)nativeBackdrops.set(image,image.getAttribute('src')||'');
    Object.defineProperty(image,'src',{
      configurable:true,
      get(){return nativeSource.get.call(this);},
      set(value){
        if(backdrop)nativeBackdrops.set(this,String(value));
        const source=backdropAddress();
        const next=backdrop&&source?source:String(value);
        if(this.getAttribute('src')!==next)nativeSource.set.call(this,next);
      }
    });
  }
}
function sendMedia(){
  if(!mediaInitialized || socket?.readyState!==FixtureSocket.OPEN)return;
  appearanceTransport();
  const hasTrack=liveMedia?.state==='ready' && Boolean(liveMedia.title);
  const native=embedded ? {
    title:hasTrack?nativeText(liveMedia.title,180):'Spotify',
    artist:hasTrack?nativeText(liveMedia.artist,160):liveMedia?.state==='ready'?'No track':'Playback unavailable',
    albumTitle:null,
    playbackStatus:hasTrack&&liveMedia.playing?'Playing':'Paused',
    albumArt:hasTrack?liveArtwork(liveMedia.artwork):idleImage||null
  } : {
    title:tracks[selected],artist:'Lum Mix fixture',albumTitle:null,
    playbackStatus:playing?'Playing':'Paused',albumArt:!playing&&idleImage?idleImage:null
  };
  const signature=JSON.stringify(native);
  if(signature===vendorMediaSignature)return;
  vendorMediaSignature=signature;
  if(!embedded){emit('UpdateMediaPlayer',native);return;}
  // The installed handler logs its event object. Suppress that one synchronous
  // dispatch so the preview does not record personal track text or album bytes.
  const original=console.log;
  console.log=()=>{};
  try { emit('UpdateMediaPlayer',native); }
  finally { console.log=original; }
}
class FixtureSocket extends EventTarget {
  static OPEN=1;
  constructor() {
    super(); socket=this; this.readyState=0; vendorMediaSignature=''; mediaInitialized=false;
    setTimeout(()=>{this.readyState=1;this.dispatchEvent(new Event('open'));},0);
  }
  emit(command,jsonData,rawData) {
    this.dispatchEvent(new MessageEvent('message',{data:JSON.stringify({command,jsonData:JSON.stringify(jsonData),rawData})}));
  }
  close(){this.readyState=3;}
  send(payload) {
    const {command,jsonData}=JSON.parse(payload);
    if (command==='SubscribeToEvents') {
      const requested=JSON.parse(jsonData||'[]');
      if(requested.length===1 && requested[0]==='Performance'){
        setTimeout(()=>emit('UpdateRuntimePerformance',performanceFixture),0);
        return;
      }
      setTimeout(()=>{
        emit('UpdateTheme',currentTheme);
        emit('UpdateSettings',{AutoMediaDetection:!embedded,AlwaysShowDetailedInformation:true,DefaultShowBatteryPercentage:true,BatteryFontScale:17,HideWristOverlay:false,MediaThemeing:!!savedSettings.MediaThemeing});
        emit('UpdateDateTime',{Date:'NOV 24 / DEMO',Time:'21:46',CurrentSessionLength:'00:42'});
        emit('UpdateDeviceInformation',[
          {name:'headset',classification:1,label:'HMD',battery:78,charging:false,connection:1,timeEstimate:'2h 15m'},
          {name:'left',classification:2,label:'LeftController',battery:64,charging:false,connection:1,timeEstimate:'3h 14m'},
          {name:'right',classification:2,label:'RightController',battery:82,charging:false,connection:1,timeEstimate:'4h 02m'}]);
        keepNativeCoverStable();
        mediaInitialized=true;
        sendMedia();
        if(embedded){
          // The native toolbar opens the panel once. Later media events keep the user’s drawer state.
          checkBoardDrawer();
        }else{mediaDrawerOpen=true;reportMediaDrawer();}
      },0);
    } else if(command==='RequestSetTheme'){
      observeVendorTheme(jsonData);
    } else if(command==='TogglePerformanceStats'){
      performanceOpen=!performanceOpen;
      setTimeout(()=>{
        emit('TogglePerformanceStats',null);
        updatePerformanceVisibility();
      },0);
    } else if(command==='ToggleMediaPlayer'){
      queueMicrotask(()=>{
        mediaDrawerOpen=document.querySelector('.media-widget')?.style.animationName==='show-media-player';
        if(!changingDrawerForAppearance)drawerPreference=mediaDrawerOpen;
        changingDrawerForAppearance=false;
        reportMediaDrawer();
        reportEffectivePalette();
      });
    } else if(command==='MediaPlayPause'){
      if(embedded){if(canTransport())window.parent.postMessage({type:'lum-wrist-spotify-action',action:'toggle'},parentTarget);}
      else{playing=!playing;sendMedia();}
    } else if(command==='MediaPrevious'||command==='MediaNext'){
      if(embedded){
        const next=command==='MediaNext';
        if(wristLinked && liveMedia?.state==='ready' && liveMedia[next?'canNext':'canPrevious'])
          window.parent.postMessage({type:'lum-wrist-spotify-action',action:next?'next':'previous'},parentTarget);
      }else{
        selected=(selected+tracks.length+(command==='MediaNext'?1:-1))%tracks.length;
        sendMedia();
      }
    }
  }
}
window.WebSocket=FixtureSocket;
let tileState = {micMuted:false, musicMuted:false, musicTarget:'spotify', musicLevel:100, linked:true};
const wristSnapKey='lum-pref-slider-snap';
let wristSnap=10;
try{const saved=localStorage.getItem(wristSnapKey);if(['0','5','10','20'].includes(saved))wristSnap=Number(saved);}catch{}
function paintTile(next){
  tileState={...tileState,...next};
  const mic=document.getElementById('lum-ref-mute'), music=document.getElementById('lum-ref-music');
  if(!mic || !music) return;
  const setButton=(button,muted,name)=>{
    button.disabled=!tileState.linked;
    button.setAttribute('aria-pressed',String(muted));
    button.title=(muted?'Unmute ':'Mute ')+name;
    button.setAttribute('aria-label',button.title);
  };
  setButton(mic,tileState.micMuted,'microphone');
  const spotify=tileState.musicTarget!=='local';
  setButton(music,tileState.musicMuted,spotify?'music in the Lummix output':'local music in the game mix');
  const level=document.getElementById('lum-personal-level');
  level.step='any';
  level.disabled=!tileState.linked;
  level.value=tileState.musicLevel;
  level.setAttribute('aria-label','Music output gain');
  level.setAttribute('aria-valuetext',tileState.musicLevel+' percent output gain');
  level.parentElement.title=level.getAttribute('aria-label');
  level.style.setProperty('--lum-fill',tileState.musicLevel/2+'%');
  document.getElementById('lum-volume-value').textContent=tileState.musicLevel+'%';
  mic.querySelector('span').setAttribute('mIcon',tileState.micMuted?'mic_off':'mic');
  music.querySelector('span').className='bi bi-music-note-beamed';
}
window.addEventListener('message',event=>{
  if(event.source!==window.parent || ![parentOrigin,'null'].includes(event.origin))return;
  const data=event.data;
  if(typeof data!=='object'||data===null)return;
  parentTarget=event.origin==='null'?'*':event.origin;
  if(data.type==='lum-global-pulse' && event.origin===parentOrigin){
    const layer=document.querySelector('.lum-wrist-pulse');
    if(layer && typeof data.value==='number' && Number.isFinite(data.value)){
      layer.style.opacity=document.hidden || matchMedia('(prefers-reduced-motion: reduce)').matches
        ? 0 : Math.max(0,Math.min(.45,data.value));
    }
  } else if(data.type==='lum-control-colors' && event.origin===parentOrigin){
    if(![data.accent,data.bed,data.ink].every(value=>typeof value==='string' && /^#[0-9a-f]{6}$/i.test(value)))return;
    const style=document.documentElement.style;
    style.setProperty('--lum-control-accent',data.accent);
    style.setProperty('--lum-control-bed',data.bed);
    style.setProperty('--lum-control-ink',data.ink);
  } else if(data.type==='lum-wrist-appearance-view' && event.origin===parentOrigin){
    if(!['desktop','wrist','background','combined'].includes(data.view))return;
    boardAppearance=data.view;
    checkBoardDrawer();
  } else if(data.type==='lum-spotify-media'){
    if(event.origin!==parentOrigin || !embedded || !data.media || typeof data.media!=='object')return;
    const incoming=data.media;
    liveMedia={
      state:incoming.state==='ready'?'ready':'unavailable',
      title:typeof incoming.title==='string'?incoming.title.trim().slice(0,180):'',
      artist:typeof incoming.artist==='string'?incoming.artist.trim().slice(0,160):'',
      artwork:typeof incoming.artwork==='string' && incoming.artwork.length<=maxArtworkLength?incoming.artwork:'',
      playing:incoming.playing===true,canPlay:incoming.canPlay===true,canPause:incoming.canPause===true,
      canNext:incoming.canNext===true,canPrevious:incoming.canPrevious===true
    };
    sendMedia();
  } else if(data.type==='lum-idle-art' && typeof data.jpeg==='string' && data.jpeg.length<400000 && (!data.jpeg || /^\/9j\/[a-zA-Z0-9+/=]+$/.test(data.jpeg))){
    const mode=data.mode==='custom'&&data.jpeg?'custom':'album';
    if(data.jpeg!==idleImage||mode!==backdropMode){
      idleImage=data.jpeg;
      backdropMode=mode;
      pinnedTheme=null;
      if(embedded){
        if(mode==='album'){
          effectiveAlbum=null;
          emit('UpdateTheme',currentTheme);
        }
        syncNativeBackdrop();
        sendMedia();
        reportEffectivePalette();
      }else if(!playing)sendMedia();
    }
  } else if(data.type==='lum-wrist-state'){
    if(!['micMuted','linked','outputActive'].every(key=>typeof data[key]==='boolean'))return;
    wristLinked=data.linked;
    appearanceTransport();
    if(event.origin===parentOrigin && [0,5,10,20].includes(data.snapPoints) && data.snapPoints!==wristSnap){
      wristSnap=data.snapPoints;
      try{localStorage.setItem(wristSnapKey,String(wristSnap));}catch{}
    }
    paintTile({micMuted:data.micMuted, musicMuted:data.musicMuted===true, musicTarget:data.musicTarget==='local'?'local':'spotify',musicLevel:Number.isFinite(data.musicLevel) && data.musicLevel>=0 && data.musicLevel<=200?data.musicLevel:tileState.musicLevel,linked:data.linked});
  } else if(data.type==='lum-wrist-theme'){
    const theme=data.theme;
    if(!theme || ![1,2].includes(theme.ThemeType) ||
       !paletteKeys.every(key=>typeof theme[key]==='string' && /^[0-9a-f]{6}(?:[0-9a-f]{2})?$/i.test(theme[key])))return;
    if(theme.ThemeType!==1)return;
    currentTheme={ThemeType:theme.ThemeType,UseMediaTheme:theme.UseMediaTheme===true};
    for(const key of paletteKeys)currentTheme[key]=theme[key];
    if(backdropMode==='custom' && pinnedTheme){pinnedTheme.Contrast=currentTheme.Contrast;applyPinnedTheme();}
    else emit('UpdateTheme',currentTheme);
    queueMicrotask(reportEffectivePalette);
  }
});
function fitNativeCanvas() {
  const width = window.innerWidth;
  // The board keeps this iframe mounted while Desktop hides it.
  if (document.body && width > 0) document.body.style.zoom = String(Math.min(1,width/740));
}
window.addEventListener('resize',fitNativeCanvas);
let designRevision='';
async function watchLocalDesign(){
  if(document.hidden)return;
  try{
    const response=await fetch('/revision',{cache:'no-store'});
    if(!response.ok)return;
    const current=await response.text();
    if(designRevision && current!==designRevision){location.reload();return;}
    designRevision=current;
  }catch{}
}
watchLocalDesign();setInterval(watchLocalDesign,2200);
window.addEventListener('load',()=>requestAnimationFrame(trackNativeMediaSkin),{once:true});
window.addEventListener('DOMContentLoaded',()=>{
  installSparsePaletteFallback();
  fitNativeCanvas();
  new MutationObserver(reportEffectivePalette).observe(document.documentElement,{attributes:true,attributeFilter:['style']});
  setTimeout(reportEffectivePalette,150);
  const standalone=window.parent===window;
  const wristLevel=document.getElementById('lum-personal-level');
  wristLevel.step='any';
  wristLevel.addEventListener('input',e=>{
    const step=wristSnap||1;
    const value=Math.min(200,Math.max(0,Math.round(e.target.valueAsNumber/step)*step));paintTile({musicLevel:value});
    if(!standalone)window.parent.postMessage({type:'lum-wrist-music-level',value},parentTarget);
  });
  wristLevel.addEventListener('keydown',event=>{
    const v=wristLevel.valueAsNumber,s=wristSnap||1;
    const values={ArrowRight:(Math.floor(v/s)+1)*s,ArrowUp:(Math.floor(v/s)+1)*s,
      ArrowLeft:(Math.ceil(v/s)-1)*s,ArrowDown:(Math.ceil(v/s)-1)*s,Home:0,End:200,
      PageUp:Math.round((v+20)/s)*s,PageDown:Math.round((v-20)/s)*s};
    if(event.altKey||event.ctrlKey||event.metaKey||!Object.hasOwn(values,event.key))return;
    event.preventDefault();
    const next=Math.min(200,Math.max(0,values[event.key]));
    if(next===v)return;
    wristLevel.value=next;
    for(const name of ['input','change'])wristLevel.dispatchEvent(new Event(name,{bubbles:true}));
  });
  for(const [id,key,type] of [['lum-ref-mute','micMuted','lum-wrist-toggle-mic'],['lum-ref-music','musicMuted','lum-wrist-toggle-music']]){
    document.getElementById(id).addEventListener('click',()=>{if(standalone)paintTile({[key]:!tileState[key]});else window.parent.postMessage({type},parentTarget);});
  }
  if(standalone) paintTile({}); reportMediaDrawer();
  window.parent.postMessage({type:'lum-wrist-ready'},'*');
});

