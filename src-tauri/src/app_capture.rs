//! App sound for Go Live: the sound of other apps (game, browser, music player) is copied into
//! the virtual cable, so Discord hears it too. You still hear the apps on your own speakers.
//!
//! How: Windows "process loopback" capture (WASAPI), one capture per app. It needs Windows 11
//! or a recent Windows 10 (build 20348 or newer). On older Windows the capture fails and the
//! reason is shown once.
//!
//! This file has no audio output code. It fills one queue per app with stereo f32 frames at the
//! rate of the cable; `audio.rs` plays the sum of those queues into the cable.

use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Apps that are never captured. Rustboard would send its own sounds twice; Discord would send
/// your friends' voices back to them (echo).
const NEVER: &[&str] = &["rustboard", "discord", "discordptb", "discordcanary", "vesktop"];

/// Background programs that make no sound of their own. Hidden in the list.
const NOISE: &[&str] = &[
    "system", "idle", "registry", "smss", "csrss", "wininit", "services", "lsass", "svchost",
    "fontdrvhost", "dwm", "winlogon", "conhost", "powershell", "audiodg", "searchhost", "sihost",
    "runtimebroker", "taskhostw", "ctfmon", "wmiprvse", "dllhost", "spoolsv", "memory compression",
    "securityhealthsystray", "msedgewebview2", "wudfhost", "lsm", "unsecapp", "cmd",
];

/// At most this many apps are captured at once (each one is a small audio session).
const MAX_JOBS: usize = 12;
/// How often the list of running programs is read again.
const RESCAN_SECS: u64 = 6;
/// Never keep more than this much sound per app in the queue (milliseconds).
const MAX_QUEUE_MS: usize = 250;

/// One queue of stereo frames (L, R, L, R ...) for one captured app.
pub struct Src {
    pub q: VecDeque<f32>,
    pub primed: bool,
}

pub type SrcRef = Arc<Mutex<Src>>;
/// All queues. The cable output adds them together.
pub type Sources = Arc<Mutex<Vec<SrcRef>>>;

#[derive(Clone, Debug, Serialize)]
pub struct AppEntry {
    /// lowercase, without ".exe": what the config stores
    pub name: String,
    pub title: String,
    /// the app has a window right now
    pub window: bool,
}

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub title: String,
    pub window: bool,
}

/// "Chrome.EXE" -> "chrome"
pub fn normalize(name: &str) -> String {
    let n = name.trim().to_lowercase();
    n.strip_suffix(".exe").unwrap_or(&n).trim().to_string()
}

pub fn is_never(name: &str) -> bool {
    NEVER.contains(&name)
}

