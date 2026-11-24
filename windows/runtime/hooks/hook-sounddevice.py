"""Bundle the x64 WASAPI-capable PortAudio variant used by Lummix."""
from importlib.metadata import distribution

folder = "_sounddevice_data/portaudio-binaries"
wheel = distribution("sounddevice")
binaries = [(str(wheel.locate_file(folder + "/libportaudio64bit.dll")), folder)]
datas = [(str(wheel.locate_file(folder + "/README.md")), folder)]
