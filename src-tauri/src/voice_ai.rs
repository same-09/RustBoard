//! AI Voice (your voice becomes another voice, live).
//! - `detect`: is the graphics card strong enough? (NVIDIA, 6 GB or more)
//! - `list` / `import` / `remove`: the voice models the user brings from voice-models.com
//!   (a .zip with a .pth model and often an .index file).
//! - the converter: the real RVC engine, run as a helper program. Rustboard starts
//!   the engine's Python with `engine/rb_bridge.py` and talks to it over stdin/stdout. The Python
//!   is the one Rustboard installed itself (`ai_setup.rs`) or the `.venv` of an engine folder.
//!   `convert` does one clip (Test voice), `Live` does the live mic.

use serde::Serialize;
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};

/// Folder inside `Documents/RustBoard/` that holds one sub-folder per imported voice.
pub const FOLDER: &str = "Voices";

/// The helper script, packed into the program and written next to the voices when needed.
const SCRIPT: &str = include_str!("../engine/rb_bridge.py");

/// What the engine hands back: mono audio and its sample rate.
pub struct Converted {
    pub samples: Vec<f32>,
    pub rate: u32,
}

/* ---------- finding the engine ---------- */

/// The engine folder and the Python inside it (the one that has PyTorch), plus our script.
#[derive(Clone, Debug)]
pub struct Engine {
    pub root: PathBuf,
    pub python: PathBuf,
    pub script: PathBuf,
    /// extra environment for the helper (our own install keeps models and caches in its folder)
    pub env: Vec<(&'static str, PathBuf)>,
    /// true = the engine Rustboard installed itself, false = an engine folder
    pub own: bool,
}

/// A real engine folder has the `rustboard/ai` package in it.
fn is_root(dir: &Path) -> bool {
    dir.join("rustboard").join("ai").is_dir()
}

fn python_in(root: &Path) -> Option<PathBuf> {
    let c = [
        root.join(".venv").join("Scripts").join("python.exe"),
        root.join("venv").join("Scripts").join("python.exe"),
        root.join(".venv").join("bin").join("python"),
        root.join("venv").join("bin").join("python"),
    ];
    c.into_iter().find(|p| p.is_file())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

/// Places we look when the user has not picked a folder.
fn guesses(work: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(e) = std::env::var_os("RUSTBOARD_ENGINE_DIR") {
        v.push(PathBuf::from(e));
    }
    v.push(work.join("Engine"));
    if let Some(h) = home_dir() {
        for parent in [h.clone(), h.join("Documents"), h.join("Downloads"), h.join("Desktop")] {
            for name in ["RustBoard-Engine", "rustboard-engine"] {
                v.push(parent.join(name));
            }
        }
    }
    v
}

/// Write our helper script next to the voices (only when it changed).
fn write_script(work: &Path) -> Result<PathBuf, String> {
    let dir = work.join("engine");
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot make the engine folder: {e}"))?;
    let script = dir.join("rb_bridge.py");
    if fs::read_to_string(&script).map(|t| t != SCRIPT).unwrap_or(true) {
        fs::write(&script, SCRIPT).map_err(|e| format!("Cannot write the helper script: {e}"))?;
    }
    Ok(script)
}

/// Find the engine. Order: the engine folder picked in Settings, then the engine Rustboard
/// installed itself (`Install AI voice`), then an engine folder in the usual places.
/// `work` = `Documents/RustBoard`.
pub fn locate(configured: &str, work: &Path) -> Result<Engine, String> {
    let configured = configured.trim();
    if configured.is_empty() && crate::ai_setup::installed(work) {
        let p = crate::ai_setup::paths(work);
        crate::ai_setup::write_code(&p.code)?; // the code in this version of Rustboard
        let script = write_script(work)?;
        return Ok(Engine { root: p.code.clone(), python: p.python.clone(), script, env: crate::ai_setup::env(&p), own: true });
    }
    let root = if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if !is_root(&p) {
            return Err("This folder is not an engine folder (it has no 'rustboard' folder inside). Pick the folder that contains the 'rustboard' folder, or press Auto and use Install AI voice.".into());
        }
        p
    } else {
        guesses(work).into_iter().find(|p| is_root(p)).ok_or_else(|| {
            "The AI voice engine is not installed yet. Press Install AI voice.".to_string()
        })?
    };
    let python = python_in(&root).ok_or_else(|| {
        "This engine folder has no .venv yet. Press Auto and use Install AI voice.".to_string()
    })?;
    let script = write_script(work)?;
    Ok(Engine { root, python, script, env: Vec::new(), own: false })
}

fn command(e: &Engine, args: &[&str]) -> Command {
    let mut c = Command::new(&e.python);
    // -I: do not load stray modules from the current folder
    c.arg("-I").arg(&e.script).args(args).current_dir(&e.root);
    for (k, v) in &e.env {
        c.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c
}

/* ---------- raw audio + the helper's reply ---------- */

pub fn f32_to_bytes(x: &[f32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(x.len() * 4);
    for s in x {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

pub fn bytes_to_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// The helper prints one JSON line. Libraries may print other things first: use the last JSON line.
/// Returns the engine rate (convert: "rate", live: "sr") or the helper's own error text.
pub fn parse_reply(stdout: &str) -> Result<serde_json::Value, String> {
    let line = stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).ok_or("The voice engine said nothing.")?;
    let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("Bad answer from the voice engine: {e}"))?;
    let ok = v.get("ok").or_else(|| v.get("ready")).and_then(|x| x.as_bool()).unwrap_or(false);
    if ok {
        Ok(v)
    } else {
        Err(v.get("error").and_then(|x| x.as_str()).unwrap_or("The voice engine failed.").to_string())
    }
}

/// Turn recorded mono audio into the voice stored in `voice_dir` (Test voice).
/// Slow the first time (loads PyTorch and the models): call from a worker thread.
pub fn convert(engine: &Engine, voice_dir: &Path, samples: &[f32], rate: u32) -> Result<Converted, String> {
    let tmp = std::env::temp_dir().join("rustboard-ai-engine");
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let (src, dst) = (tmp.join("in.f32"), tmp.join("out.f32"));
    let _ = fs::remove_file(&dst);
    fs::write(&src, f32_to_bytes(samples)).map_err(|e| format!("Cannot write the clip: {e}"))?;
    let out = command(
        engine,
        &[
            "convert",
            &engine.root.to_string_lossy(),
            &voice_dir.to_string_lossy(),
            &src.to_string_lossy(),
            &rate.to_string(),
            &dst.to_string_lossy(),
        ],
    )
    .stdin(Stdio::null())
    .output()
    .map_err(|e| format!("Cannot start the voice engine's Python: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v = parse_reply(&stdout).map_err(|e| with_tail(e, &out.stderr))?;
    let rate = v.get("rate").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let bytes = fs::read(&dst).map_err(|e| format!("The voice engine made no audio: {e}"))?;
    let samples = bytes_to_f32(&bytes);
    if rate == 0 || samples.is_empty() {
        return Err("The voice engine gave an empty clip.".into());
    }
    Ok(Converted { samples, rate })
}

fn with_tail(msg: String, stderr: &[u8]) -> String {
    let t = String::from_utf8_lossy(stderr);
    let last = t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    if last.is_empty() || msg.contains(last) { msg } else { format!("{msg} ({last})") }
}

/* ---------- live ---------- */

/// What the plan for the live mic is. Equal plans = nothing to restart.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub engine_dir: String,
    pub voice_dir: PathBuf,
    pub work: PathBuf,
}

/// Is AI Voice wanted right now? Only when it is on and the picked voice exists.
pub fn plan(work: &Path, on: bool, voice: &str, engine_dir: &str) -> Option<Plan> {
    if !on {
        return None;
    }
    let voice_dir = path(&work.join(FOLDER), voice).ok()?;
    Some(Plan { engine_dir: engine_dir.to_string(), voice_dir, work: work.to_path_buf() })
}

#[derive(Clone, Debug, PartialEq)]
pub enum AiState {
    Loading,
    /// e.g. "Balanced · cuda"
    Live(String),
    Failed(String),
}

/// The running helper. Dropping it stops the helper.
pub struct Live {
    shared: Arc<Shared>,
    child: Arc<Mutex<Option<Child>>>,
}

struct Shared {
    ready: AtomicBool,
    failed: AtomicBool,
    stop: AtomicBool,
    /// the engine's sample rate and block size (0 until ready)
    rate: AtomicU32,
    block: AtomicU32,
    /// converted audio waiting to be played, at `rate`
    out: Mutex<VecDeque<f32>>,
    tx: Mutex<Option<SyncSender<Vec<f32>>>>,
}

impl Live {
    /// Returns at once. The helper loads in the background (10 s or more the first time);
    /// `report` is told when it is live or has failed.
    pub fn start(plan: Plan, report: impl Fn(AiState) + Send + Sync + 'static) -> Live {
        let shared = Arc::new(Shared {
            ready: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            rate: AtomicU32::new(0),
            block: AtomicU32::new(0),
            out: Mutex::new(VecDeque::new()),
            tx: Mutex::new(None),
        });
        let child: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
        report(AiState::Loading);
        let (sh, ch) = (Arc::clone(&shared), Arc::clone(&child));
        std::thread::spawn(move || {
            if let Err(e) = run_live(&plan, &sh, &ch, &report) {
                if !sh.stop.load(Ordering::Relaxed) {
                    sh.failed.store(true, Ordering::Relaxed);
                    sh.ready.store(false, Ordering::Relaxed);
                    report(AiState::Failed(e));
                }
            }
        });
        Live { shared, child }
    }

    /// Ready to convert, and nothing has gone wrong.
    pub fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::Relaxed) && !self.shared.failed.load(Ordering::Relaxed)
    }

    /// Sample rate of the engine (0 until it is ready).
    pub fn rate(&self) -> u32 {
        self.shared.rate.load(Ordering::Relaxed)
    }

    pub fn block(&self) -> usize {
        self.shared.block.load(Ordering::Relaxed) as usize
    }

    /// One block of mic audio at `rate()`. Dropped when the helper is behind.
    pub fn send(&self, block: Vec<f32>) {
        if let Ok(g) = self.shared.tx.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.try_send(block);
            }
        }
    }

