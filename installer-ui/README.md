# Setup UI custom

`index.html` = tampilan setup dengan CSS yang sama dengan `ui/style.css` (token Liner, rail, panel, kartu setting, switch, tombol hitam, latar berganti warna per langkah). Terang dan gelap ikut sistem.
Buka di browser untuk coba. Tanpa Tauri, file ini memakai mock.

**Sudah tersambung ke build.** `tools/build_setup.ps1` menyalin file ini ke `setup/dist/index.html`,
lalu app Tauri di `setup/` (window 760x520, `decorations: false`) memuatnya. Rust-nya ada di `setup/src-tauri/src/main.rs`.

Command yang dipanggil UI: `pick_dir`, `install({ dir, desktop, startMenu, autostart })`, `finish({ run })`, `cancel`, dan `default_dir`.
Event dari Rust: `install-progress` `{ percent, message }`.
