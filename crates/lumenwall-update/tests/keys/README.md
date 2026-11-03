# Test-only minisign key pair

`TEST-ONLY-lumenwall-update.key` / `.pub` sign the fake releases served by the tests
(`tests/support/mod.rs`) and by `cargo run -p lumenwall-update --example fake_github`.

- The secret key is public on purpose (it is in this repository) and has no password.
- It is **never** the release key: `RELEASE_PUBLIC_KEY` in `src/lib.rs` must never be set to
  this public key (`RWQm89fQDBm9XR6E…`), and the secret must never be stored in the
  repository secrets.
- The production key pair is created and kept outside the repository: see
  [`docs/RELEASING.md`](../../../../docs/RELEASING.md).
