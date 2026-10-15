# PRD: Klien SSH/SFTP Ringan untuk macOS (Full Rust)

Oct 15, 2026 · @Tommy Lumos Empire

## 1. Ringkasan

Produk ini adalah aplikasi desktop macOS untuk SSH dan SFTP, ditulis 100% Rust termasuk UI, dengan manajemen bookmark yang bisa di-export dan di-import. Tujuannya menggantikan Electerm bagi pengguna yang terganggu oleh pemakaian memori dan waktu start aplikasi berbasis Electron.

Nama kerja sementara: **peler** (bisa diganti).

**Masalah.** Electerm lengkap fiturnya, tetapi membawa Chromium dan Node.js penuh. Akibatnya aplikasi berat untuk sekadar membuka satu sesi SSH dan mentransfer file.

**Peluang.** Satu binary native yang hanya mengerjakan SSH, SFTP, dan bookmark. Fokus sempit ini membuat aplikasi bisa kecil, cepat, dan hemat baterai di MacBook.

**Posisi produk.** Pengganti ringan untuk pengguna Electerm, bukan klon fitur penuh. RDP, VNC, Telnet, dan Serial sengaja tidak dibuat pada versi awal.

## 2. Tujuan dan target terukur

Target utama: satu sesi SSH aktif memakai kurang dari 100 MB RAM dan aplikasi siap dipakai kurang dari 1 detik setelah dibuka. Angka di bawah adalah target desain awal, bukan hasil pengukuran, dan perlu diverifikasi saat prototipe berjalan.

| Metrik | Target | Catatan |
| --- | --- | --- |
| Memori idle, 1 sesi SSH | < 100 MB | Diukur lewat Activity Monitor (memori fisik) |
| Memori, 5 tab SSH + 1 panel SFTP | < 200 MB | Scrollback default 10.000 baris |
| Waktu start dingin | < 1 detik | Sampai jendela bisa diketik |
| Ukuran binary (.app) | < 30 MB | Universal binary Apple Silicon + Intel jika memungkinkan |
| Latensi input ke layar | < 16 ms | Tidak terasa tertinggal dibanding Terminal.app |
| Beban CPU idle | \~0% | Render hanya saat ada perubahan |

**Non-tujuan (versi 1.0)**

- Tidak mendukung RDP, VNC, Telnet, dan Serial.
- Tidak ada terminal lokal (shell lokal); cukup Terminal.app atau iTerm2.
- Tidak ada plugin atau sistem ekstensi.
- Tidak ada sinkronisasi cloud bawaan; perpindahan data lewat export/import.
- Tidak mendukung Windows dan Linux di versi awal, meski kode dijaga agar mudah di-port.

## 3. Target pengguna

Pengguna utama adalah engineer yang mengelola puluhan server dari MacBook dan menginginkan klien yang cepat, hemat baterai, dan tidak memakan RAM.

| Persona | Kebutuhan utama | Skenario khas |
| --- | --- | --- |
| Software engineer / DevOps | Konek cepat ke banyak server, bookmark terorganisir | Buka 3–5 sesi SSH ke staging dan production sekaligus |
| Pengguna Electerm yang ingin beralih | Memindahkan bookmark lama tanpa input ulang | Import bookmark dari Electerm, lalu pakai sehari-hari |
| Pengguna web hosting / VPS pribadi | Upload dan download file dengan GUI sederhana | Drag file ke panel SFTP untuk deploy situs |

**Skenario utama**

1. Pengguna membuka aplikasi, mencari bookmark lewat kolom pencarian, lalu menekan Enter untuk konek.
2. Pengguna bekerja di terminal SSH, lalu membuka panel SFTP pada sesi yang sama untuk mengunggah file.
3. Pengguna mengekspor semua bookmark ke satu file terenkripsi, lalu mengimpornya di MacBook lain.

## 4. Lingkup fitur per fase

MVP hanya mencakup SSH, bookmark, dan export/import. SFTP masuk di fase kedua agar terminal stabil lebih dulu, karena rendering terminal adalah bagian paling berisiko.

