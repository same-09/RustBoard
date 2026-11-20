# Setup UI custom

`index.html` = tampilan setup dengan CSS yang sama dengan `ui/style.css` (token Liner, rail, panel, kartu setting, switch, tombol hitam, latar berganti warna per langkah). Terang dan gelap ikut sistem.
Buka di browser untuk coba. Tanpa Tauri, file ini memakai mock.

Belum tersambung ke build. `cargo tauri build` masih membuat setup NSIS (lihat `tauri.conf.json`).
Untuk memakai UI ini, butuh app installer Tauri sendiri: window 760x520, `decorations: false`,
`withGlobalTauri: true`, dan izin window (minimize, close, start-dragging).

Command yang dipanggil UI: `pick_dir`, `install(opts)`, `finish({ run })`, `cancel`, dan `default_dir` (opsional).
`opts` = `{ dir, desktop, startMenu, autostart }`.
Event dari Rust: `install-progress` `{ percent, message }`.
