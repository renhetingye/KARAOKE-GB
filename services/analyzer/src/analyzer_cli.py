#!/usr/bin/env python3
"""
Karaoke Analyzer CLI (§13.2, §13.4)
Offline Vocal Separation & Pitch Analysis Job Runner
"""
import sys
import json
import time

def test_environment():
    import numpy as np
    import scipy
    import soundfile as sf
    import torch

    report = {
        "status": "ok",
        "python_version": sys.version.split()[0],
        "numpy_version": np.__version__,
        "scipy_version": scipy.__version__,
        "soundfile_version": sf.__version__,
        "torch_version": torch.__version__,
        "cuda_available": torch.cuda.is_available(),
        "device": "cuda" if torch.cuda.is_available() else "cpu",
    }
    print(json.dumps(report, indent=2))
    return report

def main():
    if len(sys.argv) < 2:
        print("Usage: python analyzer_cli.py [test-env | analyze ...]")
        sys.exit(1)

    cmd = sys.argv[1]
    if cmd == "test-env":
        test_environment()
    else:
        print(f"Unknown command: {cmd}")
        sys.exit(1)

if __name__ == "__main__":
    main()
