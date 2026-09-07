# ADR-0002: Practice-speed preparation

- Status: Provisional, pending T15 listening tests
- Date: 2026-09-07
- Design source: v4 R08, §6.2, §6.5, §7.1, T15

## Decision

Prepare 0.70x to 1.30x backing PCM before a singing session. Playback callbacks only
read prepared PCM. The transport maps elapsed time to Song time using the selected
ratio, and scoring uses the same ratio when matching pitch frames.

The first implementation uses the existing Rust FFT path: duration resampling followed
by inverse pitch correction. It requires no additional redistributable native library.
It is an implementation baseline, not a claim of production audio quality.

## Consequences

- Backing pitch, target bars, scoring time, seek offset, and recorded master share one
  speed setting.
- Key and speed preparation is cached by song, semitone, and integer speed percent.
- Preparation runs outside the real-time callback.
- T15 transient/listening tests must compare this baseline with Rubber Band or another
  licensed high-quality implementation before G4 can be accepted.

