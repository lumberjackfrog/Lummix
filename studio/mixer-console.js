(() => {
  'use strict';
  const el = id => document.getElementById(id);
  const svgNS='http://www.w3.org/2000/svg';
  const shapes={
    headset:'<path d="M3 12a9 9 0 0 1 18 0v5a2 2 0 0 1-2 2h-2v-7h4M3 12h4v7H5a2 2 0 0 1-2-2z"/>',
    arrow:'<path d="m9 5 7 7-7 7"/>', mic:'<rect x="9" y="2.5" width="6" height="12" rx="3"/><path d="M5.5 11.5a6.5 6.5 0 0 0 13 0M12 18v3M8.5 21h7"/>',
    music:'<path d="M9 17V5l11-2v12M9 8l11-2"/><ellipse cx="6.1" cy="17.8" rx="2.9" ry="2.3"/><ellipse cx="17.1" cy="15.8" rx="2.9" ry="2.3"/>',
    play:'<path d="m7 4 13 8-13 8z" fill="currentColor" stroke="none"/>',pause:'<path d="M8 5v14m8-14v14" stroke-width="4"/>',
    stop:'<rect x="5.5" y="5.5" width="13" height="13" rx="2" fill="currentColor" stroke="none"/>',
    settings:'<path d="M3 6h7m6 0h5M3 12h2m6 0h10M3 18h11m6 0h1"/><circle cx="13" cy="6" r="2.5"/><circle cx="8" cy="12" r="2.5"/><circle cx="17" cy="18" r="2.5"/>',
    previous:'<path d="M5 5v14m14-14L8 12l11 7z" fill="currentColor"/>',
    next:'<path d="M19 5v14M5 5l11 7-11 7z" fill="currentColor"/>',
    back15:'<path d="M5 8a8 8 0 1 1-1 7M4 3v5h5"/><text x="12" y="15.5" text-anchor="middle" font-size="8.1" font-weight="700" font-family="system-ui" fill="currentColor" stroke="none">15</text>',
    forward15:'<path d="M19 8a8 8 0 1 0 1 7m0-12v5h-5"/><text x="12" y="15.5" text-anchor="middle" font-size="8.1" font-weight="700" font-family="system-ui" fill="currentColor" stroke="none">15</text>',
    background:'<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M8 15h8"/>'
  };
  function icon(holder,key){
    const svg=document.createElementNS(svgNS,'svg');svg.setAttribute('viewBox','0 0 24 24');
    svg.setAttribute('fill','none');svg.setAttribute('stroke','currentColor');svg.setAttribute('stroke-width','1.8');
    svg.setAttribute('stroke-linecap','round');svg.setAttribute('stroke-linejoin','round');svg.setAttribute('aria-hidden','true');
    svg.innerHTML=shapes[key];holder.replaceChildren(svg);holder.setAttribute('aria-hidden','true');
  }
  document.querySelectorAll('[data-glyph]').forEach(node=>icon(node,node.dataset.glyph));
  document.querySelectorAll('dialog.studio-sheet').forEach(sheet=>{
    sheet.addEventListener('toggle',event=>{
      if(event.newState==='open')sheet.querySelector('.sheet-scroll').scrollTop=0;
    });
  });
  for(const [id,key] of Object.entries({'mic-mute':'mic','personal-volume':'music','settings':'settings'})){
    const b=el(id),mark=document.createElement('i'),label=document.createElement('span');
    label.className='button-label';label.textContent=b.textContent;mark.className='glyph';icon(mark,key);b.replaceChildren(mark,label);
  }
  const spotifyOrigin='http://127.0.0.1:41838';
  const setupBridge=(()=>{
    try {return new URL(top.location.href).searchParams.get('app')==='1'?top.chrome?.webview:null;}
    catch {return null;}
  })();
  let spotifySetupPhase='idle';
  const setupMessages={checking:'Checking for Spotify…',downloading:'Getting Spotify desktop from Spotify…',
    installing:'Installing Spotify desktop…',installed:'Spotify desktop is installed. Sign in to Spotify to play music.',
    opened:'Opening Spotify. Sign in if asked.',failed:'Spotify setup couldn’t finish. Try again or download directly from Spotify.',
    unconfirmed:'Spotify setup is still unconfirmed. Finish any open Spotify installer, then check again.',
    elevated:'Reopen Lummix normally to set up Spotify for your Windows account.'};
  const missingPlayer=info=>info?.installationKnown===true && !info.installations?.length && !['ready','detected','ambiguous'].includes(info.state);
  function paintPlayerSetup(missing){
    const busy=['checking','downloading','installing'].includes(spotifySetupPhase);
    const problem=['failed','unconfirmed','elevated'].includes(spotifySetupPhase);
    const connected=['ready','detected'].includes(currentPlayer?.state);
    const message=connected?'':setupMessages[spotifySetupPhase]||'';
    for(const id of ['player-setup-status','setup-player-status']){
      el(id).textContent=message;el(id).hidden=!message;
    }
    const unknown=!currentPlayer?.installationKnown && !currentPlayer?.installations?.length && currentPlayer?.state!=='ambiguous';
    el('install-player').hidden=!setupBridge || connected || (!missing && !problem && !unknown);
    el('install-player').textContent=missing?'Install Spotify desktop':'Set up Spotify';
    el('install-player').disabled=busy;
    el('download-player').hidden=connected || !((!setupBridge && missing) || problem);
    el('setup-player-help').hidden=connected || !problem;
  }
  if(setupBridge){
    setupBridge.addEventListener('message',({data})=>{
      if(data?.type!=='lummix-spotify-setup')return;
      const changed=spotifySetupPhase!==data.phase;
      spotifySetupPhase=data.phase;
      paintPlayerSetup(missingPlayer(currentPlayer));
      if(changed && ['installed','opened','ready'].includes(data.phase))scanPlayer(true);
    });
    el('install-player').addEventListener('click',event=>{if(event.isTrusted)setupBridge.postMessage('spotify-setup');});
  }
  let currentPlayer=null, scanPending=false, playerPending=false, actionPending=false, selectionEpoch=0;
  const transportActions={'media-back':['seekBack15','canSeek'],'media-forward':['seekForward15','canSeek'],'media-next':['next','canNext'],'media-previous':['previous','canPrevious'],'media-stop':['stop','canStop']};
  let lastArtworkId=null, lastArtwork=null, artworkRetry=0, retryTrack='', retries=0;
  async function query(url,options={}){
    const result=await fetch(url,{cache:'no-store',credentials:'omit',signal:AbortSignal.timeout(12000),...options});
    if(!result.ok)throw new Error(String(result.status));
    return result.json();
  }
  function busy(button,yes){button.disabled=yes;button.setAttribute('aria-busy',String(yes));}
  function drawPlayer(info){
    if(info?.artworkId && info.artworkId===lastArtworkId && info.artwork===undefined){
      info={...info,artwork:lastArtwork};
    }
    currentPlayer=info;
    lastArtworkId=info?.artworkId||null;lastArtwork=info?.artwork||null;
    const ready=info?.state==='ready', editions=info?.installations||[];
    const detected=info?.state==='detected';
    el('player-connection').hidden=!(ready || detected);
    if(ready || detected){
      el('player-edition').textContent=info.edition==='store'?'Microsoft Store':info.edition==='desktop'?'Spotify desktop':'Spotify';
      el('player-playback-state').textContent=detected?'Running on this PC':info.playing?'Playing':'Not playing';
    }
    const missing=missingPlayer(info);
    const song=ready ? [info.title,info.artist].filter(Boolean).join(' · ') : '';
    el('spotify-summary').textContent=ready?'':detected?'Local player running':missing?'Get Spotify for Windows':editions.length?'Installed on this PC':'Personal player';
    el('spotify-summary').hidden=ready;
    el('connect-spotify').querySelector('strong').textContent=ready?'Spotify connected':detected?'Spotify detected':editions.length?'Spotify installed':'Connect Spotify';
    const states={no_session:editions.length?'Open Spotify and play a track to connect.':'Open Spotify, then check again.',unsupported:'Windows media controls are unavailable on this PC.',ambiguous:'Pause the other Spotify player, then check again.',unavailable:'The local player is unavailable.'};
    el('player-result').textContent=ready?(song||'Spotify is connected.') : detected?'Lummix found Spotify. You can start a mix and use Spotify’s playback controls while Windows reconnects.':missing?'Spotify isn’t installed on this PC.':states[info?.state]||'Couldn’t reach the local player connection.';
    paintPlayerSetup(missing);
    el('open-player').hidden=!(detected || info?.transportSource==='spotify_window' || (editions.length && info?.state==='no_session'));
    const transport=el('personal-transport'), shortcut=el('spotify-play');
    transport.hidden=!ready;transport.disabled=!ready || (info.playing?!info.canPause:!info.canPlay);
    transport.textContent=ready && info.playing?'Pause Spotify':'Play Spotify';
    const label=ready && info.playing?'Pause Spotify':'Play Spotify';
    shortcut.setAttribute('aria-label',label);shortcut.title=label;
    icon(shortcut.querySelector('.glyph'),info?.playing?'pause':'play');shortcut.disabled=actionPending || !ready || transport.disabled;
    for(const [id,[,cap]] of Object.entries(transportActions)){el(id).disabled=actionPending || !ready || !info[cap];}
    window.LumGlobalPulse?.bind(info);
    const media={state:info?.state||'unavailable',playing:Boolean(info?.playing),canPlay:Boolean(info?.canPlay),canPause:Boolean(info?.canPause),canPrevious:Boolean(info?.canPrevious),canNext:Boolean(info?.canNext),title:info?.title||'',artist:info?.artist||'',artwork:info?.artwork||'',itemUrl:info?.itemUrl||''};
    const prev=window.LumSpotifyMedia;
    if(!prev || Object.keys(media).some(key=>media[key]!==prev[key])){
      window.LumSpotifyMedia=media;
      document.dispatchEvent(new CustomEvent('lum-spotify-media',{detail:media}));
      publishWristMedia();
    }
    const track=ready && info.transportSource!=='spotify_window'?JSON.stringify([media.title,media.artist]):'';
    if(track!==retryTrack){clearTimeout(artworkRetry);artworkRetry=0;retries=0;retryTrack=track;}
    if(media.artwork || !track){clearTimeout(artworkRetry);artworkRetry=0;}
    else if(!artworkRetry && retries<2 && !document.hidden){
      // Spotify sometimes publishes the track before its thumbnail.
      artworkRetry=setTimeout(()=>{artworkRetry=0;if(!document.hidden)scanPlayer(true);},++retries*1200);
    }
  }
  async function scanPlayer(quiet=false){
    if(playerPending || actionPending)return false;
    const epoch=selectionEpoch;
    playerPending=true;
    if(!quiet){busy(el('retry-player'),true);el('player-result').textContent='Looking for Spotify…';}
    try{const snapshot=await query(spotifyOrigin+'/spotify/status'+(lastArtworkId?'?artworkId='+encodeURIComponent(lastArtworkId):''));if(epoch!==selectionEpoch)return false;drawPlayer(snapshot);return true;}
    catch{if(epoch===selectionEpoch)drawPlayer(null);return false;}finally{playerPending=false;busy(el('retry-player'),false);}
  }
  function openPlayer(){el('player-connect-dialog').showModal();setupBridge?.postMessage('spotify-setup-state');scanPlayer();}
  el('connect-spotify').onclick=openPlayer;
  el('retry-player').onclick=()=>scanPlayer();
  el('spotify-play').onclick=()=>{if(currentPlayer?.state==='ready')el('personal-transport').click();};
  async function control(action){
    if(actionPending || currentPlayer?.state!=='ready')return;
    actionPending=true;selectionEpoch++;busy(el('personal-transport'),true);busy(el('spotify-play'),true);
    for(const id of Object.keys(transportActions))el(id).disabled=true;
    try{
      const snapshot=await query(spotifyOrigin+'/spotify/control',{method:'POST',headers:{'Content-Type':'application/json','X-Lum-Spotify-Token':currentPlayer.token},body:JSON.stringify({action})});
      actionPending=false;drawPlayer(snapshot);el('transport-message').textContent='';
    }catch{
      actionPending=false;drawPlayer(currentPlayer);
      el('transport-message').textContent='Spotify could not complete that action.';
      await scanPlayer(true);
    }finally{for(const id of ['personal-transport','spotify-play'])el(id).removeAttribute('aria-busy');}
  }
  el('personal-transport').onclick=()=>control(currentPlayer?.playing?'pause':'play');
  for(const [id,[action]] of Object.entries(transportActions))el(id).onclick=()=>control(action);
  function listEntry(parent,heading,items){
    if(!items.length)return;
    const title=document.createElement('strong'),list=document.createElement('ul');title.textContent=heading;
    for(const name of items){const li=document.createElement('li');li.textContent=String(name);list.append(li);}parent.append(title,list);
  }
  const setupHost='http://127.0.0.1:41836';
  const setupBusy=new Set(['checking','preparing','awaiting-windows']);
  const retryable=new Set(['missing','failed','cancelled']);
  const cableStates={
    checking:['Checking VB-CABLE…',''],
    missing:['VB-CABLE needs to be installed','First-time setup needs Windows administrator approval and a restart.'],
    preparing:['Preparing VB-CABLE','Verifying the original VB-Audio installer.'],
    'awaiting-windows':['Waiting for Windows','Approve the Windows administrator prompt for the signed VB-Audio installer.'],
    cancelled:['Setup was cancelled','Choose Retry setup when you’re ready to approve the Windows prompt.'],
    failed:['Setup could not finish','Choose Retry setup to run the verified installer again.'],
    partial:['The cable is only partly available','If you just installed VB-CABLE, restart Windows. Otherwise, check that both VB-CABLE endpoints are enabled in Windows Sound.'],
    unavailable:['Couldn’t check VB-CABLE','Reopen Lummix or check again.'],
    'restart-needed':['Restart Windows to finish','After restarting, reopen Lummix from Start and choose Set up Lummix.']
  };
  let scanVersion=0;
  const micPanelOpen=()=>el('mic-connect-dialog').open||el('settings-dialog').open;
  const currentScan=version=>version===scanVersion && micPanelOpen();
  const micSource=el('mic-recording-source'),micButton=el('mic-live-action');
  const micURL=spotifyOrigin+'/mix/mic';
  const captureStates={
    off:['Mix off','Start the mix to send your microphone and available Spotify audio to VRChat.'],
    waiting_spotify:['Waiting for Spotify','Your mic is live. Open Spotify on this PC to add it to the mix.'],
    connecting:['Connecting','Your mic is live. Connecting to Spotify audio…'],
    waiting_audio:['Waiting for audio','Your mic is live. Waiting for sound from Spotify on this PC.'],
    receiving:['In the mix','Spotify audio is reaching the mix.'],
    unsupported:['Mic only','Your mic is live. This Windows installation does not support Spotify audio capture.'],
    unavailable:['Mic only','Your mic is live. Spotify audio capture is unavailable.'],
    ambiguous:['Choose one player','Your mic is live. Close the extra Spotify player so Lummix can select one.'],
    direct_to_cable:['Direct to cable','Spotify is already on VB-CABLE. In Windows Settings → System → Sound → Volume mixer, set Spotify’s output to your headphones. Lummix can then mix and duck it.']
  };
  function showCapture(value){
    const item=captureStates[value]||['Status unknown','Spotify audio routing could not be confirmed.'];
    state.spotifyRouteLabel=item[0];el('mix-spotify-status').textContent=item[1];
    if(value==='off')state.nativeMixActive=false;
    else if(captureStates[value])state.nativeMixActive=true;
    el('music-state').textContent=state.spotifyMuted?'Output muted':item[0];
  }
  window.addEventListener('lum-mix-capture',({detail})=>{
    showCapture(detail?.state);
    if(captureStates[detail?.state])quickState(detail.state!=='off',quickBusy);
  });
  let micSnapshot=null,micRead=0,micReading=0,micWriting=false,micNotice='',micPoll=0,ownedRoute=null;
  let quickBusy=false, quickLive=false;
  function quickState(live,working=false){
    quickLive=live;
    el('quick-mix-label').textContent=working?'Setting up…':live?'Stop mix':'Set up Lummix';
    el('set-up-lummix').setAttribute('aria-label',working?'Setting up Lummix':live?'Stop mix':'Set up Lummix');
    el('set-up-lummix').setAttribute('aria-describedby','headset-summary');
    el('set-up-lummix').disabled=working;
    el('set-up-lummix').title=live?'Stop sending your mic and music to the virtual microphone':'Set up the virtual microphone and start sending mic and Spotify audio';
  }
  const micErrors={
    stop_before_select:'Stop the live mic before changing its recording device.',
    refresh_mix_settings:'Check the mic again before continuing.',
    cable_unavailable:'Finish virtual microphone setup before starting.',
    cable_restart_needed:'Restart Windows, then reopen Lummix and choose Set up Lummix.',
    no_input:'Connect a recording device, then check again.',
    selected_input_unavailable:'Reconnect your saved mic or choose another input.',
    input_disconnected:'The recording device disconnected. Reconnect it and start the mic again.',
    route_interrupted:'The audio route was interrupted. Check the devices before starting again.',
    restart_audio_helper:'Reopen Lummix to refresh Windows recording devices.',
    settings_not_saved:'The input choice could not be saved. Check again before starting.',
    saved_settings_unreadable:'The audio host could not read your saved input. Reopen Lummix and check again.',
    route_already_active:'A mic route is already live.',
    route_unavailable:'Windows could not open the local audio route. Check the devices and try again.',
    selection_changed:'The recording device changed. Check the selected input and try again.',
    route_start_failed:'Windows could not start this route. Check microphone access and the selected input.',
    stop_failed:'Windows could not confirm the route stopped. Check again.'
  };
  function describeMicError(code,fallback){return micErrors[code]||fallback;}
  function paintInput(){
    const s=micSnapshot, devices=s?.inputs||[];
    const active=Boolean(s?.routeActive),selected=devices.find(d=>d.id===s?.selectedId);
    const live=devices.find(d=>d.id===s?.activeId);
    const saved=s?.mode==='manual'?s.savedId:null;
    const choices=[['','Auto detect',false]];
    for(const d of devices){
      const hint=d.id===s.recommendedId?' · Recommended':d.kind==='headsetBridge'?' · Headset link':'';
      choices.push([d.id,d.name+hint,false]);
    }
    if(saved && !devices.some(d=>d.id===saved))choices.push([saved,'Saved microphone (disconnected)',true]);
    const signature=JSON.stringify(choices);
    if(micSource.dataset.choices!==signature){
      micSource.replaceChildren(...choices.map(([id,name,disabled])=>{
        const option=new Option(name,id);option.disabled=disabled;return option;
      }));
      micSource.dataset.choices=signature;
    }
    micSource.value=saved||'';
    micSource.disabled=!s || active || micWriting || quickBusy || (!devices.length && !saved);
    let note='';
    if(s){
      if(active)note='Stop the mic to change its recording device.';
      else if(saved && !selected)note='Reconnect your saved mic or choose another recording device.';
      else if(!selected)note='Connect a recording device, then choose Check again.';
      else if(!saved){
        const reason=selected.headset?'headset input':selected.defaultCommunications?'Windows communications default':selected.defaultInput?'Windows default input':'available input';
        note='Auto detect selected '+selected.name+' ('+reason+').';
      }else note=selected.kind==='headsetBridge'?'Saved on this PC. Connect the headset to its streaming app to use this input.':'Saved on this PC.';
    }
    el('mic-source-note').textContent=note;
    micButton.textContent=active?'Stop mix':'Start mix';
    micButton.title=active?'Stop sending the mix to VB-CABLE':'Send your mic and available Spotify audio to VB-CABLE';
    micButton.disabled=!s?.token || micWriting || quickBusy || (!active && (!selected || !s.cableReady));
    const status=el('mic-live-status');
    status.dataset.active=String(active);
    status.textContent=!s?'Route status unavailable':active?'Mic live'+(live?' · '+live.name:''):!s.cableReady?'Waiting for VB-CABLE':'Mix stopped';
    if(s){
      showCapture(s.spotifyCapture ?? (active?'unknown':'off'));state.nativeMixActive=active;
      quickState(active,quickBusy);
      if(s.cableReady && typeof s.captureName==='string' && s.captureName.trim()){
        el('mic-endpoint').textContent=s.captureName;el('mic-route').hidden=false;
      }
    }
    if(s && (active||s.cableReady))el('headset-summary').textContent=active?'Mic live'+(live?' · '+live.name:''):'Start mic + Spotify';
    const message=micNotice||describeMicError(s?.error,'');
    el('mic-live-error').hidden=!message;el('mic-live-error').textContent=message;
  }
  async function readInput(){
    if(!micPanelOpen() || micWriting || (micReading && micReading===micRead))return;
    const version=++micRead;
    micReading=version;
    try{
      const s=await query(micURL);
      if(version!==micRead || !micPanelOpen())return;
      micSnapshot=Array.isArray(s.inputs) && typeof s.token==='string'?s:null;
    }catch{if(version!==micRead)return;micSnapshot=null;}
    finally{if(micReading===version)micReading=0;}
    paintInput();
  }
  async function changeInput(body){
    if(!micSnapshot?.token || micWriting)return;
    const token=micSnapshot.token;
    micWriting=true;++micRead;micNotice='';paintInput();
    el('mic-live-status').textContent={select:'Saving input…',setup:'Setting up your mix…',start:'Starting mix…',stop:'Stopping mix…'}[body.action];
    try{
      const response=await fetch(micURL,{method:'POST',cache:'no-store',credentials:'omit',
        headers:{'Content-Type':'application/json','X-Lum-Mix-Token':token},
        body:JSON.stringify(body),signal:AbortSignal.timeout(['setup','start'].includes(body.action)?35000:18000)});
      const s=await response.json();
      micSnapshot=Array.isArray(s.inputs) && typeof s.token==='string'?s:null;
      if(response.ok && ['start','setup'].includes(body.action) && s.routeActive && typeof s.ownerToken==='string')ownedRoute={token,ownerToken:s.ownerToken};
      if(s.aliasError)micNotice='Windows kept the current microphone name. Choose the name shown below in VRChat.';
      if(response.ok && body.action==='stop' && !s.routeActive)ownedRoute=null;
      if(!response.ok)micNotice=describeMicError(s.error,'The audio host could not complete that action. Check again.');
    }catch{
      micSnapshot=null;micNotice='Couldn’t confirm the route change. Check again to see its current state.';
    }finally{
      micWriting=false;paintInput();
      if(!micSnapshot)readInput();
    }
  }
  micSource.addEventListener('change',()=>changeInput({action:'select',id:micSource.value||null}));
  let routeTask=null, releaseTask=null;
  micButton.addEventListener('click',()=>{
    if(micSnapshot?.routeActive)routeTask=changeInput({action:'stop'});
    else if(micSnapshot?.selectedId)routeTask=changeInput({action:'start',id:micSnapshot.selectedId});
  });
  window.releaseLummixAudio=()=>{
    if(releaseTask)return releaseTask;
    releaseTask=(async()=>{
      if(routeTask)await routeTask;
      if(!ownedRoute)return true;
      const owner=ownedRoute;
      try{
        const response=await fetch(micURL,{method:'POST',credentials:'omit',keepalive:true,
          headers:{'Content-Type':'application/json','X-Lum-Mix-Token':owner.token},
          body:JSON.stringify({action:'release',ownerToken:owner.ownerToken})});
        if(response.ok && ownedRoute===owner)ownedRoute=null;
        return response.ok;
      }catch{return false;}
    })().finally(()=>{releaseTask=null;});
    return releaseTask;
  };
  window.addEventListener('pagehide',window.releaseLummixAudio);
  function showCable(info){
    const ready=info.ready===true, capture=micSnapshot?.captureName||info.endpoints?.capture;
    const knownName=typeof capture==='string' && capture.trim().length>0;
    const [title,note]=ready
      ? ['VB-CABLE is ready',knownName?'':'Windows verified the driver. Check again to read the microphone’s exact name.']
      : cableStates[info.state]||cableStates.unavailable;
    el('mic-status-title').textContent=title;
    el('cable-status').textContent=note || (ready?info.message||'':'');
    el('cable-status').hidden=!note;
    el('mic-route').hidden=!(ready && knownName);
    el('mic-endpoint').textContent=ready && knownName?capture:'';
    el('cable-panel').dataset.ready=String(ready);
    if(!micSnapshot?.routeActive)el('headset-summary').textContent=ready?'Start mic + Spotify':title;
    const button=el('redetect');
    button.dataset.intent=retryable.has(info.state)?'retry':'check';
    button.textContent=info.state==='missing'?'Install VB-CABLE':button.dataset.intent==='retry'?'Retry setup':'Check again';
  }
  async function setUpCable(version,intent){
    try{
      let info=await query(setupHost+'/setup-status');
      if(!currentScan(version))return;
      showCable(info);
      let attempted=false;
      const deadline=Date.now()+300000;
      while(Date.now()<deadline){
        if(!attempted && ['setup','retry'].includes(intent) && retryable.has(info.state)){
          if(!info.token)throw Error('Missing local permission');
          attempted=true;
          info=await query(setupHost+'/setup-cable',{method:'POST',headers:{'Content-Type':'application/json','X-Lum-Setup-Token':info.token},body:'{}'});
          if(!currentScan(version))return;
          showCable(info);
          continue;
        }
        if(!setupBusy.has(info.state))break;
        await new Promise(resolve=>setTimeout(resolve,1600));
        if(!currentScan(version))return;
        info=await query(setupHost+'/setup-status');
        if(!currentScan(version))return;
        showCable(info);
      }
      if(setupBusy.has(info.state)){
        el('cable-status').hidden=false;
        el('cable-status').textContent='Windows setup is still pending. Check again for the result.';
      }
      return info;
    }catch{
      if(currentScan(version))showCable({state:'unavailable'});
    }
  }
  // The detector returns present devices, running software, available Windows audio
  // endpoints and saved SteamVR profiles separately. Only real HMD names belong here.
  const isHeadset=name=>/^(PICO |Meta Quest|Oculus Rift|Valve Index(?: ×\d+)?$|VIVE (?:Pro|Focus|Cosmos)|HTC VIVE|Bigscreen Beyond|HP Reverb|Varjo Aero|Pimax Crystal|SteamVR headset)/i.test(name);
  async function scanDevices(version){
    el('detect-result').textContent='Checking for a connected headset…';
    el('device-list').replaceChildren();el('device-details').replaceChildren();el('headset-context').textContent='';
    try{
      const r=await query(setupHost+'/detect-devices');
      if(!currentScan(version))return;
      const connected=r.connected||[], headsets=connected.filter(isHeadset), saved=r.saved||[];
      const uncertain=(r.warnings||[]).some(text=>/hardware and runtime|tracked-device status/i.test(text));
      el('detect-result').textContent=headsets.length?headsets.join(' · '):uncertain?'Headset connection unknown':'No connected headset detected';
      const past=saved.filter(isHeadset).filter(name=>!headsets.includes(name));
      if(past.length)listEntry(el('device-list'),'Saved in SteamVR',past);
      el('headset-context').textContent=headsets.length?'Detected by Windows or SteamVR. Auto detect can use the connected headset to choose its microphone.':'You can use a Windows recording device in VRChat’s Desktop mode without a headset.';
      listEntry(el('device-details'),'Connected headsets',headsets);
      listEntry(el('device-details'),'Connected accessories',connected.filter(name=>!isHeadset(name)));
      listEntry(el('device-details'),'Running VR software',r.runtimes||[]);
      listEntry(el('device-details'),'Available Windows audio ports',r.audio||[]);
      listEntry(el('device-details'),'Saved SteamVR profiles',saved);
      listEntry(el('device-details'),'Scan notes',r.warnings||[]);
      el('device-evidence').hidden=!el('device-details').childElementCount;
    }catch{
      if(!currentScan(version))return;
      el('detect-result').textContent='Headset connection unknown';
      el('headset-context').textContent='Windows could not complete the headset check. Virtual mic setup works independently.';
      el('device-evidence').hidden=true;
    }
  }
  async function detect(intent,setupSpotify=false){
    if(scanPending)return;
    const version=++scanVersion;
    scanPending=true;busy(el('redetect'),true);
    micNotice='';
    const start=['setup','retry'].includes(intent),stop=intent==='stop';
    if(start||stop){quickBusy=true;quickState(quickLive,true);paintInput();}
    const scan=scanDevices(version);
    try{
      if(stop) {
        await readInput();
        if(currentScan(version) && micSnapshot?.routeActive){routeTask=changeInput({action:'stop'});await routeTask;}
        if(currentScan(version))await setUpCable(version,'check');
      } else {
        const [cable]=await Promise.all([setUpCable(version,intent),readInput()]);
        if(start && setupSpotify && currentScan(version) && (cable?.ready || cable?.state==='restart-needed')){
          setupBridge?.postMessage('spotify-setup');
        }
        if(start && cable?.ready && currentScan(version)){
          await readInput();
          if(currentScan(version) && micSnapshot?.selectedId && !micSnapshot.routeActive){
            routeTask=changeInput({action:'setup',id:micSnapshot.selectedId});await routeTask;
          }
        }
      }
      await scan;
    }
    finally{if(version===scanVersion){quickBusy=false;quickState(!!micSnapshot?.routeActive);busy(el('redetect'),false);scanPending=false;paintInput();}}
  }
  function closeMicPanel(){
    if(micPanelOpen())return;
    clearInterval(micPoll);++micRead;
    ++scanVersion;scanPending=false;quickBusy=false;quickState(!!micSnapshot?.routeActive);busy(el('redetect'),false);
  }
  el('mic-connect-dialog').addEventListener('close',closeMicPanel);
  el('settings-dialog').addEventListener('close',closeMicPanel);
  const mixPanels=el('mix-progress-panels');
  const diagnostics=[el('cable-panel'),el('headset-panel'),mixPanels.querySelector('.sheet-credit')];
  function pollMic(){
    clearInterval(micPoll);micPoll=setInterval(()=>{if(!document.hidden && !scanPending)readInput();},5000);
  }
  function openSetup(intent,setupSpotify){
    mixPanels.append(el('mic-input-panel'),...diagnostics);
    el('mic-connect-dialog').showModal();
    micSnapshot=null;micNotice='';paintInput();el('mic-live-status').textContent='Checking the audio route…';
    showCable({state:'checking'});detect(intent,setupSpotify);
    pollMic();
  }
  el('set-up-lummix').onclick=event=>openSetup(quickLive?'stop':'setup',event.isTrusted);
  function presentMicSettings(){
    el('settings-mic-controls').append(el('mic-input-panel'));
    el('settings-device-panels').append(...diagnostics);
    micNotice='';readInput();pollMic();
    if(el('settings-device-details').open)detect('inspect');
  }
  el('settings').addEventListener('click',presentMicSettings);
  if(el('settings-dialog').open)presentMicSettings();
  el('settings-device-details').addEventListener('toggle',()=>{
    if(el('settings-device-details').open && el('settings-dialog').open)detect('inspect');
  });
  el('redetect').onclick=event=>detect(el('redetect').dataset.intent,event.isTrusted);
  function publishWristMedia(){
    el('native-wrist').contentWindow?.postMessage({type:'lum-spotify-media',media:window.LumSpotifyMedia||{state:'unavailable'}},setupHost);
  }
  el('native-wrist').addEventListener('load',publishWristMedia);
  window.addEventListener('message',event=>{
    if(event.origin!==setupHost || event.source!==el('native-wrist').contentWindow)return;
    if(event.data?.type==='lum-wrist-ready')publishWristMedia();
    if(event.data?.type==='lum-wrist-spotify-action' && currentPlayer?.state==='ready' && state.linked){
      if(event.data.action==='toggle')el('personal-transport').click();
      if(event.data.action==='previous')el('media-previous').click();
      if(event.data.action==='next')el('media-next').click();
    }
  });

  // Hold one read-only notification request. A quiet session costs no artwork reads.
  let mediaWatch=null, nextWatch=0;
  function stopWatching(){
    clearTimeout(nextWatch);
    clearTimeout(artworkRetry);artworkRetry=0;
    mediaWatch?.abort();
    mediaWatch=null;
  }
  function startWatching(){
    stopWatching();
    if(document.hidden)return;
    const controller=new AbortController();
    mediaWatch=controller;
    let revision=null, lastRefresh=0;
    async function check(){
      let delay=0;
      try{
        if(currentPlayer?.state==='detected' || currentPlayer?.transportSource==='spotify_window'){
          if(await scanPlayer(true))lastRefresh=Date.now();
          revision=null;
          if(!controller.signal.aborted)nextWatch=setTimeout(check,3000);
          return;
        }
        const url=spotifyOrigin+'/spotify/changes'+(revision?'?since='+encodeURIComponent(revision):'');
        const signal=AbortSignal.any([controller.signal,AbortSignal.timeout(28500)]);
        const change=await query(url,{signal});
        if(controller.signal.aborted)return;
        if(!change.events || change.revision!==revision || Date.now()-lastRefresh>60000){
          if(await scanPlayer(true)){revision=change.revision;lastRefresh=Date.now();}
          else delay=650;
        }
        if(!change.events){revision=null;delay=5000;}
      }catch{
        if(controller.signal.aborted)return;
        await scanPlayer(true);
        revision=null;delay=5000;
      }
      if(!controller.signal.aborted)nextWatch=setTimeout(check,delay);
    }
    check();
  }
  document.addEventListener('visibilitychange',startWatching);
  window.addEventListener('pagehide',stopWatching);
  window.addEventListener('pageshow',event=>{if(event.persisted)startWatching();});
  startWatching();
  document.addEventListener('lum-view',()=>render());
  render();
})();
