# ssh-key 0.7.0-rc.11, patched

A copy of [ssh-key](https://crates.io/crates/ssh-key) 0.7.0-rc.11 (the exact version russh
0.64 requires), used through `[patch.crates-io]` in Peler's `Cargo.toml`. Its tests are not
included.

The only change is in `src/private/ecdsa.rs`: ECDSA private scalars shorter than 32 bytes are
accepted. OpenSSH stores the scalar as an `mpint` with leading zero bytes removed, so about one
P-256 key in 256 that `ssh-keygen` creates has a 31-byte scalar, which upstream rejects with
"length invalid". Peler could not log in with such a key.

Drop this copy once russh depends on an ssh-key release that accepts these keys.
