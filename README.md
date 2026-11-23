# Elastic GUI

[![CI](https://github.com/simenglabs/elastic-gui/actions/workflows/ci.yml/badge.svg)](https://github.com/simenglabs/elastic-gui/actions/workflows/ci.yml)

Aplikasi desktop native untuk Elasticsearch (Rust + [egui/eframe](https://docs.rs/eframe/0.37.0/eframe/)). Mendukung banyak cluster sekaligus. Spesifikasi lengkap ada di [`docs/PRD.md`](docs/PRD.md).

## Download macOS

| Mac | Installer terbaru |
| --- | --- |
| Apple Silicon (M1/M2/M3/M4 dan lebih baru) | [Download DMG — Apple Silicon](https://github.com/simenglabs/elastic-gui/releases/latest/download/Elastic-GUI-macos-arm64.dmg) |
| Intel | [Download DMG — Intel](https://github.com/simenglabs/elastic-gui/releases/latest/download/Elastic-GUI-macos-x86_64.dmg) |

Minimum **macOS 13**. Buka DMG, lalu seret **Elastic GUI.app** ke **Applications**. Semua versi dan file checksum tersedia di [GitHub Releases](https://github.com/simenglabs/elastic-gui/releases/latest).

Paket saat ini memakai tanda tangan **ad-hoc** dan belum [dinotarisasi Apple](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution/). Saat pertama dibuka, macOS dapat meminta persetujuan melalui **System Settings → Privacy & Security → Open Anyway**.

Modul:

- **Discover**: pilih index/alias/data stream, rentang waktu, query Lucene atau DSL, filter pills, histogram (drag untuk zoom), tabel virtualized dengan paging point-in-time, detail dokumen, dan top values per field.
- **Dev Tools**: editor multi-request dengan autocomplete endpoint, query parameter, body JSON (dari spesifikasi API resmi ES 7/8/9), nama index, dan nama field. Ada juga history, multi tab, salin cURL, dan rapikan JSON. Blok JSON bisa dilipat lewat penanda ▾/▸ di gutter (hanya tampilan, teks tidak berubah).
- **Dashboard**: panel Metric, Time series, Bar, Pie, Tabel, dan Markdown dengan waktu, query, dan filter global. Klik kategori untuk memfilter. Dashboard bisa diekspor dan diimpor sebagai JSON.
- **Index**: daftar index (health, ukuran, alias, data stream), detail overview/settings/mappings, buat index, open/close, refresh/flush/clear cache, edit setting dinamis, tambah field, dan hapus (wajib ketik nama).
- **Cluster**: profile per cluster (Basic, API key, Bearer; CA cert; header; failover node). Secret disimpan di keychain OS. Mode **read-only** memblokir semua request yang mengubah data, dan profile `prod` diberi penanda merah.

## Menjalankan

Pasang Rust stable melalui [rustup](https://rustup.rs/), lalu jalankan:

```sh
cargo run
```

Buat profile cluster di menu **Cluster**. Untuk mencoba cepat tanpa membuat profile, isi environment variable berikut. Profile `ENV` lalu muncul otomatis; secret-nya hanya disimpan di memori.

```sh
export ELASTIC_URL=https://host:9200        # beberapa node dipisah koma
export ELASTIC_USER=elastic                 # atau ELASTIC_API_KEY=...
export ELASTIC_PASSWORD=...
cargo run
```

Shortcut Dev Tools: `Ctrl/Cmd+Enter` menjalankan request, `Ctrl+Space` membuka saran, `Ctrl/Cmd+I` merapikan JSON.

`Ctrl/Cmd+K` membuka command palette untuk pindah modul, pindah cluster, atau membuka index di Discover.

Bahasa UI: pemilih **ID | EN** di top bar mengganti antarmuka antara Indonesia dan Inggris. Pilihan disimpan di `settings.json`.

## Tampilan

Tema awal memakai latar biru lembut, panel putih, dan navigasi sidebar yang bisa diciutkan. Sidebar otomatis ringkas pada jendela kecil. Tombol **Gelap / Terang** mengganti tema; cluster dan environment aktif tetap ditampilkan di header.

Dev Tools mengisi tinggi workspace dan menyediakan **Bungkus baris** pada tampilan respons teks. Form cluster memisahkan koneksi, autentikasi, dan **Pengaturan lanjutan**.

## Struktur

```text
src/
  main.rs            # Entry point dan konfigurasi jendela
  app.rs             # Shell: switcher cluster, navigasi, workspace per profile
  theme.rs           # Warna, tipografi, panel, ikon vektor, dan navigasi bersama
  task.rs            # Kerja latar belakang (thread + channel) agar UI tidak blok
  profile.rs         # Model profile, penyimpanan JSON, keychain
  es/
    client.rs        # HTTP ke ES: auth, TLS, failover, guard read-only, error
    catalog.rs       # Cache index/alias/data stream dan field (_field_caps)
    query.rs         # Query bar + filter + waktu -> bool DSL, date math, interval
    spec.rs          # Spesifikasi API untuk autocomplete
    spec/es{7,8,9}.json
  ui/
    console/         # Dev Tools (parser, autocomplete, UI)
    discover.rs
    dashboard.rs
    indices.rs
    profiles.rs
  widgets/           # Highlight, JSON tree, time picker, filter pills, dialog
tools/gen_spec.py    # Generator spec ringkas dari elastic/elasticsearch-specification
```

Data aplikasi (profile, history console, tab, dashboard) disimpan di direktori data OS, misalnya `~/Library/Application Support/id.MengLabs.elastic-gui/` di macOS.

### Memperbarui spec autocomplete

Unduh `output/schema/schema.json` dari [elastic/elasticsearch-specification](https://github.com/elastic/elasticsearch-specification) (Apache-2.0) pada branch versi yang diinginkan, lalu jalankan:

```sh
python3 tools/gen_spec.py schema.json src/es/spec/es9.json
```

## Build dan pemeriksaan

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Test integrasi ke cluster nyata hanya berjalan jika diminta. Koneksinya selalu dipaksa read-only. `LIVE_PATTERN` wajib diisi pola index yang berisi data; `LIVE_FINGERPRINT` (SHA-256 sertifikat server) dipakai test pin TLS:

```sh
ELASTIC_URL=... ELASTIC_USER=... ELASTIC_PASSWORD=... LIVE_PATTERN='logs-*' \
  LIVE_FINGERPRINT='AB:CD:…' cargo test -- --ignored --test-threads=1
```

Hasil build berada di `target/release/elastic-gui` (`elastic-gui.exe` pada Windows). Build menghasilkan executable untuk OS tempat perintah dijalankan.

### Build DMG di macOS

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release
python3 tools/package_macos.py --target aarch64-apple-darwin
```

Untuk Mac Intel, gunakan `--target x86_64-apple-darwin`. Jalankan pada Mac dengan arsitektur yang sesuai. Script memeriksa arsitektur executable, membuat app bundle, menandatanganinya secara ad-hoc, dan memverifikasi DMG. Hasilnya berada di `dist/`, beserta file `.sha256`.

### CI dan release

- Push ke `main`, pull request ke `main`, atau **Actions → CI → Run workflow** menjalankan format, Clippy, unit test, build release, dan packaging DMG pada Apple Silicon serta Intel. DMG tersedia sebagai artifact selama 14 hari.
- Push tag `v*` menjalankan validasi versi, pemeriksaan dan build yang sama, lalu menerbitkan kedua DMG serta checksum ke GitHub Releases. Tag harus sama dengan versi di `Cargo.toml`.
- Nama asset tetap sehingga tautan download di atas otomatis mengikuti release stabil terbaru. Tag versi prerelease seperti `v0.2.0-beta.1` tidak diterbitkan sebagai release stabil terbaru.

Contoh menerbitkan versi setelah memperbarui `Cargo.toml` dan `Cargo.lock`, lalu commit dan push ke `main`:

```sh
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

## Pengukuran performa

Target NFR (PRD bagian 8): startup <= 1 dtk sampai jendela tampil (release), daftar 10.000 index lancar, editor 5.000 baris responsif.

Test performa tidak butuh cluster dan harus dijalankan di build release:

```sh
cargo test --release -- --ignored perf --nocapture
```

- `perf_highlight_5k_lines`: `widgets::highlight` pada 5.000 baris Console, batas < 50 ms.
- `perf_filter_sort_10k_rows`: filter + sort 10.000 baris index, batas < 20 ms.

Waktu startup dan memori:

```sh
ELASTIC_GUI_TIMING=1 target/release/elastic-gui
```

Saat frame pertama selesai digambar, stderr mencetak `[timing] frame pertama selesai: N ms dari main(); RSS: M MB`. N adalah waktu sejak awal `main()`, M adalah memori proses (via `ps`, tidak tersedia di Windows). Tanpa variabel itu tidak ada output.

## Kebutuhan sistem

- **macOS:** Xcode Command Line Tools (`xcode-select --install`).
- **Windows:** Rust toolchain MSVC dan Visual Studio Build Tools dengan komponen Desktop development with C++.
- **Linux (Ubuntu/Debian):** compiler C/C++, pkg-config, library pengembangan X11/Wayland dan OpenGL, serta Secret Service (gnome-keyring/KWallet) untuk menyimpan secret. Contoh:

  ```sh
  sudo apt install build-essential pkg-config libx11-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev libdbus-1-dev
  ```

Dependensi dikunci melalui `Cargo.lock` agar build aplikasi konsisten.
