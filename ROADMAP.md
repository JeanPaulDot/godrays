# Godrays roadmap: lightweight and ultrafast

Goal: the shortest path from "press A" to a sharp, responsive picture, with nothing running that the player does not need. This roadmap compares Godrays with Parsec and Moonlight, sets measurable targets, and orders the work so that every step is measured before and after.

Sources for the comparison: the Moonlight-qt README and release notes (v6.2.0), the Sunshine and Apollo READMEs, and parsec.app's technology, features, pricing and Warp pages, read in November 2026. Parsec's support articles could not be read, so Parsec details below come only from its public product pages.

---

## 1. Where we stand

| | Parsec | Moonlight-qt + Sunshine / Apollo | Kyber + Godrays today |
|---|---|---|---|
| **Transport** | Proprietary "BUD" protocol over UDP, DTLS 1.2, peer-to-peer | UDP with forward error correction; end-to-end encryption with Sunshine 0.22+ | QUIC (kymux). Video and audio each choose reliable, unreliable or unreliable + FEC |
| **Internet without port forwarding** | Yes, built in (vendor claims 97% NAT traversal). Relay server listed only for Enterprise | No: port forwarding, a VPN, or Tailscale-style tools | No: same as Moonlight |
| **Codecs** | H.264 (stated on its technology page) | H.264, HEVC, AV1 | H.264, HEVC, AV1 (Kyber calls AV1 experimental) |
| **Client renderer** | Zero-copy GPU pipeline, hardware decode | Purpose-built: D3D11 with VRR and overlapped decode/render, Vulkan/libplacebo, Metal, KMSDRM with async flip | Modified VLC |
| **4:4:4 colour** | Paid (Warp), NVIDIA hosts | Experimental, Sunshine hosts only | Yes (Kyber option; depends on the host's encoder) |
| **HDR** | Not stated on the pages read | Yes | No |
| **Virtual display matched to the client** | Virtual monitors (Warp) | Apollo creates one automatically at the client's resolution and frame rate | Godrays switches the real display's resolution; Kyber's own virtual display driver is not used yet |
| **Pairing and discovery** | Account-based | PIN pairing via Sunshine's web UI; mDNS discovery | Typed address and password; LAN scan of port 8080; trust-on-first-use certificates |
| **Several players at once** | Yes | Apollo: per-client permissions | Kyber supports it (`multi_client`); Godrays shows the player count but has no per-player controls |
| **Gamepads** | Keyboard, mouse, gamepad | Rumble and motion for up to 16 players, touchpad, DualSense adaptive triggers | Rumble via Kyber; motion and touchpad unknown |
| **Join from a browser** | No | No | Yes: Kyber's WebTransport web client, served by the host |
| **Measured latency** | Vendor claim: "adds only 7 ms" on a LAN test | Host processing latency shown in its overlay | Measured per frame and per stage, plus on screen (see §2) |
| **Setup** | Account, installer on both sides | Two different apps (Sunshine on the host, Moonlight on the client), then pairing | One app on both sides; installs Kyber itself; Join or Host |
| **Licence and price** | Free for personal use; Warp $9.99/month; Teams $35/user/month | GPL, free | Godrays MIT; Kyber AGPL or commercial; free |

### What this says

- **Our real advantages:** one app for both sides, QUIC with per-stream unreliable delivery (a lost video packet does not hold up newer frames or the audio), NVENC intra-refresh, a zero-install browser client, multi-player fan-out already in the server, and now pairing, discovery and measurement built in.
- **Our real gaps, host side:** encode time balloons when a game loads the GPU, no service mode (so no UAC prompts or lock screen for the remote side, and no virtual displays), no internet play without port forwarding, no adaptive bitrate.
- **Decided: no custom player.** The client keeps Kyber's own player. Its render step (about 2.8 ms) is the largest client-side cost we measured, but replacing it would mean owning a video pipeline for under 3 ms. All work now goes into the host.

---

## 2. Measured baseline and targets

### What "1 ms" can and cannot mean

Kyber's own site claims about 8 ms on average ("below perception, 8–10 ms"). The part of the pipeline that really is around a millisecond is the network hop: we measure 0.3–0.5 ms on this PC. The whole path from the host's screen to the client's screen cannot be close to 1 ms. The host has to wait for the next frame to be composed before capturing it, and the client has to wait for its display's next refresh: at 60 Hz a single refresh is 16.7 ms, at 165 Hz it is 6 ms. So we track two numbers and attack each part separately:

- **Pipeline:** Kyber's own per-frame timestamps, from capture on the host to "displayed" on the client, split into encode, network, decode and render. This is the number comparable to Parsec's "adds 7 ms".
- **On screen:** the beacon strip read twice from one screenshot, the original and its copy in the stream window. It adds the capture and display waits, and it works the same way for any streaming app, so it is the fair comparison with Moonlight and Parsec.

### Baseline, November 2026

Loopback on one PC: Radeon RX 5700 XT, desktop at **1920 × 1080, 165 Hz**, Kyber 0.29.0, measured with Godrays' latency lab. (An earlier version of this page said 2880 × 1620: that was the other PC's screen as reported to the client.)

