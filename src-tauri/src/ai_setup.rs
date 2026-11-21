//! Installs the AI voice engine by itself, so nobody has to set it up by hand.
//!
//! Everything goes into `Documents/RustBoard/AI/` (and is gone when that folder is removed):
//!   python/      its own Python 3.12 (the "embeddable" build from python.org, no installer, no
//!                admin rights). On Linux/macOS a venv of the Python that is already there.
//!   code/        the engine code (MIT code, copied into this program; see LICENSE-ENGINE)
//!   home/, hf/   the base voice models (about 550 MB) and the Hugging Face cache
//!   ready.json   written last: "the install finished"
//!
//! Six steps: Python, pip, audio packages, PyTorch
//! (with CUDA when there is an NVIDIA card, else the CPU build), AI packages, base models.
//! The UI asks `state()` about once a second and shows the bar. Cancel stops it at any point;
//! pressing Install again continues (pip skips what is already there).

use serde::Serialize;
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

/// Folder inside `Documents/RustBoard/`.
pub const FOLDER: &str = "AI";

const PYTHON_URL: &str = "https://www.python.org/ftp/python/3.12.10/python-3.12.10-embed-amd64.zip";
const GET_PIP_URL: &str = "https://bootstrap.pypa.io/get-pip.py";
const TORCH: [&str; 2] = ["torch==2.7.1", "torchaudio==2.7.1"];
const TORCH_CUDA_INDEX: &str = "https://download.pytorch.org/whl/cu128";
const TORCH_CPU_INDEX: &str = "https://download.pytorch.org/whl/cpu";

/// Same lists as the engine's requirements.txt / requirements-ai.txt, without the window toolkit
/// (PySide6) and the keyboard hooks, which the voice engine does not use.
const REQ_BASE: &str = "numpy>=1.26.4,<2\nscipy>=1.10\nsounddevice>=0.4.6\nsoundfile>=0.12\n";
const REQ_AI: &str = "numpy>=1.26.4,<2\ntransformers>=4.49.0,<4.50\nlibrosa>=0.10.2,<0.11\ntorchfcpe>=0.0.4,<0.1\nfaiss-cpu>=1.13.0,<2\nhuggingface_hub>=0.24\n";

/// Bump when the package lists or the PyTorch version change: an older install then runs again
/// (quick: pip skips what is there).
const SETUP_VERSION: u32 = 1;

/// The helper script (same file Rustboard uses for Test voice and live).
const BRIDGE: &str = include_str!("../engine/rb_bridge.py");

/// (title, share of the bar in percent). Sums to 100. PyTorch is by far the longest.
const STEPS: [(&str, u32); 6] = [
    ("Getting Python", 4),
    ("Getting pip", 2),
    ("Installing audio packages", 6),
    ("Installing PyTorch (the big one)", 50),
    ("Installing AI packages", 18),
    ("Downloading the base voice models", 20),
];

macro_rules! engine_files {
    ($($rel:literal),* $(,)?) => {
        /// The part of the engine code (MIT) the voice engine needs, packed into the program.
        /// A test checks that no file in `engine/rustboard/` is missing here.
        const FILES: &[(&str, &str)] = &[$(($rel, include_str!(concat!("../engine/", $rel)))),*];
    };
}

engine_files!(
    "LICENSE-ENGINE",
    "rustboard/__init__.py",
    "rustboard/devices.py",
    "rustboard/effects.py",
    "rustboard/net.py",
    "rustboard/presets.py",
    "rustboard/ai/__init__.py",
    "rustboard/ai/_compat.py",
    "rustboard/ai/assets.py",
    "rustboard/ai/blend.py",
    "rustboard/ai/catalog.py",
    "rustboard/ai/engine.py",
    "rustboard/ai/hwcheck.py",
    "rustboard/ai/preview.py",
    "rustboard/ai/rvc.py",
    "rustboard/ai/rvc_lib/LICENSE-RVC",
    "rustboard/ai/rvc_lib/__init__.py",
    "rustboard/ai/rvc_lib/attentions.py",
    "rustboard/ai/rvc_lib/commons.py",
    "rustboard/ai/rvc_lib/cuda_graph.py",
    "rustboard/ai/rvc_lib/fcpe.py",
    "rustboard/ai/rvc_lib/hubert.py",
    "rustboard/ai/rvc_lib/models.py",
    "rustboard/ai/rvc_lib/modules.py",
    "rustboard/ai/rvc_lib/rmvpe.py",
    "rustboard/ai/rvc_lib/transforms.py",
);

