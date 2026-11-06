# Runtime requirements (using it, not building it)

## TL;DR

**Nothing to install.** A static source build pulls in no shared libraries at all,
so no `libxcb`, no `libX11`, no `libdbus`, no `libxdo`, no `libv4l`, and no
glibc-version dependency. TLS is provided by `rustls`/`ring`, not OpenSSL, so the
secure transport also adds no shared-library requirement.

```bash
cargo build --release --target x86_64-unknown-linux-musl
./target/x86_64-unknown-linux-musl/release/reaven --version
```

The checked-in `artifacts/reaven-0.2.0-linux-x86_64` was built before the WSS
change and still speaks plaintext TCP; build from source for a TLS-capable
binary. The same binary cargo produces is statically linked when the musl target
is used:

```
$ file target/x86_64-unknown-linux-musl/release/reaven
... statically linked ...
```

For the Android app: an Android 8.0+ device and a network connection. A TLS relay
needs no extra app install because the system CA store is used.

## Network and TLS

The relay normally serves TLS and a WebSocket upgrade; there is no public
plaintext listener. Agents, CLI operators and the Android app all connect over
WSS, normally to `wss://host:443/ws`.

| Mode | Server flags | Requirements |
|---|---|---|
| **Automatic Let's Encrypt** | `--acme-domain relay.example.com [--acme-email you@example.com]` | public DNS name pointing at the relay; TCP **443** reachable from the internet; writable ACME cache |
| **Existing certificate** | `--tls-cert fullchain.pem --tls-key privkey.pem` | PEM chain and matching unencrypted private key readable by the relay |
| **Self-signed** | `--self-signed` | local testing; every client needs `--insecure` |
| **Local reverse proxy** | `--plaintext --listen 127.0.0.1:8443` | loopback bind only; nginx/Caddy terminates public TLS |

ACME uses **TLS-ALPN-01**, so only TCP 443 is needed — there is no HTTP-01
challenge and no port 80 listener. The certificate is obtained on first start and
renewed in the background. Keep `~/.reaven/acme` (or `--acme-cache DIR`) on
persistent storage and readable only by the relay user. `--acme-staging` issues
certificates from the Let's Encrypt staging CA, which clients will not trust by
default; use it to test the flow, not for production.

Binding 443 normally needs root or `CAP_NET_BIND_SERVICE`. If another service
owns 443, listen on a higher port and forward **external TCP 443** to it — for
example `--listen 0.0.0.0:8443` plus a DNAT/load-balancer rule. The ACME
validation still arrives on external 443. A reverse proxy must pass TLS through
to the relay (TCP or TLS passthrough); terminating TLS at an HTTP proxy requires
the plaintext mode below, because the relay normally has no plaintext backend.

### nginx terminating TLS

If nginx owns 443 and manages the certificate, run the relay on loopback with TLS
disabled:

```bash
reaven server --listen 127.0.0.1:8443 --plaintext --password 'long-random-secret'
```

```nginx
server {
    listen 443 ssl;
    server_name relay.example.com;

    ssl_certificate     /etc/letsencrypt/live/relay.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.example.com/privkey.pem;

    location /ws {
        proxy_pass http://127.0.0.1:8443;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
        proxy_buffering off;
    }
}
```

The relay refuses `--plaintext` on a non-loopback address, so the unencrypted
listener cannot be exposed by accident. External agents/operators still use
`wss://relay.example.com/ws`; only the nginx-to-relay hop is plaintext. The
certificate is managed by nginx (for example certbot), not by Reaven. For a direct
local test, connect with `ws://127.0.0.1:8443/ws`.

Clients validate the certificate and hostname against the built-in WebPKI roots.
Use `--insecure` (or `REAVEN_INSECURE=true`) only for a self-signed/local relay:
it still encrypts the connection but disables authentication, so it does not
protect against a man-in-the-middle.

```bash
# Public relay
sudo reaven server --acme-domain relay.example.com \
  --acme-email you@example.com --password 'long-random-secret'

# Local self-signed relay + client
reaven server --listen 127.0.0.1:17443 --self-signed --password 'test-secret'
REAVEN_INSECURE=true reaven peers --server 127.0.0.1:17443
```

## Why this matters

Screen capture used to be built on `xcap`, which links `libxcb.so.1` and
`libdbus-1.so.3`. Because the dynamic loader resolves those *before `main()`
runs*, a machine without them could not start the binary at all — so even
`reaven exec`, which never touches a screen, died with:

```
reaven: error while loading shared libraries: libxcb.so.1: cannot open shared object file
```

