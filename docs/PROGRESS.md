# プロジェクト開発進捗記録

- 最終更新: 2026-09-07（G2 .kpk書出し・安全import追加）
- 公開資料: 本書および `docs/decisions/`（内部の計画・AI引継ぎ資料は非公開）
- 現在Gate: G2実装中。G1の有線ヘッドフォン＋マイク30分実歌唱はユーザー受入済み（計測ログを伴う形式試験は未完）

## 今回確認・修正したこと

### P0: 実機採点の時間座標

- 従来はWASAPIの絶対QPC値をScoringEngineがSong time µsとして比較していた。
- Capture frameは絶対QPCを維持し、Timelineで確定したSong timeを別値として採点へ渡すよう修正。
- WASAPIのQPC位置は100ns単位として換算し、QueryPerformanceFrequencyのtick値と混同しない。
- Renderへ書き込む先頭フレームの予定提示時刻には、既にqueuedのframesを加味する。
- 絶対QPCとSong timeが異なるテストを追加しPASS。

### P0: 歌唱時間とともに増える採点負荷

- 従来はhopごとに、全曲34,354セル × 蓄積済み全PitchFrameを線形走査していた。
- Song time順フレームを二分探索し、途中採点では未来ノートを分母から除外。
- UI用途中採点を10Hzへ制限。Pitch解析hopと採点更新頻度を分離。
- 未来ノートが途中点数を下げないテストを追加しPASS。

### 音声I/Oの安全化

- Capture packet内の各sampleへ、入力rateに基づく個別QPC時刻を付与。
- capture discontinuity、ring overflow、時刻逆行をquality flagへ反映。
- system gapはInterrupted、過大clippingはInvalidInputとして区別。
- 入出力デバイスのfloat32形式を確認し、不一致をPCMとして誤読しない。
- Renderの実channel数とsample rateをcallbackへ渡し、伴奏を線形rate変換して出力。
- IAudioClockのpositionをGetFrequencyからPCM frame座標へ正規化。
- drift estimatorをpacket数の合計ではなく最初と最後のdevice frame差で計算し、初回packet biasを除去。
- headless loopbackで出力channel数を固定2chと仮定せず、ring overflow/underflowを別々に計数。
- `drift-benchmark [sec] [json]` で機械可読な測定ログを保存可能にした。

### 譜面・AI解析の保全

- AI解析は `shining_star_full_draft.json` へ未採用候補として保存し、現行譜面を自動上書きしない。
- 60/80/90ms未満を時間だけで削除・結合する規則を撤去。
- JSON成果物を一時ファイル、flush、`os.replace`で確定。
- 現行譜面の保存にはrevision競合検出、flush、旧版 `.bak` 保全を追加。
- UIに「候補を採用してエディタで開く」を追加。採用は確認操作後のみ。
- 固定の作業ドライブパスをTauriコマンドから撤去。
- エディタのUndo/Redoをノーツ配列だけでなく、Chart全体・歌い出し・表示基準音を戻すスナップショットへ拡張。
- 編集中の有効なChartを曲別 `*.recovery.json` へ遅延自動退避し、次回起動時に復元確認する経路を追加。通常保存成功時に退避を消去する。
- `save_chart` の保存先をシャイニングスター固定から `library/songs.json` の曲別 `activeChart` 解決へ変更。
- 選択ノーツへ歌詞・ルビを割り当て、歌詞tokenをフレーズ化する編集UIを追加。ノーツの移動・分割・結合・削除時に参照と時間範囲を追従させる。
- テンポマップUIを追加。再生ヘッド位置へのstep-tempo追加、時刻・BPM変更、先頭以外のイベント削除に対応。
- `.kpk` writerとroundtripテストを追加。書出後に再読込して構造・サイズ・内部整合を検査してから確定する。
- `.kpk` のドラッグ＆ドロップimportを追加。隔離stagingでChart・音源decode・時間・件数を検証後にのみ曲カタログへ登録する。
- package内のcase-insensitive重複名、manifest未記載entry、不正packageId、chartPath不一致、chart/backing個数不正を拒否する。
- packageの完全性照合は内部処理とし、ユーザーにhash確認操作を要求しない。
- エディタの音源cacheと波形/F0参照をsongId単位へ変更し、追加曲でシャイニングスターの素材を誤使用しないよう修正。

### UI・起動

