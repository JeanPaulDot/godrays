//! The Godrays agent: a small side channel from one Godrays to another.
//!
//! The hosting PC runs it while it hosts or while pairing is open. It
//! answers two requests:
//!
//! * **pair**: the host shows a 6-digit code; the client proves it knows the
//!   code with SPAKE2 (a password-authenticated key exchange, so the code
//!   cannot be guessed offline from what crosses the network, and each
//!   connection is one guess). The agreed key then encrypts the hand-over:
//!   the sign-in, the certificate fingerprint to pin, and a per-device
//!   secret for later requests.
//! * **status**: a paired client asks, with its device secret, whether the
//!   host is hosting, how many players are connected, and which codec
//!   measured fastest there.
//!
//! Messages are length-prefixed JSON over TCP; anything secret travels in
//! a ChaCha20-Poly1305 box.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::config::Codec;
use crate::secrets;

pub const DEFAULT_PORT: u16 = 8081;
const MAX_FRAME: u32 = 64 * 1024;
const PAIRING_TIME: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: u32 = 3;
const IDENTITY: &[u8] = b"godrays-pair-v1";
const CONFIRM: &[u8] = b"godrays-pair-confirm-v1";

// ------------------------------------------------------------------ wire

fn send(stream: &mut TcpStream, message: &Value) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .and_then(|()| stream.write_all(&body))
        .map_err(|e| format!("The connection broke: {e}"))
}

fn recv(stream: &mut TcpStream) -> Result<Value, String> {
    let mut length = [0u8; 4];
    stream
        .read_exact(&mut length)
        .map_err(|e| format!("The connection broke: {e}"))?;
    let length = u32::from_be_bytes(length);
    if length > MAX_FRAME {
        return Err("Message too large.".into());
    }
    let mut body = vec![0u8; length as usize];
    stream
        .read_exact(&mut body)
        .map_err(|e| format!("The connection broke: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

fn field<'a>(message: &'a Value, name: &str) -> Result<&'a str, String> {
    message[name]
        .as_str()
        .ok_or_else(|| format!("Missing \"{name}\" in the reply."))
}

fn fail_on_error(message: &Value) -> Result<(), String> {
    if message["op"] == "error" {
        return Err(message["message"]
            .as_str()
            .unwrap_or("The host refused.")
            .to_owned());
    }
    Ok(())
}

// --------------------------------------------------------------- crypto

/// A direction-specific key from shared key material.
fn derive(material: &[u8], label: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update(material);
    hasher.finalize().into()
}

fn seal(key: &[u8; 32], plaintext: &[u8]) -> String {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).expect("the OS random source is unavailable");
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .expect("encryption of a short message cannot fail");
    let mut out = nonce.to_vec();
    out.extend(sealed);
    B64.encode(out)
}

fn open(key: &[u8; 32], boxed: &str) -> Option<Vec<u8>> {
    let raw = B64.decode(boxed).ok()?;
    if raw.len() < 12 {
        return None;
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(&raw[..12]), &raw[12..])
        .ok()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).expect("the OS random source is unavailable");
    bytes
}

/// A fresh 6-digit code, uniformly random.
pub fn new_code() -> String {
    loop {
        let n = u32::from_le_bytes(random_bytes::<4>());
        // Reject the top sliver so every code is equally likely.
        if n < 4_294_000_000 {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Kyber's certificate fingerprint format, `sha256:aa:bb:...`, of a PEM file.
pub fn fingerprint_of_pem(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let body: String = text
        .lines()
        .skip_while(|line| !line.starts_with("-----BEGIN CERTIFICATE"))
        .skip(1)
        .take_while(|line| !line.starts_with("-----END"))
        .collect();
    let der = B64.decode(body.trim()).ok()?;
    let hash = Sha256::digest(der);
    let hex: Vec<String> = hash.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("sha256:{}", hex.join(":")))
}

// ------------------------------------------------------------- payloads

/// What a successful pairing hands to the client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub host_name: String,
    pub kyber_port: u16,
    pub username: String,
    pub password: String,
    /// `sha256:...` of the host's certificate, to pin.
    pub fingerprint: String,
    pub client_id: String,
    /// Base64 device secret for later requests.
    pub client_secret: String,
    pub codec: Option<Codec>,
}

/// What a paired client may ask about.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostStatus {
    pub host_name: String,
    pub hosting: bool,
    pub players: usize,
    pub codec: Option<Codec>,
    pub best_latency_ms: Option<f32>,
}