/// Which processes to capture. An app is often many processes (a browser has dozens); the
/// capture of the first one already covers its children, so only the root of each app is kept.
/// `all` = every app that has a window; otherwise only the names in `names`.
pub fn roots(procs: &[Proc], all: bool, names: &[String]) -> Vec<(u32, String)> {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut out: Vec<(u32, String)> = Vec::new();
    for p in procs {
        if is_never(&p.name) {
            continue;
        }
        let wanted = if all {
            p.window && !NOISE.contains(&p.name.as_str())
        } else {
            names.iter().any(|n| *n == p.name)
        };
        if !wanted {
            continue;
        }
        let parent_same = by_pid.get(&p.ppid).map(|q| q.name == p.name && q.pid != p.pid).unwrap_or(false);
        if !parent_same {
            out.push((p.pid, p.name.clone()));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The programs to show in Settings: one line per app name, windowed ones first.
pub fn list_apps() -> Result<Vec<AppEntry>, String> {
    let procs = snapshot()?;
    let mut by_name: HashMap<String, AppEntry> = HashMap::new();
    for p in &procs {
        if p.name.is_empty() || is_never(&p.name) || NOISE.contains(&p.name.as_str()) {
            continue;
        }
        let e = by_name.entry(p.name.clone()).or_insert(AppEntry {
            name: p.name.clone(),
            title: String::new(),
            window: false,
        });
        if p.window && !e.window {
            e.window = true;
            e.title = p.title.clone();
        }
    }
    let mut v: Vec<AppEntry> = by_name.into_values().collect();
    v.sort_by(|a, b| b.window.cmp(&a.window).then(a.name.cmp(&b.name)));
    Ok(v)
}

// ---------- reading the running programs ----------

#[cfg(windows)]
fn snapshot() -> Result<Vec<Proc>, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // One call gives: process id, parent id, exe name, and the window title if it has a window.
    let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ErrorActionPreference='SilentlyContinue'; \
        $w=@{}; Get-Process | Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle } | \
        ForEach-Object { $w[[int]$_.Id]=$_.MainWindowTitle }; \
        Get-CimInstance Win32_Process | ForEach-Object { [pscustomobject]@{ p=[int]$_.ProcessId; \
        pp=[int]$_.ParentProcessId; n=$_.Name; t=$w[[int]$_.ProcessId] } } | ConvertTo-Json -Compress";
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(0x0800_0000) // no console window
        .output()
        .map_err(|e| format!("Cannot read the running apps: {e}"))?;
    if !out.status.success() {
        return Err("Cannot read the running apps (PowerShell failed).".into());
    }
    parse_snapshot(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(windows))]
fn snapshot() -> Result<Vec<Proc>, String> {
    Err("App sound works on Windows only.".into())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_snapshot(text: &str) -> Result<Vec<Proc>, String> {
    let text = text.trim().trim_start_matches('\u{feff}');
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("Cannot read the app list: {e}"))?;
    let items: Vec<&serde_json::Value> = match &v {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(_) => vec![&v],
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for it in items {
        let pid = it["p"].as_u64().unwrap_or(0) as u32;
        if pid == 0 {
            continue;
        }
        let title = it["t"].as_str().unwrap_or("").to_string();
        out.push(Proc {
            pid,
            ppid: it["pp"].as_u64().unwrap_or(0) as u32,
            name: normalize(it["n"].as_str().unwrap_or("")),
            window: !title.is_empty(),
            title,
        });
    }
    Ok(out)
}

// ---------- capture ----------

/// Running capture. Dropping it stops every capture thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Job {
    pid: u32,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    src: SrcRef,
}

impl Capture {
    /// `rate` = sample rate of the cable (the capture is converted to it).
    /// Problems are sent to `errors`, each one only once.
    pub fn start(rate: u32, all: bool, names: Vec<String>, sources: Sources, errors: Sender<String>) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let st = Arc::clone(&stop);
        thread::spawn(move || manage(rate, all, names, sources, st, errors));
        Capture { stop }
    }
}

