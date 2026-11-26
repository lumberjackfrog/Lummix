use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::windows::io::AsRawSocket,
    ptr::null_mut,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tiny_http::{Method, StatusCode};
use windows_sys::Win32::Security::Cryptography::{
    BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
};

const HEAD_LIMIT: usize = 8192;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Request {
    stream: TcpStream,
    pub method: Method,
    pub target: String,
    headers: Vec<(String, String)>,
    prefix: Vec<u8>,
    consumed_body: bool,
    deadline: Instant,
}

impl Request {
    fn read(mut stream: TcpStream) -> io::Result<Self> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        stream.set_nonblocking(false)?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let mut data = Vec::new();
        let mut block = [0u8; 1024];
        loop {
            let mut headers = [httparse::EMPTY_HEADER; 48];
            let mut parsed = httparse::Request::new(&mut headers);
            match parsed.parse(&data) {
                Ok(httparse::Status::Complete(offset)) if offset <= HEAD_LIMIT => {
                    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Bad HTTP request");
                    let method = parsed
                        .method
                        .and_then(|method| method.parse().ok())
                        .ok_or_else(invalid)?;
                    let target = parsed
                        .path
                        .filter(|target| target.starts_with('/'))
                        .ok_or_else(invalid)?
                        .to_owned();
                    let mut fields = Vec::with_capacity(parsed.headers.len());
                    for field in parsed.headers {
                        let value = std::str::from_utf8(field.value).map_err(|_| invalid())?;
                        fields.push((field.name.to_owned(), value.to_owned()));
                    }
                    let prefix = data[offset..].to_vec();
                    return Ok(Self {
                        stream,
                        method,
                        target,
                        headers: fields,
                        prefix,
                        consumed_body: false,
                        deadline,
                    });
                }
                Ok(httparse::Status::Partial) if data.len() < HEAD_LIMIT => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Bad HTTP request",
                    ))
                }
            }
            let read = read_before(&mut stream, &mut block, deadline)?;
            data.extend_from_slice(&block[..read]);
        }
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    fn body(&mut self, size: usize) -> io::Result<Vec<u8>> {
        let mut data = vec![0; size];
        let mut offset = self.prefix.len().min(size);
        data[..offset].copy_from_slice(&self.prefix[..offset]);
        while offset < size {
            offset += read_before(&mut self.stream, &mut data[offset..], self.deadline)?;
        }
        self.consumed_body = true;
        Ok(data)
    }
}

fn read_before(stream: &mut TcpStream, target: &mut [u8], deadline: Instant) -> io::Result<usize> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    stream.set_read_timeout(Some(remaining))?;
    match stream.read(target)? {
        0 => Err(io::ErrorKind::UnexpectedEof.into()),
        size => Ok(size),
    }
}

pub fn header_values<'a>(
    request: &'a Request,
    name: &'static str,
) -> impl Iterator<Item = &'a str> {
    request
        .headers
        .iter()
        .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

pub fn single_header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    let mut found = header_values(request, name);
    let value = found.next()?;
    found.next().is_none().then_some(value)
}

pub fn has_header(request: &Request, name: &'static str, value: &str) -> bool {
    single_header(request, name) == Some(value)
}

pub fn send(mut request: Request, status: u16, body: Vec<u8>, mime: &str, extra: &[(&str, &str)]) {
    let reason = StatusCode(status).default_reason_phrase();
    let mut headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        body.len(),
    );
    for (name, value) in extra {
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    headers.push_str("\r\n");
    let mut message = headers.into_bytes();
    if request.method != Method::Head {
        message.extend_from_slice(&body);
    }
    if request.stream.write_all(&message).is_ok() {
        let _ = request.stream.shutdown(Shutdown::Write);
        // Allow the rejection to reach the client even if its small POST body
        // is in flight. Never let rejected bytes wait out the service lifetime.
        if !request.consumed_body {
            let pending = single_header(&request, "Content-Length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default()
                .saturating_sub(request.prefix.len())
                .min(512);
            let deadline = Instant::now() + Duration::from_millis(50);
            let mut buffer = [0; 512];
            let mut drained = 0;
            while drained < pending {
                match read_before(&mut request.stream, &mut buffer[drained..pending], deadline) {
                    Ok(read) => drained += read,
                    Err(_) => break,
                }
            }
        }
    }
}

// Each listener sleeps in Winsock until a connection arrives or Drop signals shutdown.
// Handles stay alive until the accept thread has joined.
struct SocketEvent(isize);
impl SocketEvent {
    fn new() -> Option<Self> {
        use windows_sys::Win32::Networking::WinSock::{WSACreateEvent, WSA_INVALID_EVENT};
        let event = unsafe { WSACreateEvent() };
        (event != WSA_INVALID_EVENT).then_some(Self(event))
    }
}
impl Drop for SocketEvent {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Networking::WinSock::WSACloseEvent(self.0);
        }
    }
}