| Fitur | MVP (v0.1) | v1.0 | Setelah v1.0 |
| --- | --- | --- | --- |
| Bookmark + grup (CRUD, pencarian) | Ya | Ya |  |
| Export / import bookmark | Ya | Ya |  |
| Terminal SSH, 1 sesi | Ya | Ya |  |
| Tab banyak sesi | Ya | Ya |  |
| Autentikasi password dan private key | Ya | Ya |  |
| Penyimpanan kredensial di Keychain | Ya | Ya |  |
| Panel SFTP (list, upload, download, hapus, rename) |  | Ya |  |
| Drag-and-drop file |  | Ya |  |
| Split pane terminal |  | Ya |  |
| Tema warna dan pilihan font |  | Ya |  |
| Quick commands |  | Ya |  |
| Port forwarding (local/remote) |  |  | Ya |
| Jump host / ProxyJump |  |  | Ya |
| Import dari `~/.ssh/config` |  | Ya |  |
| ssh-agent dan hardware key |  |  | Ya |
| Port ke Linux dan Windows |  |  | Ya |

## 5. Persyaratan fungsional

### 5.1 Bookmark

- **FR-B1** Pengguna bisa membuat, mengubah, menduplikasi, dan menghapus bookmark.
- **FR-B2** Field bookmark: nama, host, port (default 22), username, metode autentikasi (password atau private key + passphrase), grup, tag, catatan, direktori awal, dan perintah yang dijalankan saat konek.
- **FR-B3** Bookmark bisa dikelompokkan dalam grup bertingkat dan diurutkan dengan drag-and-drop.
- **FR-B4** Pencarian cepat (fuzzy) atas nama, host, dan tag; Enter membuka koneksi.
- **FR-B5** Password dan passphrase disimpan di macOS Keychain, bukan di file bookmark.

### 5.2 Terminal SSH

- **FR-T1** Koneksi SSH dengan password dan private key (RSA, ECDSA, Ed25519).
- **FR-T2** Emulasi terminal xterm-256color dengan warna 24-bit, scrollback, seleksi teks, copy/paste, dan resize yang benar.
- **FR-T3** Verifikasi host key dengan file known\_hosts sendiri; pengguna diminta konfirmasi saat host baru atau kunci berubah.
- **FR-T4** Reconnect manual dan keepalive agar sesi tidak terputus diam-diam.
- **FR-T5** Banyak tab, dengan shortcut `⌘T`, `⌘W`, `⌘1–9`.

### 5.3 SFTP (v1.0)

- **FR-S1** Panel dua sisi: lokal dan remote, memakai koneksi dari sesi yang sama.
- **FR-S2** Operasi: list, upload, download, hapus, rename, buat folder, ubah permission.
- **FR-S3** Transfer berjalan di latar belakang dengan indikator progres, bisa dibatalkan, dan folder bisa ditransfer rekursif.
- **FR-S4** Drag-and-drop dari Finder ke panel remote.

### 5.4 Export dan import

Dirinci di bagian 6.

## 6. Export dan import bookmark

Export menghasilkan satu file JSON berversi; kredensial hanya ikut jika pengguna memilihnya, dan dalam keadaan terenkripsi dengan passphrase. Import harus aman dijalankan berulang kali tanpa menimpa data secara diam-diam.

| Aspek | Keputusan |
| --- | --- |
| Format | JSON dengan field `schema_version`, `exported_at`, `groups[]`, `bookmarks[]` |
| Cakupan export | Semua bookmark, satu grup, atau bookmark terpilih |
| Kredensial | Default **tidak** diekspor. Opsional: diekspor terenkripsi (Argon2id untuk turunan kunci + AES-256-GCM atau ChaCha20-Poly1305) dengan passphrase dari pengguna |
| Private key | Hanya path file yang diekspor, bukan isi key |
| Konflik saat import | Dialog per bookmark: lewati, timpa, atau simpan keduanya. Pencocokan lewat ID unik, lalu kombinasi host+port+user |
| Pratinjau | Import menampilkan daftar bookmark yang akan ditambah, diubah, dan dilewati sebelum disimpan |
| Validasi | File dengan versi skema lebih baru atau struktur rusak ditolak dengan pesan jelas |
| Import dari Electerm | Pembaca untuk file export JSON Electerm, memetakan field host, port, username, grup, dan catatan |
| Import dari `~/.ssh/config` | Membaca blok `Host` dengan `HostName`, `Port`, `User`, `IdentityFile` |

**Catatan kompatibilitas Electerm.** Skema export Electerm belum diverifikasi terhadap kode sumbernya. Sebelum membuat pembaca importnya, ambil contoh file export dari versi Electerm terbaru dan petakan field-nya satu per satu.

## 7. Persyaratan non-fungsional