fn sleep_unless(stop: &AtomicBool, secs: u64) {
    for _ in 0..(secs * 10) {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Watches the running programs and keeps one capture per wanted app.
fn manage(rate: u32, all: bool, names: Vec<String>, sources: Sources, stop: Arc<AtomicBool>, errors: Sender<String>) {
    let mut jobs: Vec<Job> = Vec::new();
    let mut bad: HashSet<u32> = HashSet::new(); // captures that failed: not tried again
    let mut warned_list = false;
    while !stop.load(Ordering::Relaxed) {
        match snapshot() {
            Ok(procs) => {
                warned_list = false;
                let want = roots(&procs, all, &names);
                for j in &jobs {
                    if !want.iter().any(|(p, _)| *p == j.pid) {
                        j.stop.store(true, Ordering::Relaxed); // the app closed
                    }
                }
                jobs.retain(|j| {
                    if !j.done.load(Ordering::Relaxed) {
                        return true;
                    }
                    if let Ok(mut list) = sources.lock() {
                        list.retain(|s| !Arc::ptr_eq(s, &j.src));
                    }
                    if j.failed.load(Ordering::Relaxed) {
                        bad.insert(j.pid);
                    }
                    false
                });
                bad.retain(|p| want.iter().any(|(w, _)| w == p));
                for (pid, name) in want {
                    if jobs.len() >= MAX_JOBS {
                        break;
                    }
                    if bad.contains(&pid) || jobs.iter().any(|j| j.pid == pid) {
                        continue;
                    }
                    let src: SrcRef = Arc::new(Mutex::new(Src { q: VecDeque::new(), primed: false }));
                    let job = Job {
                        pid,
                        stop: Arc::new(AtomicBool::new(false)),
                        done: Arc::new(AtomicBool::new(false)),
                        failed: Arc::new(AtomicBool::new(false)),
                        src: Arc::clone(&src),
                    };
                    if let Ok(mut list) = sources.lock() {
                        list.push(Arc::clone(&src));
                    }
                    let (s, d, f, er) = (
                        Arc::clone(&job.stop),
                        Arc::clone(&job.done),
                        Arc::clone(&job.failed),
                        errors.clone(),
                    );
                    thread::spawn(move || {
                        if let Err(e) = capture_one(pid, rate, &src, &s) {
                            f.store(true, Ordering::Relaxed);
                            let _ = er.send(format!("App sound ({name}): {e}"));
                        }
                        d.store(true, Ordering::Relaxed);
                    });
                    jobs.push(job);
                }
            }
            Err(e) => {
                if !warned_list {
                    warned_list = true;
                    let _ = errors.send(e);
                }
            }
        }
        sleep_unless(&stop, RESCAN_SECS);
    }
    for j in &jobs {
        j.stop.store(true, Ordering::Relaxed);
    }
    if let Ok(mut list) = sources.lock() {
        list.clear();
    }
}

#[cfg(not(windows))]
fn capture_one(_pid: u32, _rate: u32, _src: &SrcRef, _stop: &AtomicBool) -> Result<(), String> {
    Err("App sound works on Windows only.".into())
}

/// Capture the sound of one process (and its child processes) until `stop` is set.
#[cfg(windows)]
fn capture_one(pid: u32, rate: u32, src: &SrcRef, stop: &AtomicBool) -> Result<(), String> {
    use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};

    let _ = wasapi::initialize_mta();
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, 2, None);
    let mut client = AudioClient::new_application_loopback_client(pid, true).map_err(|e| {
        format!("{e}. App sound needs Windows 11 or a recent Windows 10 (build 20348 or newer).")
    })?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    client
        .initialize_client(&fmt, &Direction::Capture, &mode)
        .map_err(|e| e.to_string())?;
    let event = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let capture = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;

    let max_samples = rate as usize * 2 * MAX_QUEUE_MS / 1000;
    let mut raw: VecDeque<u8> = VecDeque::new();
    let mut frames: Vec<f32> = Vec::with_capacity(4096);
    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(200).is_err() {
            continue; // nothing playing right now
        }
        capture.read_from_device_to_deque(&mut raw).map_err(|e| e.to_string())?;
        frames.clear();
        while raw.len() >= 4 {
            let b = [
                raw.pop_front().unwrap_or(0),
                raw.pop_front().unwrap_or(0),
                raw.pop_front().unwrap_or(0),
                raw.pop_front().unwrap_or(0),
            ];
            frames.push(f32::from_le_bytes(b));
        }
        if frames.len() % 2 == 1 {
            frames.pop(); // keep whole L/R pairs
        }
        if let Ok(mut s) = src.lock() {
            s.q.extend(frames.iter().copied());
            while s.q.len() > max_samples {
                s.q.pop_front();
                s.q.pop_front(); // drop the oldest pair: keeps the delay low
            }
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, name: &str, window: bool) -> Proc {
        Proc { pid, ppid, name: name.into(), title: if window { "T".into() } else { String::new() }, window }
    }

    #[test]
    fn names_are_lowercase_without_exe() {
        assert_eq!(normalize("Chrome.EXE"), "chrome");
        assert_eq!(normalize(" spotify "), "spotify");
    }

    #[test]
    fn only_the_root_of_an_app_is_kept() {
        let procs = vec![p(10, 1, "chrome", true), p(11, 10, "chrome", false), p(12, 10, "chrome", false)];
        assert_eq!(roots(&procs, false, &["chrome".to_string()]), vec![(10, "chrome".to_string())]);
    }

    #[test]
    fn picked_apps_only() {
        let procs = vec![p(10, 1, "chrome", true), p(20, 1, "spotify", true)];
        assert_eq!(roots(&procs, false, &["spotify".to_string()]), vec![(20, "spotify".to_string())]);
        assert!(roots(&procs, false, &[]).is_empty());
    }

    #[test]
    fn all_means_windowed_apps_but_never_discord_or_rustboard() {
        let procs = vec![
            p(10, 1, "chrome", true),
            p(20, 1, "discord", true),
            p(30, 1, "rustboard", true),
            p(40, 1, "svchost", true),
            p(50, 1, "updater", false),
        ];
        assert_eq!(roots(&procs, true, &[]), vec![(10, "chrome".to_string())]);
    }

    #[test]
    fn discord_is_refused_even_when_picked() {
        let procs = vec![p(20, 1, "discord", true)];
        assert!(roots(&procs, false, &["discord".to_string()]).is_empty());
    }

    #[test]
    fn reads_one_object_or_a_list() {
        let one = parse_snapshot(r#"{"p":5,"pp":1,"n":"Chrome.exe","t":"Tab"}"#).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "chrome");
        assert!(one[0].window);
        let two = parse_snapshot(r#"[{"p":5,"pp":1,"n":"a.exe","t":null},{"p":6,"pp":5,"n":"b.exe","t":null}]"#).unwrap();
        assert_eq!(two.len(), 2);
        assert!(!two[0].window);
    }
}
