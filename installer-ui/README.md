# Setup UI (belum tersambung)

`index.html` = tampilan installer gaya Rustboard. Buka di browser untuk coba (pakai mock).
Butuh app installer Tauri sendiri. Command yang dipanggil UI: `install`, `pick_dir`, `finish`, `cancel`.
Event dari Rust: `install-progress` `{percent, message}`.