/* ---------- where things are ---------- */

#[derive(Clone, Debug)]
pub struct Paths {
    pub dir: PathBuf,
    pub python_dir: PathBuf,
    pub python: PathBuf,
    pub code: PathBuf,
    pub home: PathBuf,
    pub hf: PathBuf,
    pub marker: PathBuf,
}

/// `work` = `Documents/RustBoard`.
pub fn paths(work: &Path) -> Paths {
    let dir = work.join(FOLDER);
    let (python_dir, python) = if cfg!(windows) {
        let d = dir.join("python");
        let exe = d.join("python.exe");
        (d, exe)
    } else {
        let d = dir.join("venv");
        let exe = d.join("bin").join("python");
        (d, exe)
    };
    Paths {
        code: dir.join("code"),
        home: dir.join("home"),
        hf: dir.join("hf"),
        marker: dir.join("ready.json"),
        python_dir,
        python,
        dir,
    }
}

/// Environment for every Python we start: models and caches stay inside our folder.
pub fn env(p: &Paths) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("RUSTBOARD_HOME", p.home.clone()),
        ("HF_HOME", p.hf.clone()),
        ("PYTHONUTF8", PathBuf::from("1")),
        ("PYTHONIOENCODING", PathBuf::from("utf-8")),
        ("PIP_NO_INPUT", PathBuf::from("1")),
    ]
}

/// Was the install finished (by this version of the lists) and is Python still there?
pub fn installed(work: &Path) -> bool {
    let p = paths(work);
    let ok = fs::read_to_string(&p.marker)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("version").and_then(|x| x.as_u64()))
        .map(|v| v == SETUP_VERSION as u64)
        .unwrap_or(false);
    ok && p.python.is_file()
}

/// Write (or refresh) the engine code. Only files that changed are written.
pub fn write_code(dir: &Path) -> Result<(), String> {
    for (rel, text) in FILES {
        let path = dir.join(rel);
        if fs::read_to_string(&path).map(|t| t == *text).unwrap_or(false) {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Cannot make '{}': {e}", parent.display()))?;
        }
        fs::write(&path, text).map_err(|e| format!("Cannot write '{}': {e}", path.display()))?;
    }
    Ok(())
}

/// Delete the whole AI folder (Python, PyTorch, models). Your voices are not in it.
pub fn remove(work: &Path) -> Result<(), String> {
    if state().running {
        return Err("The install is still running. Cancel it first.".into());
    }
    let dir = paths(work).dir;
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("Cannot remove '{}': {e}", dir.display()))
}

/* ---------- state the UI polls ---------- */

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SetupState {
    pub running: bool,
    /// finished without error (until the next start)
    pub done: bool,
    /// 1-based, 0 = not started
    pub step: u32,
    pub steps: u32,
    pub title: String,
    /// the last line of output, shortened
    pub line: String,
    /// 0..=100
    pub percent: u32,
    /// empty = no error. "Cancelled." when the user stopped it.
    pub error: String,
}

static STATE: Mutex<SetupState> = Mutex::new(SetupState {
    running: false,
    done: false,
    step: 0,
    steps: STEPS.len() as u32,
    title: String::new(),
    line: String::new(),
    percent: 0,
    error: String::new(),
});
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn state() -> SetupState {
    STATE.lock().map(|s| s.clone()).unwrap_or_else(|e| e.into_inner().clone())
}

fn update(f: impl FnOnce(&mut SetupState)) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g);
}

pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}

/// Bar position: the shares of the finished steps plus `frac` (0..1) of the current one.
fn percent_at(step: usize, frac: f32) -> u32 {
    let before: u32 = STEPS[..step.min(STEPS.len())].iter().map(|s| s.1).sum();
    let share = STEPS.get(step).map(|s| s.1).unwrap_or(0) as f32;
    ((before as f32 + share * frac.clamp(0.0, 1.0)).round() as u32).min(100)
}

fn begin_step(step: usize, title: &str) {
    update(|s| {
        s.step = step as u32 + 1;
        s.title = title.to_string();
        s.line.clear();
        s.percent = percent_at(step, 0.0);
    });
}