| Kategori | Persyaratan |
| --- | --- |
| Platform | macOS 12 ke atas, Apple Silicon dan Intel (universal binary jika memungkinkan) |
| Performa | Render berbasis perubahan (tanpa redraw terus-menerus); output deras seperti `cat` file besar tidak membuat UI membeku |
| Memori | Scrollback dibatasi dan bisa diatur; buffer tab yang tidak aktif tidak digambar |
| Keamanan | Kredensial di Keychain; tidak ada log password; host key diverifikasi; file bookmark dengan permission 0600 |
| Privasi | Tanpa telemetri dan tanpa koneksi keluar selain ke server yang dituju pengguna |
| Keandalan | Crash di satu tab tidak mematikan tab lain; koneksi putus ditampilkan jelas |
| Distribusi | `.app` dengan code signing dan notarisasi Apple; instalasi lewat DMG dan Homebrew cask |
| Aksesibilitas | Dukungan VoiceOver terbatas pada versi awal, dicatat sebagai keterbatasan yang diketahui |
| Lokalisasi | Antarmuka Inggris dan Indonesia |
| Input | Dukungan IME (misalnya aksara Mandarin/Jepang) masuk daftar pengujian sebelum v1.0 |

## 8. Arsitektur dan tech stack

Satu proses Rust: thread UI hanya menggambar state dan mengirim perintah, sedangkan koneksi SSH, SFTP, dan akses disk berjalan di runtime async terpisah. Pemisahan ini menjaga UI tetap responsif saat output deras atau transfer besar.

&#91;embedded content: arsitektur modul · 3 lapisan, 9 komponen\]

Kredensial dibaca Session manager dari Keychain saat konek; Bookmark store hanya menyimpan referensi, bukan password.

| Fungsi | Crate | Catatan |
| --- | --- | --- |
| UI | `eframe` / `egui` | Pilihan awal; `iced` sebagai pembanding |
| Emulasi terminal | `alacritty_terminal` | Parser, grid, dan scrollback |
| SSH | `russh` | Murni Rust, berbasis async |
| SFTP | `russh-sftp` | Memakai koneksi SSH yang sama |
| Runtime async | `tokio` | Pemisahan I/O dari thread UI |
| Kredensial | `keyring` | Backend macOS Keychain |
| Serialisasi | `serde`, `serde_json` | Bookmark dan format export |
| Kriptografi export | `argon2`, `chacha20poly1305` | Turunan kunci dan enkripsi dengan passphrase |
| Font dan teks | `cosmic-text` (opsional) | Jika font bawaan egui kurang |
| Paket aplikasi | `cargo-bundle` atau `cargo-packager` | Membuat `.app` dan DMG |

Versi dan status pemeliharaan tiap crate perlu dicek saat proyek dimulai.

## 9. Desain UX

Antarmuka mengikuti pola yang sudah dikenal pengguna Electerm: sidebar bookmark di kiri, tab di atas, dan area terminal di tengah. SFTP muncul sebagai panel yang bisa dibuka dari tab SSH aktif.

**Layout**

- Sidebar kiri: pohon grup dan bookmark, dengan kolom pencarian di atas. Bisa disembunyikan dengan `⌘B`.
- Bar tab di atas: satu tab per sesi, dengan indikator status koneksi (terhubung, terputus, mencoba).
- Area utama: terminal; panel SFTP dua sisi terbuka di bawah atau sebagai tab terpisah (`⌘⇧F`).
- Mengikuti mode terang dan gelap macOS.

**Shortcut utama**

| Aksi | Shortcut |
| --- | --- |
| Cari bookmark dan konek | `⌘K` |
| Tab baru | `⌘T` |
| Tutup tab | `⌘W` |
| Pindah tab | `⌘1`–`⌘9` |
| Tampilkan/sembunyikan sidebar | `⌘B` |
| Buka panel SFTP | `⌘⇧F` |
| Export / Import bookmark | Menu File |

**Prinsip**

- Dari buka aplikasi sampai terhubung maksimal tiga langkah (buka, cari, Enter).
- Setiap error koneksi menyebut penyebabnya: host tidak ditemukan, autentikasi gagal, atau host key berubah.
- Tidak ada dialog pengaturan yang wajib diisi sebelum bisa dipakai.

## 10. Tahapan pengerjaan

