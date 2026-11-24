/* The native mixer ducks only music actually present on its own output bus. */
(() => {
  'use strict';
  const find=id=>document.getElementById(id);
  const toggle=find('duck-enable'), depth=find('duck'), alternate=find('duck-live');
  const endpoint='http://127.0.0.1:41838/mix/duck';
  const captions={unavailable:'Native mixer unavailable.',off:'Off',waiting_mic:'Ready when a mic route starts.',
    waiting_music:'Waiting for music on the mixer.', mic_muted:'Microphone muted.',
    music_muted:'Mixer music muted.',ready:'Listening to the routed microphone.'};
  let current=null, pending={}, busy=false, retryTimer=0, dragging=false, migrating=false;
  const label=s=>s.state==='ducking'?'Speaking · −'+Math.round(s.reductionDb)+' dB':captions[s.state]||captions.unavailable;
  function draw(){
    const known=Boolean(current);
    const capture={state:current?.spotifyCapture||'unknown',edition:current?.spotifyEdition||null};
    if(window.LumSpotifyCapture?.state!==capture.state || window.LumSpotifyCapture?.edition!==capture.edition){
      window.LumSpotifyCapture=capture;
      window.dispatchEvent(new CustomEvent('lum-mix-capture',{detail:capture}));
    }
    toggle.disabled=depth.disabled=!known;
    if(alternate)alternate.disabled=!known;
    state.duckEnabled=pending.enabled??current?.enabled??false;
    state.liveDuck=Boolean(current && ['ducking','ready'].includes(current.state));
    state.duckNote=known?label(current):captions.unavailable;
    if(known && !dragging && pending.depthDb===undefined)depth.value=String(current.depthDb);
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
  function migrate(){
    if(!current || current.configured || migrating)return;
    migrating=true;
    try{
      const legacy=Number(localStorage.getItem('lum-talk-depth'));
      if(Number.isInteger(legacy)&&legacy>=1&&legacy<=24)
        pending=Object.assign({depthDb:legacy,migrate:true},pending);
    }catch{}
  }
  async function sync(){
    if(busy)return;
    clearTimeout(retryTimer);
    busy=true;
    try{
      if(!current || !Object.keys(pending).length)current=await call();
      migrate();
      while(Object.keys(pending).length){
        const patch=pending;
        pending={};
        try{
          current=await call(patch,current.token);
        }catch(e){
          if(e.message!=='403')throw e;
          current=await call();
          current=await call(patch,current.token);
        }
      }
    }catch{current=null;}
    finally{
      busy=false;
      draw();
      if(!document.hidden)
        retryTimer=setTimeout(sync,current?.state==='ducking'||current?.state==='ready'?500:2500);
    }
  }
  function change(patch){
    delete pending.migrate;
    Object.assign(pending,patch);draw();sync();
  }
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
    if(!document.hidden)sync();
  });
  sync();
})();
