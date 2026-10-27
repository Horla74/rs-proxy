use std::{
    env, fs,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const MAX_HEADER_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug)]
struct AccessControl {
    whitelist: Vec<String>,
    blacklist: Vec<String>,
}

#[derive(Debug)]
struct Settings {
    bind: String,
    access: AccessControl,
}

impl Settings {
    fn load() -> io::Result<Self> {
        let config_path = config_path_from_args_or_env();
        let mut bind = "127.0.0.1:7070".to_string();
        let mut whitelist = Vec::new();
        let mut blacklist = Vec::new();

        if let Some(path) = config_path {
            let contents = fs::read_to_string(&path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("read config {}: {error}", path.display()),
                )
            })?;
            for (line_number, line) in contents.lines().enumerate() {
                let line = line.split('#').next().unwrap_or_default().trim();
                if line.is_empty() {
                    continue;
                }
                let (key, value) = line.split_once('=').ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid config line {}", line_number + 1),
                    )
                })?;
                match key.trim() {
                    "bind" => bind = parse_string(value, line_number + 1)?,
                    "whitelist" => whitelist = parse_config_list(value, line_number + 1)?,
                    "blacklist" => blacklist = parse_config_list(value, line_number + 1)?,
                    unknown => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("unknown config key '{unknown}' on line {}", line_number + 1),
                        ));
                    }
                }
            }
        }

        if let Ok(value) = env::var("PROXY_BIND") {
            if !value.trim().is_empty() {
                bind = value;
            }
        }
        if env::var_os("PROXY_WHITELIST").is_some() {
            whitelist = parse_list("PROXY_WHITELIST");
        }
        if env::var_os("PROXY_BLACKLIST").is_some() {
            blacklist = parse_list("PROXY_BLACKLIST");
        }
        if let Some(port) = port_from_args()? {
            bind = set_bind_port(&bind, port)?;
        }

        Ok(Self {
            bind,
            access: AccessControl {
                whitelist,
                blacklist,
            },
        })
    }
}

impl AccessControl {
    fn allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if self.blacklist.iter().any(|rule| matches_rule(rule, &host)) {
            return false;
        }
        self.whitelist.is_empty() || self.whitelist.iter().any(|rule| matches_rule(rule, &host))
    }
}

fn parse_list(name: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            item.trim_start_matches("*.")
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .collect()
}

fn matches_rule(rule: &str, host: &str) -> bool {
    host == rule || host.ends_with(&format!(".{rule}"))
}

#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    headers: Vec<u8>,
    remainder: Vec<u8>,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    if help_requested() {
        print_help();
        return Ok(());
    }

    let settings = Settings::load()?;
    let bind = settings.bind;
    let listener = TcpListener::bind(&bind).await?;
    let access = Arc::new(settings.access);

    println!("proxy listening on {bind}");
    if access.whitelist.is_empty() {
        println!("whitelist: disabled");
    } else {
        println!("whitelist: {}", access.whitelist.join(", "));
    }
    let blacklist = if access.blacklist.is_empty() {
        "disabled".to_string()
    } else {
        access.blacklist.join(", ")
    };
    println!("blacklist: {blacklist}");

    loop {
        let (stream, peer) = listener.accept().await?;
        let access = Arc::clone(&access);
        tokio::spawn(async move {
            if let Err(error) = handle_client(stream, access).await {
                eprintln!("{peer}: {error}");
            }
        });
    }
}

fn help_requested() -> bool {
    env::args()
        .skip(1)
        .any(|argument| argument == "-h" || argument == "--help")
}

fn print_help() {
    println!(
        "Usage:\n  rs-proxy [run] [OPTIONS]\n\nOptions:\n  -p, --port <PORT>  Override the listening port\n      --config <PATH>  Load configuration from a file\n  -h, --help         Print this help message"
    );
}

