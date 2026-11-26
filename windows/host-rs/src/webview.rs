use std::cell::{Cell, RefCell};
use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::sync::OnceLock;

use webview2_com::{
    take_pwstr, AcceleratorKeyPressedEventHandler, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, Microsoft::Web::WebView2::Win32::*,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler,
};
use windows::core::{s, w, Interface, BOOL, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, HMODULE, HWND, RECT, S_OK};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::{Shell::SHCreateMemStream, WindowsAndMessaging::GetClientRect};

use crate::studio_files;

const ORIGIN: &str = "http://127.0.0.1:41835/";
const ENTRY: PCWSTR = w!("http://127.0.0.1:41835/?view=desktop&app=1");
const READY: u32 = 0;
const ERROR: u32 = 1;
const MESSAGE: u32 = 2;
const NEW_PAGE: u32 = 3;
const KEY: u32 = 4;

type Event = extern "C" fn(u32, *const u16, *const u16, u32) -> i32;
type CreateEnvironment =
    unsafe extern "system" fn(PCWSTR, PCWSTR, *mut c_void, *mut c_void) -> HRESULT;

struct Apartment;

impl Apartment {
    unsafe fn enter() -> Result<Self, HRESULT> {
        let result = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if result.is_err() {
            return Err(result);
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[derive(Clone)]
struct Browser {
    controller: ICoreWebView2Controller,
    web: ICoreWebView2,
}

struct View {
    window: HWND,
    assets: PathBuf,
    callback: Event,
    closing: Cell<bool>,
    browser: RefCell<Option<Browser>>,
    _apartment: Apartment,
}

impl View {
    fn send(&self, event: u32, source: Option<&str>, data: Option<&str>, detail: u32) -> bool {
        if self.closing.get() {
            return false;
        }
        let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<_>>();
        let source = source.map(wide);
        let data = data.map(wide);
        (self.callback)(
            event,
            source.as_ref().map_or(null(), Vec::as_ptr),
            data.as_ref().map_or(null(), Vec::as_ptr),
            detail,
        ) != 0
    }

    fn fail(&self, message: &str) {
        self.send(ERROR, None, Some(message), 0);
    }

    unsafe fn configure(
        self: &Rc<Self>,
        environment: &ICoreWebView2Environment,
        controller: ICoreWebView2Controller,
    ) -> windows::core::Result<()> {
        let web = controller.CoreWebView2()?;
        let settings = web.Settings()?;
        settings.SetAreHostObjectsAllowed(false)?;
        if let Ok(settings) = settings.cast::<ICoreWebView2Settings9>() {
            let _ = settings.SetIsNonClientRegionSupportEnabled(true);
        }
        self.install_assets(environment, &web)?;

        let navigation = NavigationStartingEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                let uri = property(|out| args.Uri(out))?;
                if !uri.starts_with(ORIGIN) {
                    args.SetCancel(true)?;
                }
            }
            Ok(())
        }));
        web.add_NavigationStarting(&navigation, &mut 0)?;