Idle host:

| Encoder + codec | Pipeline median | p90 | Encode | Network | Decode | Render (VLC) | On screen |
|---|---|---|---|---|---|---|---|
| AMF + H.264 | 7.0–7.8 ms | 8.4–8.6 | 3.7–4.2 | 0.4 | 0.2 | 2.8 | 24 ms |
| AMF + HEVC | 8.6 ms | 11.8 | 4.7 | 0.4 | 0.3 | 2.8 | |
| x264 + H.264 | 31.8 ms | 38.1 | 27.6 | 0.7 | 0.4 | 2.7 | |

What did **not** help on this AMD host (encode stays at 3.6–3.8 ms):
- AMF options beyond Kyber's defaults (speed quality preset, CBR, no VBAQ, no HRD or filler, no sub-pixel motion search). Kyber already uses AMF's ultra-low-latency mode with no B-frames.
- A lower resolution: 1280 × 720 encodes in 3.6 ms, 1920 × 1080 in 3.7 ms. AMF's time is mostly fixed cost, not per-pixel work.
- Kyber's newer libavconv engine: equal when idle, and three to four times slower under GPU load (108 ms encode). The default (txproto) stays.

Host under full GPU load (a game stand-in holding the GPU at 98–100%, `examples/gpu_load.rs` with 256 draws per frame):

| | Encode | Pipeline median | p90 | Frame rate | On screen median, p90 |
|---|---|---|---|---|---|
| No priority change, or HIGH GPU priority | 34–37 ms | 41–44 ms | 52–60 ms | 84–95 fps | 62–65 ms, 79–90 ms |
| **REALTIME GPU priority** (now the default) | **28–30 ms** | **37–38 ms** | **46–48 ms** | **102–106 fps** | 61–62 ms, 71–77 ms |

