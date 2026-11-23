# Hasil pengukuran NFR

Tanggal: 2026-11-23. Mesin: macOS, build release.

## Test performa (tanpa cluster)

Perintah: `cargo test --release -- --ignored perf --nocapture`

| Pengukuran | Hasil | Batas | Status |
|---|---|---|---|
| `widgets::highlight` Console 5.000 baris | 0,64 ms | < 50 ms | lulus |
| Filter + sort 10.000 `Row` (`filter_sort_rows`) | 0,46 ms | < 20 ms | lulus |

## Startup (target <= 1 dtk)

Perintah: `ELASTIC_GUI_TIMING=1 target/release/elastic-gui`; stderr mencetak waktu dari `main()` sampai frame pertama dan RSS.

Belum terukur: pada sesi otomatis ini proses tertahan di `SecKeychainFindGenericPassword` (menunggu dialog izin Keychain macOS) sebelum frame pertama, sehingga angka tidak tercetak. Diukur lewat perintah di README pada sesi interaktif.
