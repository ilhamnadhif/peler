# Peler

**Klien SSH & SFTP ringan untuk macOS, ditulis sepenuhnya dalam Rust.**

Peler adalah pengganti native untuk klien SSH berbasis Electron seperti Electerm. Bentuknya satu
binary universal kecil (Apple Silicon + Intel), tanpa Chromium dan tanpa Node.js. Peler terbuka
dalam ±0,1 detik, CPU 0% saat diam, dan memakai sekitar 70 MB memori dengan satu sesi terbuka.

[English](README.md)

![Peler: koneksi tersimpan, tab, dan split pane dalam satu koneksi SSH](docs/screenshots/terminal.png)

## Sorotan

- **Koneksi tersimpan** dalam grup bertingkat, dengan drag and drop, pencarian fuzzy, dan quick connect `⌘K`.
  Password dan passphrase disimpan di **macOS Keychain**, tidak pernah di file.
- **Terminal sungguhan** (alacritty_terminal): warna 24-bit, mouse untuk vim/htop/tmux, IME, CJK,
  dan garis kotak yang digambar presisi.
- **Tab dan split pane** yang memakai satu koneksi SSH bersama.
- **Mendeteksi koneksi yang mati**: setelah jaringan berubah (VPN dinyalakan atau dimatikan, ganti
  Wi-Fi, Mac bangun dari sleep), atau saat ketikan tidak dibalas, Peler mengecek server. Koneksi
  yang tidak menjawab lagi langsung diberi tahu dalam beberapa detik dengan tombol **Reconnect**,
  bukan menggantung.
- **SFTP dua panel**: transfer banyak file sekaligus di latar belakang, dengan progres, kecepatan,
  dan perkiraan sisa waktu. Folder ditransfer rekursif dan permission tetap terjaga.
- **Editor bawaan untuk file remote**: ubah config YAML di server, lalu tekan `⌘S`.
- **Import** dari Electerm dan `~/.ssh/config`. **Export** ke JSON berversi, termasuk password dan
  key (terenkripsi), untuk memindahkan semuanya ke Mac lain.
- **Hati-hati dengan server Anda**: pemeriksaan host key yang ketat (`known_hosts` ter-hash,
  `@revoked`), penyimpanan yang aman, dan transfer yang tidak pernah meninggalkan file setengah jadi.
- **Update otomatis** dari rilis GitHub, dipasang dengan satu klik.
- Antarmuka bahasa Inggris dan Indonesia, tema terang dan gelap, tanpa telemetri.

## Instalasi

Butuh macOS 12 Monterey atau lebih baru, Apple Silicon maupun Intel.

### Installer satu baris (disarankan)

```sh
curl -fsSL https://raw.githubusercontent.com/ilhamnadhif/peler/master/scripts/install.sh | bash
```

Skrip ini mengunduh rilis terbaru, memasang `Peler.app` ke `/Applications`, lalu membukanya.
Jalankan lagi untuk update. Untuk versi tertentu, tambahkan versinya sebagai argumen:
`… | bash -s -- v1.0.0`.

### Paket installer atau disk image

