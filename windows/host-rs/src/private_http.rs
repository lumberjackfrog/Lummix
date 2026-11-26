use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    ptr::null_mut,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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

pub struct Listener {
    running: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
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
    let server = Arc::new(TcpListener::bind((address, bind)).ok()?);
    server.set_nonblocking(true).ok()?;
    let port = server.local_addr().ok()?.port();
    let handler = Arc::new(handler);
    let mut listener = Listener {
        running: Arc::new(AtomicBool::new(true)),
        workers: Vec::new(),
    };
    for _ in 0..2 {
        let (server, handler, running) =
            (server.clone(), handler.clone(), listener.running.clone());
        let worker = thread::Builder::new()
            .name(format!("lummix-local-{port}"))
            .spawn(move || {
                while running.load(Ordering::Acquire) {
                    match server.accept() {
                        Ok((stream, _)) => {
                            if let Ok(request) = Request::read(stream) {
                                if running.load(Ordering::Acquire) {
                                    handler(request);
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(30))
                        }
                        Err(_) => break,
                    }
                }
            })
            .ok()?;
        listener.workers.push(worker);
    }
    Some((listener, port))
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