- 歌唱画面に歌い手キー -12〜+12 を追加（初期-6〜+6から拡張）。停止中に確定し、伴奏・音程バー・採点targetへ同じ半音数を適用する。マイク原音と保存譜面は変更しない。
- 移調後もframe数とdurationを固定する純Rustの事前生成処理を追加。同じ楽曲・キーはメモリcacheを再利用する。
- 「表示音域の中心」はキー変更と分離したまま維持。
- 歌唱タブを特定曲名から汎用カラオケ歌唱へ変更し、`library/songs.json`、`list_songs`、`songId` 選択経路を追加。現登録曲はシャイニングスター1曲。
- `run_diagnostic_ui.ps1` の消失していた変数と実行式を修復し、構文検証PASS。
- 直接 `cargo build --release` するとfrontend未埋込のdevUrl版でrelease EXEを上書きできる事故を確認。正式手順を `build_release.ps1` に固定し、launcherは現在のVite assetが埋め込まれていないEXEを起動前に拒否する。
- release profileをworkspace rootへ移し、実際に適用されるよう修正。
- GUIから起動したPython解析のstdout/stderrをUTF-8固定。CP932の日本語行をRust側が読めずpipeを閉じ、Pythonが `OSError: [Errno 22]` になる不具合を修正。

## 正本 §20 に対する現在地

| Gate | 現状 | 判定 |
|---|---|---|
| G0 技術実証 | decoder・WASAPI時刻API・AI環境の試作あり。機材台帳、UI同時負荷、実測ADRが不足 | 未完了 |
| G1 基本歌唱 | 伴奏、Raw入力、F0表示、基礎採点、headlessを実装。有線ヘッドフォン＋マイク30分歌唱はユーザー受入済み。T01–T08の形式計測ログは不足 | 実用受入済み・形式試験残り |
| G2 編集・保存 | ピアノロール、歌詞・ルビ・フレーズ・step-tempo編集、全体Undo/Redo、revision保存、曲別recovery、package write/read/importを実装。DocumentStore/journalとFLAC正規化不足 | 現在実装中 |
| G3 AI下書き | 全曲解析候補と非破壊採用は先行試作。部分再解析・競合範囲保護・T13/T14不足 | 未完了 |
| G4 実用歌唱 | キー事前生成、マイクモニター/DSP、Raw/Wet/Master録音、MIX音量調整を実装。速度、実機T15/T16不足 | 実装中 |
| G5 配布 | installer、offline導入試験、schema移行、license同梱不足 | 未着手 |

したがって「コードが存在する最遠地点」はG4の一部で、現在の主作業はG2である。G1はユーザーの30分実歌唱では受入済みだが、正本どおりの計測ログが揃った形式完了とは分けて記録する。

## 実データの状態

- 現行譜面: `tests/fixtures/shining_star_chart.json`
  - revision 2、737 notes
  - AI候補が採用済み。ただし聴感確認前で、音楽的正解として未認定
  - SHA-256: `1e6373fe6c900efce5b4e94cb1961a3cb2dc00d95449f6b6018572567b6b910f`
- 新しい未採用候補: `tests/fixtures/shining_star_full_draft.json`
  - revision 1、737 notes
  - 最短10ms、80ms以下90件を候補として保持
  - Schema 2.0.0 / 意味検証PASS
  - SHA-256: `767d6c2a9f05e75040d702ffd0452fda93899ba607cdc64e0bdb1bff6876196c`

## 今回の検証

- `cargo test --workspace --all-targets`: 28 tests PASS、full-track性能試験1件は明示実行
- T01 30分模擬時計: ±300ppmとepoch破棄の決定論試験PASS
- T07: 同一PitchFrameログの30/60/120fps非依存試験PASS
- T08: system gapをInterrupted、継続clippingをInvalidInputとする試験PASS
- 修正版実機10秒drift観測: Capture/Render相対7.21ppm、capture discontinuity 1回。短時間観測であり合格認定には使わない
  - `docs/verification/G1_DRIFT_SHORT_2026-09-06.json`