    /// Move converted audio (at `rate()`) out of the shared queue.
    pub fn take_output(&self, f: impl FnOnce(&mut VecDeque<f32>)) {
        if let Ok(mut q) = self.shared.out.lock() {
            f(&mut q);
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Ok(mut c) = self.child.lock() {
            if let Some(ch) = c.as_mut() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
        if let Ok(mut t) = self.shared.tx.lock() {
            *t = None;
        }
    }
}

/// Mode for the helper: "" = it picks (Balanced on a graphics card, Laptop on a CPU).
const MODE: &str = "";

fn run_live(
    plan: &Plan,
    sh: &Arc<Shared>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    report: &(impl Fn(AiState) + Send + Sync + 'static),
) -> Result<(), String> {
    let engine = locate(&plan.engine_dir, &plan.work)?;
    let mut child = command(&engine, &["live", &engine.root.to_string_lossy(), &plan.voice_dir.to_string_lossy(), MODE, "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Cannot start the voice engine's Python: {e}"))?;
    let stdin = child.stdin.take().ok_or("No pipe to the voice engine.")?;
    let stdout = child.stdout.take().ok_or("No pipe from the voice engine.")?;
    {
        let mut slot = child_slot.lock().map_err(|e| e.to_string())?;
        if sh.stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        *slot = Some(child);
    }
    let mut reader = BufReader::new(stdout);
    // Libraries may print before our line: skip until a JSON line shows up.
    let reply = loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("The voice engine stopped while starting. Press Install AI voice to reinstall it, then try again.".into());
        }
        if line.trim_start().starts_with('{') {
            break line;
        }
    };
    let v = parse_reply(&reply)?;
    let rate = v.get("sr").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let block = v.get("block").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if rate == 0 || block == 0 {
        return Err("The voice engine gave bad settings.".into());
    }
    let info = format!(
        "{} · {}",
        v.get("mode").and_then(|x| x.as_str()).unwrap_or("?"),
        v.get("device").and_then(|x| x.as_str()).unwrap_or("?")
    );

    let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(6);
    std::thread::spawn(move || {
        let mut stdin = stdin;
        for blk in rx {
            if stdin.write_all(&f32_to_bytes(&blk)).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    });
    sh.rate.store(rate, Ordering::Relaxed);
    sh.block.store(block, Ordering::Relaxed);
    *sh.tx.lock().map_err(|e| e.to_string())? = Some(tx);
    sh.ready.store(true, Ordering::Relaxed);
    report(AiState::Live(info));

    let mut buf = vec![0u8; block as usize * 4];
    let max_queue = rate as usize; // never hold more than one second
    loop {
        reader.read_exact(&mut buf).map_err(|_| {
            "The voice engine stopped. Out of graphics memory? Close other heavy programs and try again.".to_string()
        })?;
        let samples = bytes_to_f32(&buf);
        if let Ok(mut q) = sh.out.lock() {
            q.extend(samples);
            while q.len() > max_queue {
                q.pop_front();
            }
        }
    }
}

/// Below this much video memory, live conversion is likely to stutter.
const MIN_VRAM_MB: u32 = 6000;
/// What we keep from a zip. Everything else (images, readme, ...) is ignored.
const KEEP_EXT: [&str; 3] = ["pth", "onnx", "index"];
const MODEL_EXT: [&str; 2] = ["pth", "onnx"];
/// One file may be this big. A zip may hold at most three times this.
const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Gpu {
    pub name: String,
    pub vram_mb: u32,
    /// "strong" (NVIDIA, 6 GB+), "weak" (anything else we found), "none" (no card found)
    pub level: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct Voice {
    /// folder name, also what the config stores
    pub id: String,
    pub name: String,
    pub size_mb: u32,
    pub has_index: bool,
}

/* ---------- graphics card ---------- */

pub fn classify(name: &str, vram_mb: u32) -> &'static str {
    let n = name.to_lowercase();
    let nvidia = ["nvidia", "geforce", "rtx", "gtx", "quadro"].iter().any(|k| n.contains(k));
    if nvidia && vram_mb >= MIN_VRAM_MB {
        "strong"
    } else {
        "weak"
    }
}

/// One line of `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits`.
fn parse_nvidia(line: &str) -> Option<(String, u32)> {
    let (name, mem) = line.rsplit_once(',')?;
    let mb = mem.trim().parse::<u32>().ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| (name.to_string(), mb))
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut c = std::process::Command::new(program);
    c.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    let out = c.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Takes a moment (starts nvidia-smi / PowerShell): call it from a worker thread.
pub fn detect() -> Gpu {
    if let Some(out) = run("nvidia-smi", &["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]) {
        if let Some((name, mb)) = out.lines().filter_map(parse_nvidia).max_by_key(|(_, mb)| *mb) {
            return Gpu { level: classify(&name, mb), name, vram_mb: mb };
        }
    }
    // not NVIDIA (or no driver tool): only the name is known, and there is no CUDA
    let names = run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name }"],
    );
    if let Some(out) = names {
        let first = out
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.to_lowercase().contains("basic render"));
        if let Some(name) = first {
            return Gpu { name: name.to_string(), vram_mb: 0, level: "weak" };
        }
    }
    Gpu { name: String::new(), vram_mb: 0, level: "none" }
}

