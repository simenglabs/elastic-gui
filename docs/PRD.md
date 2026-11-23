# PRD — Elastic GUI

| | |
|---|---|
| Status | Draft v0.1 |
| Tanggal | 23 November 2026 |
| Platform | Aplikasi desktop native (macOS, Windows, Linux) |
| Stack | Rust 2024 + egui/eframe 0.37 (lihat `Cargo.toml`) |

### Status implementasi (23 November 2026)

Bagian ini menggantikan klaim "P0 selesai" sebelumnya, yang terlalu luas. Pernyataan di bawah dibatasi pada apa yang benar-benar sudah diperiksa.

**Terimplementasi di kode (lulus `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`):**

- Cluster: deteksi capability dan privilege per index/template (PRF-11, PRF-12), PKI/mTLS dan Cloud ID (PRF-04), pin fingerprint TLS (PRF-06), HTTP proxy (PRF-08), tab cluster + modul (PRF-15), ekspor/impor profile tanpa secret (PRF-16), duplikat profile (PRF-17), failover node dengan round-robin (PRF-02).
- Dev Tools: lint (CON-06), variabel `${X}` (CON-07), saran live (CON-23), saran nilai `_terms_enum` (CON-29), link dokumentasi (CON-30), cancel task via `X-Opaque-Id` (CON-42), impor cURL (CON-62), snippet bawaan (CON-63), cluster target per tab (CON-64), serta tab, history, dan salin cURL. Folding JSON di editor (CON-09): penanda ▾/▸ di gutter, hanya visual (teks tidak berubah).
- Discover: data view per pola (DIS-03), `remote:*` (DIS-07), ES|QL (DIS-12), KQL (DIS-13), autocomplete `_terms_enum` (DIS-14), history query (DIS-15), filter sebagai DSL + pin (DIS-18), breakdown histogram (DIS-23), urutan kolom bisa diubah (DIS-31: ◀▶ di header tabel, ↑↓ di sidebar), preview pola `_resolve` (DIS-01), sort `.keyword` otomatis (DIS-32), total akurat (DIS-34), highlight (DIS-35), dokumen sekitar (DIS-38), edit/hapus dokumen dengan `if_seq_no` (DIS-39), penanda konflik tipe (DIS-41), top values akurat dengan statistik numerik min/max/avg/sum/count (DIS-43), tree field (DIS-44), saved search (DIS-50), ekspor CSV (DIS-51), async search di atas 30 hari (DIS-54).
- Dashboard: tabel dengan sort dan ekspor CSV (DSH-07), panel pencarian tersimpan (DSH-10), brush time series dan klik series untuk memfilter (DSH-14), Inspect (DSH-17), impor/ekspor (DSH-18).
- Index: aksi massal (IDX-05), settings dalam tampilan Table/JSON dengan "Tampilkan default" (IDX-11), mappings dalam tampilan Tree/JSON (IDX-13), alias (IDX-14), tab Shards + allocation explain (IDX-15), tab ILM + retry (IDX-16), tab dokumen (IDX-17), force merge (IDX-34), reindex dengan progress + batal (IDX-36), rollover (IDX-37), data stream (IDX-50), index/component template + simulasi (IDX-51, IDX-52).
- Lintas modul: tema terang/gelap (X-03), status bar (X-06), Inspect aksi terakhir (X-02), UI bahasa Indonesia/Inggris dengan pemilih ID | EN di top bar, tersimpan di `settings.json` (X-05).
- Perbaikan yang ditemukan saat pengukuran NFR: pembacaan keychain OS dipindah dari thread UI, karena sebelumnya memblokir startup di macOS.

**Pengukuran NFR (macOS, release; detail di [`docs/plans/nfr-results.md`](plans/nfr-results.md)):**

| Pengukuran | Hasil | Target |
|---|---|---|
| Startup sampai frame pertama | 683 ms | <= 1 dtk |
| RSS saat frame pertama | 88 MB | <= 200 MB |
| Highlight 5.000 baris Console | 0,64 ms | < 50 ms |
| Filter + sort 10.000 baris index | 0,46 ms | < 20 ms |

Startup diukur dengan `ELASTIC_GUI_TIMING=1 target/release/elastic-gui` memakai profile ENV, setelah perbaikan keychain.

**Belum diverifikasi (jangan dianggap selesai):**

- Tampilan GUI belum pernah diperiksa secara visual oleh sesi otomatis; yang dicek hanya kompilasi, test unit, dan bentuk request.
- Aksi tulis (buat/hapus/close index, settings, alias, reindex, by-query, edit/hapus dokumen, static settings, clone/shrink/split, aksi massal, simpan template) hanya diuji lewat kompilasi, test unit, dan bentuk request, karena cluster uji adalah cluster produksi.
- Hanya macOS + ES 9.5 yang pernah dijalankan. Windows, Linux (termasuk fallback keychain), ES 7.17/8.x, dan serverless belum diuji.
- Belum ada installer maupun code signing (PRD Q6 masih terbuka).

**Penyesuaian dari spec awal:**

