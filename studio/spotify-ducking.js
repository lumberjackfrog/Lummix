/* The native mixer ducks only music actually present on its own output bus. */
(async () => {
  'use strict';
  const find=id=>document.getElementById(id);
  const toggle=find('duck-enable'), depth=find('duck'), alternate=find('duck-live');
  const endpoint='http://127.0.0.1:41838/mix/duck';
  const captions={unavailable:'Native mixer unavailable.',off:'Off',waiting_mic:'Ready when a mic route starts.',
    waiting_music:'Waiting for music on the mixer.', mic_muted:'Microphone muted.',
    music_muted:'Mixer music muted.',ready:'Listening to the routed microphone.'};
  let current=null, token='', retryTimer=0, dragging=false;
  let duck;
  try{
    duck=(await window.LumControls).duck;
  }catch{
    toggle.disabled=depth.disabled=true;
    if(alternate)alternate.disabled=true;
    if(find('duck-live-state'))find('duck-live-state').textContent=captions.unavailable;
    return;
  }
  const label=s=>s.state==='ducking'?'Speaking · −'+Math.round(s.reductionDb)+' dB':captions[s.state]||captions.unavailable;
  function draw(){
    const known=duck.online();
    const capture={state:current?.spotifyCapture||'unknown',edition:current?.spotifyEdition||null};
    if(window.LumSpotifyCapture?.state!==capture.state || window.LumSpotifyCapture?.edition!==capture.edition){
      window.LumSpotifyCapture=capture;
      window.dispatchEvent(new CustomEvent('lum-mix-capture',{detail:capture}));
    }
    toggle.disabled=depth.disabled=!known;
    if(alternate)alternate.disabled=!known;
    state.duckEnabled=known && duck.enabled();
    state.liveDuck=Boolean(current && ['ducking','ready'].includes(current.state));
    state.duckNote=known?label(current):captions.unavailable;
    if(known && !dragging && duck.depth()>=0)depth.value=String(duck.depth());
    if(alternate){
      alternate.textContent=state.duckEnabled?'On':'Off';
      alternate.setAttribute('aria-pressed',String(state.duckEnabled));
    }
    if(find('duck-live-state'))find('duck-live-state').textContent=state.duckNote;
    render();
    if(known)find('duck-state').dataset.reduction=String(current.reductionDb);
  }
  async function call(body,token){
    const opts={cache:'no-store',credentials:'omit',signal:AbortSignal.timeout(4500)};
    if(body)Object.assign(opts,{method:'POST',headers:{'Content-Type':'application/json','X-Lum-Mix-Token':token},body:JSON.stringify(body)});
    const r=await fetch(endpoint,opts);
    if(!r.ok)throw Error(String(r.status));
    return r.json();
  }
  async function sync(poll = false){
    let patch=duck.next(Boolean(token),poll);
    if(patch===undefined)return;
    clearTimeout(retryTimer);
    let retriedToken=false;
    while(patch!==undefined){
      try{
        current=await call(patch,token);
        if(typeof current.token!=='string' || !current.token)throw Error('Missing mixer token');
        token=current.token;
        duck.commit(current);
        try{duck.migrate(Number(localStorage.getItem('lum-talk-depth')),current.configured);}catch{}
        patch=duck.next(Boolean(token),false);
      }catch(error){
        current=null;
        token='';
        duck.fail();
        if(error.message!=='403'||retriedToken)break;
        retriedToken=true;
        patch=duck.next(false,true);
      }
    }
    draw();
    if(!document.hidden)retryTimer=setTimeout(()=>sync(true),duck.pollMs());
  }
  function change(patch){duck.change(patch);draw();sync();}
  function flip(){if(current)change({enabled:!state.duckEnabled});}
  toggle.addEventListener('click',flip);
  alternate?.addEventListener('click',flip);
  depth.addEventListener('input',()=>{dragging=true;});
  depth.addEventListener('change',()=>{
    dragging=false;
    if(current)change({depthDb:Number(depth.value)});
  });
  document.addEventListener('visibilitychange',()=>{
    clearTimeout(retryTimer);
    if(!document.hidden)sync(true);
  });
  sync(true);
})();
