# Rencana: Penutupan Gap PRD

**Spec:** `docs/PRD.md` (otoritas). Item yang ditutup: DIS-31, IDX-11, IDX-13 (P0); DIS-43, DSH-14 (P1); CON-09, X-05 (P2); PRF-02 (round-robin); NFR yang bisa diukur otomatis.

## Global Constraints

- Rust 2024 + egui/eframe 0.37, egui_extras 0.37, egui_plot 0.38. **Tidak menambah dependensi baru.**
- Semua I/O jaringan di thread latar lewat `crate::task::Pending`; thread UI tidak boleh blok.
- Semua request ES lewat `crate::es::Conn` (guard read-only ada di sana). Jangan membuat HTTP client sendiri.
- Komentar dan teks UI dalam Bahasa Indonesia (kecuali tabel terjemahan English di X-05). Komentar hanya bila menjelaskan *kenapa*; tandai ID PRD (mis. `// DIS-31: ...`) di tempat fitur diimplementasikan.
- Gerbang wajib setiap task: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` hijau.
- Logika non-UI baru wajib punya unit test (`#[cfg(test)] mod tests` di file yang sama). Test yang butuh cluster diberi `#[ignore = "butuh cluster"]` dan memakai `crate::es::client::live_conn()` (selalu read-only).
- Satu commit per task (boleh lebih), pesan conventional commit berbahasa Indonesia, diakhiri baris:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
- Jangan pernah commit `.example`, `.env`, atau secret apa pun.

---

## Task 1: PRF-02 — Round-robin node

**File:** `src/es/client.rs`

PRD PRF-02: "Satu atau lebih URL node (failover/round-robin)". Saat ini `Conn::send_tagged` selalu mulai dari `urls[0]` dan hanya pindah node saat error koneksi/timeout.

Kebutuhan:
- Tambah penghitung `next: AtomicUsize` di `Conn`. Setiap panggilan `send_tagged` memulai dari node `next.fetch_add(1) % urls.len()`, lalu mencoba node berikutnya secara melingkar (setiap node paling banyak sekali) bila error koneksi/timeout — perilaku failover yang ada dipertahankan.
- `base_url()` tetap mengembalikan `urls[0]` (dipakai untuk menyalin cURL).
- Ekstrak urutan percobaan menjadi fungsi murni `fn node_order(start: usize, len: usize) -> Vec<usize>` dan unit test: `node_order(0,3) == [0,1,2]`, `node_order(4,3) == [1,2,0]`, `node_order(7,1) == [0]`.

## Task 2: DIS-31 — Urutkan ulang kolom Discover

**File:** `src/ui/discover.rs`

PRD DIS-31 (P0): user bisa tambah, hapus, **reorder**, dan resize kolom. Tambah/hapus/resize sudah ada; reorder belum.

Kebutuhan:
- Di header tabel Discover, setiap kolom terpilih (`self.columns`) punya tombol kecil `◀` dan `▶` (hover: "Geser kolom ke kiri/kanan") untuk menukar posisi dengan kolom tetangga. Tombol dinonaktifkan di ujung.
- Di sidebar bagian "Kolom terpilih", tiap baris juga punya `↑`/`↓` yang melakukan hal yang sama.
- Ekstrak logika menjadi fungsi murni `fn move_column(columns: &mut Vec<String>, index: usize, delta: isize) -> bool` (mengembalikan `false` bila di luar batas, tanpa mengubah vektor). Unit test: geser kiri/kanan, ujung kiri, ujung kanan, indeks invalid.
- Perubahan urutan tersimpan otomatis lewat mekanisme data view yang sudah ada (`remember_view`), tanpa kode tambahan.

## Task 3: DIS-43 — Statistik numerik di top values

**File:** `src/ui/discover.rs`

PRD DIS-43: "Mode akurat: terms agg di seluruh range waktu (field aggregatable). Untuk numeric: `stats`." Bagian terms sudah ada (`load_top`, tombol "Hitung akurat (seluruh rentang)"); bagian numeric belum.

Kebutuhan:
- Bila field numeric (`Field::is_numeric()`), permintaan "Hitung akurat" mengirim aggregation tambahan `{"st": {"stats": {"field": <field>}}}` dalam request yang sama, dengan query dari `self.last_body["query"]` seperti `load_top`.
- Tampilkan di popup top values: `min`, `max`, `avg` (2 desimal), `sum`, `count` memakai `widgets::format_count` untuk bilangan bulat.
- Ubah tipe hasil `TopValues` agar membawa statistik opsional; pertahankan perilaku terms untuk field non-numeric.
- Ekstrak parsing ke fungsi murni `fn parse_stats(v: &Value) -> Option<Stats>` dengan unit test (response lengkap, response tanpa aggregation `st`, field tanpa dokumen → `count == 0` dan min/max `null`).
- Tambah test live `#[ignore]` yang menjalankan stats pada field numeric pertama dari `LIVE_PATTERN` (lewati dengan `println!` bila tidak ada field numeric).

## Task 4: IDX-11 + IDX-13 — Tampilan JSON settings & mappings

