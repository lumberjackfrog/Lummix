"""Name only the signed VB-CABLE recording endpoint via Windows Sound Properties."""
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import subprocess
import sys
import time

ALIAS="Lummix"
DESC="{A45C254E-DF1C-4EFD-8020-67D146A850E0} 2"
ADAPTER="{B3F8FA53-0004-438E-9003-51A46E139BFC} 6"
HW="{A8B865DD-2E3D-4094-AD97-E593A70C75D6} 8"
ERRORS=("ok","name_unavailable","name_collision","name_not_applied","name_timed_out","name_busy")


class AliasError(Exception):
    pass


def endpoint(cable_ids):
    """Resolve the one active recording endpoint by signed ID, then verify its adapter independently."""
    import comtypes
    from pycaw.pycaw import AudioUtilities
    ids={i.casefold() for i in cable_ids.get("Capture",[]) if i.casefold().startswith("{0.0.1.")}
    if len(ids)!=1 or not cable_ids.get("Render"):raise AliasError("name_unavailable")
    comtypes.CoInitialize()
    try:
        entries=[(d.id.casefold(), d.FriendlyName or "", d.properties.get(DESC,""),
                  d.properties.get(ADAPTER,""),d.properties.get(HW,"")) for d in AudioUtilities.GetAllDevices(1,15)]
        hits=[r for r in entries if r[0] in ids and r[3]=="VB-Audio Virtual Cable" and r[4]=="VBAudioVACWDM"]
        if len(hits)!=1 or not hits[0][2]:raise AliasError("name_unavailable")
        row=hits[0]
        collision=any(other[0]!=row[0] and (str(other[2]).casefold()==ALIAS.casefold() or
                       str(other[1]).casefold()==ALIAS.casefold() or (other[2:4]==row[2:4]))
                      for other in entries)
        return dict(id=row[0],name=row[1],short=row[2],adapter=row[3],collision=collision)
    finally:
        comtypes.CoUninitialize()


def labels(cable_ids):
    try:
        item=endpoint(cable_ids)
        quality="unavailable" if item["collision"] else "ready" if item["short"]==ALIAS else "original"
        return dict(captureName=item["name"],captureAlias=quality)
    except (AliasError,ImportError,OSError):
        return dict(captureName=None,captureAlias="unavailable")


def current_desktop():
    k=ctypes.WinDLL("kernel32");u=ctypes.WinDLL("user32")
    u.GetThreadDesktop.argtypes=[wintypes.DWORD];u.GetThreadDesktop.restype=wintypes.HANDLE
    u.GetUserObjectInformationW.argtypes=[wintypes.HANDLE,ctypes.c_int,ctypes.c_void_p,wintypes.DWORD,ctypes.POINTER(wintypes.DWORD)]
    buf=ctypes.create_unicode_buffer(128);n=wintypes.DWORD()
    return buf.value if u.GetUserObjectInformationW(u.GetThreadDesktop(k.GetCurrentThreadId()),2,buf,ctypes.sizeof(buf),ctypes.byref(n)) else ""