        let view = Rc::downgrade(self);
        let popup = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                args.SetHandled(true)?;
                if let Some(view) = view.upgrade() {
                    let mut user_initiated = BOOL::default();
                    args.IsUserInitiated(&mut user_initiated)?;
                    if user_initiated.as_bool() {
                        let uri = property(|out| args.Uri(out))?;
                        view.send(NEW_PAGE, None, Some(&uri), 0);
                    }
                }
            }
            Ok(())
        }));
        web.add_NewWindowRequested(&popup, &mut 0)?;

        let view = Rc::downgrade(self);
        let message = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            if let (Some(args), Some(view)) = (args, view.upgrade()) {
                let source = property(|out| args.Source(out))?;
                if source.starts_with(ORIGIN) {
                    if let Ok(command) = property(|out| args.TryGetWebMessageAsString(out)) {
                        view.send(MESSAGE, Some(&source), Some(&command), 0);
                    }
                }
            }
            Ok(())
        }));
        web.add_WebMessageReceived(&message, &mut 0)?;

        let view = Rc::downgrade(self);
        let accelerator = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            if let (Some(args), Some(view)) = (args, view.upgrade()) {
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                args.KeyEventKind(&mut kind)?;
                if matches!(
                    kind,
                    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        | COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                ) {
                    let mut key = 0;
                    args.VirtualKey(&mut key)?;
                    if matches!(key, 0x7a | 0x1b) {
                        let mut status = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
                        args.PhysicalKeyStatus(&mut status)?;
                        let detail = key | (u32::from(status.WasKeyDown.as_bool()) << 16);
                        if view.send(KEY, None, None, detail) {
                            args.SetHandled(true)?;
                        }
                    }
                }
            }
            Ok(())
        }));
        controller.add_AcceleratorKeyPressed(&accelerator, &mut 0)?;
        let mut bounds = RECT::default();
        GetClientRect(self.window, &mut bounds)?;
        controller.SetBounds(bounds)?;
        *self.browser.borrow_mut() = Some(Browser {
            controller,
            web: web.clone(),
        });
        web.Navigate(ENTRY)?;
        self.send(READY, None, None, 0);
        Ok(())
    }

    unsafe fn install_assets(
        &self,
        environment: &ICoreWebView2Environment,
        web: &ICoreWebView2,
    ) -> windows::core::Result<()> {
        let pattern = w!("http://127.0.0.1:41835/*");
        if let Ok(recent) = web.cast::<ICoreWebView2_22>() {
            recent.AddWebResourceRequestedFilterWithRequestSourceKinds(
                pattern,
                COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
            )?;
        } else {
            web.AddWebResourceRequestedFilter(pattern, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        }
        let environment = environment.clone();
        let folder = self.assets.clone();
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else { return Ok(()) };
            let request = args.Request()?;
            let uri = property(|out| request.Uri(out))?;
            let Some(path) = uri.strip_prefix(ORIGIN) else {
                return Ok(());
            };
            if property(|out| request.Method(out))? != "GET" {
                return Ok(());
            }
            let route = format!("/{path}");
            let Some((name, mime)) = studio_files::asset(&route) else {
                return Ok(());
            };
            let (data, mime, status, reason) = match std::fs::read(folder.join(name)) {
                Ok(data) => (data, mime, 200, w!("OK")),
                Err(_) => (
                    b"Missing studio asset".to_vec(),
                    "text/plain",
                    404,
                    w!("Not Found"),
                ),
            };
            let charset = if mime.starts_with("text/") {
                "; charset=utf-8"
            } else {
                ""
            };
            let headers: Vec<u16> =
                format!("Content-Type: {mime}{charset}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff")
                    .encode_utf16().chain([0]).collect();
            let stream = SHCreateMemStream(Some(&data))
                .ok_or_else(|| windows::core::Error::from_hresult(E_FAIL))?;
            let response = environment.CreateWebResourceResponse(
                &stream,
                status,
                reason,
                PCWSTR(headers.as_ptr()),
            )?;
            args.SetResponse(&response)?;
            Ok(())
        }));
        web.add_WebResourceRequested(&handler, &mut 0)?;
        Ok(())
    }
}

fn property(
    getter: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>,
) -> windows::core::Result<String> {
    let mut value = PWSTR::null();
    getter(&mut value)?;
    Ok(take_pwstr(value))
}

unsafe fn loader(path: PCWSTR) -> windows::core::Result<CreateEnvironment> {
    // Keep the signed, installation-supplied loader resident across async completions.
    static LIBRARY: OnceLock<usize> = OnceLock::new();
    let module = match LIBRARY.get() {
        Some(handle) => HMODULE(*handle as *mut c_void),
        None => {
            let module = LoadLibraryExW(
                path,
                None,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )?;
            let _ = LIBRARY.set(module.0 as usize);
            module
        }
    };
    let export = GetProcAddress(module, s!("CreateCoreWebView2EnvironmentWithOptions"))
        .ok_or_else(windows::core::Error::from_thread)?;
    Ok(std::mem::transmute(export))
}

unsafe fn begin(
    view: &Rc<View>,
    loader_path: PCWSTR,
    profile: PCWSTR,
) -> windows::core::Result<()> {
    let create_environment = loader(loader_path)?;
    let weak_view = Rc::downgrade(view);
    let handler =
        CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |status, env| {
            let Some(view) = weak_view.upgrade().filter(|view| !view.closing.get()) else {
                return Ok(());
            };
            let Some(environment) = env.filter(|_| status.is_ok()) else {
                view.fail("Microsoft Edge WebView2 Runtime could not initialize.");
                return Ok(());
            };
            let weak_view = Rc::downgrade(&view);
            let controller_environment = environment.clone();
            let controller = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                move |status, controller| {
                    let Some(view) = weak_view.upgrade().filter(|view| !view.closing.get()) else {
                        if let Some(controller) = controller {
                            let _ = controller.Close();
                        }
                        return Ok(());
                    };
                    let Some(controller) = controller.filter(|_| status.is_ok()) else {
                        view.fail("Microsoft Edge WebView2 could not open the studio.");
                        return Ok(());
                    };
                    if let Err(error) = view.configure(&controller_environment, controller.clone())
                    {
                        let _ = controller.Close();
                        view.fail(&format!(
                            "Microsoft Edge WebView2 could not open the studio. {error}"
                        ));
                    }
                    Ok(())
                },
            ));
            if environment
                .CreateCoreWebView2Controller(view.window, &controller)
                .is_err()
            {
                view.fail("Microsoft Edge WebView2 could not open the studio.");
            }
            Ok(())
        }));
    create_environment(PCWSTR::null(), profile, null_mut(), handler.as_raw()).ok()
}