fn progress(step: usize, frac: f32, line: &str) {
    update(|s| {
        s.percent = percent_at(step, frac);
        if !line.is_empty() {
            s.line = shorten(line);
        }
    });
}

fn shorten(line: &str) -> String {
    let t = line.trim();
    if t.chars().count() <= 140 {
        t.to_string()
    } else {
        let cut: String = t.chars().take(137).collect();
        format!("{cut}...")
    }
}

/// Start the install on a worker thread. `cuda` = an NVIDIA card is there (PyTorch with CUDA).
pub fn start(work: PathBuf, cuda: bool) -> Result<(), String> {
    {
        let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if s.running {
            return Err("The install is already running.".into());
        }
        *s = SetupState {
            running: true,
            done: false,
            step: 0,
            steps: STEPS.len() as u32,
            title: "Starting".into(),
            line: String::new(),
            percent: 0,
            error: String::new(),
        };
    }
    CANCEL.store(false, Ordering::Relaxed);
    std::thread::spawn(move || {
        let r = run(&work, cuda, &CANCEL);
        update(|s| {
            s.running = false;
            match r {
                Ok(()) => {
                    s.done = true;
                    s.percent = 100;
                    s.title = "Done".into();
                    s.line.clear();
                }
                Err(e) => {
                    s.done = false;
                    s.error = e;
                }
            }
        });
    });
    Ok(())
}

/* ---------- the install ---------- */

fn run(work: &Path, cuda: bool, cancel: &AtomicBool) -> Result<(), String> {
    let p = paths(work);
    fs::create_dir_all(&p.dir).map_err(|e| format!("Cannot make '{}': {e}", p.dir.display()))?;
    write_code(&p.code)?;
    fs::write(p.dir.join("rb_bridge.py"), BRIDGE).map_err(|e| format!("Cannot write the helper script: {e}"))?;
    fs::write(p.dir.join("requirements-base.txt"), REQ_BASE).map_err(|e| e.to_string())?;
    fs::write(p.dir.join("requirements-ai.txt"), REQ_AI).map_err(|e| e.to_string())?;

    begin_step(0, STEPS[0].0);
    ensure_python(&p, cancel)?;

    begin_step(1, STEPS[1].0);
    ensure_pip(&p, cancel)?;

    begin_step(2, STEPS[2].0);
    pip_install(&p, 2, &["-r", &p.dir.join("requirements-base.txt").to_string_lossy()], cancel)?;

    begin_step(3, if cuda { "Installing PyTorch with CUDA (about 3 GB)" } else { "Installing PyTorch for the CPU (about 200 MB)" });
    let index = if cuda { TORCH_CUDA_INDEX } else { TORCH_CPU_INDEX };
    pip_install(&p, 3, &[TORCH[0], TORCH[1], "--index-url", index], cancel)?;

    begin_step(4, STEPS[4].0);
    pip_install(&p, 4, &["-r", &p.dir.join("requirements-ai.txt").to_string_lossy()], cancel)?;

    begin_step(5, STEPS[5].0);
    let mut c = python_cmd(&p);
    c.arg("-I").arg(p.dir.join("rb_bridge.py")).arg("prepare").arg(&p.code);
    run_logged(c, 5, true, cancel)?;

    let marker = serde_json::json!({ "version": SETUP_VERSION, "cuda": cuda });
    fs::write(&p.marker, marker.to_string()).map_err(|e| format!("Cannot write the finish mark: {e}"))?;
    Ok(())
}

