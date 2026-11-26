use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::core::{w, PCWSTR};
use windows_sys::Win32::Networking::WinHttp::*;

struct InternetHandle(*mut c_void);

impl InternetHandle {
    fn new(handle: *mut c_void) -> io::Result<Self> {
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        // WinHTTP created and transferred ownership of this handle.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

fn checked(success: i32) -> io::Result<()> {
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub struct LoopbackClient(InternetHandle);

pub struct Response {
    pub status: u32,
    pub studio_cors: bool,
    pub body: Vec<u8>,
}

impl LoopbackClient {
    pub fn new() -> io::Result<Self> {
        // The session exclusively uses the numeric IPv4 loopback address and never a proxy.
        let session = InternetHandle::new(unsafe {
            WinHttpOpen(
                w!("Lummix"),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                null(),
                null(),
                0,
            )
        })?;
        checked(unsafe { WinHttpSetTimeouts(session.0, 1500, 1500, 1500, 1500) })?;
        Ok(Self(session))
    }

    pub fn get(
        &self,
        port: u16,
        path: &str,
        studio_origin: bool,
        limit: usize,
    ) -> io::Result<Response> {
        let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
        // All WinHTTP inputs are fixed constants or buffers alive through the call.
        let connection =
            InternetHandle::new(unsafe { WinHttpConnect(self.0 .0, w!("127.0.0.1"), port, 0) })?;
        let request = InternetHandle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                path.as_ptr(),
                null(),
                null(),
                null(),
                0,
            )
        })?;
        let features =
            WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION;
        checked(unsafe {
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_DISABLE_FEATURE,
                (&features as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        })?;
        let (headers, length) = if studio_origin {
            (w!("Origin: http://127.0.0.1:41835\r\n"), u32::MAX)
        } else {
            (null(), 0)
        };
        checked(unsafe { WinHttpSendRequest(request.0, headers, length, null(), 0, 0, 0) })?;
        checked(unsafe { WinHttpReceiveResponse(request.0, null_mut()) })?;
        let mut status = 0u32;
        let mut needed = size_of::<u32>() as u32;
        checked(unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut needed,
                null_mut(),
            )
        })?;
        let studio_cors = studio_origin && Self::studio_cors(request.0);
        let mut body = vec![0; limit];
        let mut offset = 0;
        while offset < limit {
            let buffer = &mut body[offset..];
            let mut read = 0;
            checked(unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                )
            })?;
            if read == 0 {
                break;
            }
            offset += read as usize;
        }
        body.truncate(offset);
        Ok(Response {
            status,
            studio_cors,
            body,
        })
    }

    fn studio_cors(request: *mut c_void) -> bool {
        let header: PCWSTR = w!("Access-Control-Allow-Origin");
        let matching: Vec<u16> = "http://127.0.0.1:41835".encode_utf16().collect();
        let mut value = [0u16; 64];
        let mut length = size_of_val(&value) as u32;
        let found = unsafe {
            WinHttpQueryHeaders(
                request,
                WINHTTP_QUERY_CUSTOM,
                header,
                value.as_mut_ptr().cast(),
                &mut length,
                null_mut(),
            )
        };
        found != 0 && value[..length as usize / size_of::<u16>()] == matching
    }
}
