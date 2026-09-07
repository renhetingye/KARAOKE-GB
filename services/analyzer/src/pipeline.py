#!/usr/bin/env python3
"""
Full-song Vocal Separation & RMVPE Pitch Extraction Pipeline (§13)
Generates:
1. Chart 2.0.0 full-song draft notes (physical time preserved)
2. F0 contour data (10ms steps) for editor visualization
3. Waveform amplitude peaks for editor visualization
"""

import os
import sys
import json
import subprocess
import numpy as np
import soundfile as sf
import librosa
import torch

# Ensure local imports work
sys.path.append(os.path.dirname(os.path.abspath(__file__)))
import rmvpe
from game_onnx import GameOnnxTranscriber, model_is_available


def atomic_json_write(path, value):
    """Commit a complete JSON artifact without exposing a partial file."""
    temp_path = f"{path}.tmp-{os.getpid()}"
    with open(temp_path, "w", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temp_path, path)


def estimate_global_tempo(audio, sample_rate, fallback_bpm=120.0):
    """Estimate one representative quarter-note BPM for an imported song.

    Tempo extraction is intentionally independent from note segmentation: a bad
    or silent vocal stem must not prevent the original accompaniment from
    supplying a useful editor grid.  The result remains an AI draft because
    half/double-tempo ambiguity and the first downbeat require user review.
    """
    mono = np.asarray(audio, dtype=np.float32).reshape(-1)
    if mono.size < max(1, int(sample_rate)) or not np.any(np.isfinite(mono)):
        return {
            "bpm": float(fallback_bpm),
            "confidence": 0.0,
            "beatCount": 0,
            "usedFallback": True,
            "trackerBpm": None,
        }

    mono = np.nan_to_num(mono, copy=False)
    peak = float(np.max(np.abs(mono)))
    if peak < 1.0e-5:
        return {
            "bpm": float(fallback_bpm),
            "confidence": 0.0,
            "beatCount": 0,
            "usedFallback": True,
            "trackerBpm": None,
        }

    # Keep the source rate. Downsampling can change the winning periodicity on
    # dense arrangements and make the estimator select a different beat family.
    analysis_sr = int(sample_rate)
    hop_length = 512
    tempo, beat_frames = librosa.beat.beat_track(
        y=mono,
        sr=analysis_sr,
        hop_length=hop_length,
        units="frames",
    )
    tempo_values = np.asarray(tempo, dtype=np.float64).reshape(-1)
    tracker_bpm = float(tempo_values[0]) if tempo_values.size else 0.0
    beat_frames = np.asarray(beat_frames, dtype=np.int64).reshape(-1)

    if (
        not np.isfinite(tracker_bpm)
        or tracker_bpm < 20.0
        or tracker_bpm > 400.0
        or beat_frames.size < 2
    ):
        return {
            "bpm": float(fallback_bpm),
            "confidence": 0.0,
            "beatCount": int(beat_frames.size),
            "usedFallback": True,
            "trackerBpm": None,
        }

    # `beat_track` selects the correct tempo family, but its reported value is
    # quantized to one frame interval. Refit within that family from all stable
    # tracked intervals so the editor grid does not accumulate avoidable drift.
    intervals = np.diff(beat_frames).astype(np.float64)
    median_interval = float(np.median(intervals))
    stable_intervals = intervals[
        (intervals >= median_interval * 0.80)
        & (intervals <= median_interval * 1.20)
    ]
    mean_interval = float(np.mean(stable_intervals))
    bpm = 60.0 * analysis_sr / (hop_length * mean_interval)
    if not np.isfinite(bpm) or bpm < 20.0 or bpm > 400.0:
        bpm = tracker_bpm

    # Confidence expresses beat-spacing stability, not musical correctness.
    # In particular it cannot resolve whether a listener intends half/double
    # tempo, so the editor must continue to allow manual correction.
    relative_mad = (
        float(np.median(np.abs(intervals - median_interval))) / median_interval
        if median_interval > 0.0
        else 1.0
    )
    stability = max(0.0, min(1.0, 1.0 - relative_mad / 0.20))
    support = max(0.0, min(1.0, beat_frames.size / 32.0))

    return {
        "bpm": round(bpm, 3),
        "confidence": round(stability * support, 3),
        "beatCount": int(beat_frames.size),
        "usedFallback": False,
        "trackerBpm": round(tracker_bpm, 3),
    }