- 歌い手キーfixture: -3半音の主成分、同一frame数・durationを確認
- シャイニングスター全曲 -3: release事前生成 約4.30秒、同一frame数・durationを確認
- `cargo clippy --workspace --all-targets -- -D warnings`: PASS
- `npm.cmd run build`: Vue type-check / Vite build PASS
- `python -m py_compile services/analyzer/src/pipeline.py ...`: PASS
- UTF-8 pipe条件でAI解析を実行し、Demucs既存stem・RMVPE cacheから737ノーツ候補の再生成PASS
- `karaoke-cli validate-chart tests/fixtures/shining_star_full_draft.json`: PASS
- PowerShell launcher parse: PASS
- `npm.cmd run tauri -- build --no-bundle`: optimized release build PASS
- `target/release/appsdesktop.exe`: 3秒startup smoke PASS（検証用PIDのみ終了）
  - 正式Tauri build後、computer-useで埋込UIの実画面と歌唱操作部を確認
  - 7,169,024 bytes / SHA-256 `e8accdba85d9f6bfb618cfe9a9f177f2ffac7d2403d2975a32c8c088586d5d9b`
- 実機音声の声出し・聴感・外部ループバック測定: 今回未実施
- 有線ヘッドフォン＋マイクによる30分実歌唱: ユーザー報告で問題なし（数値ログなし）

## 未達・次に必要なこと

1. 歌唱画面の歌詞表示・ルビ表示・phrase wipeをChartの実データへ接続し、実曲歌詞の入力とタイミング確認を行う。
2. `crates/project` のDocumentStore/journalを実装する。曲別recoveryは先行する最小安全策。
3. 737-note候補を聴感と波形/F0で確認し、必要区間だけ採用できる差分UI・lock保護を実装する。
4. import音源を正本指定の `media/backing.flac` へ正規化し、追加曲の波形/F0・AI候補生成を曲別jobへ接続する。登録済み `.kpk` は歌唱・編集・保存の曲選択経路へ入る。
5. キー変更・モニター/DSP・録音/MIXの聴感、同期、transient、メモリをT15/T16で評価する。速度変更は未実装。
6. T01–T08の不足する形式計測（buffer/drop、外部ループバックのTmon/Tdisplayなど）を必要時に記録する。

「完全」「同期保証済み」「60fps達成」は対応する実測が揃うまで使用しない。
# 2026-09-07: G4 マイクモニター / Vocal DSP 第1段階

- 歌唱画面へ、初期OFFのマイクモニターと返し音量（初期値 -18 dB）を追加。
- モニター経路へノイズゲート、3バンドEQ、コンプレッサー、エコー、リバーブ、リミッターを追加。
- 採点経路は加工前の生マイク入力を維持し、モニターDSPが採点結果へ影響しない構成にした。
- Captureから採点用・モニター用の独立SPSC ringへ分岐し、Render callback内はlock / file I/O / heap allocationなしで処理する。
- 入出力のshared-mode buffer要求を20 msから10 msへ短縮。実際の周期はWindowsと音声機器が対応するengine periodへ丸める。
- 設定は再生開始時に確定し、歌唱中の変更は次回開始まで無効。スピーカー利用時のハウリングを避ける注意表示を追加。
- DSP unit test、workspace test、clippy `-D warnings`、Vue type-check / Vite buildはPASS。
- 未完了: 有線ヘッドフォンでの実聴、Tmon実測、長時間full-duplex試験。録音/MIX保存は後続節で実装済み。
# 2026-09-07: 無伴奏マイク試聴と初期音質修正

- 曲をデコード・再生せず、選択したCaptureからRenderへマイク音だけを返す `MicMonitorSession` を追加。
- 歌唱画面へ「声だけ試聴 / 試聴停止」と「原音設定へ戻す」を追加。
- 初期値を -18 dB / reverb 12% / compressor 3:1 / gate -55 dB から、-9 dB / effect OFF / compressor 1:1 / gate -80 dBへ変更。最初は原音比較でき、必要な効果だけ追加する方針に修正。
- 試聴と歌唱セッションの同時起動をbackendでも拒否する。
- 実耳による音質・遅延確認は、ハウリング防止のためユーザー操作による有線ヘッドフォン試験を待つ。
# 2026-09-07: 音声ドロップから曲登録・AIバー自動保存

