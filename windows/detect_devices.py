"""Read-only Windows VR inventory for the Lum Mix loopback preview."""
from collections import Counter
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parent
STEAM = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Steam"
_lock = threading.Lock()
_last = None
_last_time = -100.0

# Return catalog labels only. Device/USB instance IDs, serials, user-supplied
# hardware labels and paths must never travel over the local HTTP interface.
MODELS = [
    (r"vive.*tracker.*3\.0", "VIVE Tracker 3.0"), (r"vive.*tracker.*2\.0", "VIVE Tracker 2.0"),
    (r"ultimate tracker", "VIVE Ultimate Tracker"), (r"vive.*tracker", "VIVE Tracker"),
    (r"tundra.*tracker", "Tundra Tracker"), (r"base station.*2|basestation.*2", "Base Station 2.0"),
    (r"base station|basestation", "Base Station"), (r"pico[ _-]*4[ _-]*ultra", "PICO 4 Ultra"),
    (r"pico[ _-]*4[ _-]*(enterprise|pro)", "PICO 4 Enterprise/Pro"), (r"pico[ _-]*4", "PICO 4"),
    (r"pico.*neo[ _-]*3", "PICO Neo 3"), (r"quest[ _-]*3s", "Meta Quest 3S"),
    (r"quest[ _-]*3", "Meta Quest 3"), (r"quest[ _-]*2", "Meta Quest 2"), (r"quest[ _-]*pro", "Meta Quest Pro"),
    (r"(?:oculus|meta)[ _-]*quest", "Meta Quest"), (r"rift[ _-]*s", "Oculus Rift S"),
    (r"oculus[ _-]*rift", "Oculus Rift"), (r"index.*controller|knuckles", "Valve Index Controller"),
    (r"valve[ _-]*index|index[ _-]*hmd", "Valve Index"), (r"vive[ _-]*pro[ _-]*2","VIVE Pro 2"),
    (r"vive[ _-]*pro", "VIVE Pro"), (r"vive.*focus", "VIVE Focus"), (r"vive.*cosmos", "VIVE Cosmos"),
    (r"vive[ _-]*controller", "VIVE Controller"), (r"vive[ _-]*hmd", "HTC VIVE"),
    (r"bigscreen[ _-]*beyond", "Bigscreen Beyond"), (r"reverb[ _-]*g2", "HP Reverb G2"),
    (r"varjo.*aero", "Varjo Aero"), (r"pimax.*crystal", "Pimax Crystal"),
]


def model(value):
    return next((label for regex,label in MODELS if re.search(regex,str(value),re.I)), None)


def counted(items):
    return [label if count == 1 else f"{label} ×{count}" for label,count in sorted(Counter(items).items())]


def windows_snapshot():
    # Only present, physically enumerated peripheral classes are candidates. The
    # physical bus filter is internal. Nothing that identifies a device leaves here.
    script = """
    $ErrorActionPreference='Stop'
    $procs=@(Get-Process | Select-Object -ExpandProperty ProcessName)
    $rows=@(Get-CimInstance Win32_PnPEntity -Filter 'Present=True' | Where-Object {
      $_.PNPDeviceID -match '^(USB|HID|BTHENUM|BTHLEDEVICE|DISPLAY)\\\\' -and
      $_.PNPClass -notin @('MEDIA','AudioEndpoint','SoftwareDevice') -and
      $_.Name -match 'PICO|Quest|Rift|Valve Index|Knuckles|VIVE|Tundra|base station|Bigscreen|Reverb|Varjo|Pimax'
    } | Select-Object -ExpandProperty Name)
    [pscustomobject]@{names=$rows;processes=$procs} | ConvertTo-Json -Compress
    """
    binary = Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"
    result = subprocess.run([str(binary), "-NoProfile", "-NonInteractive", "-Command", script],
                            capture_output=True, text=True, timeout=7, check=True, creationflags=subprocess.CREATE_NO_WINDOW)
    return json.loads(result.stdout)