def extract_full_song_pipeline(
    input_audio_path="tmpmusic/maou_14_shining_star.ogg",
    output_dir="tmpmusic/analysis_out",
    chart_output_path="tests/fixtures/shining_star_full_draft.json",
    base_chart_path=None,
    model_path="services/analyzer/models/rmvpe.pt",
    force=False
):
    os.makedirs(output_dir, exist_ok=True)
    os.makedirs(os.path.dirname(chart_output_path), exist_ok=True)

    print("PROGRESS: 10% [Demucs] ボーカル音源の分離を確認中...", flush=True)
    print(f"[*] Step 1: Checking Demucs vocal separation for {input_audio_path}...", flush=True)
    demucs_base = os.path.join(output_dir, "demucs")
    track_name = os.path.splitext(os.path.basename(input_audio_path))[0]
    separated_vocal_path = os.path.join(demucs_base, "htdemucs", track_name, "vocals.wav")

    if force and os.path.exists(separated_vocal_path):
        os.remove(separated_vocal_path)

    if not os.path.exists(separated_vocal_path):
        print("PROGRESS: 20% [Demucs] AIボーカル分離を実行中 (Demucs HTDemucs)...", flush=True)
        print(f"[*] Running Demucs separation on full song (this takes ~1-2 min on CPU)...", flush=True)
        cmd = [
            sys.executable, "-m", "demucs",
            "--two-stems", "vocals",
            "-d", "cpu",
            "-o", demucs_base,
            input_audio_path
        ]
        res = subprocess.run(cmd, check=True)
        print("[*] Demucs separation completed!", flush=True)
    else:
        print(f"[*] Demucs vocals already exist at {separated_vocal_path}", flush=True)

    # Step 2: Load separated vocals and original audio
    print("[*] Step 2: Loading audio files...")
    vocal_data, vocal_sr = sf.read(separated_vocal_path)
    if len(vocal_data.shape) > 1:
        vocal_data = vocal_data.mean(axis=1) # Mono

    orig_data, orig_sr = sf.read(input_audio_path)
    if len(orig_data.shape) > 1:
        orig_mono = orig_data.mean(axis=1)
    else:
        orig_mono = orig_data

    total_duration_s = len(orig_mono) / orig_sr
    print(f"[*] Song duration: {total_duration_s:.2f}s ({len(orig_mono)} frames at {orig_sr}Hz)")

    print("[*] Estimating global tempo from the original audio...", flush=True)
    tempo_estimate = estimate_global_tempo(orig_mono, orig_sr)
    tempo_bpm = tempo_estimate["bpm"]
    tempo_source = "fallback" if tempo_estimate["usedFallback"] else "librosa.beat"
    print(
        f"[*] Estimated tempo: {tempo_bpm:.3f} BPM "
        f"(source={tempo_source}, confidence={tempo_estimate['confidence']:.3f}, "
        f"beats={tempo_estimate['beatCount']})",
        flush=True,
    )

    # Step 3: Compute waveform peaks for editor (10ms resolution)
    print("PROGRESS: 30% [Waveform] 音声波形ピークを生成中...", flush=True)
    print("[*] Step 3: Computing waveform overview for editor...", flush=True)
    frame_step_ms = 10
    samples_per_step = int(orig_sr * (frame_step_ms / 1000.0))
    n_waveform_steps = int(np.ceil(len(orig_mono) / samples_per_step))
    waveform_peaks = []
    for step in range(n_waveform_steps):
        s_start = step * samples_per_step
        s_end = min(len(orig_mono), s_start + samples_per_step)
        if s_start < len(orig_mono):
            chunk = orig_mono[s_start:s_end]
            max_val = float(np.max(np.abs(chunk))) if len(chunk) > 0 else 0.0
            waveform_peaks.append(round(max_val, 4))

    waveform_json_path = os.path.join(output_dir, "waveform.json")
    atomic_json_write(waveform_json_path, {
            "stepMs": frame_step_ms,
            "durationS": total_duration_s,
            "peaks": waveform_peaks
        })
    print(f"[*] Saved waveform peaks ({len(waveform_peaks)} points) to {waveform_json_path}", flush=True)

    # Step 4: Run RMVPE pitch estimation on isolated vocals (or load cached)
    print("PROGRESS: 50% [RMVPE] RMVPE高精度ピッチ軌跡を抽出中...", flush=True)
    f0_json_path = os.path.join(output_dir, "f0_contour.json")
    f0_step_s = 160.0 / 16000.0 # 10ms
    if force and os.path.exists(f0_json_path):
        os.remove(f0_json_path)

    if os.path.exists(f0_json_path):
        print(f"[*] Loading cached F0 contour from {f0_json_path}...", flush=True)
        with open(f0_json_path, "r", encoding="utf-8") as f:
            f0_cached = json.load(f)
            f0_records = f0_cached["frames"]
        print(f"[*] Loaded {len(f0_records)} cached F0 frames", flush=True)
    else:
        print("[*] Step 4: Running RMVPE pitch estimation on isolated vocals...", flush=True)
        audio_16k = librosa.resample(vocal_data, orig_sr=vocal_sr, target_sr=16000)
        rmvpe_inst = rmvpe.RMVPE(model_path, is_half=False, onnx=False, device="cpu")

        # thred=0.03 is standard RMVPE threshold
        f0 = rmvpe_inst.infer_from_audio(audio_16k, thred=0.03)
        print(f"[*] RMVPE inference finished: {len(f0)} frames ({len(f0) * f0_step_s:.2f}s)", flush=True)

        # Save raw F0 contour for editor
        f0_records = []
        for i, pitch in enumerate(f0):
            t_ms = round(i * f0_step_s * 1000.0, 1)
            hz = round(float(pitch), 2)
            if hz > 0:
                midi = round(69 + 12 * np.log2(hz / 440.0), 2)
            else:
                midi = 0.0
            f0_records.append([t_ms, hz, midi])

        atomic_json_write(f0_json_path, {
                "stepMs": round(f0_step_s * 1000.0, 1),
                "frames": f0_records
            })
        print(f"[*] Saved F0 contour ({len(f0_records)} frames) to {f0_json_path}", flush=True)

    # Step 5: Transcribe discrete notes. RMVPE is an F0 estimator, not a note
    # boundary model. Prefer GAME when its offline ONNX model is installed and
    # keep the old segmenter only as an explicit compatibility fallback.
    print("PROGRESS: 75% [Notes] F0平滑化・オンセット検出・ノート分割中...", flush=True)
    print("[*] Step 5: Transcribing singing into discrete notes...", flush=True)
    from scipy.ndimage import median_filter

    raw_f0 = np.array([r[1] for r in f0_records]) # Hz
    n_frames = len(raw_f0)

    # 5.1 Fill micro unvoiced gaps (< 40ms) and apply median filter (window=5 -> 50ms) to stabilize vibrato
    smoothed_f0 = np.copy(raw_f0)
    voiced_mask = smoothed_f0 > 0

    gap_count = 0
    for i in range(1, n_frames - 1):
        if not voiced_mask[i]:
            gap_count += 1
        else:
            if 0 < gap_count <= 4 and i - gap_count - 1 >= 0 and voiced_mask[i - gap_count - 1]:
                p_start = smoothed_f0[i - gap_count - 1]
                p_end = smoothed_f0[i]
                if abs(12 * np.log2(p_end / p_start)) < 1.5:
                    for g in range(gap_count):
                        idx = i - gap_count + g
                        smoothed_f0[idx] = p_start + (p_end - p_start) * (g + 1) / (gap_count + 1)
                        voiced_mask[idx] = True
            gap_count = 0

    start_idx = None
    for i in range(n_frames):
        if voiced_mask[i] and start_idx is None:
            start_idx = i
        elif not voiced_mask[i] and start_idx is not None:
            if i - start_idx >= 5:
                smoothed_f0[start_idx:i] = median_filter(smoothed_f0[start_idx:i], size=5)
            start_idx = None
    if start_idx is not None and n_frames - start_idx >= 5:
        smoothed_f0[start_idx:n_frames] = median_filter(smoothed_f0[start_idx:n_frames], size=5)

    midi_arr = np.zeros(n_frames)
    for i in range(n_frames):
        if smoothed_f0[i] > 0:
            midi_arr[i] = 69.0 + 12.0 * np.log2(smoothed_f0[i] / 440.0)

    # 5.2 Detect Onsets from isolated vocals for same-pitch note separation
    hop_length = int(round(f0_step_s * vocal_sr))
    onset_env = librosa.onset.onset_strength(y=vocal_data, sr=vocal_sr, hop_length=hop_length)
    onset_frames = set(librosa.onset.onset_detect(
        onset_envelope=onset_env,
        sr=vocal_sr,
        hop_length=hop_length,
        backtrack=False,
        units="frames"
    ))

    # 5.3 Segment continuous pitch into raw notes
    raw_notes = []
    in_note = False
    cur_start = 0
    cur_pitches = []

    def flush_note(start_i, end_i, pitches):
        if not pitches:
            return
        dur_ms = (end_i - start_i) * f0_step_s * 1000.0
        # Do not discard short notes here. Short candidates are required for
        # later human review (§13.3); duration alone is not evidence of noise.
        med_midi = float(np.median(pitches))
        pitch_int = int(round(med_midi))
        if pitch_int < 36 or pitch_int > 88:
            return
        raw_notes.append({
            "start_i": start_i,
            "end_i": end_i,
            "start_us": int(round(start_i * f0_step_s * 1_000_000)),
            "end_us": int(round(end_i * f0_step_s * 1_000_000)),
            "dur_ms": dur_ms,
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
                # A. Check Onset boundary (at least 100ms into current note)
                if i in onset_frames and (i - cur_start) >= 10:
                    if i < len(onset_env) and onset_env[i] > 1.2:
                        should_split = True

                # B. Check sustained pitch shift (>= 0.8 semitones sustained for >= 70ms)
                if not should_split and (i - cur_start) >= 8:
                    cur_med = np.median(cur_pitches[-6:])
                    if abs(m - cur_med) >= 0.8:
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

    # 5.4 Preserve every detected candidate. Same-pitch gaps may be repeated
    # syllables, and short candidates may be intentional notes. Confidence and
    # adoption decisions belong to the editor, not a destructive cleanup pass.
    notes = []
    analyzer_name = "Demucs-HTDemucs + RMVPE legacy segmentation"
    game_model_dir = os.path.abspath(os.path.join(
        os.path.dirname(__file__), "..", "models", "game-1.0.3-small-onnx"
    ))
    if model_is_available(game_model_dir):
        print("[*] GAME ONNX model found; replacing heuristic RMVPE note splitting.", flush=True)

        def game_progress(done, total):
            percent = 75 + int(14 * done / total)
            print(
                f"PROGRESS: {percent}% [GAME] Singing-note transcription {done}/{total}...",
                flush=True,
            )

        game_notes = GameOnnxTranscriber(game_model_dir).transcribe(
            vocal_data,
            vocal_sr,
            language="ja",
            progress=game_progress,
        )
        for note_counter, note in enumerate(game_notes, start=1):
            pitch_int = int(round(note.pitch_midi))
            if pitch_int < 36 or pitch_int > 88:
                continue
            start_us = int(round(note.start_s * 1_000_000))
            end_us = int(round(note.end_s * 1_000_000))
            if end_us - start_us < 50_000:
                continue
            notes.append({
                "id": f"candidate-{note_counter}",
                "trackId": "lead",
                "startUs": start_us,
                "endUs": end_us,
                "pitchMidi": pitch_int,
                "tuningCents": round((note.pitch_midi - pitch_int) * 100.0, 1),
                "scorable": True,
            })
        analyzer_name = "Demucs-HTDemucs + GAME 1.0.3 small ONNX + RMVPE contour"
    else:
        print(
            "[!] GAME ONNX model/onnxruntime unavailable; using legacy F0 segmentation.",
            flush=True,
        )
        for note_counter, n in enumerate(raw_notes, start=1):
            notes.append({
                "id": f"candidate-{note_counter}",
                "trackId": "lead",
                "startUs": n["start_us"],
                "endUs": n["end_us"],
                "pitchMidi": n["pitch_midi"],
                "tuningCents": n["tuning_cents"],
                "scorable": True
            })

    print(f"[*] Total notes segmented: {len(notes)} notes across full song (refined, vibrato-stabilized)")

    # Step 6: Assemble full Chart 2.0.0 JSON. Imported songs already have a
    # validated empty chart containing exact media metadata and user-entered
    # title/artist; preserve it and replace only the generated score content.
    if base_chart_path:
        with open(base_chart_path, "r", encoding="utf-8") as stream:
            chart = json.load(stream)
        duration_us = int(chart["durationUs"])
        bounded_notes = []
        for note in notes:
            note = dict(note)
            note["endUs"] = min(int(note["endUs"]), duration_us)
            if int(note["startUs"]) < note["endUs"]:
                bounded_notes.append(note)
        chart["chartId"] = f"{chart['songId']}_ai_draft"
        chart["notes"] = bounded_notes
        chart["lyricTokens"] = []
        chart["phrases"] = []
        previous_tempo_map = chart.get("tempoMap", {})
        chart["tempoMap"] = {
            "anchorTimeUs": 0,
            "anchorQuarterBeat": 0.0,
            "events": [{"timeUs": 0, "bpm": tempo_bpm}],
            "meters": previous_tempo_map.get("meters", [
                {
                    "quarterBeat": 0.0,
                    "numerator": 4,
                    "denominator": 4,
                }
            ]),
        }
        chart["extensions"] = {
            **chart.get("extensions", {}),
            "status": "ai_draft",
            "model": analyzer_name,
            "note": "Full-song draft generated from isolated vocals. Subject to editor review.",
            "tempoEstimate": {
                **tempo_estimate,
                "algorithm": "librosa.beat.beat_track+robust-interval-fit",
                "scope": "global",
                "phaseEstimated": False,
                "reviewRequired": True,
            },
        }
    else:
        chart = {
        "schemaVersion": "2.0.0",
        "requiredFeatures": ["single-melody", "step-tempo"],
        "songId": "shining_star",
        "chartId": "shining_star_rmvpe_draft",
        "chartRevision": 1,
        "title": "シャイニングスター",
        "artist": "魔王魂",
        "durationUs": int(round(total_duration_s * 1_000_000)),
        "credits": [
            {
                "text": "Music by 魔王魂 (MaouDamashii). Official BPM 158. AI Draft extracted via Demucs & RMVPE.",
                "sourceUrl": "https://maou.audio/14_shining_star/",
                "licenseName": "MaouDamashii Free License"
            }
        ],
        "media": [
            {
                "id": "backing",
                "path": "media/backing.wav",
                "role": "backing",
                "sha256": "f2718730817d75730c3e4c1e68b385581b1527f60f4cb814018df76ed2ce7495",
                "sampleRate": 48000,
                "channels": 2,
                "frames": 13283752,
                "mediaOffsetUs": 0
            }
        ],
        "tracks": [
            {
                "id": "lead",
                "role": "lead"
            }
        ],
        "tempoMap": {
            "anchorTimeUs": 0,
            "anchorQuarterBeat": 0.0,
            "events": [
                {
                    "timeUs": 0,
                    "bpm": tempo_bpm
                }
            ],
            "meters": [
                {
                    "quarterBeat": 0.0,
                    "numerator": 4,
                    "denominator": 4
                }
            ]
        },
        "notes": notes,
        "lyricTokens": [],
        "phrases": [],
        "extensions": {
            "status": "ai_draft",
            "model": analyzer_name,
            "note": "Full-song draft generated from isolated vocals. Physical timestamps preserved. Subject to visual editor correction.",
            "tempoEstimate": {
                **tempo_estimate,
                "algorithm": "librosa.beat.beat_track+robust-interval-fit",
                "scope": "global",
                "phaseEstimated": False,
                "reviewRequired": True,
            },
        }
        }

    print("PROGRESS: 90% [Chart] 譜面JSONファイルを出力中...", flush=True)
    atomic_json_write(chart_output_path, chart)
    print(f"[*] Successfully saved full-song chart to {chart_output_path} with {len(notes)} notes!", flush=True)

    print("[*] Candidate saved separately. The active chart was not overwritten.", flush=True)
    print(f"PROGRESS: 100% [Complete] 全曲AI解析候補が完了しました (全{len(notes)}ノーツ・未採用)", flush=True)

if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description="Full-song Vocal Separation & RMVPE Pitch Extraction Pipeline")
    parser.add_argument("--force", action="store_true", help="Force recomputation bypassing caches")
    parser.add_argument("--input", default="tmpmusic/maou_14_shining_star.ogg", help="Input audio path")
    parser.add_argument("--output-chart", default="tests/fixtures/shining_star_full_draft.json", help="Output chart JSON path")
    parser.add_argument("--output-dir", default="tmpmusic/analysis_out", help="Waveform, F0, and Demucs cache directory")
    parser.add_argument("--base-chart", default=None, help="Validated chart whose song/media metadata should be preserved")
    args = parser.parse_args()

    extract_full_song_pipeline(
        input_audio_path=args.input,
        output_dir=args.output_dir,
        chart_output_path=args.output_chart,
        base_chart_path=args.base_chart,
        force=args.force
    )
