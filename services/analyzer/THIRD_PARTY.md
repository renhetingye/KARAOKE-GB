# Analyzer third-party components

## GAME 1.0.3 small ONNX

- Project: OpenVPI GAME (Generative Adaptive MIDI Extractor)
- Upstream: https://github.com/openvpi/GAME
- Release asset: `GAME-1.0.3-small-onnx.zip`
- License: MIT
- Archive SHA-256: `00BA0C64115B6B874D9EA4AFD3E6CF822ABDA2A04E52569233B0A044FD40E4E8`

The model is stored under `services/analyzer/models/` and intentionally excluded
from Git. Run `services/analyzer/install_game_model.ps1` to install the verified
official release. GAME generates discrete note boundaries and pitches; RMVPE is
retained independently for the editor's high-resolution F0 contour.