- Cluster target nyata ES 9.5, jadi spec 9.x ditambahkan (`src/es/spec/es9.json`).
- Layout dashboard memakai lebar 1–3 kolom dan tombol geser (◀ ▶), bukan drag/resize bebas (DSH-02).
- PRF-15 diwujudkan sebagai tab (cluster + modul) yang bisa dipindah cepat dengan state tiap cluster tetap hidup, bukan dua cluster berdampingan di layar yang sama.

---

## 1. Ringkasan

Elastic GUI adalah aplikasi desktop native untuk bekerja dengan satu atau banyak cluster Elasticsearch dari satu jendela. Aplikasi ini menggantikan lima alur kerja Kibana yang paling sering dipakai: **Discover**, **Dev Tools Console**, **Dashboard**, **Index Management**, dan **pengaturan profile cluster**. Aplikasi ringan, tidak butuh server, dan tidak terikat ke versi cluster tertentu.

## 2. Latar belakang dan masalah

1. **Kibana berat.** Kibana butuh proses Node.js dengan RAM 1–2 GB atau lebih, startup lambat, dan harus satu versi persis dengan Elasticsearch.
2. **Kibana hanya untuk satu cluster.** Engineer yang memegang prod, staging, dan dev harus membuka beberapa instance Kibana atau berpindah-pindah URL.
3. **Fitur operasional cluster di Kibana terbatas.** Allocation explain, shard per node, reindex dengan progress, dan sejenisnya harus dikerjakan manual lewat Console.
4. **Tool alternatif** (Elasticvue, Cerebro, elasticsearch-head) kuat di admin cluster, tapi lemah di Discover, autocomplete Console, dan dashboard.

## 3. Tujuan dan non-tujuan

### Tujuan

- G1. Satu aplikasi bisa menyimpan dan berpindah antar banyak cluster tanpa restart.
- G2. Discover cukup cepat dan lengkap untuk investigasi log harian.
- G3. Console dengan autocomplete endpoint, parameter, body, nama index, dan nama field yang setara atau lebih baik dari Kibana.
- G4. Dashboard sederhana berbasis aggregation untuk memantau metrik utama.
- G5. Index management lengkap, dengan guard yang mencegah kesalahan fatal di cluster produksi.

### Non-tujuan (di luar scope v1)

- Alerting/rules (butuh scheduler server), APM, SIEM, ML, Maps, Canvas, Fleet, Reporting PDF.
- Import saved objects atau dashboard Kibana (format NDJSON Kibana).
- Mode multi-user/web server. Aplikasi bersifat personal per mesin.
- OpenSearch (lihat bagian 13, pertanyaan terbuka).

## 4. Persona

| Persona | Kebutuhan utama | Modul |
|---|---|---|
| Backend developer | Cari log saat debugging, coba query | Discover, Console |
| SRE / DevOps | Pantau cluster, tangani index, investigasi insiden lintas cluster | Semua |
| Data engineer | Desain mapping/template, reindex, cek data | Console, Index Management |
| Admin Elasticsearch | Operasi index, template, shard, settings | Index Management, Console |

## 5. Batasan dan prinsip teknis

Bagian ini mengikuti project yang ada.

- **UI 100% Rust dengan egui** (immediate mode). Tidak ada webview.
- **HTTP langsung dari proses native**, jadi tidak ada masalah CORS dan tidak butuh proxy.
- **Jaringan tidak boleh memblokir UI.** Semua request berjalan di runtime async (thread terpisah). Hasil dikirim lewat channel ke state UI, lalu UI memanggil `ctx.request_repaint()`. Frame UI tetap 60 fps walau ada query lambat.
- **Satu klien ES terpusat.** Semua modul memanggil satu lapisan `es::Client` yang mengurus auth, TLS, timeout, cancel, dan normalisasi error. Modul UI tidak boleh membuat request HTTP sendiri.
- **Dependensi minimal.** Tambah crate hanya jika fungsinya tidak masuk akal ditulis sendiri. Kandidat crate ada di bagian 9.
- **Pemeriksaan wajib** tetap seperti di README: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build --release`.

## 6. Arsitektur dan struktur modul

```text
src/
  main.rs              # Entry point dan konfigurasi jendela (sudah ada)
  app.rs               # Shell: top bar (switcher cluster), navigasi modul, routing
  es/
    client.rs          # HTTP ke ES: auth, TLS, timeout, cancel, error
    error.rs           # Model error ES (type, reason, root_cause, caused_by)
    catalog.rs         # Cache index/alias/data stream dan field (_field_caps) per profile
    query.rs           # Builder: query bar + filter + time range -> bool DSL
    spec/              # Spesifikasi API ES yang sudah diringkas, untuk autocomplete
  profile/
    model.rs           # Struktur profile
    store.rs           # Simpan/muat profile dan data per profile (JSON)
    secret.rs          # Akses keychain OS
  ui/
    profiles/          # Modul Config/Profile
    console/           # Modul Dev Tools
    discover/          # Modul Discover
    dashboard/         # Modul Dashboard
    indices/           # Modul Index Management
  widgets/
    code_editor.rs     # Editor teks + highlight + popup autocomplete
    json_view.rs       # Viewer JSON dengan folding
    time_picker.rs     # Pemilih waktu (quick, relatif, absolut, auto-refresh)
    filter_bar.rs      # Query bar + filter pills
    confirm.rs         # Dialog konfirmasi aksi destruktif
    inspector.rs       # Request inspector (request/response mentah)