def audio_endpoints():
    from pycaw.pycaw import AudioUtilities
    import comtypes
    virtual = [(r"pico\s*streaming\s*microphone","PICO Streaming Microphone"),
               (r"pico\s*streaming\s*speaker","PICO Streaming Speaker"),
               (r"steam streaming microphone","Steam Streaming Microphone"),
               (r"steam streaming speakers","Steam Streaming Speakers"),
               (r"oculus virtual audio","Oculus Virtual Audio"), (r"virtual desktop audio","Virtual Desktop Audio"),
               (r"alvr","ALVR Audio"), (r"vive virtual audio","VIVE Virtual Audio"),
               (r"vb-audio virtual cable|cable (?:input|output)","VB-Audio Virtual Cable"),
               (r"voicemeeter", "VoiceMeeter")]
    found=set()
    comtypes.CoInitialize()
    try:
        for flow,direction in [(0,"output"),(1,"input")]:
            for device in AudioUtilities.GetAllDevices(data_flow=flow,device_state=1):
                label=next((name for pattern,name in virtual if re.search(pattern,device.FriendlyName or "",re.I)),None)
                if label:
                    found.add(f"{label} · {direction}")
    finally:
        comtypes.CoUninitialize()
    return sorted(found)


def saved_profiles():
    config = STEAM / "config"
    saved = []
    if (config/"steamvr.vrsettings").is_file():
        profile=json.loads((config/"steamvr.vrsettings").read_bytes()).get("LastKnown",{})
        label=model(profile.get("HMDModel",""))
        if label:
            saved.append(label)
    for filename in (config/"lighthouse").glob("*/config.json"):
        item=json.loads(filename.read_bytes())
        label=model(item.get("model_number",""))
        if label and item.get("device_class") in ("hmd","controller","generic_tracker","tracking_reference"):
            saved.append(label)
    return counted(saved)


def scan():
    out={k:[] for k in ("connected","runtimes","audio","saved","warnings")}
    try:
        snap=windows_snapshot()
        processes={p.lower() for p in snap["processes"]}
        runtime_procs={"vrserver":"SteamVR", "ovrserver_x64":"Meta Quest Link runtime",
                       "virtualdesktop.streamer":"Virtual Desktop Streamer", "picoconnect":"PICO Connect",
                       "streamingassistant":"PICO Streaming Assistant", "piserver":"Pimax runtime",
                       "vrcompositor":"SteamVR compositor"}
        out["runtimes"]=sorted({label for process,label in runtime_procs.items() if process in processes})
        names = {label for value in snap["names"] if (label:=model(value)) and
                 not re.search(r"audio|speaker|microphone|radio|dongle|driver|bootloader",value,re.I)}
        out["connected"]=sorted(names)
        if "vrserver" in processes:
            # Background application mode cannot start SteamVR. An isolated helper
            # bounds runtime/driver response time and releases the interface on exit.
            try:
                result=subprocess.run([sys.executable,str(ROOT/"detect_openvr.py")],capture_output=True,text=True,
                                      timeout=5,check=True,creationflags=subprocess.CREATE_NO_WINDOW)
                tracked=json.loads(result.stdout)
                out["connected"]=counted(tracked)+sorted(names-set(tracked))
            except (OSError,ValueError,subprocess.SubprocessError):
                out["warnings"].append("SteamVR tracked-device status unavailable.")
    except (OSError,ValueError,subprocess.SubprocessError):
        out["warnings"].append("Windows hardware and runtime status unavailable.")
    try:
        out["audio"]=audio_endpoints()
    except Exception:
        out["warnings"].append("Windows audio endpoint status unavailable.")
    try:
        out["saved"]=saved_profiles()
    except (OSError,ValueError):
        out["warnings"].append("Saved SteamVR profiles unavailable.")
    out["scannedAt"]=datetime.now(timezone.utc).isoformat(timespec="seconds")
    return out


def get_scan():
    global _last,_last_time
    with _lock:
        if _last is None or time.monotonic()-_last_time>10:
            _last=scan()
            _last_time=time.monotonic()
        return _last


if __name__=="__main__":
    print(json.dumps(get_scan(),ensure_ascii=True))