- MP3 / WAV / FLAC / OGG / M4A / AACの単一ファイルをメイン画面へドロップできる導線を追加。
- ファイル名を曲名初期値として、曲名・歌手名だけを確認後、`library/imported/<songId>/`へ音源と初期譜面を自動保存し `library/songs.json`へ登録する。
- 音声のdecode、48 kHz canonical frames、channel数、duration、source hashをアプリ側で取得し、利用者によるJSON編集を不要にした。
- AI解析の入力、候補譜面、Demucs/F0/waveform cacheを曲ID単位へ一般化。Python側も既存の曲名・音声metadataを維持する。
- 新規音声の追加時はDemucs + RMVPE解析を自動開始し、検証後の音程バーをその曲の現行譜面へ自動採用する。既存曲の手動再解析は候補確認を維持する。
- 歌詞・ルビは自動生成対象外で、後から譜面エディタで追加する。
# 2026-09-07: `.kpk` をライブラリ正本として保持

- 拡張子は設計正本どおり `.kpk`。`library/packages/<songId>.kpk` を利用者が確認できる再生パッケージ保存先にした。
- MP3等の新規登録直後に初期 `.kpk` を生成し、AI譜面の自動採用後に同じパッケージを安全に再生成する。
- 外部 `.kpk` のimport時も検証済みアーカイブを `library/packages` に保持し、編集・再生用には検証後のworking copyを展開する。
- `.kpk` 内部構造は `manifest.json`, `chart.json`, `media/backing.<ext>`。CLIにも検証付き `export-package` を追加。
- シャイニングスターの既存データから `library/packages/shining_star.kpk` を生成し、再読込検証PASS。
- GUIの音声投入領域を高さ108pxの青い破線枠に拡大し、「ここにMP3などの音声ファイルをドロップ」と明示。
# 2026-09-07: `.kpk` 正本化と明示的な音声選択UI

- `library/packages/<songId>.kpk` が存在する曲だけをplayable catalogへ載せる。`.kpk`削除時は次回一覧読込でcatalog entryを除去し、展開済み音源やcacheだけでは再生しない。
- 登録曲が0件の場合をUIで扱い、歌唱開始を無効化して「MP3または.kpkを追加」と表示する。
- 大型drop zoneに加えてTauri公式dialog pluginによる「音声ファイルを選ぶ」ボタンを追加。
- ブラウザーpromptを廃止し、選択ファイル名、曲名、歌手名、登録・AI解析開始を確認できるアプリ内modalを追加。
- verified `.kpk` import時はarchive自体をlibraryへ保持し、展開物は編集・再生用working copyとして分離する。
# 2026-09-07: Singing-note transcription correction

- Confirmed that the 744-note Shining Star chart was not globally time-shifted;
  the old pipeline incorrectly promoted heuristic RMVPE F0 splits (including
  scoops, consonant transitions, and micro-segments) to scorable notes.
- Integrated the official OpenVPI GAME 1.0.3 small ONNX model as the preferred
  offline singing-note boundary/pitch transcriber. RMVPE remains available as
  the editor contour and as a compatibility fallback.
- GAME inference is chunked with overlap to avoid the quadratic cost of whole-song
  inference and normalizes chunk-seam overlaps.
- Generated a protected replacement candidate for `song_1788736606176`: 555
  notes, median duration 260 ms, 9 notes at or below 100 ms (old: 744 notes,
  median 185 ms, 137 notes at or below 100 ms). Active chart and editor recovery
  were not overwritten; the former candidate was backed up.
- Corrected this known Shining Star candidate's tempo map from the import
  placeholder 120 BPM to 158 BPM. Generic BPM inference/confirmation remains a
  separate import-flow task because half/double-tempo ambiguity is unavoidable.
- Added a verified model installer and third-party provenance. Archive SHA-256:
  `00BA0C64115B6B874D9EA4AFD3E6CF822ABDA2A04E52569233B0A044FD40E4E8`.

# 2026-09-07: G4 Raw microphone recording foundation

- Added the dedicated `crates/recording` worker. Capture sends an independent
  2-second SPSC feed; WAV and metadata I/O never run in the WASAPI callback.
- Recording is explicit opt-in and defaults OFF. Enabled karaoke sessions save
  mono 24-bit PCM `raw.wav` plus `recording.json` under
  `recordings/<songId>/<sessionId>/`.
- Metadata preserves input sample rate, first/last WASAPI 100ns timestamps,
  start offset, frame count, discontinuity ranges, and recorder-ring overflow.
  A damaged capture is marked `incomplete` instead of silently certified.
- Added karaoke UI controls and a post-stop result with an Explorer reveal
  action. No microphone was recorded during automated verification.
- Workspace tests: 34 passed, 1 explicitly ignored. Clippy `-D warnings` and
  Vue/Vite build passed. Release SHA-256:
  `5E7F5086B24250CB671B830A2C0C7574356F3D7F4ECECA18A57F106F82CBB698`.
