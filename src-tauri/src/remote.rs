//! Phone remote, like a Stream Deck. A tiny web server runs on your local network (Wi-Fi).
//! Open the link (or scan the QR code) on your phone: tap a sound to play it.
//!
//! Off by default. The link holds a secret token; whoever has the link can use the remote.
//! Plain HTTP, local network only: do not forward the port on your router.

use crate::audio::{AudioCmd, AudioSettings};
use crate::config::Config;
use crate::state::AppState;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tiny_http::{Header, Request, Response, Server};

const PAGE: &str = include_str!("remote.html");

struct Running {
    server: Arc<Server>,
    port: u16,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn running_lock() -> std::sync::MutexGuard<'static, Option<Running>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start, stop or move the server so it matches the settings.
pub fn apply(app: &AppHandle, on: bool, port: u16) {
    let mut guard = running_lock();
    if let Some(r) = guard.as_ref() {
        if on && r.port == port {
            return; // already right
        }
    }
    if let Some(r) = guard.take() {
        r.server.unblock(); // the worker thread ends, the port is freed
    }
    if !on {
        return;
    }

    let addr = format!("0.0.0.0:{port}");
    let mut server = None;
    let mut last_err = String::new();
    // the old server may need a moment to let go of the port
    for _ in 0..10 {
        match Server::http(&addr) {
            Ok(s) => {
                server = Some(s);
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    match server {
        Some(s) => {
            let server = Arc::new(s);
            let worker = Arc::clone(&server);
            let app = app.clone();
            thread::spawn(move || {
                for rq in worker.incoming_requests() {
                    handle(&app, rq);
                }
            });
            *guard = Some(Running { server, port });
        }
        None => {
            let _ = app.emit(
                "audio-error",
                format!("Phone remote could not start on port {port}: {last_err}. Try another port."),
            );
        }
    }
}

// ---------- link + QR ----------

#[derive(Serialize)]
pub struct Info {
    pub running: bool,
    pub url: String,
    /// QR code of the link, as an SVG picture
    pub qr: String,
}

/// The address of this PC on the local network (no packet is sent, the OS just picks the route).
fn lan_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

pub fn info(cfg: &Config) -> Info {
    let url = format!("http://{}:{}/?k={}", lan_ip(), cfg.remote_port, cfg.remote_token);
    let qr = qrcode::QrCode::new(url.as_bytes())
        .map(|c| {
            c.render::<qrcode::render::svg::Color>()
                .min_dimensions(180, 180)
                .quiet_zone(true)
                .build()
        })
        .unwrap_or_default();
    Info { running: running_lock().is_some(), url, qr }
}

// ---------- requests ----------

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn reply(rq: Request, status: u16, content_type: &str, body: String) {
    let resp = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", content_type))
        .with_header(header("Cache-Control", "no-store"));
    let _ = rq.respond(resp);
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Compare without stopping at the first different byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What the phone page needs to draw itself.
fn state_json(cfg: &Config) -> Value {
    let p = &cfg.presets[cfg.selected_preset];
    let sounds: Vec<Value> = p
        .sounds
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.files.is_empty())
        .map(|(i, s)| json!({ "i": i, "name": s.name, "error": s.error.is_some() }))
        .collect();
    json!({
        "presets": cfg.presets.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        "preset": cfg.selected_preset,
        "selected": p.selected_sound,
        "sounds": sounds,
        "volume": cfg.volume,
        "fx_on": cfg.fx_on,
        "fx": cfg.fx_preset,
        "fx_ids": crate::fx::IDS,
    })
}

/// Change the config like a hotkey would: save, tell the desktop window, return the new state.
fn mutate(app: &AppHandle, f: impl FnOnce(&mut Config, &AppState)) -> Option<Value> {
    let state = app.state::<AppState>();
    let snap = {
        let mut cfg = state.config.lock().ok()?;
        f(&mut cfg, &state);
        cfg.clone()
    };
    state.persist(&snap);
    let _ = app.emit("config-changed", snap.clone());
    Some(state_json(&snap))
}

fn handle(app: &AppHandle, rq: Request) {
    let url = rq.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let q = parse_query(query);
    let state = app.state::<AppState>();

    let allowed = match state.config.lock() {
        Ok(cfg) => cfg.remote_on && same(&cfg.remote_token, q.get("k").map(|s| s.as_str()).unwrap_or("")),
        Err(_) => false,
    };
    if !allowed {
        return reply(rq, 403, "text/plain; charset=utf-8", "Wrong link. Open Rustboard > Settings > Phone remote and scan the QR code again.".into());
    }

    let num = |name: &str| q.get(name).and_then(|v| v.parse::<usize>().ok());

    let out: Option<Value> = match path {
        "/" => return reply(rq, 200, "text/html; charset=utf-8", PAGE.to_string()),
        "/api/state" => state.config.lock().ok().map(|cfg| state_json(&cfg)),
        "/api/play" => mutate(app, |cfg, st| {
            let pi = num("p").filter(|i| *i < cfg.presets.len()).unwrap_or(cfg.selected_preset);
            if let Some(i) = num("s") {
                if let Some(sound) = cfg.presets[pi].sounds.get(i).cloned() {
                    cfg.selected_preset = pi;
                    cfg.presets[pi].selected_sound = i;
                    st.play(&sound);
                }
            }
        }),
        "/api/pause" | "/api/resume" | "/api/stop" => {
            state.send(match path {
                "/api/pause" => AudioCmd::Pause,
                "/api/resume" => AudioCmd::Resume,
                _ => AudioCmd::Stop,
            });
            state.config.lock().ok().map(|cfg| state_json(&cfg))
        }
        "/api/preset" => mutate(app, |cfg, _| {
            if let Some(i) = num("i").filter(|i| *i < cfg.presets.len()) {
                cfg.selected_preset = i;
            }
        }),
        "/api/fx" => {
            let id = q.get("id").cloned().unwrap_or_default();
            mutate(app, |cfg, st| {
                if crate::fx::is_known(&id) {
                    cfg.toggle_fx(&id);
                    st.send(AudioCmd::Configure(AudioSettings::from_config(cfg)));
                }
            })
        }
        "/api/volume" => mutate(app, |cfg, st| {
            if let Some(v) = num("v") {
                cfg.volume = (v as u32).min(100);
                st.send(AudioCmd::Configure(AudioSettings::from_config(cfg)));
            }
        }),
        _ => None,
    };

    match out {
        Some(v) => reply(rq, 200, "application/json", v.to_string()),
        None => reply(rq, 404, "text/plain; charset=utf-8", "Not found".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_split() {
        let q = parse_query("k=abc&s=3&p=0");
        assert_eq!(q["k"], "abc");
        assert_eq!(q["s"], "3");
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn token_compare() {
        assert!(same("abc123", "abc123"));
        assert!(!same("abc123", "abc124"));
        assert!(!same("abc123", "abc12"));
        assert!(!same("abc", ""));
    }

    #[test]
    fn state_lists_only_sounds_with_files() {
        let mut c = Config::default();
        c.presets[0].sounds.push(crate::config::Sound { name: "A".into(), files: vec!["a.wav".into()], ..Default::default() });
        c.presets[0].sounds.push(crate::config::Sound { name: "empty".into(), ..Default::default() });
        let v = state_json(&c);
        assert_eq!(v["sounds"].as_array().unwrap().len(), 1);
        assert_eq!(v["sounds"][0]["i"], 0);
    }

    #[test]
    fn info_has_link_and_qr() {
        let c = Config::default();
        let i = info(&c);
        assert!(i.url.contains(&format!(":8787/?k={}", c.remote_token)));
        assert!(i.qr.contains("<svg"));
    }
}