Unduh salah satu file ini dari [rilis terbaru](https://github.com/ilhamnadhif/peler/releases/latest):

- **`Peler-<versi>.pkg`**: installer macOS biasa yang memasang Peler ke `/Applications`.
- **`Peler-<versi>.dmg`**: buka, lalu seret **Peler** ke **Applications**.

> **Muncul “Peler” Not Opened / “Apple could not verify…”?** Build rilis belum dinotarisasi
> Apple, jadi macOS memblokir app dan installer yang diunduh lewat browser. Sejak macOS 15, cara
> lama klik kanan → Open sudah tidak bisa. Pilih salah satu cara berikut:
>
> - **Pakai installer satu baris** di atas. File yang diunduh lewat `curl` tidak diblokir.
> - **Izinkan sekali lewat Settings**: klik **Done** di peringatannya, buka **System Settings →
>   Privacy & Security**, gulir ke bagian **Security**, lalu klik **Open Anyway** di samping
>   “Peler was blocked…”. Konfirmasi dengan password, lalu buka Peler (atau file `.pkg`) lagi.
> - **Atau lewat Terminal**, setelah app diseret ke Applications:
>   `xattr -dr com.apple.quarantine /Applications/Peler.app`

### Homebrew

Setiap rilis menyertakan file cask `peler.rb`:

```sh
curl -fsSLO https://github.com/ilhamnadhif/peler/releases/latest/download/peler.rb
brew install --cask ./peler.rb
xattr -dr com.apple.quarantine /Applications/Peler.app   # selama rilis belum dinotarisasi
```

### Update otomatis

Setiap kali dibuka, Peler memeriksa rilis baru di GitHub. Kalau ada versi baru, jendela update
langsung muncul berisi versi sekarang, versi baru, dan daftar perubahannya. **Install Update** lalu:

1. mengunduh versi baru dan memverifikasinya (ukuran, SHA-256 yang dipublikasikan GitHub, bundle
   id, versi, dan tanda tangan kode);
2. menutup Peler, mengganti aplikasinya, dan membuka versi baru.

**Close** akan menanyakan lagi saat Peler dibuka berikutnya; centang **Don't show again for
version …** untuk melewati versi itu. Selama Peler tetap terbuka, pengecekan diulang setiap 6 jam
dan hanya memunculkan tombol **Update** di toolbar supaya tidak mengganggu. Kalau Peler dipasang
lewat `.pkg` (aplikasinya milik root), macOS meminta password administrator. Anda juga bisa
memakai **Peler → Check for Updates…**, atau mematikan pengecekan otomatis di **Settings →
General**. Pengecekan update adalah satu-satunya koneksi yang
dibuat Peler selain ke server Anda sendiri.

Versi 1.0.0 belum punya updater; versi berikutnya perlu dipasang manual sekali.

### Uninstall

```sh
rm -rf /Applications/Peler.app
rm -rf ~/Library/Application\ Support/peler      # koneksi, pengaturan, known hosts
```

Password yang tersimpan adalah item Keychain dengan service `peler`; hapus lewat Keychain Access.

## Fitur

### Koneksi

- Isian: nama, host, port, user, grup, tag, catatan, folder awal remote dan lokal, startup command,
  dan algoritma lama per server.
- Login dengan password atau private key. **Paste key-nya** (OpenSSH atau PEM; RSA, ECDSA, atau
  Ed25519, terenkripsi atau tidak): Peler langsung memeriksanya saat di-paste dan menyimpannya di
  Keychain. Atau pilih file key di `~/.ssh`.
- **SSH agent**: login dengan key dari agent macOS (`ssh-add`), 1Password, Secretive, atau agent
  YubiKey. Agent yang menandatangani; key tidak pernah keluar darinya. Peler menemukan 1Password
  dan Secretive sendiri.
- **Jump host** (`ssh -J`): sambung ke server lewat koneksi tersimpan lain, juga beberapa
  berturut-turut. Setiap hop memeriksa host key-nya sendiri dan login dengan metodenya sendiri.
- **Port forwarding** per koneksi, aktif saat tersambung: lokal (`-L`), remote (`-R`), dan proxy
  SOCKS (`-D`). Toolbar menampilkan forward-nya (⇄) dan memberi tahu kalau port sudah dipakai.
- Grup bertingkat; drag and drop untuk mengurutkan atau memindah grup.
- Pencarian fuzzy pada nama, host, user, tag, dan grup. Tekan Enter untuk konek.
- `⌘K` membuka palet perintah. Mengetik `user@host:port` di sana (IPv6 sebagai `[::1]:22`) langsung
  konek tanpa menyimpan koneksi.
- Buka dari Terminal: `open -a Peler --args web-01` atau `--args deploy@10.0.0.5:2222`. Peler hanya
  berjalan satu instance: menjalankannya lagi membuka tab di jendela yang sudah terbuka.

### Terminal

- Emulasi xterm-256color, warna 24-bit, scrollback (default 10.000 baris), layar alternatif,
  bracketed paste, mouse reporting, focus event, dan synchronized output (DEC 2026).
- Seleksi: klik ganda memilih kata, klik tiga kali memilih baris, Option-drag memilih blok.
  Copy-on-select tersedia sebagai opsi.
- Garis kotak dan karakter blok digambar sebagai bentuk, jadi garisnya menyambung mulus. Font
  cadangan CJK di-memory-map.
- Komposisi IME, dengan jendela kandidat di posisi kursor.
- Autentikasi password, keyboard-interactive (kode OTP), dan key Ed25519 / ECDSA / RSA, termasuk
  key yang terenkripsi.
- Tab dan split pane (`⌘D`, `⇧⌘D`) di atas satu koneksi. Split terbuka di folder remote yang sedang
  aktif, bila shell melaporkannya.
- Enter di pane yang sesinya selesai membuka shell baru di pane itu; `⌘R` menyambung ulang seluruh
  tab dan mempertahankan scrollback.
- Option sebagai Meta (opsional) untuk readline dan Emacs. Bekerja dengan layout keyboard apa pun,
  dan hanya aktif saat terminal yang memegang keyboard.
- Quick commands (⚡), sembilan skema warna, dan pilihan font (atau file font sendiri).

### SFTP (`⇧⌘F`)

![Panel SFTP sedang mengunggah beberapa file](docs/screenshots/sftp.png)

- Daftar lokal dan remote berdampingan, di atas koneksi SSH yang sama.
- **Upload, download, ganti nama, hapus (rekursif), folder baru, chmod.**
- **Banyak file sekaligus**: pilih dengan ⌘-klik, ⇧-klik, atau `⌘A` saat kursor di atas daftar,
  lalu pakai **Unggah (n)** / **Unduh (n)** di bawah daftar. **Unggah…** membuka pemilih file macOS.
  Bisa juga drag antar daftar, atau drop file dari Finder.
- **Transfer yang bisa dipantau**: daftar transfer (bisa di-resize dan dilipat) menampilkan seluruh
  batch: file selesai, persen, ukuran, kecepatan, dan sisa waktu. Tiap baris punya progresnya
  sendiri. Saat panel disembunyikan, tab menampilkan cincin progres dan toolbar menampilkan
  **SFTP ⬆ 45%**. Notifikasi muncul saat semuanya selesai atau ada yang gagal.
- File ditulis dengan nama sementara, lalu di-rename ke nama aslinya setelah lengkap. Membatalkan,
  menutup tab, atau quit akan menghapus file setengah jadi, termasuk di server. Ukuran diverifikasi,
  dan permission serta bit executable dipertahankan.
- Ada konfirmasi sebelum file yang sudah ada ditimpa.

### Editor file remote

![Mengedit config YAML di server](docs/screenshots/editor.png)

Klik dua kali file remote (atau klik kanan → **Edit**) untuk mengeditnya langsung:

- Nomor baris, posisi kursor, `⌘S` untuk menyimpan, `Esc` untuk menutup.
- Tab menyisipkan spasi (YAML tidak boleh tab), dan Enter mempertahankan indentasi.
- Line ending CRLF/LF, BOM UTF-8, dan permission file tetap seperti aslinya.
- **Penyimpanan aman**: isi baru ditulis di samping file lalu ditukar, jadi koneksi yang putus tidak
  pernah meninggalkan config setengah tertulis. Symlink dan file milik user lain ditulis langsung
  (in place).
- Ada peringatan bila file diubah orang lain sejak dibuka. Ada konfirmasi sebelum membuang perubahan
  yang belum disimpan, termasuk saat menutup tab atau quit.
- File biner dan file di atas 2 MB tidak dibuka di editor; tersedia tombol Unduh.

### Import dan export

- **Export** semua koneksi, satu grup, atau pilihan tertentu ke JSON berversi (`schema_version`,
  `exported_at`, `groups`, `bookmarks`).
- Password, passphrase, dan key ikut diekspor, termasuk isi file key (`~/.ssh/...`). Semuanya
  dienkripsi dengan passphrase pilihan Anda (Argon2id 64 MiB + XChaCha20-Poly1305) dan terikat ke
  host, port, dan user-nya. Kalau ada yang mengubah daftar koneksi di file agar menunjuk ke server
  lain, kredensial itu tidak ikut di-import. Di Mac yang punya file key yang sama di path itu,
  koneksi tetap memakai file tersebut; di Mac lain key-nya disimpan di Keychain.
- **Import** menampilkan pratinjau apa yang akan ditambah, diperbarui, atau dilewati. Setiap konflik
  bisa dilewati, ditimpa, atau disimpan keduanya. Import file yang sama dua kali tidak mengubah apa
  pun. Startup command dari file hanya ikut bila Anda mencentangnya.
- File export **Electerm** (koneksi dengan grup bertingkat, password, dan key yang ditempel) dan
  **`~/.ssh/config`** (`Host`, `HostName`, `Port`, `User`, `IdentityFile`, `IdentityAgent`,
  `ProxyJump`, `LocalForward`, `RemoteForward`, `DynamicForward`, `Host *`, negasi, `Include`).

## Pintasan keyboard

| Aksi | Pintasan |
| --- | --- |
| Quick connect / cari koneksi | `⌘K` |
| Koneksi baru | `⌘N` |
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
| Export koneksi | `⇧⌘E` |
| Pengaturan | `⌘,` |
| Simpan (editor file remote) | `⌘S` |

## Performa

Diukur di MacBook Apple Silicon dengan macOS 14.6, memakai build rilis:

| | Hasil |
| --- | --- |
| Memori, 1 sesi SSH | 68 MB (physical footprint, seperti di Activity Monitor) |
| Memori, 5 tab SSH | 78 MB |
| Start dingin sampai frame pertama | ±0,1 detik |
| `.app` universal (arm64 + x86_64) | 28 MB (DMG 15 MB) |
| CPU saat diam | 0,0% |
| SFTP 1 GiB ke sshd lokal | upload ±640 MB/s, download ±650–700 MB/s, sha256 diverifikasi |

Scrollback memakan sekitar 24 byte per sel. Lima tab yang masing-masing penuh dengan riwayat
10.000 baris × 120 kolom berjumlah sekitar 210 MB. Kecilkan **Scrollback** di Pengaturan kalau Anda
sering membuka banyak tab yang ramai.

## Keamanan dan data

| Apa | Lokasi |
| --- | --- |
| Koneksi | `~/Library/Application Support/peler/bookmarks.json` (mode 0600) |
| Pengaturan (termasuk ukuran dan posisi jendela) | `~/Library/Application Support/peler/settings.json` |
| Host key tepercaya | `~/Library/Application Support/peler/known_hosts` |
| Password, passphrase, key hasil import | macOS Keychain, service `peler` |
| Laporan crash | `~/Library/Logs/Peler/panic-*.log` |

- **Host key**: Peler memeriksa `known_hosts` milik aplikasi dan, hanya baca, `~/.ssh/known_hosts`,
  termasuk entri ter-hash, wildcard, negasi, dan `@revoked`.
  - Host baru harus dipercaya dengan klik; Enter tidak pernah menerima key.
  - Key yang berubah, atau key dengan jenis berbeda dari yang tercatat, memunculkan peringatan merah
    beserta fingerprint lama.
  - Key yang di-revoke langsung ditolak.
- **Algoritma**: AES-256-GCM diutamakan (AES hardware, lewat *ring*), lalu ChaCha20-Poly1305.
  Signature SHA-1, cipher CBC, dan key exchange SHA-1 hanya aktif bila **Legacy algorithms**
  dicentang di pengaturan koneksi.
- **File** ditulis secara atomik (file sementara unik, `fsync`, rename). File koneksi atau
  pengaturan yang tidak terbaca disisihkan sebagai `*.corrupt-<waktu>.json`, tidak pernah ditimpa.
  File koneksi dari versi yang lebih baru tidak pernah disentuh.
- **Rahasia** hanya ditawarkan untuk disimpan bila koneksinya memang tersimpan. Mengganti metode
  login menghapus rahasia yang tidak lagi dipakai. Tidak ada rahasia yang dicatat di log, dan tidak
  ada telemetri.

## Build dari source

Butuh toolchain Rust stable (lewat [rustup](https://rustup.rs)) dan Xcode Command Line Tools.

```sh
git clone https://github.com/ilhamnadhif/peler.git && cd peler
cargo run --release                       # build dan jalankan
cargo run --release -- web-01             # buka koneksi tersimpan (atau user@host[:port])
scripts/package.sh                        # Peler.app universal, .dmg, .pkg, dan Homebrew cask di dist/
PELER_ARCHS=aarch64 scripts/package.sh    # hanya Apple Silicon (lebih cepat)
```

### Merilis versi baru

```sh
scripts/release.sh 1.1.0              # catatan rilis dibuat dari daftar commit
scripts/release.sh 1.1.0 notes.md     # atau ditulis sendiri
```

Skrip ini menjalankan langkah-langkah berikut secara berurutan:

1. menaikkan versi di `Cargo.toml`;
2. menjalankan tes;
3. mem-build app universal beserta `.dmg`, `.pkg`, `.zip`, dan cask Homebrew;
4. commit, membuat tag `v1.1.0`, lalu push;
5. menerbitkan GitHub Release dengan file-filenya terlampir.

Aplikasi yang sudah terpasang akan menawarkannya pada pengecekan berikutnya. Butuh GitHub CLI
(`gh auth login`) dan working tree yang bersih.

### Tanda tangan dan notarisasi

Dengan akun Apple Developer:

```sh
xcrun notarytool store-credentials peler-notary --apple-id <email> --team-id <TEAMID>   # sekali saja

PELER_SIGN_IDENTITY="Developer ID Application: Nama Anda (TEAMID)" \
PELER_INSTALLER_IDENTITY="Developer ID Installer: Nama Anda (TEAMID)" \
PELER_NOTARY_PROFILE=peler-notary scripts/package.sh
```

Skrip ini menandatangani dengan hardened runtime, lalu menotarisasi dan men-staple DMG dan paket
installernya.

## Pengujian

```sh
cargo test                                                   # unit + tes integrasi
cargo test --release --test sftp_integration -- --ignored    # bolak-balik 1 GiB dengan sha256
cargo audit                                                  # advisory dependensi
```

Tes integrasi menjalankan `sshd` OpenSSH bawaan macOS tanpa root, di port acak. Yang diuji:

- autentikasi key, serta host key baru, berubah, dan revoked;
- shell, resize, dan beberapa pane;
- transfer SFTP dan editor remote.

Alur password dan keyboard-interactive diuji dengan server SSH in-process.

Build debug punya driver end-to-end. Driver ini mengirim event keyboard dan mouse sungguhan ke
jendela, lewat jalur yang sama dengan input pengguna (menu, winit, egui):

```sh
PELER_DATA_DIR=/tmp/peler-e2e PELER_SECRETS=memory \
PELER_DEV_SCRIPT='sleep 1500;; connect local dev;; sleep 2500;; keys echo hi\r;; expect-screen hi;; key cmd+d;; expect-panes 2;; quit' \
  target/debug/peler
```

Hasilnya dicetak sebagai baris `DEVTEST PASS/FAIL`. `PELER_DATA_DIR` dan `PELER_SECRETS` hanya
berlaku di build debug. `scripts/dev-sshd.sh` menjalankan sshd lokal untuk mencoba aplikasi.

## Struktur kode

```
src/
  app.rs              aplikasi: layout, tab, aksi, dialog
  bookmarks/          model, store, pencarian fuzzy, import/export, Electerm, ~/.ssh/config
  ssh/                koneksi russh, autentikasi, known_hosts, shell
  sftp/               operasi remote, transfer latar belakang, muat/simpan editor
  term/               inti terminal (alacritty_terminal), encoding key/mouse, skema warna
  ui/                 widget terminal, sidebar, editor, dialog, panel SFTP, menu, font
  crypto.rs           enkripsi export
  secrets.rs          Keychain
  i18n.rs             teks Inggris/Indonesia (ada tes yang memastikan semua teks UI diterjemahkan)
  instance.rs         satu instance dan penerusan argumen
  netwatch.rs         mendeteksi perubahan jaringan (VPN, Wi-Fi, sleep) supaya koneksi dicek
  update.rs           cek update, unduh, verifikasi, dan pasang
tests/                tes integrasi (sshd lokal, server in-process, SFTP 1 GiB)
scripts/              package.sh, release.sh, install.sh, dev-sshd.sh
vendor/ssh-key/       ssh-key dengan perbaikan untuk sebagian kunci ECDSA (lihat PELER-PATCH.md di sana)
```

Dibangun dengan [egui](https://github.com/emilk/egui) (wgpu/Metal),
[alacritty_terminal](https://github.com/alacritty/alacritty),
[russh](https://github.com/Eugeny/russh) dan russh-sftp, [tokio](https://tokio.rs), serta
[keyring](https://github.com/hwchen/keyring-rs).

## Keterbatasan yang diketahui

- Build rilis belum ditandatangani dan dinotarisasi dengan Apple Developer ID, jadi macOS memberi
  peringatan bila diunduh lewat browser; lihat catatan di bagian *Instalasi*.
- Belum tercakup tes otomatis, jadi sebaiknya dicek manual: komposisi IME, mengurutkan koneksi
  dengan drag and drop, dan file yang di-drop dari Finder.
- File yang di-drop dari Finder diunggah ke folder remote yang sedang terbuka; macOS tidak memberi
  tahu di bagian jendela mana file itu di-drop.
- Login dengan key RSA ditandatangani memakai `ring` (constant time), bukan crate `rsa` yang punya
  side-channel waktu (RUSTSEC-2023-0071, diabaikan di `.cargo/audit.toml` beserta alasannya). Crate
  `rsa` masih menandatangani di dua kasus langka: tanda tangan SHA-1 untuk server sebelum 2016
  (hanya bila legacy algorithms dinyalakan) dan key RSA di bawah 2048 bit.
- Belum tersedia: agent forwarding, X11 forwarding, dan build Linux/Windows.

## Lisensi

[MIT](LICENSE) © 2026 ilhamnadhif
