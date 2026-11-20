// no console window on Windows release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Custom setup + uninstaller for Rustboard (Windows, per-user, no admin).
//!
//! One exe, two faces:
//!   * setup:     `Rustboard_x.y.z_x64-setup.exe`  -> `dist/index.html`     (installer-ui/)
//!   * uninstall: `<install dir>\uninstall.exe`    -> `dist/uninstall.html` (uninstaller-ui/)
//!
//! The real app (`rustboard.exe`) is embedded with `include_bytes!`. Setup writes it to the
//! install dir, copies itself there as `uninstall.exe`, makes shortcuts and the
//! "Apps & features" registry entry. The uninstaller is this same exe run with `--uninstall`.
//!
//! The in-app updater starts the setup with `/P /R` (same flags the old NSIS setup took):
//!   /P or /S = no window, install over the current copy
//!   /R       = start Rustboard again when done

use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Built by CI / `tools/build_setup.ps1` before this crate is compiled.
static PAYLOAD: &[u8] = include_bytes!("../payload/rustboard.exe");

const APP_EXE: &str = "rustboard.exe";
const UNINSTALL_EXE: &str = "uninstall.exe";
const UNINST_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Rustboard";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Tauri identifier of the app / of this setup. Both keep a WebView2 cache in %LOCALAPPDATA%.
const APP_ID: &str = "com.rustboard.app";
const SETUP_ID: &str = "com.rustboard.setup";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where the last install went (for "Run Rustboard" on the last page).
static LAST_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Set when the uninstall finished, so closing the window also removes uninstall.exe.
static UNINSTALLED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------- helpers