```

### Penyimpanan lokal

- Lokasi: direktori data aplikasi per OS. Contoh di macOS: `~/Library/Application Support/elastic-gui/`.
- `profiles.json` berisi metadata profile. **Secret tidak pernah disimpan di file ini.**
- `profiles/<profile_id>/` berisi `history.json`, `console_tabs.json`, `saved_searches.json`, `dashboards/*.json`.
- Secret (password, API key, token, private key) disimpan di keychain OS: macOS Keychain, Windows Credential Manager, Linux Secret Service.

### Komponen bersama (dibangun sekali, dipakai banyak modul)

| Komponen | Dipakai oleh |
|---|---|
| `es::Client` | Semua |
| `catalog` (index + field) | Console (autocomplete), Discover, Dashboard, Index Management |
| `query` builder | Discover, Dashboard |
| `time_picker`, `filter_bar` | Discover, Dashboard |
| `code_editor` + autocomplete JSON | Console, panel DSL Dashboard, editor settings/mappings/template |
| `inspector`, `confirm` | Semua |

---

## 7. Kebutuhan fungsional

Prioritas: **P0** = wajib di MVP, **P1** = rilis berikutnya, **P2** = nanti.

### 7.1 Profile / Config (PRF)

| ID | Kebutuhan | Prioritas |
|---|---|---|
| PRF-01 | CRUD profile: nama, warna, tag environment (`prod`/`staging`/`dev`/custom) | P0 |
| PRF-02 | Satu atau lebih URL node (failover berurutan), termasuk path prefix untuk reverse proxy (`https://gw/es/`) | P0 |
| PRF-03 | Auth: None, Basic (user/password), API Key (format `id:key` atau encoded), Bearer token | P0 |
| PRF-04 | Auth PKI (client cert + key), Cloud ID | P1 |
| PRF-05 | TLS: CA cert custom (file PEM), opsi skip verify dengan banner peringatan permanen | P0 |
| PRF-06 | TLS: pin fingerprint SHA-256 (format yang ditampilkan ES 8 saat setup) | P1 |
| PRF-07 | Timeout request, custom headers | P0 |
| PRF-08 | HTTP proxy | P2 |
| PRF-09 | **Read-only mode** per profile: blokir semua request write di semua modul | P0 |
| PRF-10 | Test connection: `GET /` dan `GET /_security/_authenticate`. Tampilkan nama cluster, versi, user, roles. Kalau security nonaktif, panggilan kedua boleh gagal dan tidak dianggap error | P0 |
| PRF-11 | Deteksi capability: versi mayor, `build_flavor` (serverless), ketersediaan ES\|QL (8.11+). Fitur yang tidak didukung disembunyikan | P0 |
| PRF-12 | Deteksi privilege lewat `_security/user/_has_privileges`. Tombol yang tidak diizinkan di-disable dengan tooltip alasan | P1 |
| PRF-13 | Switcher cluster di top bar: daftar profile + dot health (polling `_cluster/health` tiap 30 detik untuk profile aktif). Pindah cluster tanpa restart | P0 |
| PRF-14 | Indikator environment: profile `prod` diberi warna aksen merah di top bar dan border | P0 |
| PRF-15 | Banyak tab modul dengan profile berbeda secara bersamaan (misalnya Discover prod dan Discover staging) | P1 |
| PRF-16 | Export/import profile **tanpa secret**. Opsi menyertakan secret hanya dalam file terenkripsi password | P1 |
| PRF-17 | Duplicate profile | P1 |

**Kriteria penerimaan (P0):**
- Secret tidak pernah muncul di `profiles.json`, log, atau pesan error. Diverifikasi lewat grep file data setelah membuat profile.
- Gagal TLS menampilkan pesan yang bisa ditindaklanjuti, misalnya: "Sertifikat tidak bisa diverifikasi. Tambahkan CA cert atau aktifkan skip verify."
- Error 401 menampilkan aksi "Edit profile".
- Saat profile A down, profile B tetap bisa dipakai dan UI tidak freeze.

### 7.2 Dev Tools — Console (CON)

#### Editor

| ID | Kebutuhan | Prioritas |
|---|---|---|
| CON-01 | Editor multi-request dengan format `METHOD /path` diikuti body JSON | P0 |
| CON-02 | Parser request block: body NDJSON untuk `_bulk`, `_msearch`, `_msearch/template`; komentar `#` dan `//`; string triple-quote `"""..."""` dikonversi menjadi string JSON escaped | P0 |
| CON-03 | Syntax highlighting (method, path, key, string, number, komentar) | P0 |
| CON-04 | Tombol jalankan per block di gutter + highlight block aktif | P0 |
| CON-05 | Format/auto-indent JSON (Ctrl/Cmd+I), auto-close bracket dan quote | P0 |
| CON-06 | Lint: JSON invalid, endpoint tidak dikenal, method tidak cocok dengan endpoint | P1 |
| CON-07 | Variabel `${NAMA}` dari panel variabel | P1 |
| CON-08 | Multi tab console bernama, autosave per profile | P0 |
| CON-09 | Folding JSON di editor | P2 |

#### Autocomplete

Parser menentukan konteks cursor, lalu memilih sumber saran:

| ID | Konteks | Sumber saran | Prioritas |
|---|---|---|---|
| CON-20 | Awal baris (method) | `GET POST PUT DELETE HEAD` | P0 |
| CON-21 | Path endpoint statis | Spesifikasi API ES sesuai versi mayor cluster | P0 |
| CON-22 | Segmen path `{index}` | **Live**: `_cat/indices`, `_alias`, `_data_stream` (cache per profile, TTL 2 menit, plus refresh manual) | P0 |
| CON-23 | Segmen path lain (`{name}` ILM/pipeline/template, `{repository}`, `{node_id}`) | Live dari endpoint list masing-masing | P1 |
| CON-24 | Query parameter setelah `?`/`&` | Nama, tipe, dan deskripsi dari spec. Value untuk boolean dan enum | P0 |
| CON-25 | Key body JSON | Schema body endpoint dari spec, ditelusuri rekursif lewat path cursor (contoh: `query > bool > must[0] > match`) | P0 |
| CON-26 | Snippet saat key diterima (`bool` menjadi `"bool": { "must": [ \| ] }`) | Template per tipe | P1 |
| CON-27 | Nama field (dalam `match`, `term`, `range`, `sort`, `aggs.*.field`, `_source`, `fields`) | `_field_caps` untuk index di path, difilter sesuai konteks (aggregatable untuk agg, numeric/date untuk `range`) | P0 |
| CON-28 | Value enum di body (`order`, `type` mapping, `calendar_interval`) | Spec | P1 |
| CON-29 | Value term untuk field keyword | `_terms_enum` (ES 7.14+) | P2 |
| CON-30 | Link dokumentasi endpoint (field `docUrl` di spec) | Spec | P1 |

Perilaku UI autocomplete:
- Popup muncul otomatis setelah huruf, `/`, `?`, `&`, `"`, dan `:`. Bisa dipicu manual dengan Ctrl+Space.
- Navigasi pakai panah. Enter/Tab menerima saran, Esc menutup popup.
- Fuzzy match. Saran yang sering dipakai naik ke atas.
- Setiap saran menampilkan tipe dan deskripsi singkat.
- Popup muncul dalam ≤ 50 ms untuk sumber statis. Untuk sumber live, data diambil dari cache; kalau cache belum ada, tampilkan loading lalu isi otomatis.

**Sumber spec:** repo `elastic/elasticsearch-specification` (lisensi Apache-2.0). Spec diringkas lewat skrip generator menjadi JSON kecil per versi mayor (7.x, 8.x), di-commit ke `src/es/spec/`, lalu di-embed dengan `include_bytes!`. **Jangan menyalin definisi autocomplete Kibana**, karena lisensinya Elastic License/SSPL/AGPL.

#### Eksekusi dan response

| ID | Kebutuhan | Prioritas |
|---|---|---|
| CON-40 | Jalankan block di cursor (Ctrl/Cmd+Enter). Kalau ada seleksi yang mencakup beberapa block, jalankan berurutan | P0 |
| CON-41 | Cancel request yang sedang berjalan | P0 |
| CON-42 | Cancel di sisi server: kirim `X-Opaque-Id`, lalu `_tasks/_cancel` | P2 |
| CON-43 | Panel response: status code berwarna, waktu client dan `took`, ukuran response | P0 |
| CON-44 | JSON viewer dengan folding dan copy. Response `_cat` ditampilkan sebagai teks monospace | P0 |
| CON-45 | Header `Warning` (deprecation) ditampilkan | P1 |
| CON-46 | Response > 5 MB dipotong dengan tombol "Simpan ke file" | P0 |
| CON-47 | Guard read-only: hanya izinkan method GET/HEAD dan allowlist POST read (`_search`, `_count`, `_msearch`, `_field_caps`, `_query`, `_async_search`, `_mget`, `_validate/query`, `_explain`, `_terms_enum`, `_resolve`, `_analyze`) | P0 |
| CON-48 | Profile prod: konfirmasi untuk `DELETE`, `_delete_by_query`, `_close`. Wildcard pada path delete diberi peringatan ekstra | P0 |

#### Produktivitas

| ID | Kebutuhan | Prioritas |
|---|---|---|
| CON-60 | History per profile (waktu, request, status, durasi). Bisa search, klik untuk insert ke editor | P0 |
| CON-61 | Copy as cURL. Kredensial disamarkan secara default | P0 |
| CON-62 | Import dari cURL | P1 |
| CON-63 | Snippet bawaan: cluster health, top index by size, allocation explain, reindex, hot threads | P1 |
| CON-64 | Pilih profile target per tab console | P1 |

### 7.3 Discover (DIS)

#### Sumber data dan waktu

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DIS-01 | Picker sumber: index, alias, data stream, atau pattern ad-hoc (`logs-*,-logs-debug*`). Preview index yang cocok lewat `_resolve/index` | P0 |
| DIS-02 | Auto-deteksi field waktu (tipe `date`/`date_nanos`, prioritas `@timestamp`). Bisa diganti user. Tanpa field waktu, histogram dan time picker disembunyikan | P0 |
| DIS-03 | Data view ringan tersimpan per profile: pattern, field waktu, kolom default | P1 |
| DIS-04 | Time picker: quick range, relatif (date math ES, misalnya `now-15m`), absolut, recently used | P0 |
| DIS-05 | Auto-refresh (off/10s/30s/1m/5m) | P0 |
| DIS-06 | Timezone: default zona lokal, bisa diganti ke UTC. Dikirim sebagai `time_zone` di `range` dan `date_histogram` | P0 |
| DIS-07 | Cross-cluster search pattern (`remote:index`) | P2 |

#### Query dan filter

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DIS-10 | Mode query **Lucene** (`query_string`) | P0 |
| DIS-11 | Mode query **DSL JSON**, dengan autocomplete dari engine Console | P0 |
| DIS-12 | Mode **ES\|QL** (`POST /_query`, ES 8.11+). Hasil berupa tabel kolom/baris | P1 |
| DIS-13 | Mode **KQL** (parser KQL ke DSL ditulis sendiri) | P1 |
| DIS-14 | Autocomplete query bar: nama field, lalu value setelah `field:` (lewat `_terms_enum`, fallback terms agg) | P1 |
| DIS-15 | History query per profile | P1 |
| DIS-16 | Filter pills dengan operator `is`, `is not`, `is one of`, `exists`, `between` | P0 |
| DIS-17 | Aksi per pill: enable/disable, negate, edit, hapus. Aksi global: hapus semua | P0 |
| DIS-18 | Edit pill sebagai DSL, pin pill (tetap aktif saat ganti sumber) | P2 |
| DIS-19 | Komposisi query: `bool.must` = query bar; `bool.filter` = range waktu + pills include; `bool.must_not` = pills negate | P0 |

#### Histogram

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DIS-20 | `date_histogram` dengan interval otomatis (target 50–100 bucket, dibulatkan ke 1s/5s/30s/1m/5m/30m/1h/3h/12h/1d/1w/1M), plus override manual | P0 |
| DIS-21 | `min_doc_count: 0` + `extended_bounds` supaya bucket kosong tetap tampil | P0 |
| DIS-22 | Drag (brush) di histogram untuk zoom range waktu; klik bar untuk zoom ke bucket | P0 |
| DIS-23 | Breakdown by field (top 5 + other, stacked) | P2 |
| DIS-24 | Hits dan histogram diambil dalam satu request (`size` + `aggs`) | P0 |

#### Tabel dokumen

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DIS-30 | Tabel virtualized. Hanya baris yang terlihat yang dirender | P0 |
| DIS-31 | Kolom default: waktu + ringkasan `_source`. User bisa tambah, hapus, reorder, dan resize kolom | P0 |
| DIS-32 | Sort hanya di field sortable. Field `text` dengan sub-field `.keyword` otomatis memakai `.keyword` | P0 |
| DIS-33 | Paging: `size` 100, "Muat lagi" pakai `search_after` + PIT, dengan tiebreaker `_shard_doc`. **Tidak memakai `from` lebih dari 10.000** | P0 |
| DIS-34 | Total hits: `track_total_hits` 10.000 secara default, ditampilkan "10.000+". Ada opsi hitung akurat | P0 |
| DIS-35 | Highlight term yang cocok | P1 |
| DIS-36 | Expand dokumen: tab Tabel (dengan aksi filter +/−, toggle kolom) dan tab JSON (`_source` + `_index`, `_id`) | P0 |
| DIS-37 | Copy JSON dokumen, salin `_id` | P0 |
| DIS-38 | Surrounding documents: N dokumen sebelum/sesudah berdasarkan waktu | P1 |
| DIS-39 | Edit/hapus dokumen (dengan guard read-only dan konfirmasi) | P2 |

#### Field sidebar

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DIS-40 | Daftar field dari `_field_caps`: ikon tipe, search, grup Selected/Available | P0 |
| DIS-41 | Penanda konflik tipe antar index | P1 |
| DIS-42 | Klik field menampilkan top 5 value dari dokumen yang sudah dimuat, dengan label "berdasarkan N dokumen" | P0 |
| DIS-43 | Mode akurat: terms agg di seluruh range waktu (field aggregatable). Untuk numeric: `stats` | P1 |
| DIS-44 | Field `nested`/`object`/`flattened` ditampilkan sebagai tree | P1 |

#### Lain-lain

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DIS-50 | Saved search per profile: sumber, query, filter, kolom, sort | P1 |
| DIS-51 | Export CSV: kolom terpilih, maksimum 100.000 baris, loop PIT, progress + cancel, streaming ke file | P1 |
| DIS-52 | Query berubah membatalkan request sebelumnya. Input di-debounce | P0 |
| DIS-53 | Banner warning jika `_shards.failed > 0` (hasil parsial) | P0 |
| DIS-54 | `_async_search` untuk range waktu lebar | P2 |

### 7.4 Dashboard (DSH)

| ID | Kebutuhan | Prioritas |
|---|---|---|
| DSH-01 | List dashboard per profile: buat, rename, duplicate, hapus, search | P0 |
| DSH-02 | Layout grid: panel bisa dipindah dan di-resize. Mode edit dan view | P0 |
| DSH-03 | Time picker, query bar, dan filter pills global (komponen yang sama dengan Discover) berlaku untuk semua panel | P0 |
| DSH-04 | Panel **Metric**: count/sum/avg/min/max/cardinality/percentile | P0 |
| DSH-05 | Panel **Time series** (line/area/bar): `date_histogram` + metric, split opsional dengan `terms` | P0 |
| DSH-06 | Panel **Bar kategori**: `terms` top N + "Other" (`sum_other_doc_count`) | P0 |
| DSH-07 | Panel **Data table**: `terms` bertingkat, sort, export CSV | P1 |
| DSH-08 | Panel **Pie/donut**: maksimal 10 slice + Other | P1 |
| DSH-09 | Panel **Markdown** | P1 |
| DSH-10 | Panel **Saved search** (reuse tabel Discover) | P1 |
| DSH-11 | Panel **Raw DSL / ES\|QL**: user menulis query, lalu memetakan path hasil ke sumbu | P2 |
| DSH-12 | Panel **Control** (dropdown value dari `terms`) yang menghasilkan filter global | P2 |
| DSH-13 | Editor panel: sumber, tipe, metric + field (difilter sesuai tipe), bucket, query/filter per panel, format angka/bytes/durasi, judul. Preview langsung | P0 |
| DSH-14 | Klik bar/slice/legend menambah filter global. Brush di time series mengubah range waktu | P1 |
| DSH-15 | Semua panel diambil dengan `_msearch`. Status loading/error/empty per panel. Satu panel gagal tidak menggagalkan dashboard | P0 |
| DSH-16 | Auto-refresh | P0 |
| DSH-17 | Inspect panel: request/response mentah + "Buka di Console" | P1 |
| DSH-18 | Export/import dashboard (format JSON milik aplikasi) | P1 |
| DSH-19 | Panel dari profile berbeda dalam satu dashboard (misalnya membandingkan prod dan staging) | P2 |

### 7.5 Index Management (IDX)

#### Daftar index

| ID | Kebutuhan | Prioritas |
|---|---|---|
| IDX-01 | Sumber: `_cat/indices?format=json&bytes=b&h=...`, digabung dengan `_alias` dan `_data_stream` | P0 |
| IDX-02 | Kolom: nama, health, status, pri/rep, docs, deleted docs, ukuran, alias, data stream, tanggal dibuat | P0 |
| IDX-03 | Search, filter health/status, sort semua kolom, tabel virtualized (lancar untuk 10.000+ index) | P0 |
| IDX-04 | Toggle index hidden/system (diawali `.`), default disembunyikan | P0 |
| IDX-05 | Multi-select + aksi massal | P1 |

#### Detail index

| ID | Tab | Kebutuhan | Prioritas |
|---|---|---|---|
| IDX-10 | Overview | `_stats`: docs, ukuran, indexing/search total, segment | P0 |
| IDX-11 | Settings | Tampilan flat/JSON (`flat_settings`, `include_defaults`). Edit setting dinamis (`number_of_replicas`, `refresh_interval`, `max_result_window`, `blocks.*`, routing allocation) | P0 |
| IDX-12 | Settings | Wizard setting statis: close, update, open, dengan peringatan downtime | P2 |
| IDX-13 | Mappings | Tree view + JSON. Tambah field baru (`PUT _mapping`). Penjelasan bahwa mengubah tipe field butuh reindex | P0 |
| IDX-14 | Aliases | Tambah/hapus atomik (`POST _aliases`), `is_write_index`, filter alias | P1 |
| IDX-15 | Shards | `_cat/shards`: primary/replica, node, state, ukuran. Shard UNASSIGNED punya tombol "Explain" (`_cluster/allocation/explain`) | P1 |
| IDX-16 | ILM | `_ilm/explain`: phase, action, step, error + tombol retry | P1 |
| IDX-17 | Dokumen | Preview 20 dokumen + "Buka di Discover" | P1 |

#### Aksi

| ID | Aksi | API | Guard | Prioritas |
|---|---|---|---|---|
| IDX-30 | Buat index | `PUT /<idx>` | Validasi nama: lowercase; tanpa `\ / * ? " < > \| , #` dan spasi; tidak diawali `- _ +`; ≤ 255 byte | P0 |
| IDX-31 | Hapus index | `DELETE /<idx>` | User wajib mengetik nama index. Tanpa wildcard | P0 |
| IDX-32 | Open/Close | `_open`, `_close` | Peringatan: index yang di-close tidak bisa di-search | P0 |
| IDX-33 | Refresh, Flush, Clear cache | `_refresh`, `_flush`, `_cache/clear` | | P0 |
| IDX-34 | Force merge | `_forcemerge?wait_for_completion=false` | Peringatan beban I/O. Progress lewat `_tasks` | P1 |
| IDX-35 | Block/unblock write | `_block/write`, update setting | | P1 |
| IDX-36 | Reindex | `_reindex?wait_for_completion=false` | Progress (`status.created/total`), cancel, source query opsional | P1 |
| IDX-37 | Rollover | `/<alias\|ds>/_rollover` | Conditions opsional | P1 |
| IDX-38 | Clone/Shrink/Split | `_clone`, `_shrink`, `_split` | Wizard prasyarat | P2 |
| IDX-39 | Delete/Update by query | `_delete_by_query`, `_update_by_query` | Async + konfirmasi + task | P2 |

#### Template dan data stream

| ID | Kebutuhan | Prioritas |
|---|---|---|
| IDX-50 | Data streams: list, backing indices, generation, template, retention. Aksi rollover dan hapus (hapus diperlakukan sebagai destruktif) | P1 |
| IDX-51 | Index templates composable: CRUD `_index_template` dan `_component_template`. Editor pattern, priority, composed_of, settings, mappings, aliases, data_stream | P1 |
| IDX-52 | Preview template lewat `_index_template/_simulate_index/<name>` | P1 |
| IDX-53 | Legacy `_template` (ES 7.x) | P2 |

#### Guard umum

- Index sistem (`.security*`, `.kibana*`, `.tasks`, dan index sistem lain): aksi destruktif dikunci.
- Profile read-only: semua aksi write disembunyikan.
- Profile prod: konfirmasi ekstra untuk semua aksi destruktif.
- Semua aksi punya "Lihat request" sebelum dieksekusi.

### 7.6 Lintas modul (X)

| ID | Kebutuhan | Prioritas |
|---|---|---|
| X-01 | Error ES ditampilkan terstruktur (`type`, `reason`, `root_cause`, `caused_by` bertingkat), bukan dump JSON | P0 |
| X-02 | Request inspector di Discover, Dashboard, dan Index Management + "Buka di Console" | P1 |
| X-03 | Tema gelap (default saat ini) dan terang | P1 |
| X-04 | Shortcut keyboard + cheatsheet (Ctrl/Cmd+K untuk command palette) | P2 |
| X-05 | Bahasa UI: Indonesian (default), English | P2 |
| X-06 | Status bar: profile aktif, health, request yang sedang berjalan | P1 |

---

## 8. Kebutuhan non-fungsional

| Area | Kebutuhan |
|---|---|
| Responsivitas | UI tetap ≥ 60 fps saat request berjalan. Tidak ada I/O di thread UI |
| Startup | Cold start ≤ 1 detik sampai jendela tampil (build release) |
| Memori | ≤ 200 MB pada pemakaian normal (Discover 1.000 baris + 1 dashboard 8 panel) |
| Skala | Daftar 10.000 index dan tabel 10.000 baris tetap scroll lancar (virtualisasi) |
| Console | Editor tetap responsif untuk file 5.000 baris. Layout teks di-cache, tidak dihitung ulang setiap frame |
| Keamanan | Secret hanya di keychain. TLS verify aktif secara default. Isi dokumen dirender sebagai teks, tidak diinterpretasi. Tidak ada telemetri |
| Kompatibilitas ES | 8.x didukung penuh, 7.17 best-effort (lihat bagian 13) |
| Portabilitas | macOS, Windows, Linux (X11/Wayland), sesuai kebutuhan sistem di README |
| Ketahanan | Satu cluster down tidak mempengaruhi profile lain. Retry ke node berikutnya pada error koneksi |
| Kualitas | Lolos `cargo fmt --check` dan `cargo clippy -D warnings`. Logika non-UI (parser console, query builder, auto interval, validasi nama index) punya unit test |

## 9. Kandidat dependensi

Semua kandidat perlu dicek kompatibilitas versinya dengan eframe 0.37 sebelum ditambahkan.

| Kebutuhan | Kandidat | Catatan |
|---|---|---|
| HTTP + TLS | `reqwest` (rustls) | CA custom lewat `add_root_certificate`, client cert lewat `Identity` |
| Runtime async | `tokio` | Berjalan di thread terpisah dari loop egui |
| JSON | `serde`, `serde_json` | `preserve_order` supaya urutan key response tidak berubah |
| Keychain | `keyring` | Fallback Linux tanpa Secret Service: minta secret setiap sesi |
| Direktori data | `directories` | |
| Waktu | `chrono` | Date math tampilan, zona waktu |
| Tabel, date picker | `egui_extras` | `TableBuilder` virtualized, `DatePickerButton` |
| Chart | `egui_plot` | Line, area, bar. Pie digambar sendiri dengan `Painter` |
| Layout dashboard | `egui_tiles` atau grid buatan sendiri | Diputuskan saat spike M3 |
| Base64 | `base64` | API key, Cloud ID |
| Dialog file | `rfd` | Pilih CA cert, simpan CSV/response |

## 10. Rencana rilis

| Milestone | Isi | Kriteria selesai |
|---|---|---|
| **M0 — Fondasi** | Struktur modul (bagian 6), `es::Client`, profile + keychain, switcher cluster, test connection | Bisa membuat 2 profile (basic auth, API key), pindah cluster, dan health tampil |
| **M1 — Console** | CON P0: editor, parser, eksekusi, response, history, autocomplete method/path/param/body/index/field | Bisa menulis `GET logs/_search` dengan query bool dari autocomplete tanpa mengetik nama key manual |
| **M2 — Discover** | DIS P0 | Bisa investigasi log: pilih pattern, range waktu, query Lucene, filter, histogram zoom, expand dokumen, muat lebih dari 10.000 baris |
| **M3 — Index Management** | IDX P0 | List 10.000 index lancar; create/delete/open/close/refresh dengan guard; edit replicas |
| **M4 — Dashboard** | DSH P0 | Dashboard 6 panel (metric, time series, bar) dengan filter global dan auto-refresh |
| **M5 — Penyempurnaan** | Semua P1 | Per item |

Urutan M1 sebelum M2 disengaja: engine autocomplete JSON dan catalog field dari Console dipakai ulang oleh Discover (mode DSL) dan Dashboard.

## 11. Metrik keberhasilan

- Pengguna internal bisa menyelesaikan investigasi log harian tanpa membuka Kibana (target: ≥ 80% sesi).
- Waktu dari buka aplikasi sampai hasil query pertama ≤ 10 detik.
- Nol insiden aksi destruktif tidak sengaja di profile prod.
- Akurasi autocomplete: saran yang benar ada di 5 teratas untuk ≥ 90% kasus uji (suite query contoh).

## 12. Risiko dan mitigasi

| Risiko | Dampak | Mitigasi |
|---|---|---|
| `TextEdit` egui tidak punya autocomplete, dan layout ulang teks besar setiap frame bisa lambat | Console terasa lambat atau kurang | Spike di awal M1: custom layouter dengan cache, popup diposisikan dari galley cursor. Jika tidak memadai, buat widget editor sendiri di atas `Painter` |
| Spec ES besar (puluhan MB) dan bentuknya kompleks (union, generic) | Ukuran binary dan kompleksitas generator | Ringkas saat generate: buang deskripsi panjang dan tipe yang tidak dipakai di body. Target ≤ 2 MB per versi mayor |
| `egui_plot` tidak punya pie dan beberapa tipe chart | Panel dashboard terbatas | Pie digambar sendiri. Tipe chart lain ditunda |
| Perbedaan API antar versi ES (7 vs 8, serverless) | Fitur error di cluster tertentu | Capability flags dari PRF-11, uji integrasi pada beberapa versi (docker) |
| Linux tanpa Secret Service | Secret tidak bisa disimpan | Fallback: minta secret per sesi, dengan peringatan |
| Query berat membebani cluster prod | Gangguan produksi | Default `track_total_hits` terbatas, `timeout` di search, cancel, peringatan untuk range waktu sangat lebar |

## 13. Pertanyaan terbuka

Jawaban default dipakai sampai ada keputusan lain.

| # | Pertanyaan | Default saat ini |
|---|---|---|
| Q1 | Dukung ES 7.x? Sampai versi berapa? | 8.x penuh, 7.17 best-effort |
| Q2 | Dukung OpenSearch? | Tidak di v1 |
| Q3 | KQL wajib di MVP? | Tidak. Lucene + DSL dulu, KQL di P1 |
| Q4 | Edit/hapus dokumen dari Discover? | P2 |
| Q5 | Dashboard multi-cluster? | P2 |
| Q6 | Distribusi: installer/app bundle dan code signing? | Belum dikonfigurasi (sesuai README). Perlu diputuskan sebelum rilis publik |

## Lampiran A — Peta API Elasticsearch

| Modul | API |
|---|---|
| Profile | `GET /`, `GET /_security/_authenticate`, `POST /_security/user/_has_privileges`, `GET /_cluster/health` |
| Console | Semua endpoint. Autocomplete: `_cat/indices`, `_alias`, `_data_stream`, `_field_caps`, `_terms_enum` |
| Discover | `_search` (bool, `date_histogram`, `search_after`, `highlight`), `_pit`, `_field_caps`, `_resolve/index`, `_query` (ES\|QL), `_doc/<id>`, `_async_search` |
| Dashboard | `_msearch`, aggregations (`date_histogram`, `terms`, metric, `percentiles`, `cardinality`) |
| Index Management | `_cat/indices`, `_cat/shards`, `_stats`, `_settings`, `_mapping`, `_aliases`, `_open`/`_close`, `_refresh`, `_flush`, `_cache/clear`, `_forcemerge`, `_block`, `_reindex`, `_rollover`, `_clone`/`_shrink`/`_split`, `_tasks`, `_ilm/explain`, `_ilm/retry`, `_cluster/allocation/explain`, `_data_stream`, `_index_template`, `_component_template`, `_index_template/_simulate_index` |
