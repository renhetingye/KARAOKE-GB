import json
import numpy as np

# Load generated draft chart
with open('tests/fixtures/shining_star_full_draft.json', 'r', encoding='utf-8') as f:
    chart = json.load(f)

print('=== CURRENT GENERATED NOTES in 28.0s - 38.0s ===')
for n in chart['notes']:
    s = n['startUs'] / 1_000_000
    e = n['endUs'] / 1_000_000
    if 28.0 <= s <= 38.0 or 28.0 <= e <= 38.0:
        dur = (e - s) * 1000
        cents = n.get('tuningCents', 0)
        print(f"{n['id']}: {s:.3f}s -> {e:.3f}s ({dur:.0f}ms) | MIDI {n['pitchMidi']} ({cents:+.1f}c)")

with open('tmpmusic/analysis_out/f0_contour.json', 'r', encoding='utf-8') as f:
    f0_data = json.load(f)

print('\n=== RAW F0 CONTOUR SEGMENTS in 28.0s - 38.0s ===')
f0_frames = [fr for fr in f0_data['frames'] if 28000 <= fr[0] <= 38000]
in_v = False
v_s = 0
pitches = []
for t_ms, hz, midi in f0_frames:
    if hz > 0:
        if not in_v:
            in_v = True
            v_s = t_ms
            pitches = [midi]
        else:
            pitches.append(midi)
    else:
        if in_v:
            in_v = False
            dur = t_ms - v_s
            med = np.median(pitches)
            p_min = np.min(pitches)
            p_max = np.max(pitches)
            print(f"Voiced segment: {v_s/1000:.3f}s -> {t_ms/1000:.3f}s ({dur:.0f}ms) | med: {med:.1f} (range: {p_min:.1f} - {p_max:.1f})")

if in_v:
    dur = f0_frames[-1][0] - v_s
    med = np.median(pitches)
    p_min = np.min(pitches)
    p_max = np.max(pitches)
    print(f"Voiced segment: {v_s/1000:.3f}s -> {f0_frames[-1][0]/1000:.3f}s ({dur:.0f}ms) | med: {med:.1f} (range: {p_min:.1f} - {p_max:.1f})")