**File:** `src/ui/indices.rs`

PRD IDX-11 (P0): Settings tampil "flat/JSON (`flat_settings`, `include_defaults`)". PRD IDX-13 (P0): Mappings "Tree view + JSON". Saat ini settings hanya tabel flat tanpa default, mappings hanya tree.

Kebutuhan:
- Tab Settings: toggle tampilan `Tabel` / `JSON`, dan checkbox `Tampilkan default`. Saat checkbox aktif, muat ulang settings dengan `include_defaults=true`; nilai default ditandai (mis. teks lemah atau kolom "sumber" = `default`). Tampilan JSON memakai `widgets::code_view` dengan `Lang::Json` dari objek settings bersarang (tanpa `flat_settings`), plus tombol "Salin".
- Tab Mappings: toggle `Tree` / `JSON`; JSON = `serde_json::to_string_pretty(&detail.mappings)` lewat `widgets::code_view`, plus tombol "Salin".
- `Detail` menyimpan settings mentah (Value) di samping daftar flat yang sudah ada; `load_detail` menerima flag `include_defaults`.
- Unit test untuk fungsi murni yang memisahkan settings eksplisit vs default dari response `include_defaults` (`settings` vs `defaults` di response ES): `fn flatten_settings(v: &Value, defaults: bool) -> Vec<(String, String, bool)>` (bool = default).
- Perluas test live `live_list_and_detail` agar memuat detail dengan `include_defaults=true` dan memastikan ada minimal satu entri default.

## Task 5: DSH-14 — Klik seri time series menjadi filter

**File:** `src/ui/dashboard.rs`

PRD DSH-14: "Klik bar/slice/legend menambah filter global. Brush di time series mengubah range waktu." Brush sudah ada; klik seri time series (yang punya split `terms`) belum menghasilkan filter.

Kebutuhan:
- Pada panel Time series dengan `panel.split` tidak kosong, klik (tanpa drag) pada area plot ketika sebuah seri sedang di-hover menambahkan `Filter::is(panel.split, <nama seri>, false)` lewat `Event::Filter`. Gunakan `PlotResponse::hovered_plot_item` dan beri setiap `Line`/`BarChart` `.id(...)` yang dapat dipetakan kembali ke nama seri.
- Drag (brush) tetap mengubah rentang waktu; klik tanpa seri ter-hover tidak melakukan apa pun.
- Hover hint plot: "Drag untuk memilih rentang waktu · klik seri untuk memfilter".
- Ekstrak pemetaan id → nama seri ke fungsi murni yang bisa diuji (mis. membangun `Vec<(egui::Id, String)>` dari daftar seri) dengan unit test.

## Task 6: CON-09 — Folding JSON di editor Console

**File:** `src/ui/console/mod.rs` (+ helper murni di file baru `src/ui/console/fold.rs`)

PRD CON-09 (P2): folding JSON di editor.

Kebutuhan:
- Gutter editor (di samping tombol ▶ yang ada) menampilkan penanda `▾` di baris yang membuka objek/array JSON multi-baris (`{` atau `[` sebagai karakter non-spasi terakhir baris, di luar string/komentar) dan penanda `▸` bila region terlipat.
- Klik penanda melipat/membuka region dari baris pembuka sampai baris penutup pasangannya. Region terlipat dirender di layouter dengan font sangat kecil dan warna transparan untuk baris dalam region, plus placeholder `… }` / `… ]` setelah pembuka (placeholder hanya visual — **teks buffer tidak diubah**).
- Status lipatan disimpan per tab sebagai himpunan offset byte baris pembuka; disesuaikan bila teks berubah (cukup: buang lipatan yang pembukanya tidak lagi valid).
- Jalankan request, format, autocomplete, dan Copy cURL tetap bekerja pada teks penuh.
- Fungsi murni di `fold.rs`: `fn fold_ranges(text: &str) -> Vec<(usize, usize)>` (offset byte awal baris pembuka, offset byte akhir baris penutup) yang mengabaikan kurung di dalam string JSON, string triple-quote `"""`, dan baris komentar `#`/`//`. Unit test mencakup objek bersarang, array, kurung di dalam string, triple-quote, dan kurung tak berpasangan.

## Task 7: NFR — Pengukuran otomatis

**File:** `src/main.rs`, `src/app.rs`, `src/widgets/mod.rs`, `src/ui/indices.rs`, `README.md`

PRD bagian 8: startup ≤ 1 dtk sampai jendela tampil (release), daftar 10.000 index lancar, editor 5.000 baris responsif.

Kebutuhan:
- Bila env `ELASTIC_GUI_TIMING=1`, aplikasi mencetak ke stderr waktu dari `main()` sampai frame pertama selesai digambar, dan RSS proses saat itu bila tersedia murah (macOS/Linux via `ps -o rss= -p <pid>`; boleh dilewati di Windows). Tanpa env, tidak ada output.
- Test performa `#[ignore = "perf"]`:
  - `widgets::highlight` pada teks Console sintetis 5.000 baris (campuran request + JSON) selesai < 50 ms di build release (`cargo test --release -- --ignored perf`).
  - Filter + sort `Indices::visible_rows` (atau fungsi murni yang diekstrak darinya) untuk 10.000 `Row` sintetis selesai < 20 ms di release.