fn config_path_from_args_or_env() -> Option<PathBuf> {
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--config" {
            return args.next().map(PathBuf::from);
        }
        if let Some(path) = argument.strip_prefix("--config=") {
            return Some(PathBuf::from(path));
        }
    }
    env::var_os("PROXY_CONFIG").map(PathBuf::from)
}

fn port_from_args() -> io::Result<Option<u16>> {
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "run" {
            continue;
        }
        if argument == "--port" || argument == "-p" {
            let value = args.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "missing value for --port")
            })?;
            return value.parse::<u16>().map(Some).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid port '{value}': {error}"),
                )
            });
        }
        if let Some(value) = argument.strip_prefix("--port=") {
            return value.parse::<u16>().map(Some).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid port '{value}': {error}"),
                )
            });
        }
    }
    Ok(None)
}

fn set_bind_port(bind: &str, port: u16) -> io::Result<String> {
    let address = bind.parse::<SocketAddr>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot set port on bind address '{bind}': {error}"),
        )
    })?;
    Ok(SocketAddr::new(address.ip(), port).to_string())
}

fn parse_string(value: &str, line_number: usize) -> io::Result<String> {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        Ok(value[1..value.len() - 1].to_string())
    } else if !value.is_empty() && !value.contains(char::is_whitespace) {
        Ok(value.to_string())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid string on line {line_number}"),
        ))
    }
}

fn parse_config_list(value: &str, line_number: usize) -> io::Result<Vec<String>> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("list must use [...] on line {line_number}"),
        ));
    }
    let inner = &value[1..value.len() - 1];
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    inner
        .split(',')
        .map(|item| parse_string(item.trim(), line_number))
        .collect()
}

async fn handle_client(mut client: TcpStream, access: Arc<AccessControl>) -> io::Result<()> {
    let request = read_request(&mut client).await?;
    let method = request.method.to_ascii_uppercase();

    if method == "CONNECT" {
        let target = normalize_connect_target(&request.target)?;
        let host = host_without_port(&target);
        if !access.allowed(host) {
            return reject(&mut client, "403 Forbidden", "destination is blocked").await;
        }

        let mut upstream = TcpStream::connect(&target).await.map_err(|error| {
            io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("connect {target}: {error}"),
            )
        })?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        if !request.remainder.is_empty() {
            upstream.write_all(&request.remainder).await?;
        }
        let _ = io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }

    let (target, host, outbound_headers) = normalize_http_request(&request)?;
    if !access.allowed(&host) {
        return reject(&mut client, "403 Forbidden", "destination is blocked").await;
    }

    let mut upstream = TcpStream::connect(&target).await.map_err(|error| {
        io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("connect {target}: {error}"),
        )
    })?;
    upstream.write_all(&outbound_headers).await?;
    if !request.remainder.is_empty() {
        upstream.write_all(&request.remainder).await?;
    }
    let _ = io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> io::Result<Request> {
    let mut buffer = Vec::with_capacity(4096);
    let header_end = loop {
        let mut byte = [0u8; 1024];
        let count = stream.read(&mut byte).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client closed connection",
            ));
        }
        buffer.extend_from_slice(&byte[..count]);
        if buffer.len() > MAX_HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };

    let text = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request headers are not UTF-8"))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || target.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed request line",
        ));
    }
    Ok(Request {
        method,
        target,
        headers: buffer[..header_end].to_vec(),
        remainder: buffer[header_end..].to_vec(),
    })
}

fn normalize_connect_target(target: &str) -> io::Result<String> {
    if target.is_empty() || target.contains('/') || !target.contains(':') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CONNECT requires host:port",
        ));
    }
    let (host, port) = split_host_port(target)?;
    validate_port(port)?;
    Ok(format_host_port(host, port))
}