- Recording stop now derives the Raw-to-render alignment from the first capture
  timestamp and render-start timestamp, applies the selected vocal DSP offline,
  and writes 48 kHz/24-bit `wet.wav` plus stereo `master.wav` without changing
  `raw.wav`. Negative alignment trims pre-roll; positive alignment inserts the
  corresponding silence before the vocal.
- The master starts at the selected song offset, resamples backing reads to
  48 kHz, mixes the aligned Wet vocal, and applies the configured output ceiling.
  MIX failures are recorded in metadata while the Raw original remains usable.
- The recording panel now exposes independent backing/vocal gains (-24..+6 dB)
  for `master.wav`. These values are retained in `recording.json`; `raw.wav` and
  `wet.wav` are intentionally unchanged by the mastering gains.
- Synthetic export verifies Raw/Wet/Master headers, frame counts, stereo layout,
  and atomic metadata replacement. Final release SHA-256:
  `3D29B86D3B62F5F643C7BC9A6F6DF7F8D5D87741E6635B879F1E9464BC3810CA`.
- Remaining recording work: real-device listening/alignment validation.
- Mixer-gain synthetic sample assertions, all 34 workspace tests, Clippy
  `-D warnings`, Vue type checking, and the production frontend build passed.
  Updated release SHA-256:
  `115D91917FA85EB71F1A30A7208B987EB57DE88DF3281DE49749F41EBE0F9CB5`.

# 2026-09-07: G4 practice speed and scoring mode

- Added stopped-state practice-speed selection from 0.70x to 1.30x in 0.05 steps.
- Backing PCM is prepared offline with duration change and pitch correction; FFT work
  never runs in the WASAPI callback. Cache identity now includes song, key, and speed.
- Transport Song time, scoring frame distance, start offset, and recording master
  alignment use the same speed ratio.
- Added UI selection for normal octave-tolerant scoring versus strict-octave scoring;
  the existing Rust-only scoring implementation remains the single scoring source.
- Synthetic 440 Hz at 1.25x retained pitch within the test tolerance and changed
  duration from 1.0 s to 0.8 s. The Shining Star full-track preparation smoke test
  also preserved expected frame count and channel layout; preparation took 26.34 s
  in the debug test build (not a release performance measurement).
- ADR: `docs/decisions/ADR-0002-practice-speed-preparation.md`.
- Still required for G4/T15: listening/transient comparison and licensed high-quality
  time-stretch backend decision. This baseline is not yet audio-quality certified.
- All 36 non-ignored workspace tests passed; 2 full-track performance tests remain
  explicitly opt-in. Clippy `-D warnings`, Vue type checking, production frontend
  build, release build, application launch, and accessibility discovery of the new
  controls passed. Release SHA-256:
  `E1C7488AA9A581FCEABCC9060221EB30F9E6E1D204D3EDFAE0E29EFC0CB15BAC`.

# 2026-09-07: Singing-view pitch display cleanup

- Replaced the ambiguous octave checkbox with two explicit buttons:
  `通常（オクターブ許容）` and `厳密（譜面どおり）`.
- The cyan display trail now rejects non-clean/low-periodicity frames, uses a
  display-only three-sample median, and breaks the path at jumps over seven
  semitones. This targets one-frame YIN subharmonic drops without changing the
  raw F0 frames or Rust scoring input.
- The live cursor uses the same filtered display trail. Score reproducibility is
  unaffected because filtering exists only in Vue rendering.
- Vue type checking, production build, release build, application launch, and
  accessibility discovery of both scoring-mode buttons passed. Release SHA-256:
  `6C5AF9D36B02414CB61D92633C49D9B85C93A14345EA12EF534F329C3C3C0911`.

## Remaining v4 work, ordered by dependency

1. G2: finish project/journal semantics, full lyric/phrase workflow, package security
   limits, and T09-T12 coverage.
2. G3: partial re-analysis, revision/manual-lock conflict protection, and independent
   T13/T14 evaluation data. Current whole-song AI output remains a draft.
3. G4: persist complete result records, singing-session loop/guide controls, device
   loss state handling, high-quality speed-backend comparison, and T15/T16.
4. G0/G1 formal evidence still missing: certified device periods/latency, T02-T05
   voice/acoustic fixtures, and measured UI frame pacing. User-accepted listening is
   recorded separately from formal measurement evidence.
