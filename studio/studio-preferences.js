/* Share studio preferences across Windows browser profiles. Reading settings never opens audio. */
(() => {
  const url = 'http://127.0.0.1:41838/mix/preferences';
  const ids = {sounds:'sounds', followTheme:'follow-theme', dark:'dark', companion:'companion',
    globalPulse:'global-pulse', closeToTray:'close-to-tray', snap:'slider-snapping'};
  const keys = {sounds:'lum-pref-sounds', followTheme:'lum-pref-follow-theme', dark:'lum-pref-dark',
    companion:'lum-pref-companion', globalPulse:'lum-pref-global-pulse', snap:'lum-pref-slider-snap',
    background:'lum-idle-style', image:'lum-idle-image'};
  const el = id => document.getElementById(id);
  const report = text => {el('preferences-status').textContent=text; el('preferences-status').hidden=!text;};
  const controls = Object.entries(ids);
  const pendingKey='lum-pending-studio-preferences';
  let pending={};
  try {
    const old=JSON.parse(localStorage.getItem(pendingKey)||'{}');
    if(old && typeof old==='object' && !Array.isArray(old)) {
      pending=Object.fromEntries(Object.entries(old).filter(([key])=>key in keys || key==='closeToTray'));
    }
  } catch {}
  function rememberPending() {
    try {
      if(Object.keys(pending).length) localStorage.setItem(pendingKey,JSON.stringify(pending));
      else localStorage.removeItem(pendingKey);
    } catch {}
  }
  let token = '', queue = Promise.resolve();
  const select = (values,names) => Object.fromEntries(names.map(name=>[name,values[name]]));
  let engine;
  const loadEngine = window.LumControls.then(({preferences,appearance})=>{
    engine=preferences;
    engine.change(pending);
    return {prefs:engine,appearance};
  });
  loadEngine.catch(()=>{});
  const native = (() => {
    try { return new URL(top.location.href).searchParams.get('app')==='1' && top.chrome?.webview; }
    catch { return null; }
  })();
  if(native && typeof native.postMessage==='function' && new URL(top.location.href).searchParams.get('view')!=='wrist'){
    const toolbar=el('window-controls'), drag=el('window-drag');
    const startup=el('start-with-windows'), startupStatus=el('startup-status'), settings=el('settings-dialog');
    el('startup-row').hidden=false;
    el('update-row').hidden=el('app-diagnostics').hidden=false;
    const updateControl=el('app-update'), updateStatus=el('update-status');
    let updateAction='check';
    native.addEventListener('message',({data})=>{
      if(data?.type==='lummix-diagnostics'){
        el('logs-status').textContent=data.message;
        return;
      }
      if(data?.type!=='lummix-update') return;
      const choices={
        idle:['check','Check for updates',''], checking:['','Checking…','Checking GitHub.'],
        current:['check','Check for updates','You’re up to date.'],
        available:['download','Download '+data.version,'Version '+data.version+' is available.'],
        downloading:['','Downloading…','Downloading and verifying the installer.'],
        ready:['install','Install and reopen','Verified and ready to install.'],
        installing:['','Opening installer…',''], launched:['','Reopening…',''],
        error:['check','Check for updates','']
      };
      if(!Object.hasOwn(choices,data.phase)) return;
      const [action,label,description]=choices[data.phase];
      updateAction=action;
      updateControl.textContent=label;
      updateControl.disabled=!action;
      el('app-version').textContent='v'+data.current;
      updateStatus.textContent=data.message || description;
      updateStatus.hidden=!updateStatus.textContent;
      const available=['available','downloading','ready'].includes(data.phase);
      el('settings').toggleAttribute('data-update',available);
      el('settings').title=available?'Settings · update available':'Settings';
    });
    updateControl.addEventListener('click',()=>{
      if(updateAction) native.postMessage('update-'+updateAction);
    });
    for(const id of ['logs-open','logs-clear']) el(id).addEventListener('click',()=>native.postMessage(id));
    native.postMessage('update-state');
    native.addEventListener('message',({data})=>{
      if(data?.type!=='lummix-startup-state') return;
      startup.indeterminate=typeof data.enabled!=='boolean';
      startup.checked=data.enabled===true;
      startup.disabled=data.editable!==true;
      startupStatus.textContent=data.message || '';
      startupStatus.hidden=!data.message;
    });
    const readStartup=()=>{if(settings.open){native.postMessage('startup-state');native.postMessage('update-state');}};
    new MutationObserver(readStartup).observe(settings,{attributes:true,attributeFilter:['open']});
    top.addEventListener('focus',readStartup);
    startup.addEventListener('change',()=>{
      startup.disabled=true;
      native.postMessage(startup.checked?'startup-on':'startup-off');
    });
    document.documentElement.classList.add('native-window');
    toolbar.hidden=false;
    drag.hidden=false;
    for(const action of ['minimize','maximize','fullscreen','close']){
      el('window-'+action).addEventListener('click',()=>native.postMessage(action));
    }
    function updateButton(id,active,label){
      const button=el('window-'+id);
      button.dataset.restore=String(active);
      button.title=label;
      button.setAttribute('aria-label',label);
    }
    native.addEventListener?.('message',({data})=>{
      if(data?.type!=='lummix-window-state' || typeof data.maximized!=='boolean' || typeof data.fullscreen!=='boolean')return;
      updateButton('maximize',data.maximized,data.maximized?'Restore window':'Maximize window');
      updateButton('fullscreen',data.fullscreen,data.fullscreen?'Leave fullscreen':'Enter fullscreen');
      el('window-fullscreen').setAttribute('aria-pressed',String(data.fullscreen));
    });
    native.postMessage('window-state');
    // WebView2 handles draggable regions itself. Older runtimes deliver normal pointer events here.
    let down=null;
    drag.addEventListener('pointerdown',event=>{
      if(!event.isPrimary || event.button!==0)return;
      down={id:event.pointerId,x:event.clientX,y:event.clientY};
      drag.setPointerCapture(event.pointerId);
    });
    drag.addEventListener('pointermove',event=>{
      if(down?.id!==event.pointerId || Math.hypot(event.clientX-down.x,event.clientY-down.y)<4)return;
      down=null;
      drag.releasePointerCapture(event.pointerId);
      native.postMessage('drag');
    });
    for(const event of ['pointerup','pointercancel','lostpointercapture'])drag.addEventListener(event,()=>{down=null;});
    drag.addEventListener('dblclick',event=>{if(event.button===0)native.postMessage('maximize');});
  }
  let fullDocument;
  try { fullDocument=top.document; } catch { fullDocument=document; }
  const fullButton=el('app-fullscreen');
  el('fullscreen-row').hidden = !native && !fullDocument.fullscreenEnabled;
  el('tray-row').hidden = el('exit-row').hidden = !native;
  el('close-to-tray').disabled = true;
  function updateFullscreen() {
    if (!native) fullButton.textContent = fullDocument.fullscreenElement ? 'Leave fullscreen' : 'Enter fullscreen';
  }
  fullDocument.addEventListener('fullscreenchange',updateFullscreen);
  updateFullscreen();
  fullButton.addEventListener('click', async () => {
    try {
      if (native) native.postMessage('fullscreen');
      else if (fullDocument.fullscreenElement) await fullDocument.exitFullscreen();
      else await fullDocument.documentElement.requestFullscreen();
    } catch {report('This window could not enter fullscreen.');}
  });
  el('app-exit').addEventListener('click',()=>native?.postMessage('exit'));

  function legacy(appearance) {
    const values={};
    try {
      for(const [name,key] of Object.entries(keys)) {
        const raw=localStorage.getItem(key);
        if(raw===null) continue;
        const value=name==='snap'?appearance.snap(raw):
          name==='background'?(appearance.mode(raw)===undefined?undefined:raw):
          name==='image'?(appearance.image(raw,true)?raw:undefined):appearance.boolean(raw);
        if(value!==undefined)values[name]=value;
      }
      const previous=JSON.parse(localStorage.getItem('lum-mix-appearance')||'{}');
      if(typeof previous?.follow==='boolean') values.followTheme ??= previous.follow;
      if(typeof previous?.dark==='boolean') values.dark ??= previous.dark;
    } catch {}
    return values;
  }
  function restore(saved) {
    const values={...saved,...pending};
    for(const [name,id] of controls) {
      if(!(name in values)) continue;
      if(name==='snap') el(id).value=String(values[name]);
      else el(id).checked=values[name];
    }
    try {
      for(const [name,key] of Object.entries(keys)) {
        if(name in values && localStorage.getItem(key)!==String(values[name]))localStorage.setItem(key,String(values[name]));
      }
      localStorage.setItem('lum-mix-appearance',JSON.stringify({follow:el('follow-theme').checked,dark:el('dark').checked}));
    } catch {}
    document.dispatchEvent(new CustomEvent('lum-preferences-applied',{detail:values}));
    render(); sendWristState();
  }
  async function request(body) {
    const options={credentials:'omit',cache:'no-store',signal:AbortSignal.timeout(5500)};
    if(body){
      const payload=JSON.stringify(body);
      Object.assign(options,{method:'POST',headers:{'Content-Type':'application/json','X-Lum-Mix-Token':token},body:payload,keepalive:payload.length<4000});
    }
    const result=await fetch(url,options);
    if(result.status===403) token='';
    if(!result.ok) throw Error('settings unavailable');
    const data=await result.json();
    if(!data.settings || !data.token) throw Error('invalid settings');
    token=data.token;
    return data.settings;
  }
  async function sync() {
    try {
      const {prefs,appearance}=await loadEngine;
      let saved;
      if(prefs.needsRead(Boolean(token))) {
        saved=await request();
        const old=legacy(appearance);
        const migrate=prefs.migration(old,saved);
        if(migrate.length) saved=await request({settings:select(old,migrate),migrate:true});
        prefs.finishMigration();
      }
      const patch=select(pending,prefs.batch());
      if(Object.keys(patch).length) {
        try { saved=await request({settings:patch}); }
        catch(error) {
          if(token) throw error;
          await request();
          saved=await request({settings:patch});
        }
        for(const key of prefs.confirm()) delete pending[key];
        rememberPending();
      }
      restore(saved);
      el('close-to-tray').checked=pending.closeToTray ?? saved.closeToTray ?? true;
      el('close-to-tray').disabled=false;
      report('');
    } catch {
      el('close-to-tray').disabled=true;
      report('Windows preferences are unavailable. Changes in this window will sync when Lummix reconnects.');
    }
  }
  function schedule() {queue=queue.then(sync,sync); return queue;}
  function save(values) {Object.assign(pending,values); engine?.change(values); rememberPending(); schedule();}
  for(const [name,id] of controls) el(id).addEventListener('change',()=>{
    save({[name]:name==='snap'?Number(el(id).value):el(id).checked});
  });
  document.addEventListener('lum-background-chosen',event=>save(event.detail));
  el('settings').addEventListener('click',schedule);
  window.addEventListener('focus',schedule);
  schedule();
})();

