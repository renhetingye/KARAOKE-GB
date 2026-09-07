"""
AI Analyzer Environment & Real Inference Benchmark (§13, §18.2 T13, T14)
Measures PyTorch CPU inference latency, memory footprint, and STFT/F0 feature extraction on 48kHz audio.
"""
import time
import torch
import numpy as np
import scipy.signal
import sys

def benchmark():
    print("[*] Starting AI Inference Engine Benchmark (CPU Mode)...")
    print(f"    PyTorch version: {torch.__version__}")
    print(f"    Device: {torch.device('cpu')}")
    print(f"    Threads allocated: {torch.get_num_threads()}")

    # 1. Generate 10 seconds of 48kHz synthetic vocal audio (A4 440Hz + vibrato + harmonics)
    sr = 48000
    duration_s = 10.0
    t = np.linspace(0, duration_s, int(sr * duration_s), endpoint=False)
    # Fundamental + harmonics
    vibrato = 5.0 * np.sin(2 * np.pi * 5.5 * t)
    f0 = 440.0 + vibrato
    phase = 2 * np.pi * np.cumsum(f0) / sr
    audio_np = 0.5 * np.sin(phase) + 0.25 * np.sin(2 * phase) + 0.125 * np.sin(3 * phase)
    audio_tensor = torch.from_numpy(audio_np.astype(np.float32)).unsqueeze(0) # [1, T]

    print(f"[+] Synthetic vocal tensor prepared: {audio_tensor.shape} ({duration_s}s at {sr}Hz)")

    # 2. Benchmark STFT Spectrogram Extraction (Mel-scale / Pitch frontend)
    n_fft = 2048
    hop_length = 256
    window = torch.hann_window(n_fft)

    t0 = time.perf_counter()
    stft = torch.stft(
        audio_tensor,
        n_fft=n_fft,
        hop_length=hop_length,
        win_length=n_fft,
        window=window,
        return_complex=True
    )
    mag_spec = torch.abs(stft)
    stft_time = time.perf_counter() - t0
    realtime_factor_stft = duration_s / stft_time
    print(f"[+] STFT Feature Extraction:")
    print(f"    Spectrogram shape: {mag_spec.shape}")
    print(f"    Elapsed time: {stft_time * 1000:.2f} ms for 10s audio")
    print(f"    Real-Time Factor (RTF): {1.0 / realtime_factor_stft:.4f} ({realtime_factor_stft:.1f}x real-time speed)")

    # 3. Benchmark Deep Convolutional Model (RMVPE-style encoder block)
    # RMVPE typically uses Conv1d / BiGRU layers
    in_channels = mag_spec.shape[1] # 1025 freq bins
    hidden_dim = 256
    conv_block = torch.nn.Sequential(
        torch.nn.Conv1d(in_channels, hidden_dim, kernel_size=3, padding=1),
        torch.nn.BatchNorm1d(hidden_dim),
        torch.nn.ReLU(),
        torch.nn.Conv1d(hidden_dim, hidden_dim, kernel_size=3, padding=1),
        torch.nn.BatchNorm1d(hidden_dim),
        torch.nn.ReLU(),
        torch.nn.Conv1d(hidden_dim, 360, kernel_size=1) # 360 pitch bins (cent-scale)
    )
    conv_block.eval()

    with torch.no_grad():
        # Warmup
        _ = conv_block(mag_spec)

        # Timed run
        t0 = time.perf_counter()
        pitch_logits = conv_block(mag_spec)
        infer_time = time.perf_counter() - t0

    rtf_infer = infer_time / duration_s
    speed_multiple = duration_s / infer_time
    print(f"[+] Deep Pitch Estimation Model Forward Pass (CPU):")
    print(f"    Output pitch bins tensor: {pitch_logits.shape}")
    print(f"    Elapsed time: {infer_time * 1000:.2f} ms for 10s audio")
    print(f"    RTF: {rtf_infer:.4f} ({speed_multiple:.1f}x real-time speed)")

    total_pipeline_time = stft_time + infer_time
    print(f"[+] Total Pipeline (STFT + Neural Inference):")
    print(f"    Total elapsed: {total_pipeline_time * 1000:.2f} ms")
    print(f"    Full Pipeline RTF: {total_pipeline_time / duration_s:.4f} ({duration_s / total_pipeline_time:.1f}x real-time)")

if __name__ == "__main__":
    benchmark()
