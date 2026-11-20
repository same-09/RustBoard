# Uninstaller UI custom

`index.html` = tampilan hapus Rustboard. CSS sama dengan `installer-ui/` dan `ui/style.css`.
Buka di browser untuk coba. Tanpa Tauri, file ini memakai mock.

**Sudah tersambung ke build.** `tools/build_setup.ps1` menyalin file ini ke `setup/dist/uninstall.html`.
Saat setup jalan, ia menyalin dirinya ke `<folder pasang>\uninstall.exe`. Exe itu dijalankan dengan `--uninstall`
(dari Settings > Apps, atau klik dua kali) dan membuka halaman ini.

Command dari UI: `uninstall({ removeData })`, `finish`, `cancel`.
Event dari Rust: `uninstall-progress` `{ percent, message }`.