Frame-rate cap (display at 60 Hz instead of 165 Hz while hosting; loopback on the same PC, so the client's display also ran at 60 Hz, which inflates the on-screen numbers at 60):

| | Encode | Pipeline median | On screen median |
|---|---|---|---|
| Idle, 165 Hz | 4.6–4.7 ms | 8.1–8.2 ms | 24–25 ms |
| Idle, 60 Hz | 5.9 ms | 9.0–9.6 ms | 50 ms |
| Full GPU load, 165 Hz (96–97 fps) | 10.9–11.3 ms | 17.3–18.8 ms | 47–48 ms |
| Full GPU load, 60 Hz (59 fps) | 5.7–6.0 ms | 17.7–17.8 ms | 50–52 ms |

60 fps halves the encoder's work under load but does not lower what you see; on an idle host the higher refresh rate wins clearly. 60 is the default to keep the GPU free for games; a two-PC measurement is needed to see the cap's real cost when only the host runs at 60 Hz.

A loaded GPU is the dominant host-side cost: encode goes from under 4 ms to about 30 ms, because Kyber's colour conversion queues behind the game's work. REALTIME scheduling priority recovers a fifth of it; the rest is the next target (see Phase 2).

Not measured yet: Moonlight + Sunshine and Parsec on the same hardware (the "Measure another streaming app" tool exists; it needs Moonlight paired with Sunshine on this PC), NVENC and Quick Sync hosts, and a two-PC run over a real network.

### Targets

| Metric | Now | Target |
|---|---|---|
| Pipeline median, idle host, wired LAN | 7.0–7.8 ms | ≤ 7 ms on every vendor (below Parsec's claim), measured per vendor |
| Encode under full GPU load | 28–30 ms | ≤ 12 ms |
| Pipeline p90 under full GPU load | 46–48 ms | ≤ 20 ms |
| Stream frame rate under full GPU load | 102–106 fps at 165 Hz | the display's full rate |
| Time from pressing A to the first frame, LAN | not yet measured | ≤ 1.5 s |
| Host features | — | UAC prompts and lock screen visible, headless hosting, virtual display at the client's resolution and refresh rate |

## 3. Phases

### Phase 0: Measure (weeks 1–2)

Without measurements, "ultrafast" is a guess. Everything after this phase reports its effect in these units.

- **0.1 Loopback latency test, automated.** Like the encoder test: start a private server and a client on the same PC. Godrays draws a small barcode on the host side that encodes a millisecond timestamp, grabs the screen, and decodes both the original barcode and the copy inside the stream window. The difference is the full pipeline latency (capture, encode, transport, decode, present) without the network. It is reproducible, so codecs, encoders, presets and renderers can be compared.
- **0.2 Two-PC glass-to-glass test.** The same barcode, with the two clocks aligned by ping-based offset estimation over the host agent channel (1.1). The client grabs its own screen as frames are presented and decodes the host time from the barcode.
- **0.3 In-stream statistics overlay.** Turn on Kyber's `--metrics` and show capture, encode, network, decode and present times plus bitrate and loss. Toggled by a shortcut, like Moonlight's.
- **0.4 Baseline report.** Godrays, Moonlight + Sunshine and Parsec on the same two PCs: 1080p60 and 1440p120, wired and Wi-Fi. Kept in the repository and re-run at each release.
- **0.5 Time to first frame.** Already visible in the client log ("Player started"); show it after each connection and record it.

### Phase 1: Zero friction, launcher only (weeks 2–6)

No changes to Kyber needed.

- **1.1 Host agent.** A small authenticated side channel run by Godrays on the host, over TLS with the host's certificate. Everything below that needs the two Godrays to talk goes through it: capabilities (client resolution, refresh rate, HDR), pairing, who is connected, clock alignment, starting hosting on request (only if the host allows it).
- **1.2 Instant discovery.** Advertise hosts with mDNS / DNS-SD (`_godrays._udp`): name, port and certificate fingerprint. Hosts appear in under a second instead of after a /24 scan. The client then pins that fingerprint instead of trusting the first certificate it sees.
- **1.3 Pairing code.** The host shows a 6-digit code. The client enters it, and both sides run a password-authenticated key exchange (SPAKE2), so the short code cannot be guessed offline. The exchange delivers the sign-in and the certificate fingerprint. Nobody types passwords again.
- **1.4 Automatic resolution match.** The client sends its display mode through the agent; the host switches to the closest mode (switching already exists) and restores it afterwards. "Match this device" becomes the default.
- **1.5 Pre-flight network check.** Before the first stream to a host, and when quality drops: round-trip time, jitter, loss and a short throughput burst, plus whether each side is on Ethernet, 5/6 GHz Wi-Fi or 2.4 GHz Wi-Fi. Pick the preset automatically: bitrate, FEC, codec. Warn plainly about 2.4 GHz.
- **1.6 Encoder chosen once, automatically.** Run the encoder test on the first hosting start, remember the result per graphics card and driver version, and re-test after a driver update.
- **1.7 Faster first frame.** When the client highlights a host card, resolve it, open the TLS session and ask the host agent to warm up the video service. Pressing A then only starts the stream. Measured with 0.5.
- **1.8 Join from a browser.** Show a QR code on the Host screen that opens Kyber's web client (`https://<host>:8080/webclient/`). Phones, tablets and TVs with a Chromium browser can join without installing anything.
- **1.9 Lighter launcher during streams.** Repaint only on events while streaming (no timer at all) and verify the CPU and memory targets.

### Phase 2: Host pipeline (now the main track)

- **2.1 Host under GPU load (in progress).** REALTIME GPU priority is done and measured. Next: find where the remaining ~25 ms goes when a game saturates the GPU (capture, colour conversion or the encoder's queue), using Kyber's per-stage stamps on more stages. Candidates: letting the encoder take the captured BGRA frame directly so the conversion runs on the encoder block instead of the busy 3D queue; a dedicated high-priority compute queue for the conversion (an upstream change to Kyber's txproto); advising a game frame cap a few frames below the display rate, which leaves the GPU the headroom it needs, and measuring how much that gives.
- **2.2 Service mode.** Host through Kyber's own Windows service instead of a user process, set up once with administrator rights. This brings what Parsec and Sunshine hosts have: Windows permission prompts (UAC) and the lock screen visible to the player, hosting before anyone logs in, and Kyber's virtual display driver (kidd). The launcher keeps start, stop, pairing and settings; the service runs the server.
- **2.3 Virtual display matched to the client.** With service mode, plug a virtual monitor at the client's resolution and refresh rate (the client sends them through the agent when it connects). A headless host just works, a handheld gets 1280 × 800 instead of a scaled 1080p picture, and a 240 Hz virtual display halves the host's wait for the next captured frame compared with 120 Hz.
- **2.4 Adaptive bitrate.** Lower the bitrate on loss or rising round-trip time and raise it back slowly, driven from the host, so Wi-Fi players get a stable picture instead of stalls.
- **2.5 Per-vendor checks.** Run the lab on NVIDIA (NVENC: Kyber uses preset p2 with one frame of output delay; p1 with no delay is the obvious candidate) and Intel (Quick Sync low-power mode), and keep a profile only where it measures faster, as with AMF.
- **2.6 Audio.** Shorter Opus frames as an option, surround sound, microphone pass-through (to check in Kyber).

### Phase 3: Client player — dropped

Writing our own video player to replace Kyber's VLC-based one could save at most the 2.8 ms render step, at the cost of owning decoding and presentation on every platform, and a licence question (Kyber is AGPL). Not a good trade: the client stays on Kyber's player.

### Phase 4: Internet play (in parallel, weeks 8–16)

Parsec's largest practical advantage is that it works across the internet without router setup.

- **4.1 VPN-aware.** Detect Tailscale and ZeroTier and list peers on the virtual network as hosts. Cheap, and works today.
- **4.2 Automatic port mapping.** UPnP-IGD, NAT-PMP and PCP on the host, for UDP and TCP on the hosting port, only with the user's consent, removed when hosting stops.
- **4.3 Peer-to-peer connection.** QUIC hole punching through a small, self-hostable rendezvous service, with a relay as fallback. Kyber's kyhub already models relay endpoints; the client would need to accept a prepared connection or a relay address.
- **4.4 Safe by default.** Never expose a host to the internet without pairing (1.3). Rate-limit sign-in attempts. Offer OIDC sign-in for those who want accounts.

### Phase 5: Reach (later)

- Steam Deck and Linux clients (Kyber's client builds on Linux); Android and iOS when Kyber publishes them; TVs through the web client.
- HDR10 end to end (HEVC Main 10 or 10-bit AV1).
- Couch co-op with per-player controls: who is connected, gamepad-only guests, kick. Kyber already fans one stream out to several clients.
- Several monitors.

---

## 4. Order of work

**Sprint 1, Measure: done.**
- Latency lab: Kyber on this PC with Kyber's per-stage timestamps and the on-screen beacon (0.1).
- "Find the fastest encoder and codec": measures every pair the graphics card offers and makes the fastest working one the hosting default.
- Live statistics over the stream (0.3), toggled with Ctrl+Alt+Shift+S.
- "Measure another streaming app" for the Moonlight/Parsec baseline (0.4): the tool is in; the comparison run itself is still to do.
- Not done: the two-PC glass-to-glass test with clock alignment (0.2) and the time-to-first-frame display (0.5).

**Sprint 2, Connect without typing: done.**
- Host agent (1.1) with an encrypted channel and a status request.
- mDNS discovery (1.2): hosts that are hosting or pairing appear on the Join screen within a second.
- Pairing code (1.3): 6 digits, SPAKE2, works once, three wrong tries burn it; hands over the sign-in, pins the certificate fingerprint (checked against Kyber's client: the right one connects, a wrong one is refused).
- The client streams with the codec the host measured fastest, when the host says.

**Sprint 3, Host under load: done in part.**
- Measured AMF options, resolution and Kyber's second engine: none help on this host (details in §2).
- Measured the host under full GPU load and found the main problem: encode from under 4 ms to about 35 ms.
- REALTIME GPU priority for the server processes, falling back to HIGH: encode −20%, worst case −12 ms under load. On by default ("Stream ahead of games" on the Host screen).
- Fixed a cause of "connected, but no picture": a test or hosting port free for TCP but taken for UDP by another program. Ports are now checked for both.
- GPU load generator (`examples/gpu_load.rs`) so every host change can be measured under load.

**Sprint 4, Host:**
1. Where the remaining encode time under load goes, and the fixes in 2.1.
2. Service mode (2.2).
3. Virtual display matched to the client (2.3).

**Sprint 5:** adaptive bitrate (2.4), per-vendor checks on NVIDIA and Intel hosts (2.5), automatic port mapping (4.2).

## 5. Staying lightweight

- No accounts and no cloud service required to play at home.
- No embedded browser or web runtime; the launcher stays one native executable under 10 MB.
- Godrays costs nothing while a stream runs: no animation, no polling beyond what the stream needs.
- Do not duplicate Kyber: when something belongs in the streaming engine, contribute it upstream rather than rebuilding it in Godrays.

## 6. Risks

- **Kyber is young** (0.29) and does not take community contributions yet. Upstream items (the colour-conversion queue in 2.1, 4.3) may take time or need a fork.
- **Licence:** Godrays is MIT and runs Kyber (AGPL or commercial) as separate programs. Anything that would link Kyber's code into Godrays itself needs a licence decision first.
- **Service mode needs administrator rights once**, and Kyber's service and Godrays' own hosting must never run on the same port at the same time.
- **REALTIME GPU priority** was granted without administrator rights on this PC; other Windows versions or drivers may refuse it, in which case Godrays falls back to HIGH, which measured no gain.
- **Windows first:** display switching, the hotkey, the quit combo and the installer flow are Windows-only today.
- **Vendor claims are not measurements:** Parsec's 7 ms and 97% figures are its own. Phase 0's baseline is what we compare against.
