"""Capture one process tree using the Windows WASAPI application loopback interface."""
import ctypes as ct
from ctypes import wintypes as wt

import comtypes
from comtypes import COMObject, COMMETHOD, GUID, IUnknown
from pycaw.api.audioclient import IAudioClient
from pycaw.api.audioclient.depend import WAVEFORMATEX
import win32event


class ActivationParams(ct.Structure):
    _fields_ = [("activation_type", wt.DWORD), ("pid", wt.DWORD), ("loopback_mode", wt.DWORD)]


class Blob(ct.Structure):
    _fields_ = [("length", wt.ULONG), ("data", ct.c_void_p)]


class BlobVariant(ct.Structure):
    _fields_ = [("type", wt.USHORT), ("reserved", wt.USHORT * 3), ("blob", Blob)]


class IActivation(IUnknown):
    _iid_ = GUID("{72A22D78-CDE4-431D-B8CC-843A71199B6D}")
    _methods_ = [COMMETHOD([], ct.HRESULT, "GetActivateResult",
                          (["out"], ct.POINTER(ct.c_long), "status"),
                          (["out"], ct.POINTER(ct.POINTER(IUnknown)), "object"))]


class ICompletion(IUnknown):
    _iid_ = GUID("{41D949AB-9862-444A-80F6-C261334DA5EB}")
    _methods_ = [COMMETHOD([], ct.HRESULT, "ActivateCompleted",
                          (["in"], ct.POINTER(IActivation), "operation"))]


class IAgileObject(IUnknown):
    _iid_ = GUID("{94EA2B94-E9CC-49E0-C0FF-EE64CA8F5B90}")
    _methods_ = []


class Completion(COMObject):
    _com_interfaces_ = [ICompletion, IAgileObject]

    def __init__(self):
        super().__init__()
        self.done = win32event.CreateEvent(None, True, False, None)

    def ActivateCompleted(self, operation):
        # This handler shares only a thread-safe event with the capture worker.
        win32event.SetEvent(self.done)
        return 0


class IAudioCaptureClient(IUnknown):
    _iid_ = GUID("{C8ADBD64-E71E-48A0-A4DE-185C395CD317}")
    _methods_ = [
        COMMETHOD([], ct.HRESULT, "GetBuffer",
                  (["out"], ct.POINTER(ct.POINTER(ct.c_short)), "samples"),
                  (["out"], ct.POINTER(wt.UINT), "frames"),
                  (["out"], ct.POINTER(wt.DWORD), "flags"),
                  (["in"], ct.POINTER(ct.c_uint64), "device_position"),
                  (["in"], ct.POINTER(ct.c_uint64), "clock_position")),
        COMMETHOD([], ct.HRESULT, "ReleaseBuffer", (["in"], wt.UINT, "frames")),
        COMMETHOD([], ct.HRESULT, "GetNextPacketSize", (["out"], ct.POINTER(wt.UINT), "frames")),
    ]


def capture(process, stop, on_packet):
    import win32process

    activate = ct.OleDLL("Mmdevapi").ActivateAudioInterfaceAsync
    activate.argtypes = [wt.LPCWSTR, ct.POINTER(GUID), ct.POINTER(BlobVariant),
                         ct.POINTER(ICompletion), ct.POINTER(ct.POINTER(IActivation))]
    activate.restype = ct.HRESULT
    # AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE.
    params = ActivationParams(1, win32process.GetProcessId(process), 0)
    variant = BlobVariant(type=65, blob=Blob(ct.sizeof(params), ct.addressof(params)))  # VT_BLOB
    done = Completion()
    operation = ct.POINTER(IActivation)()
    activate(r"VAD\Process_Loopback", ct.byref(IAudioClient._iid_), ct.byref(variant),
             done, ct.byref(operation))
    wake = win32event.WaitForMultipleObjects([stop, process, done.done], False, 5000)
    if wake == win32event.WAIT_TIMEOUT:
        raise TimeoutError("Windows did not activate Spotify capture")
    if wake != win32event.WAIT_OBJECT_0 + 2:
        return
    status, unknown = operation.GetActivateResult()
    if status < 0:
        raise OSError(f"Windows audio activation failed (0x{status & 0xffffffff:08x})")
    client = unknown.QueryInterface(IAudioClient)
    fmt = WAVEFORMATEX(1, 2, 48000, 192000, 4, 16, 0)
    # Shared mode, loopback, event callback and PCM conversion.
    client.Initialize(0, 0x00020000 | 0x00040000 | 0x80000000, 0, 0, ct.byref(fmt), None)
    source = client.GetService(ct.byref(IAudioCaptureClient._iid_)).QueryInterface(IAudioCaptureClient)
    audio = win32event.CreateEvent(None, False, False, None)
    client.SetEventHandle(int(audio))
    client.Start()
    try:
        on_packet(None, 0, 0)
        while win32event.WaitForMultipleObjects([stop, process, audio], False, win32event.INFINITE) == win32event.WAIT_OBJECT_0 + 2:
            while source.GetNextPacketSize() and win32event.WaitForSingleObject(stop, 0) == win32event.WAIT_TIMEOUT:
                samples, frames, flags = source.GetBuffer(None, None)
                try:
                    on_packet(None if flags & 2 else samples, frames, flags)
                finally:
                    source.ReleaseBuffer(frames)
    finally:
        client.Stop()


def capture_process(process, stop, on_packet):
    comtypes.CoInitializeEx(comtypes.COINIT_MULTITHREADED)
    try:
        capture(process, stop, on_packet)
    finally:
        comtypes.CoUninitialize()