pub struct Listener {
    stop: Arc<SocketEvent>,
    running: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        unsafe {
            windows_sys::Win32::Networking::WinSock::WSASetEvent(self.stop.0 as _);
        }
        if let Some(thread) = self.accept.take() {
            let _ = thread.join();
        }
        while let Some(thread) = self.workers.pop() {
            let _ = thread.join();
        }
    }
}

pub fn listen(
    address: std::net::Ipv4Addr,
    bind: u16,
    handler: impl Fn(Request) + Send + Sync + 'static,
) -> Option<(Listener, u16)> {
    listen_workers(address, bind, 2, handler)
}

pub fn listen_workers(
    address: std::net::Ipv4Addr,
    bind: u16,
    workers: usize,
    handler: impl Fn(Request) + Send + Sync + 'static,
) -> Option<(Listener, u16)> {
    let server = TcpListener::bind((address, bind)).ok()?;
    use windows_sys::Win32::Networking::WinSock::{
        WSAEnumNetworkEvents, WSAEventSelect, WSAWaitForMultipleEvents, FD_ACCEPT, WSA_INFINITE,
    };
    let pending = SocketEvent::new()?;
    let stop = Arc::new(SocketEvent::new()?);
    let socket = server.as_raw_socket() as usize;
    if unsafe { WSAEventSelect(socket, pending.0, FD_ACCEPT as i32) } != 0 {
        return None;
    }
    let port = server.local_addr().ok()?.port();
    let handler = Arc::new(handler);
    let (send, receive) = mpsc::sync_channel(workers * 2);
    let receive = Arc::new(Mutex::new(receive));
    let running = Arc::new(AtomicBool::new(true));
    let accept = {
        let running = running.clone();
        let stop = stop.clone();
        thread::Builder::new()
            .name(format!("lummix-accept-{port}"))
            .spawn(move || {
                // Move the RAII owner, not just its Copy handle, into this thread.
                let pending = pending;
                let events = [stop.0 as _, pending.0 as _];
                while running.load(Ordering::Acquire) {
                    // The stop event is first and wins if a connection arrives during shutdown.
                    if unsafe { WSAWaitForMultipleEvents(2, events.as_ptr(), 0, WSA_INFINITE, 0) }
                        != 1
                    {
                        break;
                    }
                    let mut flags = unsafe { std::mem::zeroed() };
                    if unsafe { WSAEnumNetworkEvents(socket, pending.0, &mut flags) } != 0 {
                        break;
                    }
                    while running.load(Ordering::Acquire) {
                        match server.accept() {
                            Ok((stream, _)) => {
                                // Accepted sockets inherit event selection. Return them to ordinary
                                // blocking I/O with the worker's bounded request deadline.
                                if unsafe { WSAEventSelect(stream.as_raw_socket() as usize, 0, 0) }
                                    != 0
                                {
                                    continue;
                                }
                                match send.try_send(stream) {
                                    Ok(()) | Err(TrySendError::Full(_)) => {}
                                    Err(TrySendError::Disconnected(_)) => return,
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => return,
                        }
                    }
                }
            })
            .ok()?
    };
    let mut listener = Listener {
        stop,
        running,
        accept: Some(accept),
        workers: Vec::new(),
    };
    for _ in 0..workers {
        let (handler, running, receive) =
            (handler.clone(), listener.running.clone(), receive.clone());
        let worker = thread::Builder::new()
            .name(format!("lummix-local-{port}"))
            .spawn(move || {
                while running.load(Ordering::Acquire) {
                    let stream = match receive
                        .lock()
                        .ok()
                        .and_then(|receiver| receiver.recv().ok())
                    {
                        Some(stream) if running.load(Ordering::Acquire) => stream,
                        _ => break,
                    };
                    if let Ok(request) = Request::read(stream) {
                        if running.load(Ordering::Acquire) {
                            handler(request);
                        }
                    }
                }
            })
            .ok()?;
        listener.workers.push(worker);
    }
    Some((listener, port))
}

pub fn read_payload(request: &mut Request, limit: usize) -> Result<Vec<u8>, u16> {
    let expect = if header_values(request, "Expect").next().is_some() {
        if !single_header(request, "Expect")
            .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        {
            return Err(417);
        }
        true
    } else {
        false
    };
    fn continue_if_needed(request: &mut Request, expect: bool) -> Result<(), u16> {
        if expect {
            request
                .stream
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .map_err(|_| 417u16)?;
        }
        Ok(())
    }
    if header_values(request, "Transfer-Encoding").next().is_some() {
        if !single_header(request, "Transfer-Encoding")
            .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
            || header_values(request, "Content-Length").next().is_some()
        {
            return Err(400);
        }
        continue_if_needed(request, expect)?;
        return read_chunks(request, limit);
    }
    let mut sizes = header_values(request, "Content-Length");
    let length = match sizes.next() {
        None => 0,
        Some(raw) => {
            if sizes.next().is_some()
                || raw.is_empty()
                || !raw.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(400);
            }
            raw.parse::<usize>().map_err(|_| 413u16)?
        }
    };
    drop(sizes);
    if length > limit {
        return Err(413);
    }
    continue_if_needed(request, expect)?;
    request.body(length).map_err(|_| 400)
}

fn read_chunks(request: &mut Request, limit: usize) -> Result<Vec<u8>, u16> {
    let mut data = Vec::new();
    let mut pending = std::mem::take(&mut request.prefix);
    let fill = |pending: &mut Vec<u8>, stream: &mut TcpStream| -> Result<(), u16> {
        let mut block = [0; 1024];
        let read = read_before(stream, &mut block, request.deadline).map_err(|_| 400u16)?;
        pending.extend_from_slice(&block[..read]);
        Ok(())
    };
    loop {
        let (header, size) = loop {
            match httparse::parse_chunk_size(&pending) {
                Ok(httparse::Status::Complete((header, size))) if header <= 1024 => {
                    break (header, size)
                }
                Ok(httparse::Status::Partial) if pending.len() < 1024 => {
                    fill(&mut pending, &mut request.stream)?
                }
                _ => return Err(400),
            }
        };
        if size == 0 {
            loop {
                let mut trailers = [httparse::EMPTY_HEADER; 32];
                match httparse::parse_headers(&pending[header..], &mut trailers) {
                    Ok(httparse::Status::Complete(_)) => {
                        request.consumed_body = true;
                        return Ok(data);
                    }
                    Ok(httparse::Status::Partial) if pending.len() - header < 4096 => {
                        fill(&mut pending, &mut request.stream)?
                    }
                    _ => return Err(400),
                }
            }
        }
        if size > (limit - data.len()) as u64 {
            return Err(413);
        }
        let end = header + size as usize;
        while pending.len() < end + 2 {
            fill(&mut pending, &mut request.stream)?;
        }
        if &pending[end..end + 2] != b"\r\n" {
            return Err(400);
        }
        data.extend_from_slice(&pending[header..end]);
        pending.drain(..end + 2);
    }
}

pub fn decode_path(path: &str) -> Option<String> {
    let mut result = Vec::new();
    let mut input = path.bytes();
    let digit = |byte: u8| char::from(byte).to_digit(16).map(|digit| digit as u8);
    while let Some(byte) = input.next() {
        if byte == b'%' {
            result.push(digit(input.next()?)? * 16 + digit(input.next()?)?);
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result).ok()
}

pub fn parse_body(request: &mut Request) -> Result<Value, u16> {
    let mime = single_header(request, "Content-Type").unwrap_or("");
    if !mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(415);
    }
    let size = single_header(request, "Content-Length").and_then(|n| n.parse::<usize>().ok());
    let Some(size) = size.filter(|size| (1..=512).contains(size)) else {
        return Err(400);
    };
    if header_values(request, "Transfer-Encoding").next().is_some()
        || header_values(request, "Expect").next().is_some()
    {
        return Err(400);
    }
    let data = request.body(size).map_err(|_| 400u16)?;
    serde_json::from_slice(&data).map_err(|_| 400)
}

pub fn token() -> Option<String> {
    let mut random = [0u8; 32];
    if unsafe {
        BCryptGenRandom(
            null_mut(),
            random.as_mut_ptr(),
            32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    } != 0
    {
        return None;
    }
    Some(URL_SAFE_NO_PAD.encode(random))
}