unsafe fn browser(token: *const c_void) -> Option<Browser> {
    token.cast::<Rc<View>>().as_ref()?.browser.borrow().clone()
}

fn result(result: windows::core::Result<()>) -> i32 {
    result.map_or_else(|error| error.code(), |()| S_OK).0
}

/// The caller provides valid wide strings and a synchronous callback on its UI thread.
/// Retain the callback until the token has been closed on the same thread.
#[no_mangle]
pub unsafe extern "C" fn lummix_webview_start(
    hwnd: HWND,
    loader_path: *const u16,
    profile: *const u16,
    assets: *const u16,
    callback: Option<Event>,
    token: *mut *mut c_void,
) -> i32 {
    if hwnd.0.is_null()
        || loader_path.is_null()
        || profile.is_null()
        || assets.is_null()
        || token.is_null()
    {
        return E_INVALIDARG.0;
    }
    let Some(callback) = callback else {
        return E_INVALIDARG.0;
    };
    *token = null_mut();
    let folder = PathBuf::from(OsString::from_wide(PCWSTR(assets).as_wide()));
    let apartment = match Apartment::enter() {
        Ok(apartment) => apartment,
        Err(error) => return error.0,
    };
    let view = Rc::new(View {
        window: hwnd,
        assets: folder,
        callback,
        closing: Cell::new(false),
        browser: RefCell::new(None),
        _apartment: apartment,
    });
    let status = result(begin(&view, PCWSTR(loader_path), PCWSTR(profile)));
    if status >= 0 {
        *token = Box::into_raw(Box::new(view)).cast();
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn lummix_webview_close(token: *mut c_void) {
    if !token.is_null() {
        let view = Box::from_raw(token.cast::<Rc<View>>());
        view.closing.set(true);
        let browser = view.browser.borrow_mut().take();
        if let Some(browser) = browser {
            let _ = browser.controller.Close();
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lummix_webview_resize(
    token: *const c_void,
    width: i32,
    height: i32,
) -> i32 {
    browser(token).map_or(E_FAIL.0, |browser| {
        result(browser.controller.SetBounds(RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }))
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_webview_visible(token: *const c_void, visible: i32) -> i32 {
    browser(token).map_or(E_FAIL.0, |browser| {
        result(browser.controller.SetIsVisible(visible != 0))
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_webview_moved(token: *const c_void) -> i32 {
    browser(token).map_or(E_FAIL.0, |browser| {
        result(browser.controller.NotifyParentWindowPositionChanged())
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_webview_post_json(token: *const c_void, json: *const u16) -> i32 {
    browser(token).map_or(E_FAIL.0, |browser| {
        result(browser.web.PostWebMessageAsJson(PCWSTR(json)))
    })
}

#[no_mangle]
pub unsafe extern "C" fn lummix_webview_execute(token: *const c_void, script: *const u16) -> i32 {
    browser(token).map_or(E_FAIL.0, |browser| {
        result(browser.web.ExecuteScript(PCWSTR(script), None))
    })
}

/// Return as many UTF-16 units as are required, including a NUL terminator.
#[no_mangle]
pub unsafe extern "C" fn lummix_webview_source(
    token: *const c_void,
    out: *mut u16,
    len: usize,
) -> usize {
    let Some(browser) = browser(token) else {
        return 0;
    };
    let Ok(uri) = property(|value| browser.web.Source(value)) else {
        return 0;
    };
    let required = uri.encode_utf16().count() + 1;
    if !out.is_null() && len >= required {
        for (i, unit) in uri.encode_utf16().chain([0]).enumerate() {
            *out.add(i) = unit;
        }
    }
    required
}
