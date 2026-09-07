#!/usr/bin/env python3
"""
Test script to prototype and verify refined note segmentation algorithm
Compares v1 (raw) vs refined note segmentation
"""

import os
import json
import numpy as np
import soundfile as sf
import librosa
from scipy.ndimage import median_filter

def refine_segmentation(f0_records, vocal_path, f0_step_s=0.01):
    # 1. Extract f0 array from records
    raw_f0 = np.array([r[1] for r in f0_records]) # Hz
    n_frames = len(raw_f0)

    # 2. Smooth voiced segments with median filter (window=5 -> 50ms)
    smoothed_f0 = np.copy(raw_f0)
    voiced_mask = smoothed_f0 > 0

    # Fill small unvoiced gaps (< 4 frames = 40ms) within voiced segments
    gap_count = 0
    for i in range(1, n_frames - 1):
        if not voiced_mask[i]:
            gap_count += 1
        else:
            if 0 < gap_count <= 4 and i - gap_count - 1 >= 0 and voiced_mask[i - gap_count - 1]:
                # Interpolate gap
                p_start = smoothed_f0[i - gap_count - 1]
                p_end = smoothed_f0[i]
                if abs(12 * np.log2(p_end / p_start)) < 1.5:
                    for g in range(gap_count):
                        idx = i - gap_count + g
                        smoothed_f0[idx] = p_start + (p_end - p_start) * (g + 1) / (gap_count + 1)
                        voiced_mask[idx] = True
            gap_count = 0

    # Apply median filter on continuous voiced segments
    start = None
    for i in range(n_frames):
        if voiced_mask[i] and start is None:
            start = i
        elif not voiced_mask[i] and start is not None:
            if i - start >= 5:
                smoothed_f0[start:i] = median_filter(smoothed_f0[start:i], size=5)
            start = None
    if start is not None and n_frames - start >= 5:
        smoothed_f0[start:n_frames] = median_filter(smoothed_f0[start:n_frames], size=5)

    # Convert smoothed F0 to MIDI
    midi_arr = np.zeros(n_frames)
    for i in range(n_frames):
        if smoothed_f0[i] > 0:
            midi_arr[i] = 69.0 + 12.0 * np.log2(smoothed_f0[i] / 440.0)

    # 3. Detect Onsets from separated vocals for same-pitch syllable boundaries
    vocal_data, vocal_sr = sf.read(vocal_path)
    if len(vocal_data.shape) > 1:
        vocal_data = vocal_data.mean(axis=1)

    # Compute onset strength and detect peaks
    hop_length = int(round(f0_step_s * vocal_sr))
    onset_env = librosa.onset.onset_strength(y=vocal_data, sr=vocal_sr, hop_length=hop_length)
    onset_frames = set(librosa.onset.onset_detect(
        onset_envelope=onset_env,
        sr=vocal_sr,
        hop_length=hop_length,
        backtrack=False,
        units="frames"
    ))

    # 4. Segment notes
    raw_notes = []
    in_note = False
    cur_start = 0
    cur_pitches = []

    def flush_note(start_i, end_i, pitches):
        if not pitches:
            return
        dur_ms = (end_i - start_i) * f0_step_s * 1000.0
        if dur_ms < 60.0:
            return
        med_midi = float(np.median(pitches))
        pitch_int = int(round(med_midi))
        if pitch_int < 36 or pitch_int > 88:
            return
        raw_notes.append({
            "start_i": start_i,
            "end_i": end_i,
            "start_ms": round(start_i * f0_step_s * 1000.0, 1),
            "end_ms": round(end_i * f0_step_s * 1000.0, 1),
            "dur_ms": round(dur_ms, 1),
            "pitch_midi": pitch_int,
            "tuning_cents": round((med_midi - pitch_int) * 100.0, 1)
        })

    for i in range(n_frames):
        m = midi_arr[i]
        if m > 0:
            if not in_note:
                in_note = True
                cur_start = i
                cur_pitches = [m]
            else:
                should_split = False
                # A. Check Onset boundary (at least 100ms into note)
                if i in onset_frames and (i - cur_start) >= 10:
                    # Check if there is an energy dip/rise
                    if i < len(onset_env) and onset_env[i] > 1.2:
                        should_split = True

                # B. Check sustained pitch shift (>= 0.8 semitone sustained for >= 7 frames / 70ms)
                if not should_split and (i - cur_start) >= 8:
                    cur_med = np.median(cur_pitches[-6:])
                    if abs(m - cur_med) >= 0.8:
                        # Look ahead 7 frames to confirm sustained change (not a vibrato peak)
                        if i + 7 < n_frames:
                            future = midi_arr[i:i + 7]
                            if all(f > 0 and abs(f - m) < 0.9 for f in future):
                                should_split = True

                if should_split:
                    flush_note(cur_start, i, cur_pitches)
                    cur_start = i
                    cur_pitches = [m]
                else:
                    cur_pitches.append(m)
        else:
            if in_note:
                in_note = False
                flush_note(cur_start, i, cur_pitches)
                cur_pitches = []

    if in_note:
        flush_note(cur_start, n_frames, cur_pitches)

    # 5. Post-processing: Merge adjacent notes with same pitch & small gap
    merged_notes = []
    for n in raw_notes:
        if not merged_notes:
            merged_notes.append(n)
            continue
        prev = merged_notes[-1]
        gap_ms = n["start_ms"] - prev["end_ms"]
        # If gap < 60ms and same MIDI pitch -> merge
        if 0 <= gap_ms < 60.0 and prev["pitch_midi"] == n["pitch_midi"]:
            prev["end_ms"] = n["end_ms"]
            prev["dur_ms"] = round(prev["end_ms"] - prev["start_ms"], 1)
            prev["end_i"] = n["end_i"]
        # Or if the note is extremely short (< 80ms) and adjacent to another note of same or 1 semitone
        elif n["dur_ms"] < 80.0 and 0 <= gap_ms < 40.0 and abs(prev["pitch_midi"] - n["pitch_midi"]) <= 1:
            prev["end_ms"] = n["end_ms"]
            prev["dur_ms"] = round(prev["end_ms"] - prev["start_ms"], 1)
            prev["end_i"] = n["end_i"]
        else:
            # Filter out isolated short blips (< 90ms)
            if n["dur_ms"] >= 90.0:
                merged_notes.append(n)

    return merged_notes

if __name__ == "__main__":
    with open("tmpmusic/analysis_out/f0_contour.json", "r", encoding="utf-8") as f:
        f0_data = json.load(f)

    vocal_path = "tmpmusic/analysis_out/demucs/htdemucs/maou_14_shining_star/vocals.wav"
    notes = refine_segmentation(f0_data["frames"], vocal_path)

    print(f"Refined note count: {len(notes)}")
    # Inspect vocal intro section: 33s to 38s
    print("\n--- Vocal Intro (33s - 38s) Notes in Refined Version ---")
    intro_notes = [n for n in notes if 33000 <= n["start_ms"] <= 38000]
    for n in intro_notes:
        print(f"  [{n['start_ms']}ms -> {n['end_ms']}ms] dur: {n['dur_ms']}ms, pitch: {n['pitch_midi']}")
