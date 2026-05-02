# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                          # build all crates
cargo test                           # test all crates
cargo test -p transport              # test a specific crate
cargo test <test_name>               # run a single test by name
cargo clippy                         # lint
```

## Architecture

Cargo workspace at the root. Each concern is its own crate under `crates/`.

### `crates/transport`

The timing backbone. All musical time is stored as `ticks: u64` (960 ticks per beat by default). Sample position is derived from ticks given tempo and sample rate — never stored as primary.

**Time representation:**
- Tempo is stored as `millibpm: u32` (e.g. 120 BPM = 120_000). Integer-only, no floats anywhere in the math.
- Tick/sample conversion uses `u128` intermediates to avoid overflow, with nearest rounding.
- `Position` holds only `ticks: u64`. All musical decomposition (beat, measure, tick_in_beat, etc.) is computed on demand from a `TransportConfig`.

**Event dispatch:**
- `Transport` holds a `Vec<Sender<TransportEvent>>` (crossbeam bounded channels, capacity 256).
- `subscribe()` returns a `Receiver<TransportEvent>`. Dead receivers are pruned on emit. Full channels are kept (subscriber is slow, not dead).
- Audio thread calls `advance_samples(n)` each buffer — only advances when `Playing`. Scrubbing uses `advance_ticks(n)`.

## Conventions

- No floats. Integer math throughout. Nearest rounding where non-integer results are unavoidable.
- `millibpm` is the tempo unit everywhere (not BPM, not micros-per-beat).
- 960 PPQ is the default tick resolution — chosen for divisibility by 2, 3, 4, 5 (covers triplets, quintuplets, 32nd notes without fractions).
