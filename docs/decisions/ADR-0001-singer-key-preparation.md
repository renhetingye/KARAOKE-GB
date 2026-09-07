# ADR-0001: 歌い手キー用の伴奏事前生成

- 日付: 2026-09-06
- 状態: 暫定採用（G4のT15/T16と聴感評価前）

## 課題

歌唱セッションのキー変更では、伴奏、表示音程、採点目標を同じ半音数だけ変更し、曲の長さとマイク原音は変えてはいけない。範囲は -6〜+6 半音とし、異性曲などのオクターブ違いを伴奏の±12移調で解決しない。再生中の変更は行わない。

## 決定

- 原譜の `pitchMidi` は変更しない。`keySemitones` は歌唱セッション設定として扱う。
- 停止中に48kHz canonical PCMをphase vocoderで事前生成し、元と同じframe数を保つ。
- UI音程バーとScoringEngineのtargetへ同一の `keySemitones` を渡す。
- 異性曲などは `vocalOctaveOffset`（-1/0/+1）で表示バーと採点目標だけを
  オクターブ移動できる。伴奏PCMにはこのオフセットを適用しない。
- microphone capture、F0、録音原音には移調を適用しない。
- 同じ楽曲・同じキーの準備済みPCMはメモリ上で再利用する。

## 比較

- 単純resampling: キーと同時に尺・テンポが変わるため不採用。
- Rubber Band: 品質候補だがGPL/commercialの配布条件が製品ライセンス未決の現状に合わない。
- Signalsmith Stretch: MITで適合するが、確認したRust wrapperはbuild時にlibclangを要求し、この環境で再現ビルドできなかったため現時点では不採用。
- 純Rust phase vocoder: 追加ランタイム不要で再現ビルドできるため暫定採用。

## 測定

- 440Hz fixtureを -3 半音へ変換し、FFT主成分が期待周波数±8Hz以内。
- 1秒fixtureと276.7秒の実曲で、出力frame数とdurationが入力と一致。
- 実曲 -3 のrelease事前生成はこの開発機で約4.30秒（decode時間を除く）。

## 未確定

音楽的品質、transient、左右位相、全キーでの聴感、メモリ上限はT15/T16で比較する。品質不足なら同じ `transpose_preserving_duration` 境界の実装を、配布条件を確認した別engineへ交換する。
