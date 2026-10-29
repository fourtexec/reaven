# Reaven

A small Rust CLI prototype for a server-relayed remote shell and file copy between Linux computers.

## Build

```bash
cargo build --release
sudo install -m 0755 target/release/reaven /usr/local/bin/reaven
```

## Start the relay server

```bash
reaven server --listen 0.0.0.0:7331 --password 'use-a-long-random-secret'
```

`--password` is optional; it can also be supplied with `REAVEN_PASSWORD`. If omitted, the server accepts unauthenticated connections. Do not expose that mode to the public Internet. The peer name is the client identity and should be unique.

## Keep a computer online

On each computer that should be reachable:

```bash
reaven connect --server your-server.example:7331 --name laptop --password 'use-a-long-random-secret'
```

Keep this process running. To install an idempotent interactive-shell autostart entry in `~/.bashrc`:

```bash
reaven install-autostart --server your-server.example:7331 --name laptop --password 'use-a-long-random-secret'
```

The installer leaves `.bashrc` unchanged if its Reaven marker is already present. It starts one client per user via `flock` (install `flock`/util-linux if missing). The password is stored in plaintext in `.bashrc` and should only be used with a protected account. Remove the marked block to uninstall. The daemon log is written to `~/.reaven.log`.

## Operate a remote computer

From another machine with the same server password, open a full interactive Bash shell:

```bash
reaven cl --server your-server.example:7331 --target laptop --password 'use-a-long-random-secret'
```

`cl` uses a real remote PTY, so interactive line editing, command history, Ctrl-C, job control, ANSI colors, and terminal resizing work. Type `exit` to leave the shell, or press **Ctrl-]** to terminate/detach the remote session. It requires an interactive local terminal.

Run a one-off command or transfer files:

```bash
reaven exec --server your-server.example:7331 --target laptop --command 'uname -a'
reaven upload --server your-server.example:7331 --target laptop ./local.txt /tmp/local.txt
reaven download --server your-server.example:7331 --target laptop /var/log/syslog ./syslog
```

Set `REAVEN_PASSWORD` instead of repeating `--password`.

## Security / prototype limitations

This is an early, unaudited prototype. The transport is plain TCP: the password, commands, and file data are not encrypted, and the relay is not suitable for an untrusted/public network as-is. Use a private network or an SSH/VPN tunnel, and firewall the relay port. A configured server password is a shared secret for every client; there is no per-device authorization, key management, audit trail, or command sandbox. Anyone holding that password can invoke bash and access files with the peer process's permissions. The no-password mode is especially unsafe. File transfers currently buffer the whole file in memory, so this is intended only for small files.
