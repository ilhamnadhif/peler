//! English / Indonesian UI strings.
//!
//! UI code writes English text through `t("...")`; when the active language is Indonesian the
//! string is looked up in `ID` and falls back to English if a translation is missing. A unit
//! test checks that every literal passed to `t(...)` in the source has a translation.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Follow the macOS preferred language.
    #[default]
    System,
    English,
    Indonesian,
}

static INDONESIAN: AtomicBool = AtomicBool::new(false);

/// Applies a language preference for all subsequent `t` calls.
pub fn set_language(lang: Language) {
    let id = match lang {
        Language::English => false,
        Language::Indonesian => true,
        Language::System => system_prefers_indonesian(),
    };
    INDONESIAN.store(id, Ordering::Relaxed);
}

pub fn is_indonesian() -> bool {
    INDONESIAN.load(Ordering::Relaxed)
}

fn system_prefers_indonesian() -> bool {
    sys_locale::get_locales()
        .next()
        .is_some_and(|l| l.starts_with("id") || l.starts_with("in"))
}

/// Translates an English UI string into the active language.
pub fn t(en: &'static str) -> &'static str {
    if !is_indonesian() {
        return en;
    }
    table().get(en).copied().unwrap_or(en)
}

/// `t` followed by replacing each `{}` with the next argument, in order.
pub fn tf(en: &'static str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut rest = t(en);
    let mut args = args.iter();
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        out.push_str(args.next().copied().unwrap_or(""));
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| ID.iter().copied().collect())
}

