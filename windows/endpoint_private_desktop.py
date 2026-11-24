from pathlib import Path
import os,ctypes,sys,tempfile,subprocess,uuid,time
from ctypes import wintypes
from ctypes import byref

class Startup(ctypes.Structure):
    _fields_=[("cb",wintypes.DWORD),("lpReserved",wintypes.LPWSTR),("lpDesktop",wintypes.LPWSTR),("lpTitle",wintypes.LPWSTR)]+[(n,wintypes.DWORD) for n in ("x","y","xsz","ysz","xcnt","ycnt","fill","flags")]+[("show",wintypes.WORD),("cb2",wintypes.WORD),("reserved2",ctypes.c_void_p)]+[(n,wintypes.HANDLE) for n in ("stdin","stdout","stderr")]
class Proc(ctypes.Structure):
    _fields_=[("process",wintypes.HANDLE),("thread",wintypes.HANDLE),("pid",wintypes.DWORD),("tid",wintypes.DWORD)]

def private_run(exe,args,timeout=18):
    user=ctypes.WinDLL("user32",use_last_error=True);k=ctypes.WinDLL("kernel32",use_last_error=True)
    user.CreateDesktopW.argtypes=[wintypes.LPCWSTR,wintypes.LPCWSTR,ctypes.c_void_p,wintypes.DWORD,wintypes.DWORD,ctypes.c_void_p]
    user.CreateDesktopW.restype=wintypes.HANDLE
    user.CloseDesktop.argtypes=[wintypes.HANDLE]
    k.CreateProcessW.argtypes=[wintypes.LPCWSTR,wintypes.LPWSTR,ctypes.c_void_p,ctypes.c_void_p,wintypes.BOOL,wintypes.DWORD,ctypes.c_void_p,wintypes.LPCWSTR,ctypes.POINTER(Startup),ctypes.POINTER(Proc)]
    k.CloseHandle.argtypes=[wintypes.HANDLE];k.WaitForSingleObject.argtypes=[wintypes.HANDLE,wintypes.DWORD];k.TerminateProcess.argtypes=[wintypes.HANDLE,wintypes.UINT]
    name="LummixAudioName_"+uuid.uuid4().hex
    desk=user.CreateDesktopW(name,None,None,0,0x000F01FF,None)
    if not desk:raise ctypes.WinError(ctypes.get_last_error())
    try:
        si=Startup();si.cb=ctypes.sizeof(si);si.lpDesktop='winsta0\\'+name
        pi=Proc()
        cmd=ctypes.create_unicode_buffer(subprocess.list2cmdline([str(exe),*map(str,args)]))
        k.GetExitCodeProcess.argtypes=[wintypes.HANDLE,ctypes.POINTER(wintypes.DWORD)]
        if not k.CreateProcessW(str(exe),cmd,None,None,False,0x08000000,None,None,byref(si),byref(pi)):
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            if k.WaitForSingleObject(pi.process,timeout*1000)!=0:
                k.TerminateProcess(pi.process,1)
                raise TimeoutError("Windows Sound did not respond")
            code=wintypes.DWORD()
            if not k.GetExitCodeProcess(pi.process,byref(code)):raise ctypes.WinError(ctypes.get_last_error())
            return code.value
        finally:
            k.CloseHandle(pi.thread);k.CloseHandle(pi.process)
    finally:user.CloseDesktop(desk)
