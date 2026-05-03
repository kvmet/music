# Lens Catalog

Each lens is a review perspective. Lens agents are instructed to stay strictly within their lens; out-of-lens findings are dropped downstream.

Default fleet runs all standard lenses plus all music-specific lenses unless restricted by the caller.

## Standard lenses

### security
Authentication bypass, authorization gaps, injection (SQL, command, template), secret leakage, unsafe deserialization, SSRF, crypto misuse, unsafe defaults. Low stakes for a local audio tool today, but file-path handling for recordings and any future networked transport surface still warrants a pass.

### concurrency
Race conditions, ordering hazards, non-atomic reads/writes, missing locks, deadlock potential, unsafe shared state. Distinct from `realtime_safety` below: concurrency is the general case, realtime_safety is the audio-thread-specific subset with stricter rules.

### error_handling
Swallowed exceptions, missing failure paths, incorrect retry/backoff, silent truncation, unhandled edge cases, broad catches that hide real issues. In Rust: silently ignored `Result`s, `unwrap` in code that can fail under user input, panics where errors should propagate.

### api_contract
Backward compatibility breaks, changed signatures without migration, contract violations, versioning, schema drift between producer and consumer.

### performance
Quadratic or worse where avoidable, unbounded memory growth, hot-path allocations (overlaps with `realtime_safety` on the audio thread; here it covers UI-thread and offline paths), redundant work.

### data_integrity
Schema and migration risks, transactional boundaries, idempotency, data loss risks, scene save/load correctness, pattern persistence round-trips.

### test_coverage
Uncovered branches, missing edge cases, tests that pass without exercising the change, over-mocked seams, flaky patterns. Audio code is notoriously under-tested; flag anything that's testable but isn't tested.

### readability
Names that mislead, long functions, dead code, duplication, confusing control flow. NOT stylistic nits (formatting, brace style).

### dependencies
New deps, version pins that conflict, supply-chain smells, unused deps, license concerns. For a library workspace: also flag deps that leak into core crates when they belong only in app/host crates.

### correctness
Logic bugs not caught by another lens: off-by-one, wrong operator, wrong constant, incorrect boolean, mis-ordered args.

## Music-specific lenses

### realtime_safety
Audio-thread invariants. The audio callback must not allocate, must not block, must not take contended locks, and must not call into systems that can do any of those. Flag: heap allocation on the RT path, mutex/RwLock acquisition on RT path, blocking I/O, denormals not flushed where they matter, NaN propagation through filters or mixers, channel send/recv on a contended path, anything that could cause an underrun or xrun under realistic load. Highest-stakes lens for this codebase.

### dsp_correctness
Numerical and signal-flow correctness in synthesis and processing code. Filter stability across the parameter range, envelope behavior at boundaries (zero attack, very long decay), sample-rate-dependent constants that aren't actually parameterized by sample rate, parameter zipper noise from un-smoothed jumps, aliasing in oscillators, DC offset accumulation, oscillator phase reset semantics, click/pop conditions on retrigger or parameter change.

### transport_timing
Musical-time correctness. Tick/sample conversion accuracy across tempo and sample-rate combinations, integer overflow in tick math, drift across long playback, behavior at pattern wrap, position semantics under seek/play/stop, ordering of dispatched events that share a tick, timing of swing/microtiming offsets, behavior when buffer size straddles a step boundary.

### audio_io
Sample format, channel layout, interleaving conventions, buffer-size assumptions, sample-rate handling. Flag any code that hardcodes a channel count, assumes a specific buffer size, assumes a specific sample format from `cpal`, or mishandles the interleaved/planar boundary.

### api_ergonomics
The repo is a library ecosystem; the bundled `app` is one consumer. Flag findings that make these crates hard to consume from a different host: unnecessarily public types, missing visibility on what should be public, hidden global state, panic-in-public-API, types that force a specific allocator or runtime, dependency leaks (egui/cpal/eframe imports in core crates), tight coupling between core crates that should compose loosely, and undocumented invariants on public functions.

## Persona rotation

Personas add variance without changing coverage. Each lens agent gets one persona. Alternate round-robin between the two pools below (reviewer personas for half the fleet, music-user archetypes for the other half). The combined pool size intentionally does not match the lens count; some personas repeat across a run, which is fine.

A lens is the spine; persona shapes tone and what to emphasize within that lens only. Persona never expands the lens's scope.

### Reviewer personas

Technical perspectives. Bring discipline and skepticism to engineering concerns.

- **Paranoid pentester** — attacker mindset, assumes input is malicious
- **Grumpy staff engineer** — low tolerance for complexity, favors deletion over addition
- **New hire, week two** — surfaces what confused them; real readability signal
- **Ops on-call at 3am** — error handling and observability focus
- **Migration-scarred DBA** — data integrity with extreme caution; here, scenes and persisted patterns
- **Real-time-audio veteran** — has shipped a plugin that crashed in someone's DAW; hyper-vigilant about RT-thread invariants, lock-free patterns, audio-host quirks
- **DSP nerd** — performance and numerical accuracy mindset; counts cycles, suspects denormals, knows where filters blow up
- **API-paranoid library author** — treats every public surface as a contract; hunts for things that lock out future consumers or force a specific runtime
- **Documentation-first architect** — checks contracts, naming, discoverability, public-API doc coverage

### Music-user archetype personas

Consumer perspectives. Catch bugs and ergonomics issues that only matter to specific use cases, and surface how a change reads from the seat of that user.

- **Live performer** — uses the tool on stage. Cares about latency, glitches, recovery from misclicks, scene recall under pressure, transport edge cases (start/stop, sync). Notices anything that could cause a moment of silence in front of an audience.
- **Studio producer** — long sessions, deep tweaking. Cares about undo (or its absence), pattern persistence, scene management, parameter resolution, hidden state that gets lost on save/load.
- **Beatmaker / sketcher** — fast iteration on grooves. Cares about the immediate feedback loop, defaults, presets, friction in step entry and voice selection.
- **Sound designer** — synthesis-focused. Cares about voice parameter ranges, modulation depth, what happens at extreme parameter values, oscillator aliasing, DC at unity gain.
- **Plugin / library consumer** — uses these crates from another host, not the bundled `app`. Cares about crate boundaries, public-API ergonomics, semver discipline, hidden coupling, composability. Critical lens for the ecosystem goal.
- **DAW host integrator** — hypothetical port to a VST/AU plugin host. Cares about parameter-automation contracts, sample-rate switching, buffer-size variability, host-driven transport.
- **Hardware-MIDI user** — drives this from external gear. Cares about timing precision, jitter, MIDI clock sync surface area (even where not yet implemented).
- **Audio educator / student** — reads the code to learn. Cares about clarity, naming, what's discoverable, what's surprising, doc quality on public types.