/* ---------- voices ---------- */

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default()
}

/// A folder name that is safe on Windows: letters, digits, "-" and "_" only.
fn safe_id(stem: &str) -> String {
    let s: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { ' ' }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join("_");
    let s: String = s.chars().take(60).collect();
    if s.is_empty() { "voice".to_string() } else { s }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

pub fn list(dir: &Path) -> Vec<Voice> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(id) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let (mut size, mut model, mut index) = (0u64, false, false);
        if let Ok(files) = fs::read_dir(&p) {
            for f in files.flatten() {
                let fp = f.path();
                let ext = ext_of(&fp);
                if KEEP_EXT.contains(&ext.as_str()) {
                    size += f.metadata().map(|m| m.len()).unwrap_or(0);
                }
                model |= MODEL_EXT.contains(&ext.as_str());
                index |= ext == "index";
            }
        }
        if model {
            let name = id.replace(&['_', '-'][..], " ");
            out.push(Voice { id, name, size_mb: (size / 1_048_576) as u32, has_index: index });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn has_model(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|f| MODEL_EXT.contains(&ext_of(&f.path()).as_str())))
        .unwrap_or(false)
}

fn copy_one(src: &Path, target: &Path) -> Result<(), String> {
    let name = src.file_name().ok_or("Bad file name.")?;
    let len = fs::metadata(src).map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err("This file is too big (limit 2 GB).".into());
    }
    fs::copy(src, target.join(name)).map(|_| ()).map_err(|e| format!("Cannot copy the file: {e}"))
}