def apply_in_private_desktop(ident):
    import comtypes.client as com
    from mic_routes import signed_cables
    if not current_desktop().startswith("LummixAudioName_"):raise AliasError("name_unavailable")
    target=endpoint(signed_cables())
    if target["id"]!=ident:raise AliasError("name_unavailable")
    if target["collision"]:raise AliasError("name_collision")
    if target["short"]==ALIAS:return
    mod=com.GetModule("UIAutomationCore.dll");uia=com.CreateObject(mod.CUIAutomation,interface=mod.IUIAutomation)
    u=ctypes.WinDLL("user32");procid=wintypes.DWORD()
    enum=ctypes.WINFUNCTYPE(wintypes.BOOL,wintypes.HWND,wintypes.LPARAM)
    u.EnumWindows.argtypes=[enum,wintypes.LPARAM]
    u.GetWindowThreadProcessId.argtypes=[wintypes.HWND,ctypes.POINTER(wintypes.DWORD)]
    u.GetClassNameW.argtypes=[wintypes.HWND,wintypes.LPWSTR,ctypes.c_int];u.IsWindowVisible.argtypes=[wintypes.HWND]
    u.PostMessageW.argtypes=[wintypes.HWND,wintypes.UINT,wintypes.WPARAM,wintypes.LPARAM]
    winapi=Path(os.environ["SystemRoot"])/"System32"
    process=subprocess.Popen([str(winapi/"rundll32.exe"),"shell32.dll,Control_RunDLL","mmsys.cpl,,1"],
                             cwd=winapi,creationflags=subprocess.CREATE_NO_WINDOW)
    def dialogs():
        found=[]
        @enum
        def visitor(h,_):
            u.GetWindowThreadProcessId(h,ctypes.byref(procid))
            if procid.value==process.pid and u.IsWindowVisible(h):
                kind=ctypes.create_unicode_buffer(64);u.GetClassNameW(h,kind,64)
                if kind.value=="#32770":found.append(h)
            return True
        u.EnumWindows(visitor,0)
        return found

    def wait(fn,seconds=4):
        until=time.monotonic()+seconds
        while time.monotonic()<until:
            result=fn()
            if result:return result
            time.sleep(.06)
        raise AliasError("name_timed_out")

    def elements(parent,prop,val,direct=False):
        scope=mod.TreeScope_Children if direct else mod.TreeScope_Descendants
        coll=parent.FindAll(scope,uia.CreatePropertyCondition(prop,val))
        return [coll.GetElement(i) for i in range(coll.Length)]
    def element(parent,aid,typ):
        hits=[e for e in elements(parent,mod.UIA_AutomationIdPropertyId,aid) if e.CurrentControlType==typ]
        if len(hits)!=1:raise AliasError("name_unavailable")
        return hits[0]
    def invoke(btn):
        if not btn.CurrentIsEnabled:raise AliasError("name_unavailable")
        btn.GetCurrentPattern(mod.UIA_InvokePatternId).QueryInterface(mod.IUIAutomationInvokePattern).Invoke()
    try:
        main=wait(lambda:next(iter(dialogs()),None))
        dlg=uia.ElementFromHandle(main)
        tabs=element(dlg,"12320",mod.UIA_TabControlTypeId)
        items=elements(tabs,mod.UIA_ControlTypePropertyId,mod.UIA_TabItemControlTypeId,True)
        if len(items)<2 or not items[1].GetCurrentPattern(mod.UIA_SelectionItemPatternId).QueryInterface(
                mod.IUIAutomationSelectionItemPattern).CurrentIsSelected:raise AliasError("name_unavailable")
        listing=element(dlg,"1000",mod.UIA_ListControlTypeId)
        matches=[]
        for row in elements(listing,mod.UIA_ControlTypePropertyId,mod.UIA_ListItemControlTypeId,True):
            if row.CurrentName==target["short"] and any(e.CurrentName==target["adapter"] for e in
                    elements(row,mod.UIA_ControlTypePropertyId,mod.UIA_TextControlTypeId,True)):matches.append(row)
        if len(matches)!=1:raise AliasError("name_collision" if matches else "name_unavailable")
        matches[0].GetCurrentPattern(mod.UIA_SelectionItemPatternId).QueryInterface(mod.IUIAutomationSelectionItemPattern).Select()
        invoke(element(dlg,"1003",mod.UIA_ButtonControlTypeId))
        prop=wait(lambda:next((w for w in dialogs() if w!=main),None))
        sheet=uia.ElementFromHandle(prop)
        adapter=element(sheet,"1207",mod.UIA_TextControlTypeId)
        edit=element(sheet,"1201",mod.UIA_EditControlTypeId).GetCurrentPattern(mod.UIA_ValuePatternId).QueryInterface(mod.IUIAutomationValuePattern)
        if edit.CurrentValue!=target["short"] or adapter.CurrentName!=target["adapter"] or edit.CurrentIsReadOnly:
            raise AliasError("name_unavailable")
        edit.SetValue(ALIAS)
        if edit.CurrentValue!=ALIAS:raise AliasError("name_not_applied")
        invoke(element(sheet,"1",mod.UIA_ButtonControlTypeId))
        wait(lambda:prop not in dialogs())
    finally:
        for w in dialogs():u.PostMessageW(w,0x10,0,0)
        try:process.wait(timeout=1.5)
        except subprocess.TimeoutExpired:
            process.terminate();process.wait(timeout=2)


def worker(ident):
    try:
        apply_in_private_desktop(ident);return 0
    except AliasError as exc:
        return ERRORS.index(str(exc))
    except Exception:
        return 1


def ensure(cable_ids=None):
    """Return Windows' actual post-action friendly name, even when the rename is unavailable."""
    from mic_routes import signed_cables
    from endpoint_private_desktop import private_run
    cable_ids=cable_ids if cable_ids is not None else signed_cables()
    before=None
    try:
        before=endpoint(cable_ids)
        if before["collision"]:raise AliasError("name_collision")
        if before["short"]==ALIAS: return dict(captureName=before["name"],captureAlias="ready",aliasError=None)
        if getattr(sys,"frozen",False):
            args=["name-cable-worker",before["id"]]
        else:
            args=["-B",str(Path(__file__).resolve()),"name-cable-worker",before["id"]]
        code=private_run(sys.executable,args,timeout=22)
        error=ERRORS[code] if 0<code<len(ERRORS) else "name_not_applied"
        until=time.monotonic()+(1 if not code else 0)
        while True:
            after=endpoint(cable_ids)
            if after["id"]==before["id"] and after["short"]==ALIAS:
                return dict(captureName=after["name"],captureAlias="ready",aliasError=None)
            if time.monotonic()>=until:raise AliasError(error)
            time.sleep(.1)
    except Exception as exc:
        code=str(exc) if isinstance(exc,AliasError) else "name_unavailable"
        return dict(captureName=before["name"] if before else None,captureAlias="original" if before and before["short"]!=ALIAS else "unavailable",aliasError=code)


if __name__=="__main__":
    if len(sys.argv)==3 and sys.argv[1]=="name-cable-worker":sys.exit(worker(sys.argv[2]))
    if sys.argv[1:]==["name-cable"]:
        import json
        result=ensure();print(json.dumps(result));sys.exit(0 if result["captureAlias"]=="ready" else 1)
    sys.exit(2)
