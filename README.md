# Peler — Klien SSH/SFTP ringan untuk macOS

Aplikasi desktop SSH dan SFTP yang ditulis 100% dalam Rust (termasuk UI), sebagai pengganti
Electerm yang jauh lebih ringan. Satu binary native, tanpa Chromium dan Node.js.

| Target PRD | Hasil terukur | Status |
| --- | --- | --- |
| Memori, 1 sesi SSH | **63 MB** (phys_footprint, seperti Activity Monitor) | ✅ < 100 MB |
| Memori, 5 tab SSH + panel SFTP | **83 MB** | ✅ < 200 MB |
| Waktu start dingin | **~0,1 detik** sampai frame pertama (0,44 detik pada start pertama setelah instal) | ✅ < 1 detik |
| Ukuran `.app` universal (Apple Silicon + Intel) | **29 MB** (DMG 15 MB) | ✅ < 30 MB |
| CPU saat diam | **0,0%** (hanya menggambar saat ada perubahan) | ✅ |
| Transfer SFTP 1 GB tanpa korupsi | sha256 identik (upload ~214 MB/s, download ~289 MB/s, lokal) | ✅ |

Pengukuran dilakukan di MacBook Apple Silicon, macOS 14.6.

## Fitur

**Bookmark**
- Buat, edit, duplikat, hapus; grup bertingkat; urutkan dengan drag-and-drop di sidebar.
- Field: nama, host, port, username, autentikasi (password atau private key + passphrase), grup,
  tag, catatan, folder awal, perintah yang dijalankan saat konek, opsi algoritma lama.
- Pencarian fuzzy (nama, host, user, tag, grup); Enter langsung konek. `⌘K` membuka palet pencarian.
- Password dan passphrase disimpan di **macOS Keychain**, tidak pernah di file bookmark.

**Terminal SSH**
- Emulasi xterm-256color (alacritty_terminal): warna 24-bit, scrollback (default 10.000 baris),
  seleksi (klik ganda = kata, klik tiga kali = baris, Option+drag = blok), copy/paste,
  bracketed paste, resize yang benar, mouse reporting (vim, htop, tmux), layar alternatif.
- Karakter lebar (CJK) dan garis kotak/blok digambar presisi; font cadangan CJK di-mmap
  sehingga hampir tidak memakan RAM.
- IME (input Mandarin/Jepang) dengan jendela kandidat di posisi kursor.
- Autentikasi password, keyboard-interactive (termasuk kode OTP), dan private key
  RSA/ECDSA/Ed25519 (terenkripsi maupun tidak).
- Verifikasi host key dengan file `known_hosts` milik aplikasi (juga membaca `~/.ssh/known_hosts`);
  konfirmasi untuk host baru, peringatan keras jika kunci berubah.
- Keepalive (default 30 detik) dan reconnect manual (`⌘R`, atau Enter di sesi yang sudah putus)
  tanpa kehilangan scrollback.
- Banyak tab dan split pane (`⌘D` kanan, `⇧⌘D` bawah) — semua pane memakai satu koneksi SSH.
- Quick commands (tombol ⚡ di toolbar).
- Tema warna terminal (Peler Dark/Light, Solarized, Dracula, Nord, Gruvbox, Tomorrow Night,
  GitHub Light) dan pilihan font (Menlo, SF Mono, Monaco, Courier New, Andale Mono, Hack,
  atau file font sendiri).

**SFTP** (`⇧⌘F`)
- Panel dua sisi lokal/remote memakai koneksi SSH yang sama.
- List, unggah, unduh, hapus (rekursif), ganti nama, buat folder, ubah permission (chmod).
- Transfer di latar belakang dengan progres, kecepatan, bisa dibatalkan (file setengah jadi dihapus),
  folder ditransfer rekursif, permission dan bit executable dipertahankan, ukuran diverifikasi.
- Drag-and-drop antar panel, dan drag file dari Finder ke jendela untuk mengunggah ke folder
  remote yang sedang dibuka. Konfirmasi sebelum menimpa file yang sudah ada.

**Export / import**
- Export ke satu file JSON berversi (`schema_version`, `exported_at`, `groups[]`, `bookmarks[]`):
  semua bookmark, satu grup, atau bookmark terpilih.
- Kredensial default **tidak** diekspor. Opsional: diekspor terenkripsi dengan passphrase
  (Argon2id 64 MiB + XChaCha20-Poly1305). Private key file hanya diekspor path-nya.
