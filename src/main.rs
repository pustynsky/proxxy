use std::{io, net::Ipv6Addr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

mod dns;
use dns::Dns;

const LISTEN_ADDR: &str = "127.0.0.1:8899";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::main]
async fn main() -> io::Result<()> {
    let dns = Dns::new()?;
    let listener = TcpListener::bind(LISTEN_ADDR).await?;
    println!("Proxxy listening on {LISTEN_ADDR}. HTTPS CONNECT only. Ctrl+C to stop.");
    println!("DNS: Cloudflare DoH, Google DoH on failure; TLS verified, no system DNS fallback.");

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (client, peer) = result?;
                let dns = dns.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle(client, &dns).await {
                        eprintln!("{peer}: {error}");
                    }
                });
            }
            result = tokio::signal::ctrl_c() => {
                result?;
                println!("Stopping Proxxy.");
                return Ok(());
            }
        }
    }
}

async fn read_headers(client: &mut TcpStream) -> io::Result<(Vec<u8>, usize)> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let available = chunk.len().min(MAX_HEADER_BYTES - bytes.len());
        let count = client.read(&mut chunk[..available]).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client closed before completing CONNECT headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            return Ok((bytes, end + 4));
        }
        if bytes.len() == MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "CONNECT headers exceed 16 KiB",
            ));
        }
    }
}

fn parse_target(target: &str) -> Option<(&str, u16)> {
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        host.parse::<Ipv6Addr>().ok()?;
        (host, port)
    } else {
        let (host, port) = target.split_once(':')?;
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
        {
            return None;
        }
        (host, port)
    };
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    (port != 0).then_some((host, port))
}

async fn reject(client: &mut TcpStream, status: &str, reason: &str) -> io::Result<()> {
    eprintln!("Rejecting request: {status}: {reason}");
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    client.write_all(response.as_bytes()).await?;
    client.shutdown().await
}

async fn handle(mut client: TcpStream, dns: &Dns) -> io::Result<()> {
    let (bytes, header_end) = match timeout(HEADER_TIMEOUT, read_headers(&mut client)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
            return reject(
                &mut client,
                "431 Request Header Fields Too Large",
                &error.to_string(),
            )
            .await;
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            return reject(&mut client, "408 Request Timeout", "header read timed out").await;
        }
    };

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    if !matches!(
        request.parse(&bytes[..header_end]),
        Ok(httparse::Status::Complete(_))
    ) {
        return reject(&mut client, "400 Bad Request", "invalid HTTP headers").await;
    }
    if request.method != Some("CONNECT") {
        return reject(
            &mut client,
            "501 Not Implemented",
            "only CONNECT is supported; ordinary HTTP forwarding is not implemented",
        )
        .await;
    }
    let Some((host, port)) = request.path.and_then(parse_target) else {
        return reject(
            &mut client,
            "400 Bad Request",
            "expected host:port or [IPv6]:port with a port from 1 to 65535",
        )
        .await;
    };

    println!("CONNECT {}:{port}", host);
    let connect = async {
        let addresses = dns.resolve(host, port).await?;
        TcpStream::connect(addresses.as_slice()).await
    };
    let mut upstream = match timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            return reject(&mut client, "502 Bad Gateway", &error.to_string()).await;
        }
        Err(_) => {
            return reject(
                &mut client,
                "504 Gateway Timeout",
                "upstream connection timed out",
            )
            .await;
        }
    };
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    // A client may send tunnel bytes in the same TCP packet as the headers.
    upstream.write_all(&bytes[header_end..]).await?;
    let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    println!(
        "CLOSED {host}:{port}: sent {} bytes, received {received} bytes",
        sent + (bytes.len() - header_end) as u64
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn exchange(request: &[u8]) -> Vec<u8> {
        exchange_with_dns(request, Dns::new().unwrap()).await
    }

    async fn exchange_with_dns(request: &[u8], dns: Dns) -> Vec<u8> {
        timeout(Duration::from_secs(5), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let proxy = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                handle(stream, &dns).await.unwrap();
            });
            let mut client = TcpStream::connect(addr).await.unwrap();
            client.write_all(request).await.unwrap();
            client.shutdown().await.unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            proxy.await.unwrap();
            response
        })
        .await
        .expect("proxy exchange timed out")
    }

    #[test]
    fn validates_connect_authority() {
        assert_eq!(parse_target("example.com:443"), Some(("example.com", 443)));
        assert_eq!(parse_target("127.0.0.1:8080"), Some(("127.0.0.1", 8080)));
        assert_eq!(parse_target("[::1]:443"), Some(("::1", 443)));
        for invalid in [
            "",
            "example.com",
            ":443",
            "example.com:0",
            "example.com:65536",
            "example.com:+443",
            "example.com:443/path",
            "user@host:443",
            "::1:443",
            "[invalid]:443",
            "example.com:443:80",
        ] {
            assert_eq!(parse_target(invalid), None, "{invalid}");
        }
    }

    #[tokio::test]
    async fn dns_failure_returns_bad_gateway() {
        let (dns, server) = dns::tests::broken_doh().await;
        let response = exchange_with_dns(
            b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n",
            dns,
        )
        .await;
        server.abort();
        assert!(response.starts_with(b"HTTP/1.1 502 Bad Gateway\r\n"));
    }

    #[tokio::test]
    async fn tunnels_buffered_bytes_and_preserves_half_close() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = upstream.local_addr().unwrap();
        let payload = b"\x00\xffopaque TLS bytes\r\n\r\n";
        let server = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            assert_eq!(bytes, payload);
            stream.write_all(&bytes).await.unwrap();
        });
        let mut request = format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n\r\n").into_bytes();
        request.extend_from_slice(payload);
        let response = exchange(&request).await;
        let mut expected = b"HTTP/1.1 200 Connection Established\r\n\r\n".to_vec();
        expected.extend_from_slice(payload);
        assert_eq!(response, expected);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_ordinary_http() {
        let response =
            exchange(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n").await;
        assert!(response.starts_with(b"HTTP/1.1 501 Not Implemented\r\n"));
    }

    #[tokio::test]
    async fn rejects_invalid_target_and_headers() {
        for request in [
            &b"CONNECT example.com HTTP/1.1\r\n\r\n"[..],
            &b"CONNECT example.com:443 HTTP/1.1\r\nnot a header\r\n\r\n"[..],
        ] {
            let response = exchange(request).await;
            assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
        }
    }

    #[tokio::test]
    async fn rejects_oversized_headers() {
        let response = exchange(&vec![b'A'; MAX_HEADER_BYTES]).await;
        assert!(response.starts_with(b"HTTP/1.1 431 Request Header Fields Too Large\r\n"));
    }
}
