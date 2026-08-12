# 🌍 BeMyVPN — "Be my VPN"

**A free internet, directly — with no service in the middle.** The exit node is
somebody's computer: usually a VPS or a home server, but nothing stops you from
sharing your connection from an ordinary PC or even a Raspberry Pi — if you feel
like helping. The person sharing presses one button; the person who needs access
presses one too. No sign-up, no payment.

> 🇷🇺 **Русская версия — [README.md](README.md)** (полная документация на русском).

> 💡 **Why?** To help people reach the parts of the internet where free speech and
> information still are. As a class of software this is an ordinary VPN — entirely legal.

---

## ⚠️ Read this first: the app's UI is Russian-only

The graphical and terminal interfaces currently speak **Russian only**. Strings live
inline in the code rather than in locale files; extracting them is on the roadmap and
[contributions are very welcome](#-contributing).

**But here is the part that matters if you want to help:** *running a host does not
require reading the UI.* All commands and flags are in English, and the headless
server path is two lines:

```bash
curl -fsSL https://raw.githubusercontent.com/mister-PARADISE/bemyvpn/main/install.sh | sh
sudo bemyvpn host --tunnel --autostart
```

That's it — your machine is now an exit node. The confirmation messages it prints
are Russian; [we translate them below](#-what-the-host-command-prints) so you know
exactly what you're looking at.

If you want to *use* the VPN as a guest, the Russian UI is a real barrier today.
If you want to *donate* bandwidth, it is not.

---

## 🤔 How it works

- 📡 **Host** — the one who **shares** their internet. Presses "become a host". Done.
- 🛡 **Guest** — the one who **connects**. Picks a host from the list (or enters a
  code), and all of their traffic goes through that host over an encrypted tunnel.
- 🗂 **Coordinator** — a bulletin board: it shows who is currently sharing and
  introduces the two of you. **Your traffic never passes through it at all** — not
  even encrypted. The tunnel is built directly between peers; the coordinator
  physically cannot see it.

```
      📡 HOST  ←═══ encrypted tunnel, DIRECT ═══→  🛡 GUEST
          ↖                                      ↗
            "I'm sharing!"        "who is sharing?"
                    ↘             ↙
                 🗂 COORDINATOR
        (introductions only — traffic goes around it)
```

- 🔐 Encryption is the same primitive as WireGuard (Noise, ChaCha20-Poly1305).
- 🙅 **No root/admin needed to share.** A host can be a phone, a laptop or a server.
- 📶 Sharing works without a public IP on **most** ordinary home connections
  (NAT is punched automatically). Connecting as a guest works from anywhere.

---

## 🖥 Become a host (this is what the project needs)

BeMyVPN has a chicken-and-egg problem in its purest form: **the directory often has
three hosts in it.** Someone who needs access opens the app, sees a nearly empty
list, and leaves for good. Stars don't fix that — running hosts do.

If you have a VPS with idle bandwidth, you are exactly who this project needs.

```bash
curl -fsSL https://raw.githubusercontent.com/mister-PARADISE/bemyvpn/main/install.sh | sh
sudo bemyvpn host --tunnel --autostart
```

The installer picks the right binary for your system, puts it on your `PATH` and
verifies that it starts. The second command installs a systemd unit, starts it, and
returns immediately.

> 🐳 **Already running Docker?** Even shorter — `packaging/docker` →
> `docker compose up -d`. Runs unprivileged, config survives container recreation.
> See [packaging/docker/README.md](packaging/docker/README.md).

**Your host survives logging out, losing SSH and rebooting.** Nothing to configure:
the name is taken from the machine's hostname, and the guest limit is derived from
available RAM. Check it the usual way:

```bash
systemctl status bemyvpn-host
```

> ⚠️ **`sudo` is required** — the unit is installed into `/etc/systemd/system/`.
> Without root the host only lives as long as your shell does.

### Optional flags

| Flag | Purpose |
|---|---|
| `--name "…"` | a custom directory name instead of the hostname |
| `--max 32` | a custom guest limit instead of the derived one |
| `--password …` | require a password; the network then is **always hidden** |
| `--hidden` | keep out of the public list (reachable by code only) |
| `--proto noise\|noise-obfs\|plain` | pick the protocol instead of the default "masked" mode |

Want to help just one specific person rather than the public? Use `--hidden` and
send them the network code. Nothing is listed publicly in that mode.

### 🖨 What the `host` command prints

Since the output is Russian, here is what you'll see:

| Russian output | Meaning |
|---|---|
| `Служба bemyvpn-host установлена и запущена.` | Service installed and started |
| `переживёт выход, обрыв SSH и перезагрузку` | Survives logout, SSH drop and reboot |
| `состояние: systemctl status …` | Check status with… |
| `выключить: systemctl disable --now …` | Disable with… |
| `Жду гостей (мультигость), Ctrl+C — выход.` | Waiting for guests (multi-guest); Ctrl+C to quit |
| `мои адреса: …` | My endpoints (after NAT discovery) |
| `жив ✅` | Alive (coordinator ping succeeded) |
| `пусто (нет живых хостов)` | Empty — no live hosts right now |
| `готово ✅` | Done |

---

## 📥 Download

Every file is a single binary — download and run, nothing to install.

| Device | Download |
|---|---|
| 🤖 **Android** | [`bemyvpn-android-arm64.apk`](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-android-arm64.apk) |
| 🍎 **macOS** (Apple Silicon) | [`bemyvpn-macos-arm64.dmg`](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-macos-arm64.dmg) |
| 🪟 **Windows** | [`bemyvpn-windows-x86_64.exe`](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-windows-x86_64.exe) |
| 🐧 **Linux** | [`bemyvpn-linux-x86_64.AppImage`](https://github.com/mister-PARADISE/bemyvpn/releases/latest/download/bemyvpn-linux-x86_64.AppImage) |

On macOS the first launch needs **right click → Open** (this is how macOS treats every
app not from the App Store). Removing that would require a paid Apple signature — we
deliberately don't buy one.

For terminals and servers there are `-terminal` builds of every platform in
[Releases](https://github.com/mister-PARADISE/bemyvpn/releases).

---

## 🏗 Architecture, and the one decision that shaped it

**The host needs no administrator privileges. Anywhere.**

Classic internet sharing means IP forwarding: a `tun` interface, `net.ipv4.ip_forward`,
`iptables -t nat -A POSTROUTING -j MASQUERADE`. On Windows it's ICS or RRAS; on macOS
`pfctl`; on Android, forget it. All of it needs root, all of it differs per OS, and all
of it scares away exactly the person we want to invite.

So the host never creates an interface and never touches the kernel. It receives the
guest's IP packets, **terminates TCP/UDP in a userspace TCP/IP stack**, and opens
**ordinary outbound sockets** — exactly like a browser does.

```
Guest:  OS apps → default route → TUN → IP packets → encrypt → host
Host:   decrypt → userspace TCP/IP stack → terminate TCP/UDP → ordinary sockets → internet
```

Privileges are therefore needed **only on the guest side**, because every OS requires
them for a TUN device. The asymmetry is the point: the person helping does nothing at
all, and the person who needs help confirms a single system dialog.

The cost is honest: a userspace stack is slower and more complex than kernel
forwarding. Getting it to full speed took window scaling (RFC 7323), a congestion
window, real receiver backpressure, working fast-retransmit and an adaptive RTO — all
of them tagged `BeMyVPN fork:` in [`vendor/ipstack/`](vendor/ipstack/).

### Repository layout

```
crates/         🦀 CORE (one codebase, linked into every shell):
  bmv-common      Link, keepalive, ids
  bmv-config      the single config file and every default
  bmv-protocol    protocols (noise / noise-obfs / plain)
  bmv-net         UDP, STUN, hole-punching, multi-guest
  bmv-signal      coordinator client (WebSocket)
  bmv-tunnel      userspace host stack + guest TUN
  bmv-core        orchestrator — the only facade the shells see
  bmv-desktop     TUN + routes for desktop (wintun embedded in the .exe)
  bmv-ffi         C-ABI bridge for mobile (Android JNI / iOS)
apps/           📱 SHELLS (thin front-ends; they do NOT duplicate VPN logic):
  bmv-cli         terminal `bemyvpn` (client + host + server + TUI)
  bmv-gui         desktop app (Slint) — Windows/Linux/macOS
  ios/            iOS + macOS (Catalyst) — Swift + NetworkExtension
  android/        Android — thin Kotlin shell over JNI
server/
  coordinator     directory + signalling (built into `bemyvpn server`; auto-HTTPS/ACME)
vendor/
  ipstack         userspace TCP/IP fork — our window-scaling and congestion fixes
```

### Protocols

| Name | What | When |
|---|---|---|
| `noise` | ChaCha20-Poly1305 (as in WireGuard) | when DPI isn't in the way |
| `noise-obfs` | + Elligator2, padding, header masking | **default** |
| `plain` | no encryption | trusted network, maximum speed |

Crypto comes from [`snow`](https://github.com/mcginty/snow) (the Noise framework, as
used inside WireGuard). We write none of our own.

The masked mode adds three layers against DPI:

1. **Elligator2** on the ephemeral X25519 key, so the first 32 bytes are
   indistinguishable from random rather than being a recognisable curve point. Via
   Tor's `curve25519-elligator2` crate.
2. **Padding with a per-session floor plus jitter**, inside the AEAD, with the length
   in the final byte. (A fixed floor was removed precisely because the shared constant
   was itself a fingerprint.)
3. **Header masking on the nonce**, in the style of QUIC header protection — so there
   is no monotonic counter visible on the wire.

There is **no protocol fallback**: exactly the selected protocol is used, and an
unknown name is never silently replaced by a "similar" one — otherwise a typo could
send you out in `plain` while you believed you were encrypted.

---

## 🔐 Security

- End-to-end Noise XX; DNS is always forced through the tunnel.
- **Protection in both directions.** Guests are denied the host's internal addresses
  (169.254.169.254 / 127.0.0.1 / LAN), including forms designed to look external —
  `::ffff:10.0.0.1` (v4-mapped), `64:ff9b::` (NAT64), `2002::` (6to4). And the host is
  denied the guest's home network: only packets addressed to the guest itself are
  accepted into the tunnel.
- **Anti-replay** with a 64-packet window, as in IPsec — a captured datagram cannot be
  replayed, including the goodbye message that used to be able to tear down a session.
- **The coordinator assigns each peer's address, not the client** — so NAT punching
  cannot be aimed at a third party, and the host network can't be turned into a reflector.
- Host records are bound to an owner token; network codes are signed by the server (HMAC).
- Coordinator flood limits: WS connections per IP, message size ceiling, advertised
  capacity ceiling, directory broadcast rate limiting, and a gate on concurrent
  handshakes at the host.
- Passwords are compared in constant time; a wrong one costs a deliberate delay.
- **The default host name is the country of your IP, not your machine's name** — a
  public directory should never carry someone's real hostname. Country and flag are
  resolved **on-device** from an embedded DB-IP database, with no third-party lookups.

### What we deliberately do NOT have

- **No relay.** If both sides are behind symmetric NAT or mobile CGNAT, the direct
  connection will not come up — and the app says so plainly instead of spinning.
- **No kill switch.** The config knob existed and was implemented nowhere, so it was
  *deleted rather than faked*. A real kill switch means firewall rules (a stuck rule
  leaves the machine with no network at all), and on Android an app cannot provide one
  — "block connections without VPN" is a system setting.
- **IPv6 is blocked, not tunnelled.** The tunnel carries IPv4 only; if IPv6 were left
  alone it would route around the tunnel while the UI said "Protected". So it is
  null-routed for the session (`::/1` + `8000::/1`).
- **DNS is not configurable.** Always through the tunnel.
- **No code signing.** Integrity comes from HTTPS to github.com. A signature would only
  protect against theft of the GitHub account itself, at the cost of an offline key and
  manual signing of every release.
- **No self-update on iOS** — Apple forbids apps updating themselves outside the App Store.

---

## 🌐 Run your own coordinator

Don't want to depend on the default one? Run yours — same binary, one minute. It
obtains and renews its HTTPS certificate **itself** (Let's Encrypt is built in; no
certbot, no nginx, no email required). You need a domain, an A record and port 443.

```bash
sudo bemyvpn server --domain your.domain --autostart
```

Then point clients at it — the coordinator list is an array in the config, not a
constant:

```toml
coordinators = ["https://bemyvpn.net", "https://your.domain"]
```

Block one, the other still works.

---

## 🛠 Building from source

Requires **Rust 1.85+**:

```bash
cargo build --release -p bmv-cli   # → target/release/bemyvpn (client + host + server + TUI)
cargo build --release -p bmv-gui   # → desktop GUI
```

Tests run in **three passes** — this is not an oversight. The coordinator and the
`ipstack` fork are intentionally outside the workspace (they have their own
dependencies), so `--workspace` does not see them and silently skips them:

```bash
cargo test --workspace                                    # core and shells
cargo test --manifest-path server/coordinator/Cargo.toml  # coordinator
cargo test --manifest-path vendor/ipstack/Cargo.toml      # ipstack fork
```

A fourth, the `wintun` fork, only builds on Windows and is tested there.

Android lives in `apps/android` (JDK 17 + Android SDK/NDK).

---

## 🤝 Contributing

The most useful thing you can contribute is **a running host**. After that:

- **Internationalisation.** The single biggest blocker for this project. UI strings are
  inline in the code across `apps/bmv-gui/ui/*.slint`, `apps/bmv-cli`, the Kotlin shell
  and the Swift shell. Extracting them into locale files would open the project to
  everyone who is not a Russian speaker. This is the highest-impact PR available.
- **Review of the crypto layer.** [`crates/bmv-protocol/src/noise.rs`](crates/bmv-protocol/src/noise.rs)
  is where an experienced outside eye is worth the most. We deliberately wrote no
  primitives of our own, but composition mistakes are exactly what authors miss.
- **Review of the TCP work.** [`vendor/ipstack/src/stream/tcb.rs`](vendor/ipstack/src/stream/tcb.rs)
  — the congestion control was written from RFCs and textbooks.
- **NAT traversal** in the environments we can't test: carrier CGNAT, corporate NAT.

---

## ❓ FAQ

**Is this legal?** Yes. As a class of software it is an ordinary VPN — the same
category as hundreds of VPN apps in every app store. The difference is that the exit
node doesn't belong to a service: it's someone's VPS, home server or computer, and the
traffic goes directly between the two of you.

**Am I liable for a guest's traffic?** We can't give you legal advice and won't
pretend to — jurisdictions differ. Technically, outbound connections carry your IP,
the same as any exit node anywhere. It's the same question people weigh about Tor exit
relays. If you're unsure, share in `--hidden` mode with a password and give the code
only to people you know.

**Can a guest reach my home network?** No. Internal addresses are denied by default,
in both directions.

**How is this different from just running WireGuard?** If you already have a server and
know how to configure it — it isn't, and you should use WireGuard. This is for the
other case: when someone has no server and will never configure one, and a stranger is
willing to help. And for the mirror case: when you have a server and nowhere to put
the willingness to help.

**Is this anonymity?** No. It's access. If you need anonymity you need Tor and an
understanding of how to use it. The coordinator does see that two peers were
introduced, and their IPs at that moment — it just never sees the traffic. If that
metadata matters to you, run your own coordinator.

**How is this different from Hola VPN?** Hola sold its users' bandwidth to a commercial
botnet-for-hire, without consent, to people who had no idea. Here, sharing is an
explicit action you take on purpose — a button you press, a service you install
yourself. Nothing turns you into an exit node in the background, nothing is resold,
and there's no company to sell it: no accounts, no payments, no ads, and no place in
the code for them. The whole thing is Apache-2.0 — check it.

**Cost?** Nothing. Open source, no sign-up, no subscriptions, no ads.

---

## 📄 License

Apache-2.0.

Host country and flag are resolved **on-device** from an embedded IP-to-country
database by [DB-IP](https://db-ip.com/db/download/ip-to-country-lite)
([CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)).
