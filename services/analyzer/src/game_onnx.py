"""Offline GAME ONNX inference for singing-note transcription.

GAME predicts note boundaries and pitches together.  This is deliberately kept
separate from RMVPE: RMVPE remains the high-resolution F0 overlay used by the
editor, while GAME produces the discrete, singable score candidates.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass

import librosa
import numpy as np

try:
    import onnxruntime as ort
except ImportError:  # The caller can fall back to the legacy segmenter.
    ort = None


MODEL_FILES = (
    "encoder.onnx",
    "segmenter.onnx",
    "estimator.onnx",
    "bd2dur.onnx",
    "config.json",
)


@dataclass(frozen=True)
class GameNote:
    start_s: float
    end_s: float
    pitch_midi: float


def model_is_available(model_dir: str) -> bool:
    return ort is not None and all(
        os.path.isfile(os.path.join(model_dir, name)) for name in MODEL_FILES
    )


class GameOnnxTranscriber:
    """Chunked CPU inference for the official OpenVPI GAME ONNX release."""

    def __init__(self, model_dir: str):
        if ort is None:
            raise RuntimeError("onnxruntime is not installed")
        missing = [
            name for name in MODEL_FILES
            if not os.path.isfile(os.path.join(model_dir, name))
        ]
        if missing:
            raise FileNotFoundError(f"GAME model is incomplete: {', '.join(missing)}")

        with open(os.path.join(model_dir, "config.json"), encoding="utf-8") as stream:
            config = json.load(stream)
        self.sample_rate = int(config["samplerate"])
        self.timestep = float(config["timestep"])
        self.language_ids = config.get("languages") or {}
        self.has_sampling_loop = bool(config.get("loop", False))

        # GAME's D3PM graph samples boundaries. Pin the ONNX Runtime seed so
        # re-analysis of identical audio produces the same chart.
        ort.set_seed(0)
        options = ort.SessionOptions()
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        providers = ["CPUExecutionProvider"]

        def load(name: str):
            return ort.InferenceSession(
                os.path.join(model_dir, name),
                sess_options=options,
                providers=providers,
            )

        self.encoder = load("encoder.onnx")
        self.segmenter = load("segmenter.onnx")
        self.estimator = load("estimator.onnx")
        self.bd2dur = load("bd2dur.onnx")

    def _infer_window(
        self,
        waveform: np.ndarray,
        language: str,
        boundary_threshold: float,
        note_threshold: float,
        sampling_steps: int,
    ) -> list[GameNote]:
        waveform = np.asarray(waveform, dtype=np.float32)[None, :]
        duration = np.asarray([waveform.shape[1] / self.sample_rate], dtype=np.float32)
        x_seg, x_est, mask_t = self.encoder.run(
            None, {"waveform": waveform, "duration": duration}
        )

        known = np.zeros(mask_t.shape, dtype=np.bool_)
        boundaries = known.copy()
        language_id = np.asarray([self.language_ids.get(language, 0)], dtype=np.int64)
        threshold = np.asarray(boundary_threshold, dtype=np.float32)
        radius = np.asarray(2, dtype=np.int64)

        if self.has_sampling_loop:
            sample_times = np.arange(sampling_steps, dtype=np.float32) / sampling_steps
        else:
            sample_times = np.asarray([0.0], dtype=np.float32)

        for sample_time in sample_times:
            inputs = {
                "x_seg": x_seg,
                "language": language_id,
                "known_boundaries": known,
                "prev_boundaries": boundaries,
                "t": np.asarray([sample_time], dtype=np.float32),
                "maskT": mask_t,
                "threshold": threshold,
                "radius": radius,
            }
            accepted = {value.name for value in self.segmenter.get_inputs()}
            boundaries = self.segmenter.run(
                None, {key: value for key, value in inputs.items() if key in accepted}
            )[0]

        durations, mask_n = self.bd2dur.run(
            None, {"boundaries": boundaries, "maskT": mask_t}
        )
        presence, scores = self.estimator.run(
            None,
            {
                "x_est": x_est,
                "boundaries": boundaries,
                "maskT": mask_t,
                "maskN": mask_n,
                "threshold": np.asarray(note_threshold, dtype=np.float32),
            },
        )

        result: list[GameNote] = []
        cursor = 0.0
        for note_duration, voiced, score, valid in zip(
            durations[0], presence[0], scores[0], mask_n[0]
        ):
            if not valid:
                break
            start = cursor
            cursor += float(note_duration)
            if voiced and cursor > start:
                result.append(GameNote(start, cursor, float(score)))
        return result

    def transcribe(
        self,
        waveform: np.ndarray,
        sample_rate: int,
        *,
        language: str = "ja",
        core_seconds: float = 25.0,
        context_seconds: float = 2.0,
        boundary_threshold: float = 0.2,
        note_threshold: float = 0.2,
        sampling_steps: int = 8,
        progress=None,
    ) -> list[GameNote]:
        if waveform.ndim > 1:
            waveform = waveform.mean(axis=1)
        if sample_rate != self.sample_rate:
            waveform = librosa.resample(
                np.asarray(waveform, dtype=np.float32),
                orig_sr=sample_rate,
                target_sr=self.sample_rate,
            )
        waveform = np.asarray(waveform, dtype=np.float32)
        total_seconds = waveform.size / self.sample_rate
        notes: list[GameNote] = []

        core_start = 0.0
        chunk_index = 0
        chunk_count = max(1, int(np.ceil(total_seconds / core_seconds)))
        while core_start < total_seconds:
            core_end = min(total_seconds, core_start + core_seconds)
            window_start = max(0.0, core_start - context_seconds)
            window_end = min(total_seconds, core_end + context_seconds)
            start_sample = int(round(window_start * self.sample_rate))
            end_sample = int(round(window_end * self.sample_rate))
            window_notes = self._infer_window(
                waveform[start_sample:end_sample],
                language,
                boundary_threshold,
                note_threshold,
                sampling_steps,
            )

            # Select by midpoint so overlapping context cannot duplicate notes.
            for note in window_notes:
                absolute = GameNote(
                    note.start_s + window_start,
                    note.end_s + window_start,
                    note.pitch_midi,
                )
                midpoint = (absolute.start_s + absolute.end_s) * 0.5
                is_last = core_end >= total_seconds
                if midpoint >= core_start and (midpoint < core_end or is_last):
                    notes.append(absolute)

            chunk_index += 1
            if progress is not None:
                progress(chunk_index, chunk_count)
            core_start = core_end

        notes.sort(key=lambda note: (note.start_s, note.end_s))
        normalized: list[GameNote] = []
        for note in notes:
            if not normalized or note.start_s >= normalized[-1].end_s:
                normalized.append(note)
                continue

            previous = normalized[-1]
            # Two context windows can describe the same note across a core
            # boundary with a few milliseconds of overlap.
            if round(previous.pitch_midi) == round(note.pitch_midi):
                previous_duration = previous.end_s - previous.start_s
                note_duration = note.end_s - note.start_s
                combined_pitch = (
                    previous.pitch_midi * previous_duration
                    + note.pitch_midi * note_duration
                ) / (previous_duration + note_duration)
                normalized[-1] = GameNote(
                    previous.start_s,
                    max(previous.end_s, note.end_s),
                    combined_pitch,
                )
            else:
                boundary = (previous.end_s + note.start_s) * 0.5
                normalized[-1] = GameNote(
                    previous.start_s, boundary, previous.pitch_midi
                )
                normalized.append(GameNote(boundary, note.end_s, note.pitch_midi))
        return normalized