5. G5 remains last as requested: clean-Windows installer, offline bundle, schema
   migrations, SBOM/licenses, and end-user documentation.

# 2026-09-07: Recoverable song deletion

- Added `選択中の曲を削除` beside package export. The UI requires an explicit
  confirmation and blocks deletion while singing, monitoring, or analysis is active.
- Deletion removes the selected song from `songs.json` and the playable catalog.
  The library `.kpk`, `.bak`, and owned `library/imported/<songId>` project are moved
  to one timestamped `library/trash/<songId>-<time>` directory instead of being
  permanently erased. `deleted.json` retains the original catalog descriptor and
  recovery information.
- Audio/editor/prepared-backing caches are released so a deleted selection cannot
  continue playing from memory.
- A temporary-library test verifies that package, backup, project, and tombstone are
  moved while the real user library remains untouched. All 37 non-ignored workspace
  tests passed; 2 full-track performance tests remain explicitly opt-in. Clippy
  `-D warnings`, Vue type checking, and production frontend build passed.
- The production executable was rebuilt and launched; accessibility inspection found
  both package export and song-delete buttons. Release SHA-256:
  `4E040D3996A69919DAD366DD5448F6E24C1E522BC483C51E5C15D91E1195E9E6`.

# 2026-09-07: Singer key expanded to one octave

- Product requirement R08 and the singing UI now accept -12..+12 semitones.
  Backing preparation, target-bar display, and Rust scoring continue to share the
  same session key; the microphone Raw signal and saved chart are unchanged.
- Added strict-scoring assertions for both -12 and +12 boundaries and FFT assertions
  that a 220 Hz fixture becomes approximately 110 Hz / 440 Hz while duration remains
  fixed.
- The new boundary test exposed and fixed a phase-vocoder bug: when multiple source
  bins mapped to one lower target bin, synthesis phase had been advanced multiple
  times per frame. Magnitudes/frequency weights are now accumulated first and each
  target-bin phase advances once.
- All 38 non-ignored workspace tests passed; 2 full-track performance tests remain
  explicitly opt-in. Clippy `-D warnings`, Vue type checking, and production frontend
  build passed.
- The release executable was rebuilt, launched, and its -12..+12 label was found by
  accessibility inspection. Release SHA-256:
  `2621087BEE3696B7282F72BE09206B66C30E6FBC63BF470B9474931079F93491`.

# 2026-09-07: Singing-to-editor navigation lock

- Fixed the singing-view `音程バーを編集する` shortcut bypassing the global
  running-state lock. Editing is now disabled while karaoke playback, key
  preparation, microphone testing, monitor preview, or synthetic sync is active.
- While locked, the shortcut explicitly says that playback/preview must be stopped
  first. The karaoke tab also remains available for an active karaoke session, so
  an already inconsistent/stale UI state can return to the visible stop control.
- Vue type checking and the production frontend build passed. The official release
  executable was rebuilt. A live singing-session UI check confirmed that the editor
  tab is disabled, the in-view shortcut changes to `再生・試聴の停止後に編集`, and
  the singing tab/stop control stay reachable. Release SHA-256:
  `83FBB446DFE42C7ACF8243887673EAAEC84D2E65A534F241AFB6AFA3FFBB02DC`.

# 2026-09-07: Microphone diagnostic pitch display and karaoke key policy

- Fixed a missing frontend connection in microphone-only diagnostics: the Rust
  pipeline already produced pitch frames, but Vue polled only the summary telemetry
  and never appended those frames to the cyan pitch trail. Diagnostics now polls
  both streams, deduplicates frame sequence numbers, and uses a local monotonic
  display timeline.
- The microphone view now has its own visible `表示音域の中心` controls and hides
  unrelated song target notes. A Japanese signal summary distinguishes no PCM,
  silence, audible/unpitched speech, and detected voiced pitch. Ordinary consonants
  can therefore be shown as audible speech without inventing an F0.
- Restored accompaniment key control to -6..+6 semitones. Added a separate singing
  octave selector (-1/0/+1) which moves only target bars and scoring; backing PCM is
  not octave-shifted. This lets a low voice sing a female melody one octave down
  while accompaniment key remains original or receives only modest adjustment.
- Vue/Vite build, workspace check, all 38 non-ignored workspace tests, focused media
  and scoring tests, and Clippy `-D warnings` passed. Two full-track performance
  tests remain explicitly ignored.
