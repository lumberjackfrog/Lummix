(async function(){
  const root=document.documentElement, follow=document.getElementById('follow-theme'), dark=document.getElementById('dark');
  const frame=document.getElementById('native-wrist'), wristOrigin='http://127.0.0.1:41836';
  const boardOrigin='http://127.0.0.1:41835';
  const inBoard=window.parent!==window && location.href==='about:srcdoc';
  const queued=[];
  function waitForAppearance(event){
    const board=inBoard && event.source===window.parent && event.origin===boardOrigin;
    const companion=event.source===frame.contentWindow && event.origin===wristOrigin;
    if((board||companion)&&queued.length<32)queued.push(event);
  }
  window.addEventListener('message',waitForAppearance);
  let appearance;
  try{({appearance}=await window.LumControls);}catch{return;}
  finally{window.removeEventListener('message',waitForAppearance);}
  const roles={Dark:'--lum-bg',Mid:'--lum-panel',Hi:'--lum-raised',Contrast:'--lum-text',Accent:'--lum-accent',AccentBright:'--lum-accent-bright',Error:'--lum-error'};
  let hostBase=null,hostAlbum=null,wrist=null,socket;
  let controls;
  function sendControlColors(){
    if(controls)frame.contentWindow?.postMessage({type:'lum-control-colors',...controls},wristOrigin);
  }
  function setControlColors(colors){
    controls=colors;
    root.style.setProperty('--lum-control-accent',controls.accent);
    root.style.setProperty('--lum-control-bed',controls.bed);
    root.style.setProperty('--lum-control-ink',controls.ink);
    sendControlColors();
  }
  function show(){
    document.getElementById('manual-appearance').hidden=follow.checked;
    for(const name of [...Object.values(roles),'--lum-field','--lum-dim','--lum-contrast-on-accent'])root.style.removeProperty(name);
    const {source,theme}=appearance.paletteChoice(follow.checked,root.dataset.background==='custom',wrist,hostBase,hostAlbum);
    if(source===0){
      document.getElementById('theme-status').textContent='Manual desktop theme';
      root.dataset.theme=dark.checked?'dark':'light';
      root.dataset.themeOrigin='manual';
      setControlColors(appearance.colors(appearance.manual(dark.checked)).controls);
      return;
    }
    const colors=appearance.colors(theme);
    root.dataset.themeOrigin=['manual','xs-custom','xs-album','xs-base','xs-saved'][source];
    root.dataset.theme=colors.dark?'dark':'light';
    document.getElementById('theme-status').textContent=['','Custom background','Colors from album artwork','XSOverlay colors','Saved XSOverlay colors'][source];
    for(const [key,name] of Object.entries(roles))root.style.setProperty(name,'#'+theme[key]);
    root.style.setProperty('--lum-text',colors.ink);
    root.style.setProperty('--lum-field','color-mix(in srgb,var(--lum-bg) 52%,var(--lum-panel))');
    root.style.setProperty('--lum-dim','color-mix(in srgb,var(--lum-text) 86%,var(--lum-panel))');
    root.style.setProperty('--lum-contrast-on-accent',colors.onAccent);
    setControlColors(colors.controls);
  }
  function syncWrist(){
    sendControlColors();
    frame.contentWindow?.postMessage({type:'lum-wrist-appearance-view',view:root.dataset.boardView||'combined'},wristOrigin);
    if(hostBase)frame.contentWindow?.postMessage({type:'lum-wrist-theme',theme:hostBase},wristOrigin);
  }
  function acceptHost(value){
    const type=Number(value?.ThemeType);
    if(type===0){hostAlbum=null;show();return;}
    const palette=appearance.palette(value);
    if(!palette)return;
    const theme={...palette,ThemeType:type,UseMediaTheme:value.UseMediaTheme===true};
    if(type===1){
      hostBase=theme;
      frame.contentWindow?.postMessage({type:'lum-wrist-theme',theme},wristOrigin);
    }else if(type===2&&theme.UseMediaTheme)hostAlbum=theme;
    show();
  }
  function receive(event){
    if(inBoard && event.source===window.parent && event.origin===boardOrigin){
      if(event.data?.type==='lum-board-theme')acceptHost(event.data.theme);
      if(event.data?.type==='lum-board-offline'){hostBase=null;hostAlbum=null;show();}
      return;
    }
    if(event.source!==frame.contentWindow || event.origin!==wristOrigin)return;
    if(event.data?.type==='lum-wrist-ready')syncWrist();
    if(event.data?.type==='lum-wrist-effective'){
      const theme=appearance.palette(event.data.theme);
      if(!theme)return;
      wrist={theme,album:event.data.album===true,custom:event.data.custom===true};
      show();
    }
  }
  window.addEventListener('message',receive);
  document.addEventListener('lum-view',syncWrist);
  document.addEventListener('lum-background',show);
  frame.addEventListener('load',syncWrist);
  function connect(){
    if(inBoard||socket||!follow.checked)return;
    socket=new WebSocket('ws://127.0.0.1:42070/?client=lum-mix-desktop-preview');
    socket.onopen=()=>socket.send(JSON.stringify({sender:'lum-mix-desktop-preview',target:'xsoverlay',command:'SubscribeToEvents',jsonData:'["Theme"]',rawData:null}));
    socket.onmessage=e=>{
      try{const msg=JSON.parse(e.data);if(msg.command==='UpdateTheme')acceptHost(typeof msg.jsonData==='string'?JSON.parse(msg.jsonData):msg.jsonData);}catch{}
    };
    socket.onclose=()=>{socket=null;hostBase=null;hostAlbum=null;show();};
  }
  try{
    const prefs=JSON.parse(localStorage.getItem('lum-mix-appearance'));
    if(prefs){follow.checked=prefs.follow!==false;dark.checked=prefs.dark!==false;}
  }catch{}
  for(const element of [follow,dark])element.addEventListener('change',()=>{
    try{localStorage.setItem('lum-mix-appearance',JSON.stringify({follow:follow.checked,dark:dark.checked}));}catch{}
    show();connect();
  });
  document.addEventListener('lum-preferences-applied',()=>{show();connect();});
  for(const event of queued)receive(event);
  show();syncWrist();connect();if(!inBoard)setInterval(connect,15000);
})();
