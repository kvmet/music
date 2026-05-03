# Event-model refactor

Status: proposed. Not yet implemented.

## Why

Today, "trigger this voice" and "set this voice's pan on step 5" are entangled.
Param locks are stored on a `Step` and applied only when that step's trigger
fires. This makes parameter automation independent of triggers impossible and
forces every adjacent feature into a workaround:

- Per-voice swing needs a side-channel pending-fire queue.
- Microtiming locks would need a second side channel.
- "Super overdub" (write locks to all steps) is inert because the engine never
  reads locks on inactive steps.
- Per-voice "automation mode" toggles would be band-aids over the same issue.

Each of these is the same coupling: trig and param-at-tick are conflated. The
cleanest fix is to separate them at the data-model level.

## Model

```rust
pub struct Pattern {
    pub voices: [VoicePattern; VOICES],
    pub length_ticks: u64,  // loop length; default 16 * TICKS_PER_STEP
}

pub struct VoicePattern {
    pub trigs:  Vec<TrigEvent>,    // sorted by tick
    pub params: Vec<ParamEvent>,   // sorted by tick
}

pub struct TrigEvent  { pub tick: u64, pub velocity: f32 }
pub struct ParamEvent { pub tick: u64, pub edit: FieldEdit }
```

Two sorted lists per voice. Edits are O(log n) lookup + O(n) shift; n is small
(probably under 100 in practice). The audio thread reads with one cursor index
per list per voice.

`FieldEdit` already exists from the macro work. A `ParamEvent` is just a
`FieldEdit` plus a tick. No new value-type machinery.

### Sticky semantics

A `ParamEvent`'s value persists on the voice until a later `ParamEvent` for the
same field overrides it. There's no implicit revert-to-default between events.
Voice defaults apply only at playback start, pattern wrap, or for fields no
event has touched yet.

This is the central semantic shift from today. Under the current model, every
trig resets the voice to (defaults + step locks). Under the new model, voice
params drift forward, mutated only by explicit `ParamEvent`s.

### Pattern wrap

On wrap to tick 0: reset cursors and re-apply voice defaults. A pattern is
self-contained. Starting playback at tick 0 always produces the same initial
param state regardless of where you stopped.

### Seek

When the user clicks step 8 to jump there: re-apply voice defaults, then
fast-forward `params` events with `tick < seek_tick`, applying each. No trigs
fire. Voice param state at the seek point matches what it would have been
playing from 0.

## Engine

### Render walk

The current loop fires at step boundaries. Replace with an event walk:

1. Compute end-of-buffer tick.
2. Loop: find soonest pending event tick across all voices and event types.
3. If that tick is past end-of-buffer, render remaining samples and exit.
4. Otherwise: render samples up to that tick, process events at that tick
   (param events first, then trigs, so trigs see the latest params), advance
   the relevant cursors, continue.
5. If pattern wraps inside the buffer, reset cursors and re-apply defaults at
   the wrap tick, then continue.

This single loop subsumes:
- Step boundaries (events at multiples of `TICKS_PER_STEP`).
- Microtiming (trig events at non-boundary ticks).
- Swing (trig events at boundary + offset; either pre-baked or computed on
  scheduling).
- Param automation at any density.

No per-feature special cases.

### Voice param application

`event.edit.apply_to_params(&mut voice.params)` then re-derive any precomputed
state in `voice.apply_params`. The macro already generates this. No envelope
retrigger.

### Triggers

`voice.trigger(velocity)` at the trig's tick. Mute is checked here. There's no
"merge defaults + locks" step at trig time anymore; the voice's params are
already whatever the most recent param events left them at.

## Commands

Replace:
```
SetStep, SetStepLocks, ClearStepLocks
```

With:
```
SetTrig         { voice, tick, velocity }   // upsert at tick
ClearTrig       { voice, tick }
SetParamEvent   { voice, tick, edit }       // upsert at (tick, edit's field)
ClearParamEvent { voice, tick, field }
```