/// English → Indonesian.
const ID: &[(&str, &str)] = &[
    ("(copy)", "(salinan)"),
    // connection errors
    ("Host not found: {}", "Host tidak ditemukan: {}"),
    (
        "Connection refused: nothing is listening on that port.",
        "Koneksi ditolak: tidak ada layanan di port tersebut.",
    ),
    ("Connection timed out.", "Koneksi melewati batas waktu."),
    ("Network unreachable: {}", "Jaringan tidak terjangkau: {}"),
    ("Host key not accepted.", "Host key tidak diterima."),
    (
        "The server's host key has changed. Connection refused to protect you.",
        "Host key server berubah. Koneksi ditolak untuk melindungi Anda.",
    ),
    ("Authentication failed: {}", "Autentikasi gagal: {}"),
    ("Cancelled.", "Dibatalkan."),
    ("Private key error: {}", "Kesalahan private key: {}"),
    ("Disconnected: {}", "Terputus: {}"),
    // UI
    ("Close pane", "Tutup panel"),
    ("Follow macOS", "Ikuti macOS"),
    ("Last folder used", "Folder terakhir dipakai"),
    ("Local start folder", "Folder awal lokal"),
    ("Remote start folder", "Folder awal remote"),
    ("Switch to dark theme", "Ganti ke tema gelap"),
    ("Switch to light theme", "Ganti ke tema terang"),
    (
        "The local side of SFTP opens here.",
        "Sisi lokal SFTP terbuka di sini.",
    ),
    (
        "The terminal and the remote side of SFTP open here.",
        "Terminal dan sisi remote SFTP terbuka di sini.",
    ),
    ("{} items", "{} item"),
    (" lines", " baris"),
    (
        "\"{}\" and its saved password will be removed.",
        "\"{}\" beserta password tersimpannya akan dihapus.",
    ),
    ("(choose)", "(pilih)"),
    ("(none)", "(tanpa grup)"),
    (
        "0 = off. Applies to new connections.",
        "0 = mati. Berlaku untuk koneksi baru.",
    ),
    ("About Peler", "Tentang Peler"),
    ("Accept New Key", "Terima Kunci Baru"),
    ("Action", "Tindakan"),
    ("Actual Size", "Ukuran Asli"),
    ("Add", "Tambah"),
    ("Add command", "Tambah perintah"),
    ("Address", "Alamat"),
    ("All bookmarks ({})", "Semua bookmark ({})"),
    ("All files", "Semua file"),
    (
        "Allow legacy algorithms (old servers)",
        "Izinkan algoritma lama (server lawas)",
    ),
    ("Already present", "Sudah ada"),
    (
        "Also offer ssh-rsa/SHA-1 key exchange and CBC ciphers. Only enable this for servers that need it.",
        "Juga tawarkan ssh-rsa, pertukaran kunci SHA-1, dan cipher CBC. Aktifkan hanya untuk server yang membutuhkannya.",
    ),
    ("Appearance", "Tampilan"),
    (
        "Ask before closing a connected tab",
        "Tanya sebelum menutup tab yang terhubung",
    ),
    ("Authenticating as {}…", "Autentikasi sebagai {}…"),
    ("Authentication", "Autentikasi"),
    ("Bar", "Garis"),
    ("Bigger Text", "Teks Lebih Besar"),
    ("Blink", "Berkedip"),
    ("Block", "Blok"),
    ("Bookmark", "Bookmark"),
    ("Bookmark exports", "Ekspor bookmark"),
    ("Bring All to Front", "Bawa Semua ke Depan"),
    ("Browse…", "Pilih…"),
    ("Cancel", "Batal"),
    ("Cancelled", "Dibatalkan"),
    (
        "Chinese/Japanese/Korean fallback fonts",
        "Font cadangan Mandarin/Jepang/Korea",
    ),
    ("Choose a private key file.", "Pilih file private key."),
    ("Clear Scrollback", "Hapus Riwayat Gulir"),
    ("Clear finished", "Bersihkan yang selesai"),
    ("Close", "Tutup"),
    ("Close Tab", "Tutup Tab"),
    ("Close pane or tab", "Tutup panel atau tab"),
    ("Close this tab?", "Tutup tab ini?"),
    ("Closing tabs", "Menutup tab"),
    ("Colors (dark)", "Warna (gelap)"),
    ("Colors (light)", "Warna (terang)"),
    ("Command", "Perintah"),
    (
        "Commands you can send to the active terminal from the toolbar.",
        "Perintah yang bisa dikirim ke terminal aktif dari toolbar.",
    ),
    ("Connect", "Konek"),
    (
        "Connect to a server to upload files.",
        "Konek ke server dulu untuk mengunggah file.",
    ),
    (
        "Connect to… (type to search bookmarks)",
        "Konek ke… (ketik untuk mencari bookmark)",
    ),
    ("Connected", "Terhubung"),
    ("Connecting to {}…", "Menghubungkan ke {}…"),
    ("Connecting…", "Menghubungkan…"),
    ("Connection cancelled.", "Koneksi dibatalkan."),
    ("Connection closed.", "Koneksi ditutup."),
    ("Copy", "Salin"),
    ("Copy Address", "Salin Alamat"),
    ("Copy Path", "Salin Path"),
    ("Copy text when selected", "Salin teks saat dipilih"),
    ("Could not open a shell: {}", "Tidak bisa membuka shell: {}"),
    (
        "Could not read the bookmarks file ({}). It was kept as {}.",
        "File bookmark tidak bisa dibaca ({}). File disimpan sebagai {}.",
    ),
    ("Create", "Buat"),
    (
        "Credentials are encrypted with Argon2id + XChaCha20-Poly1305. You need this passphrase to import them.",
        "Kredensial dienkripsi dengan Argon2id + XChaCha20-Poly1305. Passphrase ini diperlukan saat mengimpornya.",
    ),
    ("Cursor", "Kursor"),
    ("Cut", "Potong"),
    ("Dark", "Gelap"),
    ("Data folder", "Folder data"),
    ("Delete", "Hapus"),
    ("Delete Group…", "Hapus Grup…"),
    ("Delete bookmark?", "Hapus bookmark?"),
    ("Delete group?", "Hapus grup?"),
    (
        "Delete {} item(s) {}? Folders are deleted with everything in them. This cannot be undone.",
        "Hapus {} item {}? Folder dihapus beserta seluruh isinya. Tindakan ini tidak bisa dibatalkan.",
    ),
    ("Delete…", "Hapus…"),
    ("Disconnected", "Terputus"),
    ("Disconnected.", "Terputus."),
    ("Done", "Selesai"),
    ("Download", "Unduh"),
    (
        "Drop to upload to the current remote folder",
        "Lepas untuk mengunggah ke folder remote saat ini",
    ),
    ("Duplicate", "Duplikat"),
    ("Duplicate Tab", "Duplikat Tab"),
    ("Edit", "Edit"),
    ("Edit Bookmark", "Edit Bookmark"),
    ("Edit Bookmark…", "Edit Bookmark…"),
    ("Edit quick commands…", "Edit quick command…"),
    ("Edit…", "Edit…"),
    ("Enter Full Screen", "Layar Penuh"),
    ("Execute", "Eksekusi"),
    ("Export Bookmarks", "Ekspor Bookmark"),
    ("Export Bookmarks…", "Ekspor Bookmark…"),
    ("Export Group…", "Ekspor Grup…"),
    ("Exported to {}", "Diekspor ke {}"),
    ("Export…", "Ekspor…"),
    ("Failed", "Gagal"),
    ("File", "File"),
    ("Fingerprint", "Sidik jari"),
    ("Folder name", "Nama folder"),
    ("Font", "Font"),
    ("Fonts", "Font"),
    (
        "Forget the saved passphrase",
        "Lupakan passphrase tersimpan",
    ),
    ("Forget the saved password", "Lupakan password tersimpan"),
    ("General", "Umum"),
    ("Group", "Grup"),
    ("Group name", "Nama grup"),
    ("Help", "Bantuan"),
    ("Hide Others", "Sembunyikan Lainnya"),
    ("Hide Peler", "Sembunyikan Peler"),
    ("Home folder", "Folder home"),
    ("Host", "Host"),
    ("Host is required.", "Host wajib diisi."),
    ("Import", "Impor"),
    ("Import Bookmarks", "Impor Bookmark"),
    ("Import Bookmarks…", "Impor Bookmark…"),
    (
        "Import finished: {} added, {} updated, {} skipped.",
        "Impor selesai: {} ditambah, {} diperbarui, {} dilewati.",
    ),
    ("Import from ~/.ssh/config", "Impor dari ~/.ssh/config"),
    ("Imported key (Keychain)", "Key hasil impor (Keychain)"),
    (
        "Include saved passwords and passphrases (encrypted)",
        "Sertakan password dan passphrase tersimpan (terenkripsi)",
    ),
    (
        "Invalid permissions; use octal like 644 or 755.",
        "Permission tidak valid; gunakan oktal seperti 644 atau 755.",
    ),
    ("Keep both", "Simpan keduanya"),
    ("Keepalive", "Keepalive"),
    ("Key file", "File key"),
    ("Key passphrase", "Passphrase key"),
    ("Key type", "Jenis kunci"),
    ("Keyboard", "Keyboard"),
    ("Keyboard Shortcuts", "Pintasan Keyboard"),
    ("Language", "Bahasa"),
    (
        "Leave empty to ask when connecting",
        "Kosongkan agar ditanya saat konek",
    ),
    ("Light", "Terang"),
    (
        "Lightweight SSH and SFTP client",
        "Klien SSH dan SFTP yang ringan",
    ),
    ("Local", "Lokal"),
    ("Minimize", "Perkecil"),
    ("Modified", "Diubah"),
    ("Name", "Nama"),
    ("New Bookmark", "Bookmark Baru"),
    ("New Bookmark…", "Bookmark Baru…"),
    ("New Group", "Grup Baru"),
    ("New Subgroup", "Subgrup Baru"),
    ("New Tab", "Tab Baru"),
    ("New folder", "Folder baru"),
    ("New tab (same server)", "Tab baru (server yang sama)"),
    ("Next Pane", "Panel Berikutnya"),
    ("No bookmarks yet", "Belum ada bookmark"),
    (
        "No common algorithm with the server ({}). Try enabling legacy algorithms in the bookmark.",
        "Tidak ada algoritma yang cocok dengan server ({}). Coba aktifkan algoritma lama di bookmark.",
    ),
    (
        "No hosts found in ~/.ssh/config.",
        "Tidak ada host di ~/.ssh/config.",
    ),
    ("No matches", "Tidak ada yang cocok"),
    ("No quick commands", "Belum ada quick command"),
    ("Not connected.", "Tidak terhubung."),
    ("Notes", "Catatan"),
    ("OK", "OK"),
    ("Octal", "Oktal"),
    ("One command per line", "Satu perintah per baris"),
    ("One group:", "Satu grup:"),
    ("Only if the key is encrypted", "Hanya jika key terenkripsi"),
    ("Open", "Buka"),
    ("Opening SFTP…", "Membuka SFTP…"),
    ("Other font file…", "File font lain…"),
    ("Others", "Lainnya"),
    ("Overwrite", "Timpa"),
    ("Owner", "Pemilik"),
    ("Parent folder", "Folder induk"),
    ("Passphrase", "Passphrase"),
    ("Password", "Password"),
    ("Password for {}", "Password untuk {}"),
    (
        "Passwords and keys from the file will be stored in the macOS Keychain.",
        "Password dan key dari file ini akan disimpan di Keychain macOS.",
    ),
    ("Paste", "Tempel"),
    ("Permissions", "Permission"),
    ("Permissions…", "Permission…"),
    ("Port", "Port"),
    (
        "Port must be a number between 1 and 65535.",
        "Port harus angka antara 1 dan 65535.",
    ),
    ("Press Enter after sending", "Tekan Enter setelah dikirim"),
    ("Previous / next pane", "Panel sebelumnya / berikutnya"),
    ("Previous / next tab", "Tab sebelumnya / berikutnya"),
    ("Previous Pane", "Panel Sebelumnya"),
    ("Private key", "Private key"),
    (
        "Private key files are referenced by path only; their contents are never exported.",
        "File private key hanya dirujuk lewat path; isinya tidak pernah diekspor.",
    ),
    ("Quick Commands", "Quick Command"),
    ("Quick Connect…", "Konek Cepat…"),
    ("Quit", "Keluar"),
    ("Quit Peler", "Keluar dari Peler"),
    ("Quit Peler?", "Keluar dari Peler?"),
    ("Read", "Baca"),
    ("Reconnect", "Konek Ulang"),
    ("Refresh", "Muat ulang"),
    ("Remote", "Remote"),
    ("Remove", "Hapus"),
    ("Rename", "Ganti Nama"),
    ("Rename Group", "Ganti Nama Grup"),
    ("Rename…", "Ganti Nama…"),
    ("Repeat", "Ulangi"),
    ("Replace", "Ganti"),
    ("Replace existing items?", "Ganti item yang sudah ada?"),
    ("Run", "Jalankan"),
    ("Run on connect", "Jalankan saat konek"),
    ("SFTP", "SFTP"),
    ("SFTP Panel", "Panel SFTP"),
    (
        "SFTP is not available on this server ({})",
        "SFTP tidak tersedia di server ini ({})",
    ),
    (
        "SFTP opens once the connection is ready.",
        "SFTP terbuka setelah koneksi siap.",
    ),
    ("SFTP unavailable: {}", "SFTP tidak tersedia: {}"),
    ("Save", "Simpan"),
    ("Save & Connect", "Simpan & Konek"),
    ("Save in Keychain", "Simpan di Keychain"),
    (
        "Saved in Keychain — type to replace",
        "Tersimpan di Keychain — ketik untuk mengganti",
    ),
    ("Scroll back", "Gulir ke atas"),
    ("Scrollback", "Riwayat gulir"),
    ("Search bookmarks", "Cari bookmark"),
    ("Search bookmarks and connect", "Cari bookmark dan konek"),
    ("Select All", "Pilih Semua"),
    (
        "Select at least one bookmark.",
        "Pilih minimal satu bookmark.",
    ),
    ("Selected bookmarks", "Bookmark terpilih"),
    ("Selection", "Seleksi"),
    ("Services", "Layanan"),
    ("Session ended", "Sesi berakhir"),
    ("Settings", "Pengaturan"),
    ("Settings…", "Pengaturan…"),
    ("Show All", "Tampilkan Semua"),
    ("Show Next Tab", "Tab Berikutnya"),
    ("Show Previous Tab", "Tab Sebelumnya"),
    ("Show in Finder", "Tampilkan di Finder"),
    ("Show/Hide Sidebar", "Tampilkan/Sembunyikan Sidebar"),
    ("Size", "Ukuran"),
    ("Skip", "Lewati"),
    (
        "Skipped bookmarks that are not SSH connections: {}.",
        "Bookmark yang bukan koneksi SSH dilewati: {}.",
    ),
    ("Smaller Text", "Teks Lebih Kecil"),
    (
        "Some bookmarks refer to missing groups; they will be placed at the top level.",
        "Beberapa bookmark merujuk ke grup yang tidak ada; bookmark tersebut ditaruh di tingkat teratas.",
    ),
    ("Split Down", "Bagi ke Bawah"),
    ("Split Right", "Bagi ke Kanan"),
    ("Split right / down", "Bagi ke kanan / bawah"),
    ("Start folder", "Folder awal"),
    ("Switch to tab", "Pindah ke tab"),
    ("System", "Sistem"),
    ("Tags", "Tag"),
    ("Terminal", "Terminal"),
    ("Text size", "Ukuran teks"),
    (
        "The Electerm file contains no SSH bookmarks.",
        "File Electerm ini tidak berisi bookmark SSH.",
    ),
    (
        "The connection to {} will be closed.",
        "Koneksi ke {} akan ditutup.",
    ),
    (
        "The export file is damaged or incomplete: {}",
        "File ekspor rusak atau tidak lengkap: {}",
    ),
    ("The file is not UTF-8 text.", "File ini bukan teks UTF-8."),
    (
        "The file is not valid JSON or an SSH config file: {}",
        "File ini bukan JSON yang valid maupun file SSH config: {}",
    ),
    (
        "The group \"{}\" will be removed. Its bookmarks and sub-groups move up one level.",
        "Grup \"{}\" akan dihapus. Bookmark dan subgrupnya naik satu tingkat.",
    ),
    (
        "The passphrase must not be empty.",
        "Passphrase tidak boleh kosong.",
    ),
    ("The passphrases do not match.", "Passphrase tidak sama."),
    (
        "The private key ({}) is protected by a passphrase.",
        "Private key ({}) dilindungi passphrase.",
    ),
    (
        "The private key file does not exist.",
        "File private key tidak ditemukan.",
    ),
    ("The server asks", "Pertanyaan dari server"),
    (
        "The server presented a different key than the one saved for it. Someone could be intercepting the connection, or the server may have been reinstalled. Only continue if you know why the key changed.",
        "Server mengirim kunci yang berbeda dari yang tersimpan. Bisa jadi ada pihak yang menyadap koneksi, atau server baru diinstal ulang. Lanjutkan hanya jika Anda tahu alasan kunci berubah.",
    ),
    (
        "There are no bookmarks to export yet.",
        "Belum ada bookmark untuk diekspor.",
    ),
    (
        "These Electerm settings are not supported and were left out: {}.",
        "Pengaturan Electerm berikut belum didukung dan tidak diimpor: {}.",
    ),
    (
        "This file contains encrypted passwords. Enter the export passphrase to import them, or continue without them.",
        "File ini berisi password terenkripsi. Masukkan passphrase ekspor untuk mengimpornya, atau lanjutkan tanpa password.",
    ),
    (
        "This file was exported by a newer version of peler (schema {}); this version reads up to schema {}. Please update the app.",
        "File ini diekspor oleh versi peler yang lebih baru (skema {}); versi ini hanya membaca sampai skema {}. Silakan perbarui aplikasi.",
    ),
    (
        "This server has not been seen before. Check that the fingerprint matches the server's key before trusting it.",
        "Server ini belum pernah dihubungi. Pastikan sidik jari cocok dengan kunci server sebelum mempercayainya.",
    ),
    (
        "Tip: double-click a bookmark, or select it in the search and press Enter.",
        "Tips: klik dua kali bookmark, atau pilih lewat pencarian lalu tekan Enter.",
    ),
    ("Transfers", "Transfer"),
    ("Trust and Connect", "Percaya dan Konek"),
    ("Underline", "Garis bawah"),
    (
        "Unknown bookmark or address: {}",
        "Bookmark atau alamat tidak dikenal: {}",
    ),
    ("Unknown host", "Host tidak dikenal"),
    ("Unlock", "Buka"),
    (
        "Unlocked credentials for {} bookmark(s).",
        "Kredensial untuk {} bookmark berhasil dibuka.",
    ),
    (
        "Unrecognized file format: expected a peler export, an Electerm bookmark export or an SSH config.",
        "Format file tidak dikenali: harus berupa ekspor peler, ekspor bookmark Electerm, atau SSH config.",
    ),
    ("Upload", "Unggah"),
    (
        "Use Option as Meta key",
        "Gunakan Option sebagai tombol Meta",
    ),
    (
        "Use a passphrase of at least 8 characters.",
        "Gunakan passphrase minimal 8 karakter.",
    ),
    ("Username", "Username"),
    ("View", "Tampilan"),
    ("Waiting…", "Menunggu…"),
    (
        "Warning: the host key has changed!",
        "Peringatan: host key berubah!",
    ),
    ("What to export", "Yang diekspor"),
    ("Window", "Jendela"),
    ("Write", "Tulis"),
    (
        "Wrong passphrase, or the file has been modified.",
        "Passphrase salah, atau file telah diubah.",
    ),
    (
        "Wrong passphrase, try again.",
        "Passphrase salah, coba lagi.",
    ),
    ("Wrong password, try again.", "Password salah, coba lagi."),
    ("Zoom", "Perbesar"),
    (
        "[Connection closed. Press Enter or ⌘R to reconnect.]",
        "[Koneksi ditutup. Tekan Enter atau ⌘R untuk konek ulang.]",
    ),
    (
        "[Session ended, exit code {}. Press Enter or ⌘R to reconnect.]",
        "[Sesi berakhir, kode keluar {}. Tekan Enter atau ⌘R untuk konek ulang.]",
    ),
    ("comma separated", "pisahkan dengan koma"),
    ("e.g. Production web", "mis. Web produksi"),
    ("jump hosts", "jump host"),
    ("on the server", "di server"),
    ("on this Mac", "di Mac ini"),
    (
        "password login is not allowed for {} (server accepts: {})",
        "login dengan password tidak diizinkan untuk {} (server menerima: {})",
    ),
    ("port forwarding", "port forwarding"),
    ("proxies", "proxy"),
    ("reconnecting", "konek ulang"),
    ("same bookmark, changed", "bookmark sama, sudah berubah"),
    ("same host, port and user", "host, port, dan user sama"),
    (
        "the key is encrypted and needs a passphrase",
        "key terenkripsi dan memerlukan passphrase",
    ),
    (
        "the private key is missing from the Keychain",
        "private key tidak ada di Keychain",
    ),
    (
        "the server closed the connection during the handshake",
        "server menutup koneksi saat handshake",
    ),
    (
        "the server rejected the key for user {} (server accepts: {})",
        "server menolak key untuk user {} (server menerima: {})",
    ),
    (
        "unsupported key format ({})",
        "format key tidak didukung ({})",
    ),
    ("wrong passphrase", "passphrase salah"),
    ("wrong password for {}", "password salah untuk {}"),
    ("{} already exists.", "{} sudah ada."),
    ("{} bookmarks", "{} bookmark"),
    ("{} conflict(s). For all:", "{} konflik. Untuk semua:"),
    (
        "{} connection(s) are open and will be closed.",
        "{} koneksi terbuka dan akan ditutup.",
    ),
    (
        "{} credential(s) could not be saved to the Keychain.",
        "{} kredensial tidak bisa disimpan ke Keychain.",
    ),
    (
        "{} did not answer as an SSH server.",
        "{} tidak merespons sebagai server SSH.",
    ),
    (
        "{} item(s) already exist at the destination and will be replaced:",
        "{} item sudah ada di tujuan dan akan diganti:",
    ),
    (
        "{} to add · {} to update · {} to skip",
        "{} ditambah · {} diperbarui · {} dilewati",
    ),
    (
        "Bookmark changes are not saved: the bookmarks file could not be read safely.",
        "Perubahan bookmark tidak disimpan: file bookmark tidak bisa dibaca dengan aman.",
    ),
    (
        "Close the open dialog first, then drop the files again.",
        "Tutup dialog yang terbuka dulu, lalu drop file-nya lagi.",
    ),
    ("Connect to {}", "Hubungkan ke {}"),
    (
        "Could not open {} ({}); showing the home folder instead.",
        "Tidak bisa membuka {} ({}); menampilkan folder home.",
    ),
    (
        "Could not read the saved credentials of {} from the Keychain: {}",
        "Tidak bisa membaca kredensial tersimpan milik {} dari Keychain: {}",
    ),
    (
        "Could not read the settings ({}). The file was kept as {}.",
        "Tidak bisa membaca pengaturan ({}). File disimpan sebagai {}.",
    ),
    ("Could not read {} ({})", "Tidak bisa membaca {} ({})"),
    ("Could not read {}", "Tidak bisa membaca {}"),
    ("Could not save {}", "Tidak bisa menyimpan {}"),
    ("{} is damaged", "{} rusak"),
    (
        "Could not save the host key in {} ({})",
        "Tidak bisa menyimpan host key di {} ({})",
    ),
    ("Decryption failed.", "Dekripsi gagal."),
    (
        "Encryption parameters in the file are out of range.",
        "Parameter enkripsi di file berada di luar batas.",
    ),
    (
        "Import startup commands ({} bookmark(s))",
        "Import startup command ({} bookmark)",
    ),
    (
        "Looking up {} timed out.",
        "Pencarian alamat {} melewati batas waktu.",
    ),
    (
        "Missing \"]\" after the IPv6 address.",
        "Kurang \"]\" setelah alamat IPv6.",
    ),
    ("Not an Electerm export.", "Bukan file export Electerm."),
    ("Open a remote folder first.", "Buka folder remote dulu."),
    (
        "Overwriting changes the address; the credentials saved for the old address are removed.",
        "Menimpa akan mengubah alamat; kredensial yang tersimpan untuk alamat lama dihapus.",
    ),
    ("Reconnect this tab?", "Hubungkan ulang tab ini?"),
    ("Saved keys", "Key tersimpan"),
    ("Show hidden files", "Tampilkan file tersembunyi"),
    (
        "Startup commands run on the server every time the bookmark connects. Only import them from a file you trust.",
        "Startup command dijalankan di server setiap kali bookmark terhubung. Import hanya dari file yang Anda percaya.",
    ),
    (
        "The bookmarks were saved by a newer version of Peler (schema {}). Please update the app; until then bookmark changes are not saved.",
        "Bookmark disimpan oleh versi Peler yang lebih baru (schema {}). Perbarui aplikasi; sampai saat itu perubahan bookmark tidak disimpan.",
    ),
    (
        "The encrypted credentials in the file are damaged.",
        "Kredensial terenkripsi di file ini rusak.",
    ),
    (
        "The file is too large to be a bookmark export (more than {} MB).",
        "File terlalu besar untuk sebuah export bookmark (lebih dari {} MB).",
    ),
    (
        "The font {} could not be loaded; the built-in Hack font is used instead.",
        "Font {} tidak bisa dimuat; font bawaan Hack dipakai sebagai gantinya.",
    ),
    (
        "The host name contains characters that are not allowed.",
        "Nama host berisi karakter yang tidak diizinkan.",
    ),
    (
        "The running sessions on {} will be closed and opened again.",
        "Sesi yang berjalan di {} akan ditutup dan dibuka lagi.",
    ),
    (
        "The server's host key is marked as revoked in known_hosts. Connection refused.",
        "Host key server ditandai sebagai dicabut (revoked) di known_hosts. Koneksi ditolak.",
    ),
    (
        "This file is from an older version: check each bookmark's address before connecting with the imported credentials.",
        "File ini dari versi lama: periksa alamat setiap bookmark sebelum terhubung dengan kredensial yang di-import.",
    ),
    (
        "Unsupported encryption ({} / {}).",
        "Enkripsi tidak didukung ({} / {}).",
    ),
    (
        "Uploads will start after the open dialog is closed.",
        "Upload akan dimulai setelah dialog yang terbuka ditutup.",
    ),
    (
        "Uploads will start when SFTP is ready.",
        "Upload akan dimulai saat SFTP siap.",
    ),
    (
        "[Session ended by signal {}. Press Enter or ⌘R to reconnect.]",
        "[Sesi berakhir karena sinyal {}. Tekan Enter atau ⌘R untuk menghubungkan ulang.]",
    ),
    (
        "the server only accepts RSA keys with SHA-1 signatures. Enable legacy algorithms in the bookmark to allow it.",
        "server hanya menerima key RSA dengan signature SHA-1. Aktifkan algoritma lama (legacy) di bookmark untuk mengizinkannya.",
    ),
    (
        "{} file transfer(s) are still running and will be cancelled.",
        "{} transfer file masih berjalan dan akan dibatalkan.",
    ),
    ("{} is already being transferred.", "{} sedang ditransfer."),
    (
        "{} saved credential(s) were left out: the bookmark's address in the file differs from the address they were saved for.",
        "{} kredensial tersimpan tidak diikutkan: alamat bookmark di file berbeda dengan alamat saat kredensial disimpan.",
    ),
    ("{} skipped", "{} dilewati"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_and_format() {
        INDONESIAN.store(true, Ordering::Relaxed);
        assert_eq!(t("(copy)"), "(salinan)");
        assert_eq!(t("__missing__"), "__missing__");
        INDONESIAN.store(false, Ordering::Relaxed);
        assert_eq!(tf("{} of {}", &["1", "2"]), "1 of 2");
    }

    /// Every `t("...")` / `tf("...")` literal in the source must have an Indonesian entry.
    #[test]
    fn every_ui_string_is_translated() {
        let mut missing = Vec::new();
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("i18n.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for lit in extract_literals(&text) {
                    if !table().contains_key(lit.as_str()) && !missing.contains(&lit) {
                        missing.push(lit);
                    }
                }
            }
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "untranslated strings:\n{}",
            missing.join("\n")
        );
    }

    /// Finds string literals passed directly to `t(` or `tf(`.
    fn extract_literals(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for pat in ["t(", "tf("] {
            let mut rest = text;
            while let Some(pos) = rest.find(pat) {
                let prev = rest[..pos].chars().last();
                rest = &rest[pos + pat.len()..];
                if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue; // e.g. `print(` or `contains_key(`
                }
                // The literal may start on the next line.
                let trimmed = rest.trim_start();
                let Some(after_quote) = trimmed.strip_prefix('"') else {
                    continue;
                };
                rest = after_quote;
                let mut lit = String::new();
                let mut chars = rest.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some('n') => lit.push('\n'),
                            Some(other) => lit.push(other),
                            None => break,
                        },
                        '"' => break,
                        c => lit.push(c),
                    }
                }
                out.push(lit);
            }
        }
        out
    }
}
