# Proxxy

A small Rust CONNECT proxy for learning and diagnosing HTTPS connections,
with DNS-over-HTTPS and a Chrome launcher for Windows.
It listens only on `127.0.0.1:8899` and connects directly to the requested
host using Cloudflare DNS-over-HTTPS, with Google as a fallback.
TLS bytes pass through unchanged:
no HTTPS decryption, certificates, drivers, or system proxy changes.

## Why this exists

- Learn how CONNECT handling, DNS resolution and bidirectional TCP tunnels work.
- Experiment with DNS-over-HTTPS, provider failover and DNS caching.
- Diagnose connection failures and observe destinations and tunnel byte counts
  without decrypting HTTPS traffic.
- Launch Chrome through the local proxy without changing other applications'
  system proxy settings, and stop the proxy when Chrome closes.

This is a learning and diagnostic prototype, not a VPN, an anonymity tool,
or a production proxy. It connects directly to destinations, not through a
remote relay. Chrome already supports DoH; the value here is an inspectable
implementation and a place to experiment.

## Run

Install the Rust toolchain, then run from this directory:

```powershell
cargo run --release
```

Stop with Ctrl+C. Active tunnels close when the process exits.
The compiled executable is `target\release\proxxy.exe`.

## Chrome-only launcher

Disable the manually configured Windows proxy first, so Edge and other
applications no longer use it. Completely close Chrome, including background
processes, then double-click `Start-Chrome.cmd`.

The launcher starts the release proxy if necessary and opens your normal
Chrome profile with `--proxy-server=https=127.0.0.1:8899`. It does not change
Windows settings. Before launching, it forcibly closes any remaining Chrome
processes, so save your work first. Chrome must be installed in a standard
location. Managed browser policies can override the command-line proxy setting.

Newly started proxy logs appear in the launcher window, not a second console.
Keep the launcher window open while browsing. Closing the last Chrome window
automatically stops the proxy, including an already running proxy reused by
the launcher. Chrome background mode is disabled for this launch so background
apps do not keep the proxy running. A Chrome startup failure also stops the
proxy. Do not close the proxy console while Chrome is using it.

To stop using the proxy, close Chrome and reopen it with its usual shortcut.
HTTP URLs connect directly.

The launcher uses Windows PowerShell with the process-scoped `RemoteSigned`
execution policy. It does not change the saved execution policy; enforced
organization policies may prevent the script from running.

## Manual check

Close Fiddler before comparing the results:

```powershell
curl.exe -v --noproxy "" --proxy http://127.0.0.1:8899 https://example.com/
curl.exe -v --noproxy "*" https://example.com/
```

Replace the URL with a destination you are authorized to access. The first
command should report `200 Connection Established`, followed by the server's
HTTPS response. Certificate verification remains enabled.

For a browser, manually select the HTTPS proxy `127.0.0.1:8899` in its supported
proxy settings. Some browsers use Windows proxy settings instead. Remember to
undo any manual configuration before stopping the proxy. Ordinary HTTP URLs
are not supported and return `501 Not Implemented`.

## Limits

### DNS privacy

Domain names are resolved by Hickory over HTTPS (HTTP/2) using Cloudflare's
`https://cloudflare-dns.com/dns-query`. The DNS service IPs are configured
directly (1.1.1.1, 1.0.0.1 and their IPv6 counterparts); no system DNS lookup
is needed to bootstrap the service. TLS certificates are checked against
bundled public WebPKI roots. Certificate verification is never disabled.

If Cloudflare fails (including TLS errors, SERVFAIL, or a 3-second provider
deadline), the resolver tries Google DoH at `https://dns.google/dns-query`,
bootstrapped with 8.8.8.8, 8.8.4.4 and their IPv6 counterparts. Google also
has a 3-second deadline. Provider failover is logged. Successful Cloudflare
answers and definitive negative answers (NXDOMAIN or no A/AAAA records)
do not query Google. Failure to connect to a resolved site does not trigger
DNS failover.

Both A and AAAA records are requested, and results are cached according to
DNS TTLs. Connections use the resulting IP addresses directly. IP literals
skip DNS. System DNS settings, search suffixes and hosts-file overrides are
not used. Local and corporate-only names may therefore stop resolving.

There is no plaintext DNS fallback. A failed DoH lookup is logged and returns
502; the overall 10-second DNS/connection deadline returns 504 if exceeded.
If both DoH providers fail, domain-name connections fail rather than switching
to system DNS. The existing 10-second overall DNS/connection deadline remains.

Cloudflare can see queried names; Google also sees names sent during failover.
Destination IPs and potentially TLS server
names remain visible to the network. This does not encrypt independent DNS
queries from Chrome or other programs, or hide activity from endpoint software.
CONNECT destinations are still printed to the console.

After upgrading, fully close Chrome and stop the old proxy console before
rebuilding and relaunching. The launcher reuses an already running proxy,
which may still be an older build.

### Other limits

- This is a CONNECT tunnel, not a complete replacement for Fiddler.
- Matching Fiddler's connectivity is not guaranteed. DNS, address selection,
  browser settings, and process-specific network filtering can differ.
- Headers are limited to 16 KiB and 64 fields, with a 10-second read deadline.
  Opening an upstream connection, including DNS, has a 10-second deadline.
  Established tunnels have no idle deadline.
- Invalid requests, connection failures, and timeouts are logged to stderr.
  CONNECT destinations and completed tunnel byte counts are logged to stdout.
- No authentication or destination restrictions. Any local process can use
  the listener, including to reach local/private services. Do not expose it
  to other machines or use it as an untrusted multi-user service.
- There is no connection cap or automatic restart. This is a diagnostic
  prototype, not a production proxy.

## Development

```powershell
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Launcher tests use Windows PowerShell and Pester (included with Windows
PowerShell on Windows 10/11):

```powershell
powershell.exe -NoProfile -Command "Invoke-Pester .\tests\Start-Chrome.Tests.ps1 -EnableExit"
```

Tests use loopback servers and do not require external network access.
An optional live test forces primary-provider failure and resolves through
Google DoH: `cargo test live_google_fallback -- --ignored --nocapture`.

## License

MIT. See [LICENSE](LICENSE).