fn python_cmd(p: &Paths) -> Command {
    let mut c = Command::new(&p.python);
    for (k, v) in env(p) {
        c.env(k, v);
    }
    c.current_dir(&p.dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c
}

fn pip_install(p: &Paths, step: usize, args: &[&str], cancel: &AtomicBool) -> Result<(), String> {
    let mut c = python_cmd(p);
    c.args(["-m", "pip", "install", "--no-warn-script-location", "--disable-pip-version-check", "--progress-bar", "off"]);
    c.args(args);
    run_logged(c, step, false, cancel)
}

fn ensure_python(p: &Paths, cancel: &AtomicBool) -> Result<(), String> {
    if p.python.is_file() {
        return Ok(());
    }
    if cfg!(windows) {
        install_embedded_python(p, PYTHON_URL, cancel)
    } else {
        make_venv(p, cancel)
    }
}

/// Windows: download the embeddable Python and unpack it. No installer, nothing in the registry.
fn install_embedded_python(p: &Paths, url: &str, cancel: &AtomicBool) -> Result<(), String> {
    let part = p.dir.join("python-embed.zip.part");
    progress(0, 0.0, "Downloading Python 3.12 ...");
    download(url, &part, 0, "Downloading Python 3.12", cancel)?;
    let tmp = p.dir.join("python.tmp");
    let _ = fs::remove_dir_all(&tmp);
    unzip(&part, &tmp)?;
    patch_pth(&tmp)?;
    let _ = fs::remove_dir_all(&p.python_dir); // a half install from before
    fs::rename(&tmp, &p.python_dir).map_err(|e| format!("Cannot place Python: {e}"))?;
    let _ = fs::remove_file(&part);
    Ok(())
}

/// Linux/macOS: use the Python that is already on the PC, in a venv inside our folder.
fn make_venv(p: &Paths, cancel: &AtomicBool) -> Result<(), String> {
    let ok = |name: &str| {
        Command::new(name)
            .args(["-c", "import sys;sys.exit(0 if (3,10)<=sys.version_info[:2]<=(3,13) else 1)"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let found = ["python3.12", "python3.13", "python3.11", "python3.10", "python3", "python"].into_iter().find(|n| ok(n));
    let Some(name) = found else {
        return Err("Python 3.10 to 3.13 was not found on this PC. Install it from python.org, then press Install again.".into());
    };
    let mut c = Command::new(name);
    c.args(["-m", "venv"]).arg(&p.python_dir).current_dir(&p.dir);
    run_logged(c, 0, false, cancel)
}

fn ensure_pip(p: &Paths, cancel: &AtomicBool) -> Result<(), String> {
    let mut have = python_cmd(p);
    have.args(["-m", "pip", "--version"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if have.status().map(|s| s.success()).unwrap_or(false) {
        return Ok(());
    }
    let script = p.dir.join("get-pip.py");
    download(GET_PIP_URL, &script, 1, "Downloading pip", cancel)?;
    let mut c = python_cmd(p);
    c.arg(&script).args(["--no-warn-script-location", "--disable-pip-version-check"]);
    run_logged(c, 1, false, cancel)?;
    let _ = fs::remove_file(&script);
    Ok(())
}

/* ---------- pieces ---------- */

/// The embeddable Python ignores PYTHONPATH and does not load `site` (so no site-packages)
/// until its `._pth` file says so.
fn patch_pth(dir: &Path) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("Cannot read '{}': {e}", dir.display()))?;
    let pth = rd
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("_pth"))
        .ok_or("This Python has no ._pth file.")?;
    let text = fs::read_to_string(&pth).map_err(|e| e.to_string())?;
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    let site = |l: &String| l.trim() == "#import site" || l.trim() == "import site";
    if !lines.iter().any(|l| l.trim() == "Lib\\site-packages") {
        let at = lines.iter().position(site).unwrap_or(lines.len());
        lines.insert(at, "Lib\\site-packages".to_string());
    }
    for l in lines.iter_mut() {
        if l.trim() == "#import site" {
            *l = "import site".to_string();
        }
    }
    if !lines.iter().any(|l| l.trim() == "import site") {
        lines.push("import site".to_string());
    }
    fs::write(&pth, lines.join(nl) + nl).map_err(|e| format!("Cannot write '{}': {e}", pth.display()))
}

fn unzip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("The download is not a zip file: {e}"))?;
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = f.enclosed_name() else { continue }; // skips "../" tricks
        let out = dest.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut w = fs::File::create(&out).map_err(|e| format!("Cannot write '{}': {e}", out.display()))?;
        std::io::copy(&mut f, &mut w).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Download to `dest` (through a `.part` name inside the caller). Progress goes to the bar.
fn download(url: &str, dest: &Path, step: usize, what: &str, cancel: &AtomicBool) -> Result<(), String> {
    let resp = ureq::get(url).call().map_err(|e| format!("Download failed ({url}): {e}"))?;
    let total: u64 = resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(dest).map_err(|e| format!("Cannot write '{}': {e}", dest.display()))?;
    let (mut buf, mut done, mut last) = (vec![0u8; 64 * 1024], 0u64, 0u64);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if done - last >= 256 * 1024 {
            last = done;
            let mb = done as f32 / 1_048_576.0;
            if total > 0 {
                progress(step, done as f32 / total as f32, &format!("{what}: {:.1} of {:.1} MB", mb, total as f32 / 1_048_576.0));
            } else {
                progress(step, 0.5, &format!("{what}: {mb:.1} MB"));
            }
        }
    }
    Ok(())
}

/// A JSON line `{"ok": false, "error": "..."}` from rb_bridge.py (it exits with code 0 then).
fn json_error(lines: &VecDeque<String>) -> Option<String> {
    lines.iter().rev().find_map(|l| {
        let t = l.trim();
        if !t.starts_with('{') {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(t).ok()?;
        if v.get("ok").and_then(|x| x.as_bool()) == Some(false) {
            Some(v.get("error").and_then(|x| x.as_str()).unwrap_or("The engine setup failed.").to_string())
        } else {
            None
        }
    })
}

/// Run a program, show its last output line, stop it when `cancel` is set.
/// The bar moves a little with every line (we cannot know how long pip will take).
fn run_logged(mut cmd: Command, step: usize, json_result: bool, cancel: &AtomicBool) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("Cannot start the program: {e}"))?;
    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        }));
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    let mut tail: VecDeque<String> = VecDeque::with_capacity(32);
    let mut count: u32 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cancelled.".into());
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                count += 1;
                // 1 - 1/(1+n/10): moves with every line, never reaches the end of the step
                let frac = 1.0 - 1.0 / (1.0 + count as f32 / 10.0);
                progress(step, frac, &line);
                if tail.len() == 32 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    for r in readers {
        let _ = r.join();
    }
    if json_result {
        if let Some(e) = json_error(&tail) {
            return Err(e);
        }
    }
    if !status.success() {
        let last: Vec<&str> = tail.iter().rev().take(4).map(|s| s.trim()).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("This step failed (exit code {}). {}", status.code().unwrap_or(-1), last.join(" | ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustboard-aisetup-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }

    #[test]
    fn every_engine_file_is_packed_and_nothing_else() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("engine");
        let mut on_disk = Vec::new();
        walk(&base, &base, &mut on_disk);
        // rb_bridge.py is packed separately (BRIDGE); caches are not source
        on_disk.retain(|f| f != "rb_bridge.py" && !f.contains("__pycache__"));
        on_disk.sort();
        let mut packed: Vec<String> = FILES.iter().map(|f| f.0.to_string()).collect();
        packed.sort();
        assert_eq!(packed, on_disk);
    }

    #[test]
    fn write_code_writes_once_and_refreshes_changes() {
        let d = temp("code");
        write_code(&d).unwrap();
        assert!(d.join("rustboard").join("ai").join("engine.py").is_file());
        assert!(d.join("rustboard").join("ai").join("rvc_lib").join("LICENSE-RVC").is_file());
        // a changed file is put back
        let f = d.join("rustboard").join("__init__.py");
        fs::write(&f, "old").unwrap();
        write_code(&d).unwrap();
        assert_ne!(fs::read_to_string(&f).unwrap(), "old");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_bar_adds_up_and_never_goes_back() {
        assert_eq!(STEPS.iter().map(|s| s.1).sum::<u32>(), 100);
        assert_eq!(percent_at(0, 0.0), 0);
        assert_eq!(percent_at(3, 0.0), 12);
        assert_eq!(percent_at(3, 0.5), 37);
        assert_eq!(percent_at(5, 1.0), 100);
        let mut last = 0;
        for step in 0..STEPS.len() {
            for i in 0..=10 {
                let p = percent_at(step, i as f32 / 10.0);
                assert!(p >= last, "step {step} frac {i}: {p} < {last}");
                last = p;
            }
        }
    }

    #[test]
    fn pth_gets_site_and_site_packages() {
        let d = temp("pth");
        fs::write(d.join("python312._pth"), "python312.zip\r\n.\r\n\r\n# Uncomment to run site.main() automatically\r\n#import site\r\n").unwrap();
        patch_pth(&d).unwrap();
        let t = fs::read_to_string(d.join("python312._pth")).unwrap();
        assert!(t.contains("\r\nimport site\r\n"), "{t:?}");
        assert!(!t.contains("#import site"));
        let lines: Vec<&str> = t.lines().collect();
        let sp = lines.iter().position(|l| *l == "Lib\\site-packages").unwrap();
        let site = lines.iter().position(|l| *l == "import site").unwrap();
        assert!(sp < site);
        // running it again changes nothing
        patch_pth(&d).unwrap();
        assert_eq!(fs::read_to_string(d.join("python312._pth")).unwrap(), t);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unzip_unpacks_and_ignores_paths_that_climb_out() {
        use zip::write::SimpleFileOptions;
        let d = temp("zip");
        let zp = d.join("a.zip");
        {
            let mut w = zip::ZipWriter::new(fs::File::create(&zp).unwrap());
            let o = SimpleFileOptions::default();
            w.start_file("python.exe", o).unwrap();
            w.write_all(b"exe").unwrap();
            w.start_file("sub/dir/x.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.start_file("../evil.txt", o).unwrap();
            w.write_all(b"bad").unwrap();
            w.finish().unwrap();
        }
        let out = d.join("out");
        unzip(&zp, &out).unwrap();
        assert_eq!(fs::read(out.join("python.exe")).unwrap(), b"exe");
        assert_eq!(fs::read(out.join("sub").join("dir").join("x.txt")).unwrap(), b"x");
        assert!(!d.join("evil.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn json_error_reads_the_last_failure_line() {
        let mut l: VecDeque<String> = VecDeque::new();
        l.push_back("downloading x ...".into());
        assert_eq!(json_error(&l), None);
        l.push_back(r#"{"ok": true}"#.into());
        assert_eq!(json_error(&l), None);
        l.push_back(r#"{"ok": false, "error": "no internet"}"#.into());
        assert_eq!(json_error(&l).as_deref(), Some("no internet"));
    }

    #[test]
    fn installed_needs_the_mark_and_python() {
        let work = temp("inst");
        assert!(!installed(&work));
        let p = paths(&work);
        fs::create_dir_all(p.python.parent().unwrap()).unwrap();
        fs::write(&p.python, "x").unwrap();
        assert!(!installed(&work), "no mark yet");
        fs::write(&p.marker, format!("{{\"version\": {}}}", SETUP_VERSION)).unwrap();
        assert!(installed(&work));
        fs::write(&p.marker, "{\"version\": 0}").unwrap();
        assert!(!installed(&work), "older lists: install again");
        let _ = fs::remove_dir_all(&work);
    }

    #[cfg(unix)]
    #[test]
    fn run_logged_shows_output_and_reports_failure() {
        let no = AtomicBool::new(false);
        let mut ok = Command::new("sh");
        ok.args(["-c", "echo one; echo two 1>&2; exit 0"]);
        run_logged(ok, 0, false, &no).unwrap();
        assert!(state().line == "one" || state().line == "two");

        let mut bad = Command::new("sh");
        bad.args(["-c", "echo first; echo ERROR: boom 1>&2; exit 3"]);
        let e = run_logged(bad, 0, false, &no).unwrap_err();
        assert!(e.contains("exit code 3") && e.contains("boom"), "{e}");

        // the helper script says "failed" in JSON and exits with 0
        let mut j = Command::new("sh");
        j.args(["-c", r#"echo '{"ok": false, "error": "PyTorch is missing"}'; exit 0"#]);
        assert_eq!(run_logged(j, 5, true, &no).unwrap_err(), "PyTorch is missing");
    }

    /// Real run of steps 1 to 3 with a tiny package (not PyTorch). `cargo test -- --ignored`
    #[cfg(unix)]
    #[ignore = "needs Python and the internet"]
    #[test]
    fn real_python_pip_and_a_package() {
        let no = AtomicBool::new(false);
        let work = temp("real");
        let p = paths(&work);
        fs::create_dir_all(&p.dir).unwrap();
        ensure_python(&p, &no).unwrap();
        assert!(p.python.is_file());
        ensure_pip(&p, &no).unwrap();
        pip_install(&p, 2, &["six"], &no).unwrap();
        let mut c = python_cmd(&p);
        c.args(["-c", "import six; print(six.__version__)"]);
        run_logged(c, 2, false, &no).unwrap();
        assert!(state().line.chars().next().unwrap().is_ascii_digit(), "{:?}", state().line);
        let _ = fs::remove_dir_all(&work);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_kills_the_program() {
        let stop = AtomicBool::new(false);
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(400));
                stop.store(true, Ordering::Relaxed);
            });
            let e = run_logged(c, 0, false, &stop).unwrap_err();
            assert_eq!(e, "Cancelled.");
        });
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
