use std::net::Ipv4Addr;
use std::path::PathBuf;

use tiny_http::Method;

use crate::private_http::{has_header, listen_workers, send, Listener};
use crate::studio_response;

pub fn listen(root: PathBuf, address: Ipv4Addr, port: u16) -> Option<(Listener, u16)> {
    listen_workers(address, port, 4, move |request| {
        let reply = studio_response::response(
            request.method == Method::Get,
            has_header(&request, "Host", studio_response::HOST),
            &request.target,
            &root,
        );
        send(request, reply.status, reply.body, reply.mime, &[]);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};

    fn get(port: u16, target: &str, header: &str) -> Vec<u8> {
        let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        write!(socket, "GET {target} HTTP/1.1\r\n{header}\r\n\r\n").unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        response
    }

    #[test]
    fn lifetime_is_in_process_and_shutdown_is_bounded_with_partial_http() {
        let root = std::env::temp_dir().join(format!("LummixStudioFixture-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("index.html"), b"Studio fixture").unwrap();
        let (listener, port) = listen(root.clone(), Ipv4Addr::LOCALHOST, 0).unwrap();
        assert!(get(port, "/", "Host: 127.0.0.1:41835").ends_with(b"Studio fixture"));
        assert!(get(port, "/lummix-health", "Host: 127.0.0.1:41835")
            .ends_with(br#"{"service":"lummix-studio","ready":true}"#));
        assert!(
            get(port, "/%2e%2e/index.html", "Host: 127.0.0.1:41835").starts_with(b"HTTP/1.1 404")
        );
        assert!(
            get(port, "/", "Host: 127.0.0.1:41835\r\nHost: 127.0.0.1:41835")
                .starts_with(b"HTTP/1.1 403")
        );
        let mut partial = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        partial.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let before = Instant::now();
        drop(listener);
        assert!(before.elapsed() < Duration::from_secs(4));
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
        std::fs::remove_file(root.join("index.html")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
