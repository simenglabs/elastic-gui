# Elastic GUI

Aplikasi desktop native untuk Elasticsearch (Rust + [egui/eframe](https://docs.rs/eframe/0.37.0/eframe/)). Mendukung banyak cluster sekaligus. Spesifikasi lengkap ada di [`docs/PRD.md`](docs/PRD.md).

Modul:

- **Discover**: pilih index/alias/data stream, rentang waktu, query Lucene atau DSL, filter pills, histogram (drag untuk zoom), tabel virtualized dengan paging point-in-time, detail dokumen, dan top values per field.
- **Dev Tools**: editor multi-request dengan autocomplete endpoint, query parameter, body JSON (dari spesifikasi API resmi ES 7/8/9), nama index, dan nama field. Ada juga history, multi tab, salin cURL, dan rapikan JSON.
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

## Struktur

```text
src/
  main.rs            # Entry point dan konfigurasi jendela
  app.rs             # Shell: switcher cluster, navigasi, workspace per profile
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

Hasil build berada di `target/release/elastic-gui` (`elastic-gui.exe` pada Windows). Build menghasilkan executable untuk OS tempat perintah dijalankan; installer dan app bundle belum dikonfigurasi.

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