fn extract_zip(src: &Path, target: &Path) -> Result<(), String> {
    let file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("This is not a valid zip file: {e}"))?;
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if entry.is_dir() {
            continue;
        }
        // only the file name is used (no folders, no ".."), so nothing can land outside `target`
        let name = Path::new(entry.name()).file_name().and_then(|n| n.to_str()).map(str::to_string);
        let Some(name) = name else { continue };
        if !KEEP_EXT.contains(&ext_of(Path::new(&name)).as_str()) {
            continue;
        }
        if entry.size() > MAX_FILE {
            return Err("A file in this zip is too big (limit 2 GB).".into());
        }
        let mut out = fs::File::create(target.join(&name)).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry.by_ref().take(MAX_FILE), &mut out)
            .map_err(|e| format!("Cannot unzip: {e}"))?;
        total += copied;
        if total > 3 * MAX_FILE {
            return Err("This zip is too big.".into());
        }
    }
    Ok(())
}

/// Add a voice from a .zip (voice-models.com) or a bare .pth / .onnx file.
pub fn import(dir: &Path, src: &Path) -> Result<Voice, String> {
    if !src.is_file() {
        return Err("File not found.".into());
    }
    let ext = ext_of(src);
    if !matches!(ext.as_str(), "zip" | "pth" | "onnx") {
        return Err("Pick the .zip you downloaded from voice-models.com, or a .pth / .onnx model file.".into());
    }
    let base = safe_id(src.file_stem().and_then(|s| s.to_str()).unwrap_or("voice"));
    let mut id = base.clone();
    let mut n = 2;
    while dir.join(&id).exists() {
        id = format!("{base}_{n}");
        n += 1;
    }
    let target = dir.join(&id);
    fs::create_dir_all(&target).map_err(|e| format!("Cannot create the Voices folder: {e}"))?;

    let result = if ext == "zip" { extract_zip(src, &target) } else { copy_one(src, &target) }.and_then(|_| {
        if has_model(&target) { Ok(()) } else { Err("No voice model (.pth or .onnx) found in this file.".to_string()) }
    });
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&target); // the folder we just made, nothing else
        return Err(e);
    }
    list(dir).into_iter().find(|v| v.id == id).ok_or_else(|| "Import failed.".to_string())
}

