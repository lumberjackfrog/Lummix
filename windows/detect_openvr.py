"""Query an already running SteamVR session without creating an overlay or scene.

Function slots correspond to Valve's versioned IVRSystem_022 C table, openvr
v1.23.7 headers/openvr_capi.h. Only the public model property (1001) is read.
"""
import ctypes as c
import json
import sys
from detect_devices import STEAM, model


def devices():
    if c.sizeof(c.c_void_p)!=8:
        raise RuntimeError("64-bit helper required")
    api=c.WinDLL(str(STEAM/"steamapps/common/SteamVR/bin/win64/openvr_api.dll"))
    init=api.VR_InitInternal
    init.argtypes=[c.POINTER(c.c_int),c.c_int];init.restype=c.c_uint32
    get=api.VR_GetGenericInterface
    get.argtypes=[c.c_char_p,c.POINTER(c.c_int)];get.restype=c.c_void_p
    error=c.c_int()
    init(c.byref(error),3)  # VRApplication_Background never starts a missing server.
    if error.value:
        raise RuntimeError("No running compatible server")
    try:
        table=get(b"FnTable:IVRSystem_022",c.byref(error))
        if not table or error.value:
            raise RuntimeError("Interface unavailable")
        entries=c.cast(table,c.POINTER(c.c_void_p))
        category=c.WINFUNCTYPE(c.c_int,c.c_uint32)(entries[19])
        connected=c.WINFUNCTYPE(c.c_bool,c.c_uint32)(entries[20])
        prop=c.WINFUNCTYPE(c.c_uint32,c.c_uint32,c.c_int,c.c_void_p,c.c_uint32,c.POINTER(c.c_int))(entries[27])

        def string_property(idx,key):
            buf=c.create_string_buffer(256);e=c.c_int()
            prop(idx,key,buf,len(buf),c.byref(e))
            return buf.value.decode("utf8","replace") if not e.value else ""

        found=[]
        classes={1:"SteamVR headset",2:"SteamVR controller",3:"SteamVR tracker",4:"SteamVR base station"}
        for idx in range(64):
            if not connected(idx):
                continue
            kind=category(idx)
            if kind not in classes:
                continue
            actual=string_property(idx,1001)
            tracking=string_property(idx,1000).lower()  # Prop_TrackingSystemName_String
            if tracking in ("null","simulation","simulator") or any(x in actual.lower() for x in ("simulator","nullhmd","null driver")):
                continue
            found.append(model(actual) or classes[kind])
        return found
    finally:
        api.VR_ShutdownInternal()


if __name__=="__main__":
    try:
        print(json.dumps(devices()))
    except (OSError,RuntimeError):
        sys.exit(1)
