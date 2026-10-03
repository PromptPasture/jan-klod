# guest-fs

shared pure-Rust helpers for host-fs-routed guests: glob matching, a bounded tree walk, and output capping. Not a component — a compile-time library.

It is a workspace member of `pkgs/extensions` and is compiled into the guests that depend on it (`tool-fs`, `tool-find`, `tool-edit`, ...); it builds no `.wasm` of its own.