/// What the host shares; the UI keeps it current.
#[derive(Clone, Debug, Default)]
pub struct HostInfo {
    pub host_name: String,
    pub kyber_port: u16,
    pub username: String,
    pub password: String,
    pub fingerprint: String,
    pub hosting: bool,
    pub players: usize,
    pub codec: Option<Codec>,
    pub best_latency_ms: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Paired { id: String, name: String },
    WrongCode,
}

struct Pairing {
    code: String,
    until: Instant,
    attempts: u32,
}

struct Shared {
    info: Mutex<HostInfo>,
    pairing: Mutex<Option<Pairing>>,
    events: Mutex<Vec<Event>>,
    stop: AtomicBool,
}

/// The listening side, on the hosting PC.
pub struct Agent {
    shared: Arc<Shared>,
    pub port: u16,
}

impl Agent {
    pub fn start(port: u16, wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| {
            format!("Port {port} is in use on this PC, so pairing cannot start ({e}).")
        })?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let shared = Arc::new(Shared {
            info: Mutex::new(HostInfo::default()),
            pairing: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let accept_shared = shared.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        std::thread::spawn(move || {
            while !accept_shared.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = accept_shared.clone();
                        let wake = wake.clone();
                        std::thread::spawn(move || {
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
                            let _ = handle(stream, &shared);
                            wake();
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });
        Ok(Self { shared, port })
    }

    pub fn set_info(&self, info: HostInfo) {
        *self.shared.info.lock().unwrap() = info;
    }

    /// Opens pairing with a fresh code, valid for two minutes.
    pub fn open_pairing(&self) -> String {
        let code = new_code();
        *self.shared.pairing.lock().unwrap() = Some(Pairing {
            code: code.clone(),
            until: Instant::now() + PAIRING_TIME,
            attempts: 0,
        });
        code
    }

    pub fn close_pairing(&self) {
        *self.shared.pairing.lock().unwrap() = None;
    }

    /// The open code and the time left, if pairing is open.
    pub fn pairing(&self) -> Option<(String, Duration)> {
        let mut pairing = self.shared.pairing.lock().unwrap();
        match &*pairing {
            Some(p) if p.until > Instant::now() => Some((p.code.clone(), p.until - Instant::now())),
            Some(_) => {
                *pairing = None;
                None
            }
            None => None,
        }
    }

    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.shared.events.lock().unwrap())
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

fn error(stream: &mut TcpStream, message: &str) -> Result<(), String> {
    send(stream, &json!({"op": "error", "message": message}))?;
    Err(message.to_owned())
}

fn handle(mut stream: TcpStream, shared: &Shared) -> Result<(), String> {
    let request = recv(&mut stream)?;
    match request["op"].as_str() {
        Some("pair") => host_pair(&mut stream, shared, &request),
        Some("status") => host_status(&mut stream, shared, &request),
        _ => error(&mut stream, "Unknown request."),
    }
}

fn host_pair(stream: &mut TcpStream, shared: &Shared, request: &Value) -> Result<(), String> {
    let code = {
        let pairing = shared.pairing.lock().unwrap();
        match &*pairing {
            Some(p) if p.until > Instant::now() => p.code.clone(),
            _ => {
                return error(
                    stream,
                    "Pairing is not open on that PC. On it, open Host > Show a code.",
                )
            }
        }
    };
    let name = request["name"].as_str().unwrap_or("A device").to_owned();
    let theirs = B64
        .decode(field(request, "spake")?)
        .map_err(|e| e.to_string())?;
    let (state, ours) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(code.as_bytes()),
        &Identity::new(IDENTITY),
    );
    let key = state.finish(&theirs).map_err(|e| format!("{e:?}"))?;
    send(stream, &json!({"op": "pair", "spake": B64.encode(ours)}))?;

    let confirm = recv(stream)?;
    let proven = field(&confirm, "box")
        .ok()
        .and_then(|boxed| open(&derive(&key, "c2h"), boxed))
        .is_some_and(|plain| plain == CONFIRM);
    if !proven {
        let mut pairing = shared.pairing.lock().unwrap();
        if let Some(p) = pairing.as_mut() {
            p.attempts += 1;
            if p.attempts >= MAX_ATTEMPTS {
                // Too many wrong codes: this one is burnt.
                *pairing = None;
            }
        }
        shared.events.lock().unwrap().push(Event::WrongCode);
        return error(stream, "Wrong code.");
    }

    let id: String = random_bytes::<8>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let secret = B64.encode(random_bytes::<32>());
    secrets::set_named(&format!("device-{id}"), &secret)?;
    let info = shared.info.lock().unwrap().clone();
    let welcome = Welcome {
        host_name: info.host_name,
        kyber_port: info.kyber_port,
        username: info.username,
        password: info.password,
        fingerprint: info.fingerprint,
        client_id: id.clone(),
        client_secret: secret,
        codec: info.codec,
    };
    let body = serde_json::to_vec(&welcome).map_err(|e| e.to_string())?;
    send(
        stream,
        &json!({"op": "welcome", "box": seal(&derive(&key, "h2c"), &body)}),
    )?;
    *shared.pairing.lock().unwrap() = None;
    shared
        .events
        .lock()
        .unwrap()
        .push(Event::Paired { id, name });
    Ok(())
}

fn host_status(stream: &mut TcpStream, shared: &Shared, request: &Value) -> Result<(), String> {
    let id = field(request, "client")?;
    // Ids are hex we made; anything else cannot be a device of ours.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return error(stream, "Unknown device.");
    }
    let Ok(Some(secret)) = secrets::get_named(&format!("device-{id}")) else {
        return error(stream, "This device is not paired with that PC any more.");
    };
    let fresh = field(request, "box")
        .ok()
        .and_then(|boxed| open(&derive(secret.as_bytes(), "status-c2h"), boxed))
        .and_then(|plain| serde_json::from_slice::<Value>(&plain).ok())
        .and_then(|body| body["time"].as_u64())
        .is_some_and(|time| time.abs_diff(now()) < 120);
    if !fresh {
        return error(stream, "The request could not be verified.");
    }
    let info = shared.info.lock().unwrap().clone();
    let status = HostStatus {
        host_name: info.host_name,
        hosting: info.hosting,
        players: info.players,
        codec: info.codec,
        best_latency_ms: info.best_latency_ms,
    };
    let body = serde_json::to_vec(&status).map_err(|e| e.to_string())?;
    send(
        stream,
        &json!({"op": "status", "box": seal(&derive(secret.as_bytes(), "status-h2c"), &body)}),
    )
}

// ---------------------------------------------------------------- client

fn connect(address: SocketAddr) -> Result<TcpStream, String> {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|e| format!("Could not reach {address}: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    Ok(stream)
}

/// Pairs with the host at `address` using the code it shows.
pub fn pair(address: SocketAddr, code: &str, my_name: &str) -> Result<Welcome, String> {
    let code: String = code.chars().filter(char::is_ascii_digit).collect();
    if code.len() != 6 {
        return Err("The code has 6 digits.".into());
    }
    let mut stream = connect(address)?;
    let (state, ours) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(code.as_bytes()),
        &Identity::new(IDENTITY),
    );
    send(
        &mut stream,
        &json!({"op": "pair", "name": my_name, "spake": B64.encode(ours)}),
    )?;
    let reply = recv(&mut stream)?;
    fail_on_error(&reply)?;
    let theirs = B64
        .decode(field(&reply, "spake")?)
        .map_err(|e| e.to_string())?;
    let key = state.finish(&theirs).map_err(|e| format!("{e:?}"))?;
    send(
        &mut stream,
        &json!({"op": "confirm", "box": seal(&derive(&key, "c2h"), CONFIRM)}),
    )?;
    let welcome = recv(&mut stream)?;
    fail_on_error(&welcome)?;
    let plain = open(&derive(&key, "h2c"), field(&welcome, "box")?)
        .ok_or("The host's answer could not be decrypted.")?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

/// Asks a paired host for its status.
pub fn status(address: SocketAddr, client_id: &str, secret: &str) -> Result<HostStatus, String> {
    let mut stream = connect(address)?;
    let body = serde_json::to_vec(&json!({"time": now()})).map_err(|e| e.to_string())?;
    send(
        &mut stream,
        &json!({
            "op": "status",
            "client": client_id,
            "box": seal(&derive(secret.as_bytes(), "status-c2h"), &body),
        }),
    )?;
    let reply = recv(&mut stream)?;
    fail_on_error(&reply)?;
    let plain = open(
        &derive(secret.as_bytes(), "status-h2c"),
        field(&reply, "box")?,
    )
    .ok_or("The host's answer could not be decrypted.")?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

/// Forgets a device's secret on the host.
pub fn forget_device(id: &str) {
    let _ = secrets::delete_named(&format!("device-{id}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        let agent = Agent::start(0, || {}).unwrap();
        agent.set_info(HostInfo {
            host_name: "RIG".into(),
            kyber_port: 8080,
            username: "kyber".into(),
            password: "abcd-2345-wxyz".into(),
            fingerprint: "sha256:aa:bb".into(),
            hosting: true,
            players: 1,
            codec: Some(Codec::Hevc),
            best_latency_ms: Some(8.4),
        });
        agent
    }

    fn address(agent: &Agent) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], agent.port))
    }

