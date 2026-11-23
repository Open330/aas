This directory contains the library source and licenses from crates.io
`proc-macro-error2` 2.0.1. The original archive SHA-256 is
`11ec05c52be0a07b08061f7dd003e7d7092e0472bc731b4af7bb1ef876109802`.
The local manifest retains the upstream library dependencies and features.

The source change makes the `proc_macro` import public so the existing
`__export` module can re-export it. This resolves Rust's
[`pub_use_of_private_extern_crate`](https://github.com/rust-lang/rust/issues/127909)
future-compatibility diagnostic for the build-time dependency used by
`i18n-embed-fl` 0.9.4 and `age` 0.11.5.

The upstream crate is unmaintained ([RUSTSEC-2026-0173](https://rustsec.org/advisories/RUSTSEC-2026-0173.html)).
The registry advisory no longer matches this local source, so keep that
maintenance status visible here.

Remove this patch when a published version of `age` drops this build-time
dependency and passes the dependency policy checks.