- Import menampilkan pratinjau (ditambah/diperbarui/dilewati) dengan pilihan per bookmark untuk
  konflik: lewati, timpa, atau simpan keduanya. Pencocokan lewat ID, lalu host+port+user.
  Import ulang file yang sama tidak menduplikasi apa pun.
- Import dari **Electerm** (format export bookmark versi 5.6.10, file "export all data", atau array
  bookmark). Grup bersarang, password, passphrase, dan key yang ditempel di Electerm dipindahkan ke
  Keychain. Bookmark non-SSH (telnet, RDP, VNC, dsb.) dilewati dengan pemberitahuan.
- Import dari `~/.ssh/config` (Host, HostName, Port, User, IdentityFile, `Host *`, pola `!`, Include).
- File dengan skema lebih baru atau rusak ditolak dengan pesan jelas.

**Lainnya**
- Menu bar macOS native, mengikuti mode terang/gelap macOS, antarmuka **Inggris dan Indonesia**
  (otomatis mengikuti bahasa sistem, bisa diganti di Pengaturan).
- Tanpa telemetri; tidak ada koneksi keluar selain ke server yang Anda tuju.
- Buka koneksi dari Terminal: `open -a Peler --args "nama bookmark"` atau `user@host:port`.

## Pintasan keyboard

| Aksi | Pintasan |
| --- | --- |
| Cari bookmark dan konek | `⌘K` |
| Bookmark baru | `⌘N` |
| Tab baru (server yang sama) | `⌘T` |
| Tutup pane / tab | `⌘W` |
| Pindah tab | `⌘1`–`⌘9`, `⇧⌘[` / `⇧⌘]` |
| Konek ulang | `⌘R` |
| Split kanan / bawah | `⌘D` / `⇧⌘D` |
| Pane sebelumnya / berikutnya | `⌘[` / `⌘]` |
| Sidebar | `⌘B` |
| Panel SFTP | `⇧⌘F` |
| Ukuran teks | `⌘=` / `⌘-` / `⌘0` |
| Hapus scrollback | `⌥⌘K` |
| Gulir riwayat | `⇧PageUp` / `⇧PageDown` / `⇧Home` / `⇧End` |
| Export bookmark | `⇧⌘E` (import ada di menu File) |
| Pengaturan | `⌘,` |

## Instalasi

**Dari DMG:** buka `dist/Peler-1.0.0.dmg`, seret **Peler** ke Applications.

Build saat ini ditandatangani *ad-hoc* (tanpa Apple Developer ID). Di Mac yang membangunnya, aplikasi
langsung jalan. Di Mac lain, macOS Gatekeeper akan memblokirnya: klik kanan → **Open** sekali, atau
jalankan `xattr -dr com.apple.quarantine /Applications/Peler.app`. Untuk distribusi publik, lihat
bagian *Code signing dan notarisasi* di bawah.

## Build dari source

Prasyarat: Rust stable (rustup) dan Xcode Command Line Tools.

```sh
cargo run --release                     # jalankan langsung
cargo run --release -- "nama bookmark"  # langsung konek
scripts/package.sh                      # .app universal + DMG + Homebrew cask di dist/
PELER_ARCHS=aarch64 scripts/package.sh  # hanya Apple Silicon (lebih cepat)
```

### Code signing dan notarisasi

Dibutuhkan akun Apple Developer (berbayar):

```sh
# sekali saja: simpan kredensial notarytool di Keychain
xcrun notarytool store-credentials peler-notary --apple-id <email> --team-id <TEAMID>

PELER_SIGN_IDENTITY="Developer ID Application: Nama Anda (TEAMID)" \
PELER_NOTARY_PROFILE=peler-notary scripts/package.sh
```

Skrip menandatangani dengan hardened runtime, membuat DMG, mengirim ke notarisasi Apple, lalu
men-*staple* tiket. File cask Homebrew (`dist/homebrew/peler.rb`) berisi sha256 DMG; ganti `url`
dengan lokasi unggahan DMG Anda.

## Pengujian

```sh
cargo test                                  # unit + integrasi (otomatis menyalakan sshd lokal)
cargo test --release --test sftp_integration -- --ignored   # uji transfer 1 GB + sha256
cargo audit                                 # audit keamanan dependensi
```

Tes integrasi menjalankan OpenSSH `sshd` milik macOS tanpa root di port acak (key auth, host key
baru/berubah, shell, resize, beberapa pane, SFTP) dan server SSH in-process untuk alur password dan
keyboard-interactive. Untuk mencoba aplikasi secara manual ke server lokal:

