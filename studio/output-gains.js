/* Desktop and wrist faders share the native Lummix output gain. */
(() => {
  const url='http://127.0.0.1:41838/mix/gains';
  let token='', pending={}, sending=false, reading=false, timer=0, generation=0;
  const feedback=document.getElementById('gain-feedback');
  function draw(values){
    if(Number.isFinite(values.micGain))get('mic').value=values.micGain;
    if(Number.isFinite(values.musicGain))get('music').value=values.musicGain;
    if(typeof values.micMuted==='boolean')state.micMuted=values.micMuted;
    if(typeof values.musicMuted==='boolean')state.spotifyMuted=values.musicMuted;
    render();
  }
  async function request(patch){
    const options={cache:'no-store',credentials:'omit',signal:AbortSignal.timeout(5000)};
    if(patch)Object.assign(options,{method:'POST',headers:{'Content-Type':'application/json','X-Lum-Mix-Token':token},body:JSON.stringify(patch)});
    const response=await fetch(url,options);
    if(!response.ok)throw new Error(String(response.status));
    return response.json();
  }
  function apply(response){
    if(typeof response.token==='string')token=response.token;
    if(feedback)feedback.textContent='';
    draw({...response,...pending});
  }
  function retry(){
    if(feedback)feedback.textContent=Object.keys(pending).length?'Output host offline · changes pending':'Output host unavailable';
    token='';clearTimeout(timer);timer=setTimeout(refresh,3000);
  }
  async function refresh(){
    if(sending || reading)return;
    reading=true;
    const seq=generation;
    try{
      const result=await request();
      if(seq===generation)apply(result);
      else if(!token)token=result.token;
    }catch{retry();return;}
    finally{reading=false;}
    if(Object.keys(pending).length)flush();
  }
  async function flush(){
    if(sending || !Object.keys(pending).length)return;
    if(!token){refresh();return;}
    sending=true;generation++;
    try{
      while(Object.keys(pending).length){
        const patch=pending;pending={};
        try{apply(await request(patch));}
        catch{pending={...patch,...pending};retry();return;}
      }
    }finally{sending=false;}
  }
  function set(patch){
    pending={...pending,...patch};generation++;
    draw(patch);
    clearTimeout(timer);timer=setTimeout(flush,45);
  }
  for(const [id,key] of [['mic','micGain'],['music','musicGain']]){
    get(id).addEventListener('input',()=>set({[key]:Number(get(id).value)}));
    get(id).addEventListener('change',flush);
  }
  get('mic-mute').addEventListener('click',()=>{if(usable()){set({micMuted:!state.micMuted});cue();}});
  get('personal-volume').addEventListener('click',()=>{if(usable()){set({musicMuted:!state.spotifyMuted});cue();}});
  window.LumOutputGains={set};
  refresh();
  setInterval(()=>{if(!document.hidden)refresh();},4000);
  document.addEventListener('visibilitychange',()=>{if(!document.hidden)refresh();});
})();
