// Built only if something enables burn's `tch` backend, which the workspace never does (Cargo.toml's
// `[patch.crates-io]`): fail that build plainly instead of with missing items.
compile_error!("burn-tch is stubbed out in this workspace (vendor/burn-tch): the LibTorch backend is not available");