/// Folder of one imported voice (checks the name and that it exists).
pub fn path(dir: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    if !valid_id(id) {
        return Err("Pick a voice first.".into());
    }
    let p = dir.join(id);
    if p.is_dir() {
        Ok(p)
    } else {
        Err("This voice is gone. Import it again.".into())
    }
}

pub fn remove(dir: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Bad voice name.".into());
    }
    let p = dir.join(id);
    if !p.is_dir() {
        return Ok(());
    }
    fs::remove_dir_all(&p).map_err(|e| format!("Cannot remove the voice: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_needs_nvidia_and_6gb() {
        assert_eq!(classify("NVIDIA GeForce RTX 3060", 12288), "strong");
        assert_eq!(classify("NVIDIA GeForce GTX 1060", 6144), "strong");
        assert_eq!(classify("NVIDIA GeForce GTX 1050", 2048), "weak");
        assert_eq!(classify("AMD Radeon RX 6800", 16384), "weak");
        assert_eq!(classify("Intel(R) UHD Graphics", 0), "weak");
    }

    #[test]
    fn nvidia_smi_line() {
        assert_eq!(parse_nvidia("NVIDIA GeForce RTX 3060, 12288"), Some(("NVIDIA GeForce RTX 3060".to_string(), 12288)));
        assert_eq!(parse_nvidia("no comma here"), None);
        assert_eq!(parse_nvidia("name, notanumber"), None);
    }

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(safe_id("My Voice (v2) [RVC]"), "My_Voice_v2_RVC");
        assert_eq!(safe_id("../../evil"), "evil");
        assert_eq!(safe_id("???"), "voice");
        assert!(valid_id("My_Voice-2"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
        assert!(!valid_id("a/b"));
    }

    #[test]
    fn raw_audio_round_trips() {
        let x = [0.0f32, 0.5, -1.0, 0.25];
        assert_eq!(bytes_to_f32(&f32_to_bytes(&x)), x.to_vec());
        assert_eq!(bytes_to_f32(&[1, 2, 3]), Vec::<f32>::new()); // a cut-off sample is ignored
    }

    #[test]
    fn helper_reply_uses_the_last_json_line() {
        let v = parse_reply("warning: x\n{\"ok\": true, \"rate\": 40000}\n").unwrap();
        assert_eq!(v["rate"], 40000);
        let v = parse_reply("{\"ready\": true, \"sr\": 48000, \"block\": 5760}").unwrap();
        assert_eq!(v["sr"], 48000);
        assert_eq!(parse_reply("{\"ok\": false, \"error\": \"no torch\"}").err().as_deref(), Some("no torch"));
        assert_eq!(parse_reply("{\"ready\": false}").err().as_deref(), Some("The voice engine failed."));
        assert!(parse_reply("nothing useful").is_err());
        assert!(parse_reply("{broken").is_err());
    }

    #[test]
    fn rustboard_folder_needs_the_ai_package() {
        let d = std::env::temp_dir().join("rb-vf-test");
        let _ = fs::remove_dir_all(&d);
        assert!(!is_root(&d));
        fs::create_dir_all(d.join("rustboard").join("ai")).unwrap();
        assert!(is_root(&d));
        assert!(python_in(&d).is_none());
        let e = locate(&d.to_string_lossy(), &d).err().unwrap();
        assert!(e.contains(".venv"));
        fs::create_dir_all(d.join(".venv").join("Scripts")).unwrap();
        fs::write(d.join(".venv").join("Scripts").join("python.exe"), b"").unwrap();
        let eng = locate(&d.to_string_lossy(), &d).unwrap();
        assert_eq!(fs::read_to_string(&eng.script).unwrap(), SCRIPT);
        assert!(locate("C:/nope/not/here", &d).is_err());
        assert!(!eng.own && eng.env.is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn own_install_wins_over_guesses_and_keeps_its_models_inside() {
        let work = std::env::temp_dir().join(format!("rb-own-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        // nothing installed, nothing picked: the message says what to press
        let e = locate("", &work).err().unwrap();
        assert!(e.contains("Install AI voice"), "{e}");
        // fake a finished install
        let p = crate::ai_setup::paths(&work);
        fs::create_dir_all(p.python.parent().unwrap()).unwrap();
        fs::write(&p.python, b"").unwrap();
        fs::write(&p.marker, r#"{"version": 1}"#).unwrap();
        let eng = locate("", &work).unwrap();
        assert!(eng.own);
        assert_eq!(eng.root, p.code);
        assert!(is_root(&eng.root), "the code was written");
        assert!(eng.env.iter().any(|(k, v)| *k == "RUSTBOARD_HOME" && *v == p.home));
        assert!(eng.env.iter().any(|(k, _)| *k == "HF_HOME"));
        // a picked RustBoard folder still wins when the user chose one
        assert!(locate("C:/nope/not/here", &work).is_err());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn plan_needs_switch_and_voice() {
        let d = std::env::temp_dir().join("rb-plan-test");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(FOLDER).join("Anna")).unwrap();
        assert!(plan(&d, false, "Anna", "").is_none());
        assert!(plan(&d, true, "", "").is_none());
        assert!(plan(&d, true, "Bob", "").is_none());
        assert!(plan(&d, true, "Anna", "").is_some());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn remove_rejects_paths() {
        let dir = std::env::temp_dir();
        assert!(remove(&dir, "..").is_err());
        assert!(remove(&dir, "a\\b").is_err());
    }
}
