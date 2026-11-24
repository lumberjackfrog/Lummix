(function(){
  const root=document.documentElement, follow=document.getElementById('follow-theme'), dark=document.getElementById('dark');
  const frame=document.getElementById('native-wrist'), wristOrigin='http://127.0.0.1:41836';
  const boardOrigin='http://127.0.0.1:41835';
  const inBoard=window.parent!==window && location.href==='about:srcdoc';
  const saved={Dark:'232126',Mid:'37353b',Hi:'46434b',Contrast:'f6f4f8',Accent:'7a0aa6',AccentBright:'cc91e3',Error:'ff7474'};
  const roles={Dark:'--lum-bg',Mid:'--lum-panel',Hi:'--lum-raised',Contrast:'--lum-text',Accent:'--lum-accent',AccentBright:'--lum-accent-bright',Error:'--lum-error'};
  const required=['Dark','Mid','Hi','Contrast','Accent'];
  const paletteKeys=[...Object.keys(roles),'Confirm','Warning'];
  let hostBase=null,hostAlbum=null,wrist=null,socket;

  function valid(value){return typeof value==='string'&&/^[0-9a-f]{6}(?:[0-9a-f]{2})?$/i.test(value);}
  function cleanTheme(value){
    if(!value || typeof value!=='object')return null;
    const clean={};
    for(const key of paletteKeys){
      const component=value[key]||(key==='AccentBright'&&value.AccentHi);
      if(valid(component))clean[key]=component.slice(0,6);
    }
    return required.every(key=>clean[key])?{...saved,...clean,AccentBright:clean.AccentBright||clean.Accent}:null;
  }
  function luminance(hex){
    const rgb=hex.slice(0,6).match(/../g).map(c=>parseInt(c,16)/255)
      .map(v=>v<=.04045?v/12.92:((v+.055)/1.055)**2.4);
    return .2126*rgb[0]+.7152*rgb[1]+.0722*rgb[2];
  }
  function contrastText(hex){return luminance(hex)>.179?'#000000':'#ffffff';}
  function contrastRatio(foreground,background){
    const a=luminance(foreground),b=luminance(background);
    return (Math.max(a,b)+.05)/(Math.min(a,b)+.05);
  }
  function blendHex(from,to,fraction){
    return [0,2,4].map(i=>{
      const a=parseInt(from.slice(i,i+2),16), b=parseInt(to.slice(i,i+2),16);
      return Math.round(a+(b-a)*fraction).toString(16).padStart(2,'0');
    }).join('');
  }
  // Keep the sampled hue when possible. Move its lightness only as far as needed
  // for active strokes to read on each panel, including almost uniform covers.
  function legibleStroke(source,surfaces,threshold){
    const score=color=>Math.min(...surfaces.map(surface=>contrastRatio(color,surface)));
    let best=source, bestScore=score(source);
    if(bestScore>=threshold)return source;
    for(let step=1;step<=32;step++){
      for(const extreme of ['000000','ffffff']){
        const color=blendHex(source,extreme,step/32), next=score(color);
        if(next>=threshold)return color;
        if(next>bestScore){best=color;bestScore=next;}
      }
    }
    return best;
  }
  let controls;
  function sendControlColors(){
    if(controls)frame.contentWindow?.postMessage({type:'lum-control-colors',...controls},wristOrigin);
  }
  function setControlColors(theme){
    const accent=legibleStroke(theme.Accent,[theme.Dark,theme.Mid,theme.Hi],3.1);
    const bed=legibleStroke(blendHex(theme.Dark,theme.Mid,.3),[accent],4.5);
    controls={accent:'#'+accent,bed:'#'+bed,ink:contrastText(accent)};
    root.style.setProperty('--lum-control-accent',controls.accent);
    root.style.setProperty('--lum-control-bed',controls.bed);
    root.style.setProperty('--lum-control-ink',controls.ink);
    sendControlColors();
  }
  function show(){
    document.getElementById('manual-appearance').hidden=follow.checked;
    for(const name of [...Object.values(roles),'--lum-field','--lum-dim','--lum-contrast-on-accent'])root.style.removeProperty(name);
    if(!follow.checked){
      document.getElementById('theme-status').textContent='Manual desktop theme';
      root.dataset.theme=dark.checked?'dark':'light';
      root.dataset.themeOrigin='manual';
      setControlColors(dark.checked
        ? {Accent:'efa4df',Dark:'12141b',Mid:'191b24',Hi:'272a38'}
        : {Accent:'922870',Dark:'edefef',Mid:'f7f8fa',Hi:'e4e8ef'});
      return;
    }
    const pinned=root.dataset.background==='custom';
    const theme=pinned ? (wrist?.custom ? wrist.theme : hostBase||saved) : wrist?.theme||hostAlbum||hostBase||saved;
    const album=!pinned && (wrist ? wrist.album : Boolean(hostAlbum));
    const source=pinned?'Custom background':album?'Colors from album artwork':wrist||hostBase?'XSOverlay colors':'Saved XSOverlay colors';
    root.dataset.themeOrigin=pinned?'xs-custom':album?'xs-album':wrist||hostBase?'xs-base':'xs-saved';
    const minimum=ink=>Math.min(...['Dark','Mid','Hi'].map(role=>contrastRatio(ink,theme[role])));
    const resolvedInk=minimum(theme.Contrast)>=4.5?'#'+theme.Contrast:
      minimum('ffffff')>=minimum('000000')?'#ffffff':'#000000';
    root.dataset.theme=luminance(resolvedInk.slice(1))>.45?'dark':'light';
    document.getElementById('theme-status').textContent=source;
    for(const [key,name] of Object.entries(roles))root.style.setProperty(name,'#'+theme[key]);
    root.style.setProperty('--lum-text',resolvedInk);
    root.style.setProperty('--lum-field','color-mix(in srgb,var(--lum-bg) 52%,var(--lum-panel))');
    root.style.setProperty('--lum-dim','color-mix(in srgb,var(--lum-text) 86%,var(--lum-panel))');
    root.style.setProperty('--lum-contrast-on-accent',contrastText(theme.Accent));
    setControlColors(theme);
  }
  function syncWrist(){
    sendControlColors();
    frame.contentWindow?.postMessage({type:'lum-wrist-appearance-view',view:root.dataset.boardView||'combined'},wristOrigin);
    if(hostBase)frame.contentWindow?.postMessage({type:'lum-wrist-theme',theme:hostBase},wristOrigin);
  }
  function acceptHost(value){
    const type=Number(value?.ThemeType);
    if(type===0){hostAlbum=null;show();return;}
    const palette=cleanTheme(value);
    if(!palette)return;
    const theme={...palette,ThemeType:type,UseMediaTheme:value.UseMediaTheme===true};
    if(type===1){
      hostBase=theme;
      frame.contentWindow?.postMessage({type:'lum-wrist-theme',theme},wristOrigin);
    }else if(type===2&&theme.UseMediaTheme)hostAlbum=theme;
    show();
  }
  window.addEventListener('message',event=>{
    if(inBoard && event.source===window.parent && event.origin===boardOrigin){
      if(event.data?.type==='lum-board-theme')acceptHost(event.data.theme);
      if(event.data?.type==='lum-board-offline'){hostBase=null;hostAlbum=null;show();}
      return;
    }
    if(event.source!==frame.contentWindow || event.origin!==wristOrigin)return;
    if(event.data?.type==='lum-wrist-ready')syncWrist();
    if(event.data?.type==='lum-wrist-effective'){
      const theme=cleanTheme(event.data.theme);
      if(!theme)return;
      wrist={theme,album:event.data.album===true,custom:event.data.custom===true};
      show();
    }
  });
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
  show();syncWrist();connect();if(!inBoard)setInterval(connect,15000);
})();