Pengerjaan dibagi menjadi empat tahap berurutan. Tiap tahap selesai hanya jika gerbang kualitasnya lolos, sehingga hasilnya bisa diverifikasi sebelum tahap berikutnya dimulai.

&#91;embedded content: roadmap · 4 tahap, 3 gerbang\]

Setiap tahap baru dimulai hanya setelah gerbang sebelumnya lolos. Gate 1 sengaja paling awal: jika terminal di egui tidak memadai, keputusan mengganti framework diambil sebelum fitur lain dibangun.

## 11. Risiko dan mitigasi

Risiko terbesar ada di rendering terminal dan input teks, bukan di protokol SSH. Karena itu terminal dibuktikan lebih dulu sebelum fitur lain dibangun.

| Risiko | Dampak | Mitigasi |
| --- | --- | --- |
| Widget terminal di egui/iced belum matang | Rendering salah, seleksi teks kikuk | Spike terminal di tahap awal; cadangan: render grid `alacritty_terminal` sendiri |
| IME dan layout keyboard non-Latin kurang baik | Pengguna CJK tidak bisa mengetik | Uji IME sebelum v1.0; jika gagal di egui, pertimbangkan `iced` atau `slint` |
| Font monospace, emoji, dan karakter lebar ganda | Tampilan berantakan | Pakai `cosmic-text` atau font stack terpilih; sertakan test grid unicode |
| Output sangat deras membuat UI lambat | Terasa patah-patah | Batasi frame rate, gabungkan update, pisahkan thread I/O dari UI |
| Perbedaan perilaku `russh` dengan OpenSSH di server lama | Gagal konek ke server tertentu | Uji ke beberapa versi OpenSSH dan algoritma lama; sediakan opsi algoritma |
| Keamanan penyimpanan kredensial | Kebocoran password | Hanya Keychain; export kredensial wajib dienkripsi; audit dependensi dengan `cargo audit` |
| Skema Electerm berubah | Import gagal | Versi pembaca import; pesan error jelas; fallback ke import manual JSON |
| Lingkup melebar menyaingi Electerm | Proyek tak selesai | Pertahankan non-tujuan; fitur baru lewat PRD terpisah |

## 12. Metrik keberhasilan dan kriteria rilis

Versi 1.0 dianggap siap rilis jika pengguna bisa memindahkan seluruh alur kerja SSH/SFTP hariannya dari Electerm tanpa kehilangan bookmark.

**Kriteria rilis v1.0**

- [ ] Target memori dan waktu start di bagian 2 tercapai pada MacBook Apple Silicon dan Intel
- [ ] Terminal lolos uji aplikasi umum: `vim`, `tmux`, `htop`, `less`, dan `ssh` bersarang
- [ ] Export lalu import menghasilkan bookmark identik (uji round-trip otomatis)
- [ ] Import dari file export Electerm nyata berhasil tanpa kehilangan field utama
- [ ] Transfer SFTP file 1 GB selesai tanpa korupsi (verifikasi checksum)
- [ ] Tidak ada password atau passphrase tersimpan dalam teks biasa di disk
- [ ] Aplikasi lolos notarisasi Apple dan terpasang lewat DMG
- [ ] Uji IME dasar berhasil

**Metrik setelah rilis**

- Jumlah pengguna aktif mingguan (hanya jika ada opsi pelaporan yang diaktifkan sukarela).
- Jumlah isu terbuka berlabel `crash` dan `data-loss`; targetnya nol sebelum v1.0.
- Umpan balik kualitatif dari pengguna yang beralih dari Electerm.

## 13. Pertanyaan terbuka

Lima keputusan di bawah menentukan arah teknis dan perlu dijawab sebelum minggu pertama pengembangan.

- [ ] **Framework UI**: `egui` (cepat dimulai) atau `iced` (struktur lebih rapi untuk aplikasi besar)? Rekomendasi awal: `egui`, dengan spike untuk membandingkan.
- [ ] **Target chip**: hanya Apple Silicon, atau juga Intel? Universal binary menambah ukuran dan waktu build.
- [ ] **Lisensi proyek**: MIT, Apache-2.0, atau tertutup? Jika memakai kode Electerm, pastikan atribusi sesuai lisensinya.
- [ ] **Skema Electerm**: file export Electerm versi terbaru perlu diperiksa untuk memastikan peta field import.
- [ ] **Distribusi**: hanya DMG dan Homebrew, atau juga Mac App Store? App Store menambah batasan sandbox untuk akses file dan jaringan.
