# AGENTS.md

Rust workspace of music tooling crates. The bundled `app` is one example consumer; the goal is a broader ecosystem of reusable crates.

## Quick Reference

```bash
cargo build                      # build all crates
cargo test                       # all tests
cargo test -p <crate>            # one crate
cargo clippy                     # lint
cargo fmt                        # format
```

## Crates

- `transport`: tick/sample math, tempo, transport state.
- `synth`: drum voice DSP, params macro (`define_params!`), FX.
- `engine`: audio-thread state. Owns voices, pattern, scenes. RT-safe.
- `record`: interleaved-stereo WAV recorder on a background thread.
- `app`: egui sequencer UI. Reference consumer; no other crate depends on it.

Dependency direction: `app -> engine -> synth -> transport`. Core crates (`transport`, `synth`, `engine`, `record`) do not depend on `egui`, `eframe`, or `cpal`. Those belong to `app` or future host crates only.

## Key Rules

- **Audio thread**: no allocation, no blocking, no contended locks in `Engine::process` or anything reachable from it. Pre-allocate; grow lazily off the hot path.
- **Timing math**: integer only. Time is `ticks: u64`; tempo is `millibpm: u32`; conversions use `u128` intermediates.
- **Don't edit `Cargo.toml` / `Cargo.lock`**. Notify the user with the command.
- **`cargo fmt`, `cargo clippy`, `cargo build`, `cargo test`** after editing and before declaring done. UI changes can't be auto-tested; say so. Fallback: if `cargo fmt` would produce large unrelated diffs, match the file's existing style and defer the broader format pass.
- **After editing `.agents/`, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`** run `python3 .agents/tools/clean_rot.py`.

## Skills

Vendor-agnostic at `.agents/skills/`; vendor dirs symlink there. Notable: `fleet-review` (multi-agent review with music-specific lenses).

## Docs (in `docs/`)

- [event-model.md](docs/event-model.md): proposed tick-keyed event model. Update when changing `crates/engine/src/lib.rs` or `crates/synth/src/lib.rs`.
- [transport.md](docs/transport.md): timing math, tempo, event dispatch in the `transport` crate. Update when changing `crates/transport/src/lib.rs`.