/* XSOverlay installation belongs to the Windows wrist host. Opening Settings only checks its state. */
(() => {
  const endpoint='http://127.0.0.1:41839/lummix-addon';
  const el=id=>document.getElementById(id);
  const install=el('xs-addon-install'), remove=el('xs-addon-remove'), refresh=el('xs-addon-refresh');
  const state=el('xs-addon-state'), instruction=el('xs-addon-instruction');
  let current=null, busy=false;

  function display(snapshot) {
    current=snapshot;
    el('xs-addon-name').textContent=snapshot?.name||'Lummix wrist add-on';
    state.textContent=snapshot?.message||'The wrist helper could not check XSOverlay. Reopen Lummix or choose Check again.';
    install.hidden=!(snapshot?.canInstall || snapshot?.canUpdate);
    install.textContent=snapshot?.canUpdate?'Update add-on':'Install add-on';
    remove.hidden=!snapshot?.canRemove;
    instruction.textContent=snapshot?.state==='ready'
      ? 'Restart XSOverlay, then tap the music note on your wrist. You can also open Lummix from XSOverlay’s Apps menu.' : '';
    instruction.hidden=!instruction.textContent;
  }
  async function read() {
    const response=await fetch(endpoint,{cache:'no-store',credentials:'omit',signal:AbortSignal.timeout(6500)});
    if(!response.ok)throw Error();
    const snapshot=await response.json();
    if(!snapshot || typeof snapshot.state!=='string')throw Error();
    return snapshot;
  }
  async function run(action) {
    if(busy || (action && !current?.token))return;
    if(action==='install' && !(current.canInstall || current.canUpdate))return;
    if(action==='remove' && !current.canRemove)return;
    busy=true;
    for(const button of [refresh,install,remove])button.disabled=true;
    try{
      if(!action)display(await read());
      else{
        state.textContent=action==='remove'?'Removing add-on…':current.canUpdate?'Updating add-on…':'Installing add-on…';
        const response=await fetch(endpoint,{method:'POST',cache:'no-store',credentials:'omit',
          headers:{'Content-Type':'application/json','X-Lummix-Addon-Token':current.token},
          body:JSON.stringify({action}),signal:AbortSignal.timeout(30000)});
        const result=await response.json().catch(()=>null);
        if(!response.ok){
          display(await read());
          if(typeof result?.message==='string')state.textContent=result.message;
          else state.textContent='The change was not confirmed. Check the installation and try again.';
        }else display(await read());
      }
    }catch{
      display(null);
      if(action)state.textContent='Couldn’t confirm the change. Choose Check again to read the installed state.';
    }finally{
      busy=false;
      for(const button of [refresh,install,remove])button.disabled=false;
    }
  }
  refresh.addEventListener('click',()=>run());
  install.addEventListener('click',()=>run('install'));
  remove.addEventListener('click',()=>run('remove'));
  el('settings').addEventListener('click',()=>run());
  if(el('settings-dialog').open)run();
})();
