# rs-proxy

A small forward proxy written in Rust. It supports:

- HTTP proxy requests (`GET http://example.com/path HTTP/1.1`)
- HTTPS proxying through `CONNECT host:port`
- concurrent clients
- optional domain whitelist and blacklist rules

## Run

View command-line help with:

```bash
cargo run -- --help
```

```bash
cargo run --release
```

The default listening address is `127.0.0.1:7070`. Use `--config` to load a config file:

```bash
cargo run --release -- --config proxy.conf
```

Use the `run` subcommand with `--port` (or `-p`) to override only the port:

```bash
cargo run --release -- run --port 7072
```

The command-line port takes precedence over the config file and `PROXY_BIND`.

Example `proxy.conf`:

```text
bind = "0.0.0.0:7070"
whitelist = ["example.com", "*.rust-lang.org"]
blacklist = ["ads.example.com"]
```

The config path can also be supplied with `PROXY_CONFIG`. Command-line/config values are overridden by `PROXY_BIND`, `PROXY_WHITELIST`, and `PROXY_BLACKLIST` when those environment variables are set.

Configuration is supplied with environment variables:

```bash
PROXY_BIND=0.0.0.0:7070 \
PROXY_WHITELIST='example.com,*.rust-lang.org' \
PROXY_BLACKLIST='ads.example.com' \
cargo run --release
```

`PROXY_BIND` defaults to `127.0.0.1:7070`. A non-empty whitelist permits a domain and its subdomains only when it matches. The blacklist always wins. Rules are comma-separated and are matched case-insensitively; `example.com`, `.example.com`, and `*.example.com` all match `example.com` and its subdomains.

Configure a client such as `curl`:

```bash
curl -x http://127.0.0.1:7070 http://example.com/
curl -x http://127.0.0.1:7070 https://example.com/
```

This is an unauthenticated proxy. Do not expose it to the public internet without adding authentication, resource limits, logging, and stronger network policy.