- README: bagian "Pengukuran performa" berisi perintah di atas dan cara membaca output timing.

## Task 8: X-05a — Infrastruktur i18n + shell

**File:** `src/i18n.rs` (baru), `src/main.rs`, `src/app.rs`, `src/profile.rs` (Store), `src/widgets/*.rs`, `src/ui/profiles.rs`

PRD X-05 (P2): bahasa UI Indonesia (default) dan English.

Desain (wajib diikuti task 9–12):
- `src/i18n.rs`:
  - `enum Lang { Id, En }` (`Id` default), disimpan global (mis. `AtomicU8`); `pub fn set_lang(Lang)`, `pub fn lang() -> Lang`.
  - `pub fn t(src: &'static str) -> &'static str`: teks sumber Bahasa Indonesia; bila `Lang::En` dan ada di tabel → terjemahan, selain itu teks sumber.
  - `pub fn tf(template: &'static str, args: &[&dyn std::fmt::Display]) -> String`: terjemahkan template lalu ganti `{0}`, `{1}`, … dengan argumen.
  - Tabel = gabungan konstanta per modul `pub const EN: &[(&str, &str)]` (didaftarkan di `i18n.rs`; tiap task menambah konstanta modulnya sendiri). Lookup lewat `HashMap` dalam `OnceLock`.
  - Unit test: tidak ada kunci ganda antar tabel; `tf` mengganti placeholder berulang dan argumen hilang dibiarkan apa adanya.
  - Helper test `pub fn assert_translated(source: &str, table: &[(&str, &str)])` yang memindai literal `t("…")` dan `tf("…"` di teks sumber dan gagal bila ada yang tidak ada di tabel. Tiap modul memanggilnya dengan `include_str!` file sendiri.
- Preferensi bahasa disimpan di `settings.json` direktori data aplikasi (tambah `Store::load_settings/save_settings` dengan struct `AppSettings { lang }`), dimuat saat start.
- Pemilih bahasa `ID | EN` di top bar.
- Terjemahkan semua teks UI yang terlihat di `app.rs`, `ui/profiles.rs`, `widgets/mod.rs`, `widgets/time.rs`, `widgets/filters.rs`, label `Module`, `Env`, `AuthKind`, `TargetKind`, `FilterOp` (label "is"/"exists" boleh tetap). Pesan error ES dari server tidak diterjemahkan.
- Setiap file yang disentuh mendapat test `assert_translated(include_str!("<file>"), EN)`.

## Task 9: X-05b — Terjemahan Dev Tools

**File:** `src/ui/console/mod.rs`, `src/ui/console/parse.rs`, `src/ui/console/complete.rs`, `src/i18n.rs` (registrasi tabel)

Terjemahkan semua teks UI dan pesan yang tampil ke user di modul Console (termasuk teks sambutan tab, snippet nama, notice, lint, error parser yang tampil di UI) memakai `t`/`tf` sesuai desain Task 8. Konstanta `EN` per file atau satu untuk modul; test `assert_translated` untuk setiap file yang disentuh.

## Task 10: X-05c — Terjemahan Discover

**File:** `src/ui/discover.rs`, `src/es/kql.rs` (pesan error), `src/es/query.rs` (pesan error/label), `src/i18n.rs`

Sama seperti Task 9 untuk modul Discover dan pesan error KQL/query builder.

## Task 11: X-05d — Terjemahan Dashboard

**File:** `src/ui/dashboard.rs`, `src/i18n.rs`

Sama seperti Task 9 untuk modul Dashboard (termasuk label `Kind`, `Agg`, gaya, format).

## Task 12: X-05e — Terjemahan Index, Data stream, Template

**File:** `src/ui/indices.rs`, `src/ui/streams.rs`, `src/es/catalog.rs`, `src/es/client.rs` (pesan yang tampil di UI, mis. guard read-only dan petunjuk TLS), `src/i18n.rs`

Sama seperti Task 9 untuk modul sisanya. Setelah task ini, `grep` literal string Bahasa Indonesia yang ditampilkan langsung tanpa `t`/`tf` di `src/ui`, `src/widgets`, dan `src/app.rs` tidak boleh menemukan teks UI yang tersisa (komentar dan pesan log boleh).

## Task 13: Dokumentasi status

**File:** `docs/PRD.md`, `README.md`

- Perbarui bagian "Status implementasi" PRD: DIS-31, IDX-11, IDX-13, DIS-43, DSH-14, CON-09, X-05, PRF-02 selesai; hapus klaim yang tidak akurat; cantumkan hasil pengukuran NFR bila tersedia di laporan Task 7 (berikan angka dari `docs/plans/nfr-results.md` bila ada, jika tidak tulis "diukur lewat perintah di README").
- README: sebut pemilih bahasa ID/EN dan folding Console.