fn normalize_http_request(request: &Request) -> io::Result<(String, String, Vec<u8>)> {
    let host_header = header_value(&request.headers, "host").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "HTTP request has no Host header",
        )
    })?;
    let (host, port, path) = if request.target.starts_with("http://") {
        parse_http_url(&request.target)?
    } else if request.target.starts_with("https://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "use CONNECT for HTTPS proxying",
        ));
    } else {
        let (host, port) = split_authority(host_header, 80)?;
        (host.to_string(), port, request.target.clone())
    };
    validate_port(port)?;
    let target = format_host_port(&host, port);
    let mut headers = request.headers.clone();
    let first_line_end = headers
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap();
    let method = request.method.as_bytes();
    let replacement = format!("{} {} HTTP/1.1", String::from_utf8_lossy(method), path);
    let mut outbound = replacement.into_bytes();
    outbound.extend_from_slice(&headers[first_line_end..]);
    headers = outbound;
    remove_header(&mut headers, "proxy-connection");
    Ok((target, host, headers))
}

fn parse_http_url(url: &str) -> io::Result<(String, u16, String)> {
    let remainder = &url[7..];
    let authority_end = remainder.find('/').unwrap_or(remainder.len());
    let (authority, suffix) = remainder.split_at(authority_end);
    let (host, port) = split_authority(authority, 80)?;
    Ok((
        host.to_string(),
        port,
        if suffix.is_empty() {
            "/".to_string()
        } else {
            suffix.to_string()
        },
    ))
}

fn split_host_port(value: &str) -> io::Result<(&str, u16)> {
    if let Some(rest) = value.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid IPv6 host"))?;
        let host = &rest[..end];
        let port = rest[end + 1..]
            .strip_prefix(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing port"))?;
        return Ok((
            host,
            port.parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port"))?,
        ));
    }
    let (host, port) = value
        .rsplit_once(':')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing port"))?;
    Ok((
        host,
        port.parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port"))?,
    ))
}

fn split_authority(value: &str, default_port: u16) -> io::Result<(&str, u16)> {
    if let Some(rest) = value.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid IPv6 host"))?;
        let host = &rest[..end];
        let port = match rest[end + 1..].strip_prefix(':') {
            Some(port) => port
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port"))?,
            None => default_port,
        };
        return Ok((host, port));
    }
    match value.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => Ok((
            host,
            port.parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port"))?,
        )),
        _ if !value.is_empty() => Ok((value, default_port)),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "missing host")),
    }
}

fn format_host_port(host: &str, port: u16) -> String {
    if host
        .parse::<IpAddr>()
        .map(|ip| ip.is_ipv6())
        .unwrap_or(false)
    {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn validate_port(port: u16) -> io::Result<()> {
    if port == 0 {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "port must not be zero",
        ))
    } else {
        Ok(())
    }
}

fn host_without_port(target: &str) -> &str {
    target
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
        .unwrap_or_else(|| {
            target
                .rsplit_once(':')
                .map(|(host, _)| host)
                .unwrap_or(target)
        })
}

fn header_value<'a>(headers: &'a [u8], name: &str) -> Option<&'a str> {
    let text = std::str::from_utf8(headers).ok()?;
    text.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.eq_ignore_ascii_case(name)).then(|| value.trim())
    })
}

fn remove_header(headers: &mut Vec<u8>, name: &str) {
    let text = String::from_utf8_lossy(headers);
    let mut output = String::new();
    for (index, line) in text.split("\r\n").enumerate() {
        if index == 0
            || !line
                .split_once(':')
                .map(|(key, _)| key.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        {
            output.push_str(line);
            output.push_str("\r\n");
        }
    }
    *headers = output.into_bytes();
}

async fn reject(stream: &mut TcpStream, status: &str, message: &str) -> io::Result<()> {
    let body = format!("{message}\n");
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\n{body}", body.len());
    stream.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_match_domains_and_subdomains() {
        assert!(matches_rule("example.com", "example.com"));
        assert!(matches_rule("example.com", "api.example.com"));
        assert!(!matches_rule("example.com", "badexample.com"));
    }

    #[test]
    fn parses_http_url() {
        assert_eq!(
            parse_http_url("http://example.com/path?q=1").unwrap(),
            ("example.com".into(), 80, "/path?q=1".into())
        );
    }
}