/// A command that never flashes a console window.
fn hidden(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

fn run_ok(mut cmd: Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn progress(app: &AppHandle, event: &str, percent: u32, message: &str) {
    let _ = app.emit(event, json!({ "percent": percent, "message": message }));
}

/// Files may stay locked for a moment after the app is killed. Try a few times.
fn retry<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut last = None;
    for _ in 0..10 {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    Err(last.expect("loop runs at least once"))
}

fn kill_app() {
    let mut c = hidden("taskkill");
    c.args(["/IM", APP_EXE, "/F"]);
    let _ = c.output();
    std::thread::sleep(Duration::from_millis(400));
}

fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

fn default_dir_path() -> PathBuf {
    // same place the old NSIS setup used (per-user install)
    local_app_data().unwrap_or_else(|| PathBuf::from(r"C:\")).join("Rustboard")
}

/// The uninstaller lives in the install dir.
fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(default_dir_path)
}

// ---------------------------------------------------------------- registry (reg.exe)

fn reg_add(key: &str, name: &str, kind: &str, data: &str) -> Result<(), String> {
    let mut c = hidden("reg");
    c.args(["add", key, "/v", name, "/t", kind, "/d", data, "/f"]);
    if run_ok(c) {
        Ok(())
    } else {
        Err(format!("Tidak bisa menulis registry ({name})."))
    }
}

fn reg_delete_value(key: &str, name: &str) {
    let mut c = hidden("reg");
    c.args(["delete", key, "/v", name, "/f"]);
    let _ = c.output();
}

fn reg_delete_key(key: &str) {
    let mut c = hidden("reg");
    c.args(["delete", key, "/f"]);
    let _ = c.output();
}

/// Reads a REG_SZ value. `reg query` prints:  `    Name    REG_SZ    value`
fn reg_get(key: &str, name: &str) -> Option<String> {
    let out = hidden("reg").args(["query", key, "/v", name]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    text.lines().find_map(|line| {
        let line = line.trim();
        let rest = line.strip_prefix(name)?.trim_start();
        let rest = rest.strip_prefix("REG_SZ")?.trim();
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

// ---------------------------------------------------------------- shortcuts (PowerShell + WScript.Shell)

fn ps(script: &str) -> bool {
    let mut c = hidden("powershell");
    c.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    run_ok(c)
}

/// PowerShell string literal: single quotes doubled.
fn q(s: &str) -> String {
    s.replace('\'', "''")
}

/// `kind` = "Desktop" or "Programs" (Start menu). Windows resolves OneDrive-moved folders for us.
fn make_shortcut(kind: &str, exe: &Path, dir: &Path) -> bool {
    let exe = q(&exe.display().to_string());
    let dir = q(&dir.display().to_string());
    ps(&format!(
        "$p = Join-Path ([Environment]::GetFolderPath('{kind}')) 'Rustboard.lnk'; \
         $s = (New-Object -ComObject WScript.Shell).CreateShortcut($p); \
         $s.TargetPath = '{exe}'; $s.WorkingDirectory = '{dir}'; $s.IconLocation = '{exe},0'; $s.Save()"
    ))
}

fn remove_shortcut(kind: &str) {
    ps(&format!(
        "$p = Join-Path ([Environment]::GetFolderPath('{kind}')) 'Rustboard.lnk'; \
         Remove-Item -LiteralPath $p -Force -ErrorAction SilentlyContinue"
    ));
}

// ---------------------------------------------------------------- install

struct Opts {
    dir: PathBuf,
    desktop: bool,
    start_menu: bool,
    autostart: bool,
}

fn do_install(app: &AppHandle, o: &Opts) -> Result<(), String> {
    let step = |pc: u32, msg: &str| progress(app, "install-progress", pc, msg);

    step(5, "Menutup Rustboard…");
    kill_app();

    step(15, "Membuat folder…");
    std::fs::create_dir_all(&o.dir).map_err(|e| format!("Tidak bisa membuat folder: {e}"))?;

    step(30, "Menyalin rustboard.exe");
    let exe = o.dir.join(APP_EXE);
    retry(|| std::fs::write(&exe, PAYLOAD))
        .map_err(|e| format!("Tidak bisa menulis rustboard.exe: {e}"))?;

    step(55, "Menyalin uninstaller");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstall = o.dir.join(UNINSTALL_EXE);
    if me != uninstall {
        retry(|| std::fs::copy(&me, &uninstall).map(|_| ()))
            .map_err(|e| format!("Tidak bisa menulis uninstall.exe: {e}"))?;
    }

    step(70, "Membuat shortcut");
    // best effort: a blocked PowerShell must not fail the whole install
    if o.desktop {
        make_shortcut("Desktop", &exe, &o.dir);
    }
    if o.start_menu {
        make_shortcut("Programs", &exe, &o.dir);
    }
    if o.autostart {
        reg_add(RUN_KEY, "Rustboard", "REG_SZ", &format!("\"{}\"", exe.display()))?;
    } else {
        reg_delete_value(RUN_KEY, "Rustboard");
    }

    step(85, "Menulis registry uninstall");
    let exe_s = exe.display().to_string();
    let un_cmd = format!("\"{}\" --uninstall", uninstall.display());
    reg_add(UNINST_KEY, "DisplayName", "REG_SZ", "Rustboard")?;
    reg_add(UNINST_KEY, "DisplayVersion", "REG_SZ", env!("CARGO_PKG_VERSION"))?;
    reg_add(UNINST_KEY, "Publisher", "REG_SZ", "Rustboard")?;
    reg_add(UNINST_KEY, "InstallLocation", "REG_SZ", &o.dir.display().to_string())?;
    reg_add(UNINST_KEY, "DisplayIcon", "REG_SZ", &exe_s)?;
    reg_add(UNINST_KEY, "UninstallString", "REG_SZ", &un_cmd)?;
    reg_add(UNINST_KEY, "NoModify", "REG_DWORD", "1")?;
    reg_add(UNINST_KEY, "NoRepair", "REG_DWORD", "1")?;

    step(100, "Selesai");
    Ok(())
}

/// `/P /R` from the in-app updater: no window, same folder as the current install.
fn silent_install(app: &AppHandle) -> Result<(), String> {
    let dir = reg_get(UNINST_KEY, "InstallLocation")
        .map(PathBuf::from)
        .unwrap_or_else(default_dir_path);
    let opts = Opts {
        dir: dir.clone(),
        desktop: false,    // shortcuts from the first install stay as they are
        start_menu: false,
        autostart: reg_get(RUN_KEY, "Rustboard").is_some(), // keep the user's choice
    };
    do_install(app, &opts)?;
    *LAST_DIR.lock().map_err(|e| e.to_string())? = Some(dir);
    Ok(())
}

// ---------------------------------------------------------------- uninstall

fn do_uninstall(app: &AppHandle, remove_data: bool) -> Result<(), String> {
    let step = |pc: u32, msg: &str| progress(app, "uninstall-progress", pc, msg);
    let dir = install_dir();

    step(10, "Menutup Rustboard…");
    kill_app();

    step(30, "Menghapus shortcut");
    remove_shortcut("Desktop");
    remove_shortcut("Programs");
    reg_delete_value(RUN_KEY, "Rustboard");

    step(55, "Menghapus file program");
    let exe = dir.join(APP_EXE);
    if exe.exists() {
        retry(|| std::fs::remove_file(&exe))
            .map_err(|e| format!("Tidak bisa menghapus rustboard.exe: {e}"))?;
    }

    step(75, "Menghapus entri Windows");
    reg_delete_key(UNINST_KEY);

    step(90, "Merapikan sisa file");
    if remove_data {
        // settings + sound list (config.json) and the WebView2 cache of the app
        if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
            let _ = std::fs::remove_dir_all(roaming.join(APP_ID));
        }
        if let Some(local) = local_app_data() {
            let _ = std::fs::remove_dir_all(local.join(APP_ID));
        }
    }

    step(100, "Selesai");
    Ok(())
}

/// After we exit: remove our own WebView2 cache and (for the uninstaller) uninstall.exe + the
/// install folder. `rmdir` without /S only removes an EMPTY folder, so files the user put there
/// are never deleted.
fn schedule_cleanup(uninstall_dir: Option<&Path>) {
    let mut script = String::from("ping -n 3 127.0.0.1 >nul");
    if let Some(local) = local_app_data() {
        script += &format!(" & rmdir /S /Q \"{}\"", local.join(SETUP_ID).display());
    }
    if let Some(dir) = uninstall_dir {
        script += &format!(
            " & del /F /Q \"{}\" & rmdir \"{}\"",
            dir.join(UNINSTALL_EXE).display(),
            dir.display()
        );
    }
    let mut c = Command::new("cmd");
    #[cfg(windows)]
    {
        c.raw_arg(format!("/C {script}"));
        c.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    c.arg(script);
    let _ = c.spawn();
}

fn launch_installed() {
    let dir = LAST_DIR.lock().ok().and_then(|g| g.clone());
    if let Some(dir) = dir {
        let _ = Command::new(dir.join(APP_EXE)).current_dir(&dir).spawn();
    }
}

fn close(app: &AppHandle) {
    if UNINSTALLED.load(Ordering::SeqCst) {
        schedule_cleanup(Some(&install_dir()));
    } else {
        schedule_cleanup(None);
    }
    app.exit(0);
}

// ---------------------------------------------------------------- commands (see installer-ui / uninstaller-ui README)

#[tauri::command]
fn default_dir() -> String {
    default_dir_path().display().to_string()
}

/// Folder picker. The result always ends in `Rustboard`, so uninstall never touches a shared folder.
#[tauri::command(async)]
fn pick_dir(app: AppHandle) -> Option<String> {
    let picked = app.dialog().file().blocking_pick_folder()?;
    let mut path = picked.into_path().ok()?;
    let named = path
        .file_name()
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("rustboard"))
        .unwrap_or(false);
    if !named {
        path.push("Rustboard");
    }
    Some(path.display().to_string())
}

#[tauri::command(async)]
fn install(
    app: AppHandle,
    dir: String,
    desktop: bool,
    start_menu: bool,
    autostart: bool,
) -> Result<(), String> {
    let dir = PathBuf::from(dir.trim());
    if !dir.is_absolute() || dir.parent().is_none() {
        return Err("Folder tidak valid. Pilih satu folder, bukan drive.".into());
    }
    do_install(&app, &Opts { dir: dir.clone(), desktop, start_menu, autostart })?;
    *LAST_DIR.lock().map_err(|e| e.to_string())? = Some(dir);
    Ok(())
}

#[tauri::command(async)]
fn uninstall(app: AppHandle, remove_data: bool) -> Result<(), String> {
    do_uninstall(&app, remove_data)?;
    UNINSTALLED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Setup: `run` = start Rustboard. Uninstaller: no args. Then close.
#[tauri::command]
fn finish(app: AppHandle, run: Option<bool>) {
    if run.unwrap_or(false) {
        launch_installed();
    }
    close(&app);
}

#[tauri::command]
fn cancel(app: AppHandle) {
    close(&app);
}

// ---------------------------------------------------------------- main

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a.eq_ignore_ascii_case(flag));

    let by_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .map(|n| n.starts_with("uninstall"))
        .unwrap_or(false);
    let uninstall_mode = has("--uninstall") || by_name;
    let silent = !uninstall_mode && (has("/P") || has("/S"));
    let run_after = has("/R");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            default_dir,
            pick_dir,
            install,
            uninstall,
            finish,
            cancel
        ])
        .setup(move |app| {
            if silent {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    match silent_install(&handle) {
                        Ok(()) => {
                            if run_after {
                                launch_installed();
                            }
                        }
                        Err(e) => {
                            handle
                                .dialog()
                                .message(format!("Update gagal:\n{e}"))
                                .title("Rustboard")
                                .kind(MessageDialogKind::Error)
                                .blocking_show();
                        }
                    }
                    handle.exit(0);
                });
                return Ok(());
            }

            let (page, title) = if uninstall_mode {
                ("uninstall.html", "Hapus Rustboard")
            } else {
                ("index.html", "Pasang Rustboard")
            };
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()))
                .title(title)
                .inner_size(760.0, 520.0)
                .resizable(false)
                .decorations(false)
                .center()
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Rustboard setup");
}