Bulk operations (clear everything at a step, clear voice's events) become
helpers built on these.

`LoadScene` ships a full `Pattern`. Shape change in `SceneData`, otherwise
mechanical.

## UI

The 16-step grid stays. It's a view layer.

### Reading

A virtual `StepLocks` for step N is "walk `params`, collect events with
`tick == N * TICKS_PER_STEP`, fold into a `StepLocks`." Cheap; called for the
editor panel. Trigger presence at step N: any `TrigEvent` at that tick.

### Writing

- Toggling a step's trig → `SetTrig` / `ClearTrig` at `N * TICKS_PER_STEP`.
- Editing a param while step held → `SetParamEvent` at the step's tick.
- Editing voice defaults → unchanged (`SetVoiceParams`).
- Overdub: while a voice is held during playback, slider edits emit
  `SetParamEvent` at the current playhead tick. No commit-at-step-boundary
  buffering. The `overdub_locks` buffer goes away.

### Marker / "currently audible value" display

Walk events from 0 to playhead, apply latest of each field over voice defaults.
Cache per frame. Roughly the same cost as today's `playing_marker_params`.

### "Lock present" badge on a step

A step has a badge if any `ParamEvent` exists at its tick. Computed at draw
time from the events list.

## Scenes

`SceneData.pattern: [[bool; STEPS]; VOICES]` and `SceneData.locks` collapse
into `SceneData.pattern: Pattern`. Voice swing stays as a parallel non-event
property for now. It can become events later if we want it lockable.

## Real-time safety

Audio thread reads `Pattern`; UI thread mutates via `Command` deltas. The
engine applies deltas in `apply()` on the audio thread, which means `Vec`
inserts run there. With small n (~100 events max per voice) this is fine. If
it ever isn't, swap to `Arc<Pattern>` snapshot-and-replace, with the UI thread
building the new snapshot.

## What this absorbs

- Microtiming: trig events at off-boundary ticks. No new mechanism.
- Per-voice swing: stays as today (a transport-side time deformation, or
  pre-bake into trig event ticks). The pending-fire queue I was about to add
  is unnecessary; the event walk handles it.
- "Super overdub" mode: gone. Overdub always emits at playhead tick; whether
  there's a trig nearby is irrelevant.
- The per-voice "automation mode" toggle I floated earlier: gone. There's only
  one mode.

## Phasing

Big-bang within a branch. The data model change is too pervasive to migrate
piecewise without double-bookkeeping. The macro work and `FieldEdit` are good
leverage; the engine and the UI editor are the bulk of the work.

Suggested order within the branch:

1. **Engine types and walk.** Define `Pattern`, `VoicePattern`, events. Rewrite
   `process()` around the event walk. Stub the new commands as no-ops. Hand-
   build a fixed pattern in code and verify a single voice triggers and updates
   params correctly.
2. **Command set.** Replace step-oriented commands with event commands. UI
   still writes the old way (compiles, doesn't work yet).
3. **UI write path.** Switch UI edits to emit event commands.
4. **UI read path.** Switch editor reads to compute virtual `StepLocks` from
   events. "Lock present" badges, marker display.
5. **Overdub.** Emit `SetParamEvent` at playhead tick; delete `overdub_locks`
   buffer and commit logic.
6. **Scenes.** Update `SceneData` shape and serialization.
7. **Cleanup.** Delete `Step`, `StepLocks` runtime usage (StepLocks may stay as
   a UI-side aggregation type), the dead "super overdub" / pending-fire ideas.
8. **Land follow-up features that were waiting on this:** microtiming locks,
   smooth pan/send automation, anything else gated on per-tick semantics.

Effort estimate: deliberately not giving one. The model change is small but it
touches enough that subtle bugs in the cursor/wrap logic could eat time.

## Decisions to make before starting

- **Velocity per trig.** Today every trig fires at 1.0. The new model makes
  per-trig velocity natural (`TrigEvent.velocity`). Worth wiring through to
  `Voice::trigger` now or later? Probably later; keep V1 to 1.0 to avoid
  scope creep.
- **Pattern length.** Per-voice or global? Today implicitly 16 steps for
  everything. New model trivially supports per-voice, but UI has no concept of
  it. Recommend: keep global for now, leave the door open in `Pattern`.
- **Atomic step clear.** Clearing "step 5" should remove all events at
  `tick == 5 * TICKS_PER_STEP`. Single command (`ClearStepEvents { voice, tick
  }`) or fanout of `ClearTrig` + per-field `ClearParamEvent`? Single, with the
  fanout internal to the engine.
- **Event ordering at the same tick.** When a `ParamEvent` and a `TrigEvent`
  share a tick, params apply first so the trig fires with the new params. This
  matches today's "merge locks then trigger" order. Document and enforce in
  the walk.