    #[test]
    fn codes_are_six_digits_and_vary() {
        let codes: Vec<String> = (0..20).map(|_| new_code()).collect();
        assert!(codes
            .iter()
            .all(|c| c.len() == 6 && c.chars().all(|d| d.is_ascii_digit())));
        assert!(codes.iter().any(|c| c != &codes[0]));
    }

    #[test]
    fn sealed_boxes_open_only_with_the_right_key() {
        let key = derive(b"material", "c2h");
        let boxed = seal(&key, b"hello");
        assert_eq!(open(&key, &boxed).as_deref(), Some(&b"hello"[..]));
        assert_eq!(open(&derive(b"material", "h2c"), &boxed), None);
        assert_eq!(open(&key, "not base64!"), None);
    }

    #[test]
    fn pairing_needs_an_open_code_and_the_right_one() {
        let agent = agent();
        let error = pair(address(&agent), "123456", "LAPTOP").unwrap_err();
        assert!(error.contains("not open"), "{error}");

        let code = agent.open_pairing();
        let wrong = if code == "000000" { "111111" } else { "000000" };
        let error = pair(address(&agent), wrong, "LAPTOP").unwrap_err();
        assert!(error.contains("Wrong code"), "{error}");
        assert!(agent.pairing().is_some(), "one wrong guess keeps it open");

        let welcome = pair(
            address(&agent),
            &format!("{} {}", &code[..3], &code[3..]),
            "LAPTOP",
        )
        .unwrap();
        assert_eq!(welcome.host_name, "RIG");
        assert_eq!(welcome.password, "abcd-2345-wxyz");
        assert_eq!(welcome.codec, Some(Codec::Hevc));
        assert!(agent.pairing().is_none(), "a code works once");
        let events = agent.take_events();
        assert!(events.contains(&Event::WrongCode));
        assert!(events.contains(&Event::Paired {
            id: welcome.client_id.clone(),
            name: "LAPTOP".into()
        }));

        // The device secret now opens status requests.
        let status = status(address(&agent), &welcome.client_id, &welcome.client_secret).unwrap();
        assert!(status.hosting);
        assert_eq!(status.players, 1);
        assert_eq!(status.best_latency_ms, Some(8.4));
        // A wrong secret does not.
        assert!(super::status(address(&agent), &welcome.client_id, "bogus").is_err());
        forget_device(&welcome.client_id);
        assert!(
            super::status(address(&agent), &welcome.client_id, &welcome.client_secret).is_err()
        );
    }

    #[test]
    fn three_wrong_codes_burn_the_code() {
        let agent = agent();
        let code = agent.open_pairing();
        let wrong = if code == "000000" { "111111" } else { "000000" };
        for _ in 0..3 {
            let _ = pair(address(&agent), wrong, "X");
        }
        assert!(agent.pairing().is_none());
        assert!(pair(address(&agent), &code, "X").is_err());
    }

    #[test]
    fn kyber_fingerprints_are_computed_from_the_pem() {
        let dir = std::env::temp_dir().join(format!("godrays-pem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cert.pem");
        let der = b"not really a certificate";
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            B64.encode(der)
        );
        std::fs::write(&path, pem).unwrap();
        let fp = fingerprint_of_pem(&path).unwrap();
        let expected: Vec<String> = Sha256::digest(der)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(fp, format!("sha256:{}", expected.join(":")));
        assert_eq!(fp.matches(':').count(), 32);
    }
}
