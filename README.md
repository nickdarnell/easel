# Easel

[![Docker image](https://github.com/nickdarnell/easel/actions/workflows/docker.yml/badge.svg)](https://github.com/nickdarnell/easel/actions/workflows/docker.yml)

A photo manager for Samsung The Frame TVs. It runs as a small web app, so anyone on your Wi‑Fi
can open a page (or scan a QR code shown on the TV) and send photos to a chosen TV. No app is
needed. Before sending, you choose how each photo sits on the screen: fill it edge to edge, or
show the whole photo inside a matte. It also manages what's on each TV: browse, reframe, show,
delete, mattes, favorites, slideshow, art mode, folder sync, and Wake‑on‑LAN. Underneath is a
Rust library and the `frametv` command.

## Run it with Docker

```yaml
# compose.yaml
services:
  easel:
    image: ghcr.io/nickdarnell/easel:latest
    container_name: easel
    restart: unless-stopped
    network_mode: host
    environment:
      PORT: 3273   # the web app's port
      PUID: 1000   # the user and group that own ./config
      PGID: 1000
    volumes:
      - ./config:/config
```

```sh
docker compose up -d
```

Or with plain Docker:

```sh
docker run -d --name easel --restart unless-stopped --network host \
  -e PORT=3273 -e PUID=1000 -e PGID=1000 -v ./config:/config ghcr.io/nickdarnell/easel:latest
```

`PORT` (default 3273) is the port the web app listens on. With host networking there's no
`ports:` mapping, so change `PORT` if something else already uses 3273. `PUID` and `PGID`
(both default to 1000) set the user and group the app runs as. Use the output of `id -u` and
`id -g` for whoever should own `./config`. Everything else is configured in the web page and
saved in `/config`:

1. Open `http://<server-ip>:3273`.
2. Choose **Add TV** and pick your TV (or type its IP). **The TV asks whether to allow
   "Easel"; press Allow with the remote.** You have about a minute, and you only do this once
   per TV.
3. Open **App settings** (⚙) and set an admin password. Until you do, anyone on your network
   can delete photos or change settings.
4. Click **Share…** on a TV to get its guest link and QR code, for example
   `http://<server-ip>:3273/tv/Living%20Room`. **Show this QR code on the TV** puts the code
   on the TV itself, so guests can scan it with their phone camera. The slideshow pauses while
   it's up; **Done, back to my photos** puts back what was showing and resumes the slideshow
   (this also happens on its own after 30 minutes).

To build the image yourself instead of pulling it, run `docker compose up -d --build`.

`latest` follows the main branch. To stay on a release line, use a version tag instead:
`ghcr.io/nickdarnell/easel:0.1` (newest 0.1.x), `:0.1.0` (exactly that build), or `:0`. The
version comes from `Cargo.toml`, and every build is also tagged `sha-<commit>`. The image
carries the usual OCI labels, plus the web UI link and icon Unraid shows and Watchtower's
enable label.

### Portainer

Either paste the compose file above into **Stacks → Add stack**, or use the app template:
under **Settings → App Templates**, set the URL to
`https://raw.githubusercontent.com/nickdarnell/easel/main/portainer-template.json` (this
replaces Portainer's built-in list), then pick **Easel** from **App Templates**. The template
sets host networking, the environment variables and a `/config` volume.

To update, open the stack and choose **Update the stack** with **Re-pull image and redeploy**
on, or for a lone container, **Recreate** with **Re-pull image** on. Portainer's update
indicator (when turned on for the environment) compares the image you have with the registry's
for the same tag, so it works whether you follow `latest` or pin something like `0.1`.

### What's in /config

| Path | |
|---|---|
| `config.toml` | TVs and their pairing tokens, per‑TV defaults, and the app settings (password hash, share address, LocalSend). |
| `photos/<tv>/` | Every photo sent through the web page or LocalSend (or the TV's sync folder, if you set one). Reframing re‑crops from these originals. |
| `state/` | What was uploaded where, used to skip duplicates and to reframe. |
| `thumbs/` | Thumbnail cache (safe to delete). |
| `cache/` | The last photo list each TV sent, so pages still load while a TV is off or slow (safe to delete). |
| `pending/` | Changes waiting for each TV (see [Changes wait for the TV](#changes-wait-for-the-tv)). |

On start, the container hands `/config` to `PUID:PGID` if anyone else owns it.

**Forgot the password?** Delete the `admin_password` line under `[server]` in
`config/config.toml` and restart the container.

To sync a folder of photos to a TV, mount it (for example `- /srv/photos:/photos:ro`) and set
the TV's folder in **TV settings**.

### Networking: use host mode

Run the container with host networking (`network_mode: host`, or `--network host`). The app
needs to be on your LAN for TV discovery, Wake‑on‑LAN broadcasts and LocalSend's multicast
announcements, and it means share links show the server's real address. Host networking needs
a Linux Docker host (a home server, NAS, or Raspberry Pi). On Docker Desktop for
Windows/macOS, host mode is limited, so expect to type TV IPs and miss Wake‑on‑LAN.

Bridge networking (`ports: ["3273:3273", "53317:53317", "53317:53317/udp"]` instead of
`network_mode: host`, or `-p` with `docker run`) also works if you must. Leave `PORT` alone
then and change the left-hand side of the mapping instead.
Pairing by IP and uploads work, but discovery, Wake‑on‑LAN and LocalSend discovery don't. Set
the **share address** in App settings so QR codes don't point at the container's private
address.

### Security

Guests (anyone who can reach the page) can **see the photos on a TV and add new ones**.
Everything else needs the admin password (the lock button in the top bar). This is meant for a
home network. Don't expose it to the internet. If you need remote access, put it behind a VPN
or an authenticating reverse proxy.

## Framing

Choosing photos (button, drag and drop, or paste) opens the framing editor, which shows each
photo on a mock‑up of that TV's screen:

- **Fill the screen**: no border. Drag to choose what stays visible; zoom with the slider, the
  mouse wheel, or a pinch.
- **Show the whole photo**: nothing is cropped, and the TV puts a matte around it. Pick the
  matte style and color and see roughly how it will look.

It starts from what the TV prefers (its per‑TV framing and matte settings, marked
**Suggested**): photos close to the screen's shape fill it, others get a matte. With several
photos, a filmstrip lets you frame each one or **use this framing for all**.

Changed your mind later? Open a photo and choose **Reframe…**. It goes back to the original,
re‑uploads it with the new framing, and swaps it in (keeping it on screen if it was showing).

## Changes wait for the TV

Frame TVs are often asleep, slow to wake, or briefly unresponsive. So changes to photos and the
slideshow don't make you wait: adding, deleting, reframing, mattes, favorites, and slideshow
settings are written down at once and made on the TV whenever it answers, oldest first. They
survive a restart. You can keep working while the TV is off, and it catches up when it's back.

- A pill next to the TV's name says whether it's **up to date** or how many changes are
  **waiting** (or **failed**). Click it to see what the server is waiting on: what it's asking
  the TV right now and for how long, and each waiting change with its last error and when it
  will try again. From there you can **Try now** (which wakes the TV if it can) or **Cancel** a
  change.
- Photos waiting to be added show at the front of the grid. Photos with unsynced changes are
  marked, and ones being deleted are dimmed.
- A new change replaces an older waiting one it makes pointless (a second slideshow change, a
  second matte for the same photo, a change to a photo that's being deleted).
- The first try after you ask is eager: it wakes the TV and waits for it. Later retries only
  happen while the TV is on (they back off up to 5 minutes, and resume as soon as the TV
  answers). If the TV turns a change down three times, it's marked failed and kept until you
  retry or cancel it.

Things that only make sense now happen straight away instead: showing a photo, art mode, the
QR code, wake, and the remote.

**Remote**: the **Remote** button opens a remote control for the TV (power, home, source,
menu, arrows and OK, back, volume, channels, playback). Tapping power switches between art mode
and TV; holding it turns the TV off. The arrow keys, Enter, and Backspace work too.

## What guests see

`/tv/<name>` is a single‑purpose page: a big **Choose photos…** button, the framing editor,
and a "put it on the TV right away" toggle. Phones open their photo picker directly. Uploads show progress, and duplicates
are recognized ("Already on the TV"). iPhone HEIC photos work: Safari sends JPEG anyway, and the
Docker image decodes HEIC with libheif.

Admins get the full view: a "now showing" preview, art mode, a slideshow (all your photos or
just favorites, how often, shuffled or not), the photo grid with show / reframe / matte /
favorite / delete, for one photo or several at once (**Select**, or Ctrl/⌘/Shift‑click), per‑TV defaults (matte, portrait matte, fit,
folder), folder sync, and Wake. You can drag photos anywhere onto the page or paste a screenshot.
The page updates live when someone else sends a photo. On a phone, "Add to Home Screen" makes
it feel like an app.

Pages never talk to a TV directly. The server does that in the background, one request at a time
(that's all the TV handles), with commands first, then whatever a page is waiting for, then its own
upkeep. Pages get their answers from what the server last heard and are told when that changes, so
they load straight away even when a TV is slow. Requests from a page that was closed are dropped
before they reach the TV. Commands, though, always finish once started.

Requests don't time out when a TV is slow or stuck. They wait while the server gets it to answer,
and the page shows what it's doing ("Waking Fireplace…", with a timer). It reconnects first, then
sends Wake‑on‑LAN. If that doesn't work, it turns the TV off and on again by holding the power
button. Some Frames' art app stops answering until that happens. A restart happens at most once
every 15 minutes. A TV that says it's on (someone may be watching it) is only restarted for a
command such as sending a photo, or when a page has been waiting on it and a reconnect didn't help.

Photo previews come from the TV, which only hands out thumbnails (there's no way to download the
full photo back from it). If the TV is slow to send one, the page keeps waiting rather than giving
up, and previews that arrive late are still kept. Photos sent from Easel get a preview made from the
original if the TV has none.

## Command line

```text
frametv discover [--scan]          find TVs (SSDP, or probe the /24)
frametv pair <ip> [--name N]       press Allow on the TV
frametv tvs | configure <tv> … | forget <tv>
frametv status | list | current | thumbs | watch
frametv upload <files…> [--show] [--matte auto|none|shadowbox_polar…] [--fit auto|cover|contain|original]
frametv sync [folder] [--delete] [--dry-run]
frametv show <id> | delete <id…> | favorite <id>
frametv slideshow [off|minutes] [--sequential]   frametv artmode [on|off]
frametv mattes | matte <id> <matte>   frametv filters | filter <id> <filter>
frametv settings | set <item> <value>   frametv wake   frametv key KEY_POWER
frametv serve [--port 3273]        the web app (above)
```

Use `--tv NAME` (repeatable) or `--all` to target TVs, and `--wake` to wake a TV first.
Without Docker, config lives in `%APPDATA%\samsung-frame` on Windows and `~/.config/samsung-frame`
on Linux.

## TV setup notes

- **Wake‑on‑LAN**: enable *Settings → General → Network → Expert Settings → Power On with
  Mobile* (wording varies by year). The TV's MAC address is recorded when you pair.
- **Turned off vs. art mode**: in art mode the TV answers normally. When it's fully off (or
  the motion sensor has blanked it), it shows as standby or off until woken. Some TVs report
  standby even while showing art, so the server counts a TV as on whenever its art app answers.
- **Windows firewall** (running `frametv serve` natively): allow `frametv.exe` on private
  networks, or phones can't reach port 3273 / 53317.
- **Models**: tested with 2020 Frames (LS03T). 2022+ models use the same art API. Some 2024
  firmware dropped parts of it (e.g. slideshow/matte calls may be refused). Uploads and
  display work everywhere we know of.
- **HEIC**: always supported. Windows uses its built‑in decoder (if HEIC photos fail, install
  "HEIF Image Extensions" from the Microsoft Store). Linux and macOS use libheif, which the
  Docker image already includes.

## Building

```sh
# Linux needs: apt install pkg-config libssl-dev libheif-dev libheif-plugin-libde265
# macOS needs: brew install libheif
cd web && npm ci && npm run build && cd ..   # the web UI (Node 22+), embedded into the binary
cargo build --release
cargo test
```

The web UI is Vue + TypeScript in `web/`. Without `web/dist`, cargo still builds, but embeds a
placeholder page instead of the UI. While working on the UI, run the server and Vite's dev
server side by side; Vite reloads on save and passes `/api` through to the server:

```sh
cargo run --bin frametv -- serve     # http://localhost:3273
cd web && npm run dev                # http://localhost:5173
```

The API's TypeScript types in `web/src/api/` are generated from the Rust types in
`src/bin/frametv/api.rs` by [ts-rs](https://github.com/Aleph-Alpha/ts-rs) when you run
`cargo test`; commit them along with the change. Don't edit them by hand. CI fails if they're
out of date, and `npm run build` fails if the UI doesn't match them.

Before committing: `npm run format` (Prettier) in `web/`, and `cargo fmt` and
`cargo clippy --all-targets -- -D warnings` for the Rust side.
