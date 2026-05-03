# Transport crate

The timing backbone for the workspace. Owns musical time, tempo, transport state (Playing/Stopped), and dispatches events to subscribers as time advances.

## Time representation

All musical time is `ticks: u64`. Sample position is derived from ticks given tempo and sample rate; it is never stored as the primary representation.

- **Tick resolution**: 960 PPQ by default. Chosen for divisibility by 2, 3, 4, and 5: this covers triplets, quintuplets, and 32nd notes without fractional ticks.
- **Tempo**: `millibpm: u32` (e.g. 120 BPM is `120_000`). Integer only. No floats anywhere in the math.
- **Tick-to-sample conversion**: uses `u128` intermediates to avoid overflow at long positions, with nearest rounding.
- **Position decomposition**: `Position` holds only `ticks: u64`. Beat, measure, tick-in-beat, and similar values are computed on demand from a `TransportConfig`.

## Event dispatch

`Transport` holds a `Vec<Sender<TransportEvent>>` using crossbeam bounded channels (capacity 256).

- `subscribe()` returns a `Receiver<TransportEvent>`.
- Dead receivers are pruned at emit time.
- Full channels are retained: a slow subscriber is not a dead one. The send for that channel is dropped, the subscription stays.
- The audio thread calls `advance_samples(n)` each buffer. It only advances when `Playing`. Scrubbing uses `advance_ticks(n)` instead.

## Conventions enforced here

- No floats anywhere. Integer math throughout. Nearest rounding where non-integer results are unavoidable.
- `millibpm` is the tempo unit everywhere. Never BPM, never micros-per-beat.
- 960 PPQ is the default. Other resolutions are settable via `TransportConfig` but the default is the assumed shape.
