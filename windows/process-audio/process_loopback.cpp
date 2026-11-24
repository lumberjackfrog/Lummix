// Public WASAPI application-loopback API, Windows build 20348 or newer.
// Captures a specific live process tree into memory. No endpoint loopback path.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <wrl.h>
#include <wrl/implements.h>
#include <cstdint>
using namespace Microsoft::WRL;

namespace {
struct Handle {
    HANDLE value;
    explicit Handle(HANDLE h) : value(h) {}
    ~Handle() { if (value) CloseHandle(value); }
    Handle(const Handle&) = delete;
};
class Activation final : public RuntimeClass<RuntimeClassFlags<ClassicCom>, IActivateAudioInterfaceCompletionHandler, FtmBase> {
public:
    Handle done{CreateEventW(nullptr, TRUE, FALSE, nullptr)};
    HRESULT result = E_PENDING;
    ComPtr<IAudioClient> client;
    STDMETHOD(ActivateCompleted)(IActivateAudioInterfaceAsyncOperation* operation) override {
        HRESULT activation = E_FAIL;
        ComPtr<IUnknown> object;
        result = operation->GetActivateResult(&activation, &object);
        if (SUCCEEDED(result)) result = activation;
        if (SUCCEEDED(result)) result = object.As(&client);
        SetEvent(done.value);
        return S_OK;
    }
};
using Samples = void (__stdcall *)(const int16_t*, UINT32, DWORD);
HRESULT capture(HANDLE process, HANDLE stop, Samples callback) {
    const DWORD pid = GetProcessId(process);
    if (!pid || !callback || !stop) return E_INVALIDARG;
    // Binary layout of AUDIOCLIENT_ACTIVATION_PARAMS in audioclientactivationparams.h.
    // This also allows building with the Windows 10.0.19041 SDK.
    struct { DWORD type, target, includeTree; } params{1, pid, 0};
    static_assert(sizeof(params)==12);
    PROPVARIANT prop{};
    prop.vt = VT_BLOB;
    prop.blob.cbSize = sizeof(params);
    prop.blob.pBlobData = reinterpret_cast<BYTE*>(&params);
    auto activation = Make<Activation>();
    Handle audioEvent(CreateEventW(nullptr, FALSE, FALSE, nullptr));
    if (!activation || !activation->done.value || !audioEvent.value) return E_OUTOFMEMORY;
    ComPtr<IActivateAudioInterfaceAsyncOperation> op;
    HRESULT hr = ActivateAudioInterfaceAsync(L"VAD\\Process_Loopback", __uuidof(IAudioClient),
                                             &prop, activation.Get(), &op);
    if (FAILED(hr)) return hr;
    HANDLE events[3] = {stop, process, activation->done.value};
    DWORD wait = WaitForMultipleObjects(3, events, FALSE, 5000);
    if (wait == WAIT_OBJECT_0 || wait == WAIT_OBJECT_0+1) return S_FALSE;
    if (wait != WAIT_OBJECT_0+2) return HRESULT_FROM_WIN32(wait == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError());
    if (FAILED(activation->result)) return activation->result;
    auto client = activation->client;
    WAVEFORMATEX fmt{};
    fmt.wFormatTag=WAVE_FORMAT_PCM;
    fmt.nChannels=2; fmt.nSamplesPerSec=48000; fmt.wBitsPerSample=16;
    fmt.nBlockAlign=4; fmt.nAvgBytesPerSec=192000;
    hr=client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK |
                          AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                          0, 0, &fmt, nullptr);
    if (FAILED(hr)) return hr;
    ComPtr<IAudioCaptureClient> source;
    if (FAILED(hr=client->GetService(IID_PPV_ARGS(&source)))) return hr;
    if (FAILED(hr=client->SetEventHandle(audioEvent.value))) return hr;
    if (FAILED(hr=client->Start())) return hr;
    events[2]=audioEvent.value;
    callback(nullptr, 0, 0); // stream attached
    while (true) {
        wait=WaitForMultipleObjects(3, events, FALSE, INFINITE);
        if (wait==WAIT_OBJECT_0 || wait==WAIT_OBJECT_0+1) break;
        if (wait!=WAIT_OBJECT_0+2) {hr=HRESULT_FROM_WIN32(GetLastError());break;}
        UINT32 count=0;
        while (SUCCEEDED(hr=source->GetNextPacketSize(&count)) && count) {
            BYTE* data=nullptr; DWORD flags=0; UINT32 frames=0;
            if (FAILED(hr=source->GetBuffer(&data,&frames,&flags,nullptr,nullptr))) break;
            callback((flags&AUDCLNT_BUFFERFLAGS_SILENT)?nullptr:reinterpret_cast<const int16_t*>(data),frames,flags);
            if (FAILED(hr=source->ReleaseBuffer(frames))) break;
            if (WaitForSingleObject(stop,0)==WAIT_OBJECT_0) break;
        }
        if (FAILED(hr)) break;
    }
    client->Stop();
    return hr;
}
}
extern "C" __declspec(dllexport) HRESULT __stdcall LumCaptureProcess(HANDLE process,HANDLE stop,Samples callback) {
    HRESULT hr=CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (FAILED(hr)) return hr;
    hr=capture(process,stop,callback);
    CoUninitialize();
    return hr;
}
