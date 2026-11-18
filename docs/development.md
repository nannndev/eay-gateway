# Development

## Build and check

```sh
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

CI runs the same checks on macOS, Windows and Linux. Lints forbid `unsafe` and warn on
`unwrap`/`expect` outside tests (they are errors in CI).

Run a throwaway gateway that can't touch your real accounts:

```sh
cargo run --bin eay-gateway -- --port 20199 --data-dir "$(mktemp -d)"
```

## Tests

- **Unit tests** sit next to the code (`#[cfg(test)] mod tests`), grouped by function with
  sentence-style names, one behaviour per test.
- **HTTP tests** in `crates/eay-gateway/tests/http_api.rs` drive the router in-process with
  `tower::ServiceExt::oneshot`: no network, no real providers.
- Nothing calls a real provider. Use a local fake server for end-to-end checks.

## Releasing

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`).
2. Commit, then tag and push: `git tag v0.2.0 && git push --tags`.
3. The release workflow builds `eay-gateway` for `aarch64-apple-darwin`, `x86_64-apple-darwin`
   (plus a universal binary), `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`, and
   publishes them with `SHA256SUMS.txt`.

Apps that bundle the gateway should pin a release and check `GET /health` → `version` at runtime.

## Adding an API-key provider

OpenAI-compatible providers need one entry in `PROVIDERS` (`crates/eay-gateway-core/src/providers.rs`):

```rust
Provider {
    id: "example",
    alias: "ex",            // unique; becomes ex/<model>
    name: "Example AI",
    family: OPENAI,         // OPENAI, ANTHROPIC, "openai-responses" or "commandcode"
    base_url: "https://api.example.com/v1",
    needs_key: true,
    seed_models: &["example-large", "example-small"],
},
```

The connect check, model sync, routing and request path all work from that entry. A new wire
format needs a branch in `engine.rs` (request) and a stream kind in `translate.rs` (response).

## Adding a subscription provider

1. Add an `OAuthProviderMeta` to `OAUTH_PROVIDERS` (id, alias, flow, preferred models).
2. Login: a device flow can often reuse `start_device_flow` / `poll_device_flow` with a
   `device_cfg` entry. Browser logins need a `start_*` / `complete_*` pair and a callback port.
3. Refresh: a branch in `fresh_credential`.
4. Chat: a branch in `subscription_request` returning the request and its stream kind.
5. Optional: `subscription_models_request` (model list) and `quota.rs` (live quota).
6. Tests for any parsing or translation you add.