Capture is now implemented directly on `x11rb`, which speaks the X protocol over
the existing socket and needs no C library. Two consequences:

* command-line use works on a machine with **no display and no X libraries**;
* desktop commands fail with a message instead of preventing startup:

```
$ reaven screenshot --server host --target pc
Error: connecting to the X server (is DISPLAY set? under Wayland, capture needs
XWayland): $DISPLAY variable not set and no value was provided explicitly
```

## Per-feature requirements

| Feature | Needs |
|---|---|
| relay | TCP 443 (or loopback behind a TLS proxy), a TLS mode or `--plaintext`, root/`CAP_NET_BIND_SERVICE` for 443 |
| agent / operator | outbound TCP 443, DNS, a trusted certificate (or `--insecure` for self-signed) |
| relay, `exec`, `cl`, upload/download | nothing else — works headless |
| screenshot / screen stream | an X11 display (`DISPLAY`), or XWayland on Wayland |
| mouse + keyboard control | same as above |
| webcam | a `/dev/video*` device + membership of the `video` group |
| browser viewer | any browser on the machine you are sitting at |
| `install-autostart` | `flock` from util-linux |

Fast check before blaming Reaven:

```bash
echo "$DISPLAY"                 # empty -> no X session for this process
xdpyinfo | head -5              # can we talk to the display at all?
id -nG | tr ' ' '\n' | grep video   # webcam permission
ls /dev/video*                  # webcam devices
```

## Wayland

Capture and control run through **XWayland**. That covers X11 applications,
which is most of them, but:

* a native Wayland window may be missing from the capture;
* clicks and keys only reach XWayland clients, not native Wayland ones.

Native-Wayland capture needs an `xdg-desktop-portal` + PipeWire backend, which
this build does not include — that backend is exactly what used to introduce the
`libdbus`/`libxcb` dependency. If you need it, the old `xcap` path can be
restored as an optional feature.

## What the watched machine shows

**Nothing.** There is no banner, border, tray icon, cursor highlight, flash or
animation when another device views the screen. Capture is a bare framebuffer
read (`xproto.get_image`) over the X socket, and the agent never creates a
window, so there is nothing on screen to suppress or turn off — X11 has no
"someone is capturing" notification for a client to trip in the first place.

This is asserted by the test suite rather than assumed: while a stream is
running, `tests/e2e.sh` checks that the watched display's window tree and its
pixels are identical to before (currently `windows=3, diff=0`).

Where an indicator *would* come from:

| Situation | What appears |
|---|---|
| X11 or XWayland capture (this build) | nothing |
| native-Wayland capture via the desktop portal | the compositor's own "screen is being shared" indicator. GNOME and KDE draw it, not the application, and no application can switch it off. This build does not use the portal — see [Wayland](#wayland). |
| Android | nothing: the app never touches `MediaProjection`, so no cast icon appears on the phone |

One honest limit: this is about the *screen*, not about being undetectable.
Anyone who can log in to the watched machine can still see that it is being
accessed — the agent is a process in `ps`, it writes every stream, shell command
and rejected input to its local log on purpose, and there is traffic to the
relay. A server you administer should not be a machine that silently watches its
users, so the log is not something to remove.

## Android

| Requirement | Value |
|---|---|
| Android version | 8.0 (API 26) or newer |
| Permissions | `INTERNET`, `ACCESS_NETWORK_STATE` only |
| Network | WSS to the relay on port 443, with a certificate the system trusts |
| Play Services / extra libs | none |
| Storage permission | none (saves via MediaStore) |

The checked-in `android/artifacts/reaven-control-0.2.0-debug.apk` predates the WSS
client and still uses plaintext TCP. Rebuild from `android/` to get the secure
client:

```bash
cd android && ./gradlew assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

For a local self-signed relay, enable **Allow self-signed certificate** on the
Connect screen. Leave it off for a Let's Encrypt relay: the app then validates
the certificate against the Android system CA store.

It is a **debug** build — fine to sideload, but not release-signed, so Play
Protect will ask for confirmation.

## Building it yourself

If you would rather build (or need a different architecture):

```bash
cargo build --release                                       # dynamic
cargo build --release --target x86_64-unknown-linux-musl    # static
```

The only build-time packages that matter are a C toolchain for `ring`'s crypto
assembly (and Perl on some platforms), `libclang-dev` (for the webcam v4l2
bindings) and `pkg-config`. Drop webcam support and you need neither `libclang-dev`
nor `pkg-config`; the C toolchain is still needed for TLS:
`cargo build --release --no-default-features --features screen,input`.
