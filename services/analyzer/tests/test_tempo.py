import sys
import unittest
from pathlib import Path

import numpy as np


ANALYZER_SRC = Path(__file__).resolve().parents[1] / "src"
sys.path.insert(0, str(ANALYZER_SRC))

from pipeline import estimate_global_tempo


class GlobalTempoEstimateTests(unittest.TestCase):
    def test_steady_click_track_estimates_quarter_note_bpm(self):
        sample_rate = 44_100
        expected_bpm = 158.0
        duration_s = 12.0
        audio = np.zeros(int(sample_rate * duration_s), dtype=np.float32)
        click_times = np.arange(0.5, duration_s, 60.0 / expected_bpm)
        click_starts = (click_times * sample_rate).astype(np.int64)
        audio[click_starts] = 1.0

        result = estimate_global_tempo(audio, sample_rate)

        self.assertFalse(result["usedFallback"])
        self.assertAlmostEqual(result["bpm"], expected_bpm, delta=3.0)
        self.assertGreater(result["beatCount"], 20)
        self.assertGreater(result["confidence"], 0.5)

    def test_silence_uses_explicit_low_confidence_fallback(self):
        result = estimate_global_tempo(np.zeros(44_100 * 2), 44_100)

        self.assertTrue(result["usedFallback"])
        self.assertEqual(result["bpm"], 120.0)
        self.assertEqual(result["confidence"], 0.0)
        self.assertEqual(result["beatCount"], 0)


if __name__ == "__main__":
    unittest.main()