- Official release SHA-256:
  `E3AA71E82577552183DDB411C29764B0C992AEF4BBB3556F6CC849AF213AB7CB`.

# 2026-09-07: Automatic global BPM estimate

- Whole-song AI analysis now estimates a representative quarter-note BPM from
  the original MP3/OGG audio instead of retaining the import placeholder (120)
  or the Shining Star-specific constant (158).
- The estimate is written to the draft chart's first `tempoMap` event. Provenance,
  heuristic beat-spacing confidence, detected beat count, and fallback status are
  retained under `extensions.tempoEstimate`.
- The value remains explicitly reviewable: global beat tracking cannot reliably
  settle half/double-tempo intent, the first downbeat, pickup measures, or tempo
  changes. Existing meter information is preserved and note timestamps are not
  snapped or moved.
- Added deterministic 158 BPM click-track and silence-fallback unit tests. Both
  pass, Python compilation passes, and robust interval refitting makes the local
  Shining Star source estimate 157.888 BPM against its documented 158 BPM.
- Vue type checking and the production frontend build passed. The official
  release executable was rebuilt; SHA-256:
  `91A13F82652F6304D5BBFB282FA18AFD9F30E514F47426C5AA7DFE319CAE5798`.

# 2026-09-07: Responsive wide-screen workspace

- Replaced the legacy 1240 px application cap with a fluid 100% container capped
  at 1680 px, allowing the pitch canvas and controls to use modern wide displays.
- The diagnostic sidebar now scales between 320 and 380 px while the main singing
  panel receives the remaining space. Below 960 px the workspace stacks into one
  column, with an additional compact header/device layout below 680 px.
- Vue type checking, production frontend build, and the official release build
  passed. Release SHA-256:
  `1E8FE4F1E50D14D4B010C3CE0B8F4B52B72FD591BD09BC8B27DFBA66E0B0689A`.

# 2026-09-07: Canvas aspect correction and automatic octave folding

- The responsive pitch canvas now synchronizes its backing-store dimensions to
  its CSS dimensions and device pixel ratio. Wide layouts no longer stretch the
  old fixed 960x220 bitmap, so the live cursor remains circular and text/lines
  stay sharp.
- Wide canvases reveal proportionally more timeline instead of stretching the
  same five-second interval, preserving familiar note-bar proportions.
- Normal scoring now accepts the same pitch class up to two octaves above or
  below. During scorable notes, the display-only cyan trail folds into the target
  octave so C3 visibly connects to a C4 target; raw F0, recording, backing audio,
  and strict scoring remain unchanged.
- Fixed the live cent indicator so strict mode no longer applied octave folding
  visually. Vue production build and all 11 scoring tests pass.
- The official release build passed; SHA-256:
  `4E29EA95079A9F272EC6F67AECA522D6D752BFF3CB9BDDAC67DA44D631310028`.

# 2026-09-07: Simplified octave controls and repository hygiene

- Removed the redundant low/original/high manual target-bar octave buttons from
  the main singing UI. Normal mode performs automatic octave folding; strict mode
  retains literal score-octave practice. Backing key remains the independent
  commercial-style -6..+6 control.
- Added runtime user libraries, exported packages, and editor recovery artifacts
  to `.gitignore` so songs, recordings, and local recovery state are not included
  in a source-code publication.

# 2026-09-07: Combined automatic and fixed-octave singing modes

- Reintroduced explicit low (-1 octave), original-score, and high (+1 octave)
  choices alongside the default automatic octave-folding mode. Fixed choices
  shift only target bars and the exact scoring target; backing audio remains at
  the selected -6..+6 singer key.
- Removed the separate ambiguous strict-scoring choice: selecting `原譜` now
  provides that exact-octave behavior, while `自動（おすすめ）` provides the
  commercial-style pitch-class behavior.
- Added a regression test proving that voiced frames before/after chart notes do
  not affect total score, pitch accuracy, voicing coverage, or evaluated duration.
  All 12 scoring tests and the Vue production build pass.
- The official release build passed; SHA-256:
  `F0B14D614C2BDD825E1DF6438B58D311E330454EE5F04A7B6639BF5276A3D1C6`.
- Vue checking and the official release build passed. Release SHA-256:
  `BE7397906DA2AF50D576D14E3DDAF3F0C43FCC266BB3F16140899E2107A55226`.