```sh
scripts/dev-sshd.sh /tmp/peler-sshd 2299     # menampilkan path key yang dipakai
```

## Data dan keamanan

| Apa | Lokasi |
| --- | --- |
| Bookmark | `~/Library/Application Support/peler/bookmarks.json` (permission 0600) |
| Pengaturan | `~/Library/Application Support/peler/settings.json` (0600) |
| Host key tepercaya | `~/Library/Application Support/peler/known_hosts` (0600) |
| Password, passphrase, key hasil import | macOS Keychain, service `peler` |

- File ditulis atomik (tulis ke file sementara, `fsync`, `rename`); file bookmark yang rusak tidak
  ditimpa melainkan disimpan sebagai cadangan.
- Tidak ada log yang berisi password. Tidak ada telemetri.
- Cipher yang diutamakan AES-256-GCM (memakai instruksi AES CPU), lalu ChaCha20-Poly1305.
  Algoritma lama (ssh-rsa/SHA-1, CBC) hanya aktif bila dicentang per bookmark.

## Keputusan atas pertanyaan terbuka di PRD

| Pertanyaan | Keputusan |
| --- | --- |
| Framework UI | **egui 0.36** dengan renderer wgpu/Metal. Spike terminal (Gate 1) lolos: vim, warna 24-bit, CJK, IME, mouse. Renderer OpenGL dicoba dulu tetapi memakai 5 buffer jendela (~107 MB); wgpu/Metal hanya 3 (~63 MB). |
| Target chip | **Universal binary** (Apple Silicon + Intel), tetap di bawah 30 MB. |
| Lisensi | **Belum ditentukan** — silakan pilih (MIT/Apache-2.0/tertutup). Tidak ada kode Electerm yang dipakai; importer ditulis dari dokumentasi format export-nya. |
| Skema Electerm | Diverifikasi terhadap kode sumber Electerm 5.6.10 (`bookmark-toolbar.jsx` → `handleDownload`). Contoh file ada di `tests/fixtures/electerm-sample.json`. |
| Distribusi | **DMG + Homebrew cask**, tanpa Mac App Store (sandbox App Store akan membatasi akses file dan jaringan). |

## Keterbatasan yang diketahui

- **Belum diuji manual:** tes otomatis tidak bisa menyimulasikan komposisi IME sungguhan
  (Mandarin/Jepang), drag-and-drop bookmark di sidebar, atau file yang di-drag dari Finder. Fitur-fitur
  ini perlu dicoba tangan. Komposisi IME memakai jalur resmi egui/winit.
- File yang di-drag dari Finder selalu diunggah ke folder remote yang sedang dibuka. macOS tidak
  memberi tahu posisi drop, jadi tujuannya tidak bisa dipilih per folder.
- Dukungan VoiceOver masih terbatas (sesuai PRD).
- Font tanpa varian tebal (mis. Hack, Monaco) ditebalkan secara sintetis; Menlo (default) memakai
  varian tebal/miring asli.
- Build ad-hoc: setiap kali aplikasi dibangun ulang, macOS bisa meminta izin sekali untuk membuka
  item Keychain lama. Build dengan Developer ID tidak mengalami ini.
- `cargo audit` melaporkan RUSTSEC-2023-0071 (side-channel waktu pada crate `rsa`, dipakai russh untuk
  key RSA). Belum ada versi perbaikan; risikonya rendah untuk klien yang hanya menandatangani dengan
  key milik pengguna secara lokal. Gunakan key Ed25519 bila memungkinkan.
- Fitur pasca-v1.0 di PRD (port forwarding, jump host/ProxyJump, ssh-agent, Linux/Windows) belum ada.

## Struktur kode

```
src/
  app.rs                aplikasi: layout, tab, aksi, dialog
  bookmarks/            model, store, pencarian fuzzy, import/export, Electerm, ~/.ssh/config
  ssh/                  koneksi russh, autentikasi, host key, shell channel
  sftp/                 operasi remote, transfer latar belakang
  term/                 inti terminal (alacritty_terminal), encoding keyboard/mouse, skema warna
  ui/                   widget terminal, sidebar, editor, dialog, panel SFTP, menu, font, ikon
  crypto.rs secrets.rs  enkripsi export, Keychain
  i18n.rs               teks Inggris/Indonesia (tes memastikan semua teks UI terjemahkan)
tests/                  tes integrasi (sshd lokal, server in-process, 1 GB SFTP)
scripts/                package.sh (build/sign/DMG/notarize), dev-sshd.sh
```
