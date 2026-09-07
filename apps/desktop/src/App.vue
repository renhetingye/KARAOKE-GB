<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";

interface SystemInfo {
  protocolVersion: string;
  qpcFrequencyHz: number;
  qpcResolutionNs: number;
}

interface AudioDevice {
  id: string;
  name: string;
  dataFlow: string; // "Capture" | "Render"
  isDefault: boolean;
}

interface RecordingArtifact {
  schemaVersion: string;
  sessionId: string;
  songId: string;
  status: "complete" | "incomplete" | "empty";
  rawWavPath: string;
  wetWavPath: string | null;
  masterWavPath: string | null;
  mixStatus: "pending" | "complete" | "failed";
  mixError: string | null;
  metadataPath: string;
  sampleRate: number;
  channels: number;
  bitsPerSample: number;
  sampleFrames: number;
  firstTimestamp100ns: number | null;
  lastTimestamp100ns: number | null;
  startOffsetUs: number;
  backingGainDb: number;
  vocalGainDb: number;
  captureOverflow: boolean;
  gapRanges: Array<{ startFrame: number; endFrame: number; reason: string }>;
}

interface PitchFrame {
  f0Hz: number | null;
  periodicity: number;
  voicing: string;
  qualityFlags: {
    lowConfidence: boolean;
    harmonicAmbiguity: boolean;
    clipping: boolean;
    inputGap: boolean;
    timestampInvalid: boolean;
    warmup: boolean;
    outOfRange: boolean;
  };
}

interface DisplayPacket {
  protocolVersion: string;
  sessionId: string;
  epochId: number;
  sequence: number;
  songTimeUs: number;
  anchorQpc: number;
  micLevel: number;
  pitchFrames: PitchFrame[];
  scoreSnapshot: {
    sessionStatus: string;
    currentSongTimeUs: number;
    interimScorePercent: number;
    rawPitchAccuracy: number;
    voicingCoverage: number;
    scoredDurationUs: number;
    activeNoteId: string | null;
  };
}

interface MicDiagnostic {
  deviceName: string;
  totalPcmFrames: number;
  totalPackets: number;
  rmsDbfs: number;
  peakDbfs: number;
  f0Hz: number | null;
  midiNote: number | null;
  periodicity: number;
  state: string; // "NO_PCM" | "SILENCE" | "UNVOICED" | "VOICED"
}

interface TargetNote {
  id: string;
  midi: number;
  name: string;
  startMs: number;
  durationMs: number;
  lyric?: string;
  isDraft?: boolean;
}

interface SongSummary {
  songId: string;
  title: string;
  artist: string;
  durationUs: number;
}

interface MonitorConfig {
  enabled: boolean;
  monitorGainDb: number;
  reverbMix: number;
  echoMix: number;
  echoDelayMs: number;
  eqLowDb: number;
  eqMidDb: number;
  eqHighDb: number;
  compressorThresholdDb: number;
  compressorRatio: number;
  noiseGateThresholdDb: number;
  limiterCeilingDb: number;
}

interface ChartData {
  title: string;
  artist: string;
  durationUs: number;
  notes: Array<{
    id: string;
    pitchMidi: number;
    startUs: number;
    endUs: number;
    extensions?: { status?: string };
  }>;
  lyricTokens?: Array<{
    text: string;
    noteIds: string[];
    startUs: number;
    endUs: number;
  }>;
}

import ScoreEditor from "./components/ScoreEditor.vue";
import AudioAnalyzer from "./components/AudioAnalyzer.vue";

// System & Devices state
const sysInfo = ref<SystemInfo | null>(null);
const devices = ref<AudioDevice[]>([]);
const selectedMicId = ref<string>("");
const selectedRenderId = ref<string>("");

// Active UI Mode: default to "karaoke-session" so user immediately sees singing view and settings
const activeTab = ref<"audio-analyzer" | "score-editor" | "mic-diag" | "synthetic-sync" | "karaoke-session">("karaoke-session");

// Session / Stream State
const isMicTesting = ref(false);
const isSyntheticRunning = ref(false);
const isKaraokeRunning = ref(false);
const isMonitorPreviewRunning = ref(false);
const isPreparingKey = ref(false);

const isRunningAny = computed(() => isMicTesting.value || isSyntheticRunning.value || isKaraokeRunning.value || isMonitorPreviewRunning.value || isPreparingKey.value);

function openScoreEditor() {
  // Editing unmounts the karaoke workspace. Keep the active transport visible
  // until it has been stopped so the user can always reach the stop button.
  if (isRunningAny.value) return;
  activeTab.value = "score-editor";
}

// Real-time Mic Diagnostic Telemetry
const micDiagnostic = ref<MicDiagnostic>({
  deviceName: "未接続",
  totalPcmFrames: 0,
  totalPackets: 0,
  rmsDbfs: -96,
  peakDbfs: -96,
  f0Hz: null,
  midiNote: null,
  periodicity: 0,
  state: "STANDBY",
});
const micSignalLabel = computed(() => {
  switch (micDiagnostic.value.state) {
    case "VOICED": return "歌声音程を検出中";
    case "UNVOICED": return "音声あり（高さを測定できない区間）";
    case "SILENCE": return "無音または入力が小さすぎます";
    case "NO_PCM": return "マイク信号が届いていません";
    default: return "待機中";
  }
});

// Song & Playback State
const currentSongTimeMs = ref<number>(0);
const songDurationMs = ref<number>(276744);
const songTitle = ref<string>("シャイニングスター");
const songArtist = ref<string>("魔王魂");
const songs = ref<SongSummary[]>([]);
const selectedSongId = ref<string>("shining_star");
const isPackageBusy = ref(false);
const isPackageDragOver = ref(false);
const packageStatus = ref("");
const pendingAudioPath = ref("");
const pendingSongTitle = ref("");
const pendingSongArtist = ref("");
let unlistenPackageDrop: (() => void) | null = null;

// Scoring Snapshot
const latestScore = ref<number>(0);
const pitchAccuracy = ref<number>(0);
const voicingCoverage = ref<number>(0);

// Reference Result (NOT Certified)
const referenceResult = ref<{
  totalScore: number;
  pitchAccuracyScore: number;
  voicingCoverageScore: number;
  status: string;
  isDraft: boolean;
} | null>(null);

// Cent Pitch Deviation
const centDiff = ref<number | null>(null);
const centStatus = ref<"perfect" | "sharp" | "flat" | "none">("none");

// Canvas & Loop
const pitchCanvas = ref<HTMLCanvasElement | null>(null);
let pollTimer: number | null = null;
let animFrameId: number | null = null;
let lastPacketLocalTime = 0;
let baseSongTimeMs = 0;

// User Configurable Settings for Karaoke View (Persisted in chart JSON)
const karaokeVocalIntroSec = ref<number>(25.0); // 歌い出し秒数 (直接入力・変更可能)
const karaokeBaseMidi = ref<number>(69); // 基準キー / Base Pitch (デフォルト A4 = 69, 自由に変更可能)
const SINGER_KEY_LIMIT = 6;
const singerKeySemitones = ref<number>(0);
const playbackSpeedRatio = ref<number>(1.0);
const octaveMode = ref<"auto" | "low" | "original" | "high">("auto");
const recordingEnabled = ref(false);
const mixBackingGainDb = ref(-6);
const mixVocalGainDb = ref(0);
const lastRecording = ref<RecordingArtifact | null>(null);
const dspPanelOpen = ref(false);
const monitorConfig = ref<MonitorConfig>({
  enabled: false,
  monitorGainDb: -9,
  reverbMix: 0,
  echoMix: 0,
  echoDelayMs: 180,
  eqLowDb: 0,
  eqMidDb: 0,
  eqHighDb: 0,
  compressorThresholdDb: -18,
  compressorRatio: 1,
  noiseGateThresholdDb: -80,
  limiterCeilingDb: -1,
});

function resetCleanMonitorSettings() {
  monitorConfig.value = {
    enabled: monitorConfig.value.enabled,
    monitorGainDb: -9,
    reverbMix: 0,
    echoMix: 0,
    echoDelayMs: 180,
    eqLowDb: 0,
    eqMidDb: 0,
    eqHighDb: 0,
    compressorThresholdDb: -18,
    compressorRatio: 1,
    noiseGateThresholdDb: -80,
    limiterCeilingDb: -1,
  };
}

async function toggleMonitorPreview() {
  try {
    if (isMonitorPreviewRunning.value) {
      await invoke("stop_monitor_preview");
      isMonitorPreviewRunning.value = false;
      return;
    }

    monitorConfig.value.enabled = true;
    await invoke("start_monitor_preview", {
      micDeviceId: selectedMicId.value.trim() || null,
      renderDeviceId: selectedRenderId.value.trim() || null,
      monitorConfig: monitorConfig.value,
    });
    isMonitorPreviewRunning.value = true;
  } catch (error) {
    isMonitorPreviewRunning.value = false;
    alert("マイク試聴に失敗しました: " + error);
  }
}

function changeSingerKey(delta: number) {
  singerKeySemitones.value = Math.max(-SINGER_KEY_LIMIT, Math.min(SINGER_KEY_LIMIT, singerKeySemitones.value + delta));
}

function displayTargetSemitones(): number {
  return activeTab.value === "karaoke-session"
    ? singerKeySemitones.value + selectedVocalOctaveOffset() * 12
    : 0;
}

function selectedVocalOctaveOffset(): number {
  if (octaveMode.value === "low") return -1;
  if (octaveMode.value === "high") return 1;
  return 0;
}

function usesAutomaticOctaveMatching(): boolean {
  return octaveMode.value === "auto";
}

function shiftKaraokeView(deltaSemi: number) {
  karaokeBaseMidi.value = Math.max(36, Math.min(96, karaokeBaseMidi.value + deltaSemi));
}

// Notes and pitch trails
const targetNotes = ref<TargetNote[]>([]);
const pitchHistory: Array<{ midi: number; voiced: boolean; timeMs: number }> = [];
const displayMidiWindow: number[] = [];
let lastDisplayedMidi: number | null = null;
let micTestStartLocalTime = 0;
let lastMicDisplaySequence = -1;

function resetPitchDisplay() {
  pitchHistory.length = 0;
  displayMidiWindow.length = 0;
  lastDisplayedMidi = null;
}

function appendDisplayPitch(frame: PitchFrame, timeMs: number, historyLimit: number) {
  const flags = frame.qualityFlags;
  const clean = frame.voicing === "voiced"
    && frame.f0Hz !== null
    && frame.f0Hz > 0
    && frame.periodicity >= 0.60
    && !flags.lowConfidence
    && !flags.harmonicAmbiguity
    && !flags.clipping
    && !flags.inputGap
    && !flags.timestampInvalid
    && !flags.warmup
    && !flags.outOfRange;

  if (!clean) {
    pitchHistory.push({ midi: 0, voiced: false, timeMs });
    displayMidiWindow.length = 0;
    lastDisplayedMidi = null;
  } else {
    displayMidiWindow.push(hzToMidi(frame.f0Hz!));
    if (displayMidiWindow.length > 3) displayMidiWindow.shift();
    const ordered = [...displayMidiWindow].sort((a, b) => a - b);
    const rawMidi = ordered[Math.floor(ordered.length / 2)];
    const midi = midiForSingingDisplay(rawMidi, timeMs);
    if (lastDisplayedMidi !== null && Math.abs(midi - lastDisplayedMidi) > 7) {
      pitchHistory.push({ midi: 0, voiced: false, timeMs });
    }
    pitchHistory.push({ midi, voiced: true, timeMs });
    lastDisplayedMidi = midi;
  }
  while (pitchHistory.length > historyLimit) pitchHistory.shift();
}

const noteNames = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
function hzToMidi(hz: number): number {
  return 69 + 12 * Math.log2(hz / 440);
}
function midiToName(midi: number): string {
  const roundMidi = Math.round(midi);
  const note = noteNames[((roundMidi % 12) + 12) % 12];
  const octave = Math.floor(roundMidi / 12) - 1;
  return `${note}${octave}`;
}

function activeTargetMidiAt(timeMs: number): number | null {
  const activeNote = targetNotes.value.find(
    (note) => timeMs >= note.startMs - 80 && timeMs <= note.startMs + note.durationMs + 80
  );
  return activeNote ? activeNote.midi + displayTargetSemitones() : null;
}

function midiForSingingDisplay(rawMidi: number, timeMs: number): number {
  if (activeTab.value !== "karaoke-session" || !usesAutomaticOctaveMatching()) return rawMidi;
  const targetMidi = activeTargetMidiAt(timeMs);
  if (targetMidi === null) return rawMidi;
  const octaveShift = Math.max(-2, Math.min(2, Math.round((targetMidi - rawMidi) / 12)));
  return rawMidi + octaveShift * 12;
}

function formatTime(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
}

const captureDevices = computed(() => devices.value.filter((d) => d.dataFlow.toLowerCase() === "capture"));
const renderDevices = computed(() => devices.value.filter((d) => d.dataFlow.toLowerCase() === "render"));

// Load audio endpoints
async function loadAudioEndpoints() {
  try {
    sysInfo.value = await invoke<SystemInfo>("get_system_info");
    const devs = await invoke<AudioDevice[]>("list_audio_devices");
    devices.value = devs;

    const defaultCapture = devs.find((d) => d.dataFlow.toLowerCase() === "capture" && d.isDefault);
    if (defaultCapture && !selectedMicId.value) {
      selectedMicId.value = defaultCapture.id;
    } else if (!selectedMicId.value) {
      const first = devs.find((d) => d.dataFlow.toLowerCase() === "capture");
      if (first) selectedMicId.value = first.id;
    }

    const defaultRender = devs.find((d) => d.dataFlow.toLowerCase() === "render" && d.isDefault);
    if (defaultRender && !selectedRenderId.value) {
      selectedRenderId.value = defaultRender.id;
    } else if (!selectedRenderId.value) {
      const first = devs.find((d) => d.dataFlow.toLowerCase() === "render");
      if (first) selectedRenderId.value = first.id;
    }
  } catch (e) {
    console.error("Failed to load audio devices:", e);
  }
}

async function loadSongs() {
  songs.value = await invoke<SongSummary[]>("list_songs");
  if (!songs.value.some((song) => song.songId === selectedSongId.value)) {
    selectedSongId.value = songs.value[0]?.songId ?? "";
  }
  if (songs.value.length === 0) {
    songTitle.value = "曲が登録されていません";
    songArtist.value = "MP3または.kpkを追加してください";
    currentSongTimeMs.value = 0;
    songDurationMs.value = 0;
    targetNotes.value = [];
  }
}

async function selectSong() {
  currentSongTimeMs.value = 0;
  referenceResult.value = null;
  await loadChart(false);
}

async function exportSelectedPackage() {
  if (isPackageBusy.value || isRunningAny.value) return;
  isPackageBusy.value = true;
  packageStatus.value = "曲パッケージを書き出しています...";
  try {
    const outputPath = await invoke<string>("export_song_package", { songId: selectedSongId.value });
    packageStatus.value = `書き出しました: ${outputPath}`;
  } catch (error) {
    packageStatus.value = `書き出しエラー: ${error}`;
  } finally {
    isPackageBusy.value = false;
  }
}

async function deleteSelectedSong() {
  if (isPackageBusy.value || isRunningAny.value || !selectedSongId.value) return;
  const selected = songs.value.find((song) => song.songId === selectedSongId.value);
  const label = selected ? `${selected.title} / ${selected.artist}` : selectedSongId.value;
  if (!window.confirm(`「${label}」をライブラリから削除しますか？\nデータは library/trash に退避され、完全消去はされません。`)) return;

  isPackageBusy.value = true;
  packageStatus.value = `${label} を削除しています...`;
  try {
    const trashPath = await invoke<string>("delete_song", { songId: selectedSongId.value });
    await loadSongs();
    if (selectedSongId.value) await loadChart(false);
    referenceResult.value = null;
    lastRecording.value = null;
    packageStatus.value = `削除しました（復元用退避先: ${trashPath}）`;
  } catch (error) {
    packageStatus.value = `削除エラー: ${error}`;
  } finally {
    isPackageBusy.value = false;
  }
}

async function importPackagePath(packagePath: string) {
  if (isPackageBusy.value || isRunningAny.value) return;
  isPackageBusy.value = true;
  packageStatus.value = "曲パッケージを検査・準備しています...";
  try {
    const imported = await invoke<SongSummary>("import_song_package", { packagePath });
    await loadSongs();
    selectedSongId.value = imported.songId;
    await loadChart(false);
    packageStatus.value = `登録しました: ${imported.title} / ${imported.artist}`;
  } catch (error) {
    packageStatus.value = `読み込みエラー: ${error}`;
  } finally {
    isPackageBusy.value = false;
    isPackageDragOver.value = false;
  }
}

const supportedAudioPattern = /\.(mp3|wav|flac|ogg|m4a|aac)$/i;

function prepareAudioImport(audioPath: string) {
  if (isPackageBusy.value || isRunningAny.value) return;
  const filename = audioPath.split(/[\\/]/).pop() || "新しい曲";
  pendingAudioPath.value = audioPath;
  pendingSongTitle.value = filename.replace(/\.[^.]+$/, "");
  pendingSongArtist.value = "";
}

function cancelAudioImport() {
  pendingAudioPath.value = "";
  pendingSongTitle.value = "";
  pendingSongArtist.value = "";
}

async function chooseSongFile() {
  if (isPackageBusy.value || isRunningAny.value) return;
  const selected = await open({
    multiple: false,
    directory: false,
    title: "音声ファイルまたは.kpkを選択",
    filters: [
      { name: "音声・カラオケパッケージ", extensions: ["mp3", "wav", "flac", "ogg", "m4a", "aac", "kpk"] },
    ],
  });
  if (typeof selected !== "string") return;
  if (selected.toLowerCase().endsWith(".kpk")) await importPackagePath(selected);
  else prepareAudioImport(selected);
}

async function confirmAudioImport() {
  if (!pendingAudioPath.value || !pendingSongTitle.value.trim()) return;
  const audioPath = pendingAudioPath.value;
  const title = pendingSongTitle.value.trim();
  const artist = pendingSongArtist.value.trim();

  isPackageBusy.value = true;
  packageStatus.value = "音声をライブラリへ登録しています...";
  try {
    const imported = await invoke<SongSummary>("import_audio_file", {
      audioPath,
      title,
      artist,
    });
    await loadSongs();
    selectedSongId.value = imported.songId;
    await loadChart(false);
    packageStatus.value = `${imported.title} を登録しました。AI音程バーを自動生成しています...`;
    await invoke("run_audio_analysis", {
      force: false,
      songId: imported.songId,
      autoAdopt: true,
    });
    cancelAudioImport();
    activeTab.value = "audio-analyzer";
  } catch (error) {
    packageStatus.value = `音声追加エラー: ${error}`;
  } finally {
    isPackageBusy.value = false;
    isPackageDragOver.value = false;
  }
}

// Load chart data
async function loadChart(isSynthetic: boolean = false) {
  try {
    const cmd = isSynthetic ? "get_synthetic_chart" : "get_current_chart";
    const chart = await invoke<ChartData>(cmd, isSynthetic ? {} : { songId: selectedSongId.value });
    if (chart) {
      songTitle.value = chart.title;
      songArtist.value = chart.artist;
      songDurationMs.value = Math.round(chart.durationUs / 1000);

      const lyricMap: Record<string, string> = {};
      if (chart.lyricTokens) {
        for (const tok of chart.lyricTokens) {
          for (const nid of tok.noteIds) {
            lyricMap[nid] = tok.text;
          }
        }
      }

      targetNotes.value = chart.notes.map((n) => {
        const startMs = Math.round(n.startUs / 1000);
        const endMs = Math.round(n.endUs / 1000);
        const isDraft = n.extensions?.status === "draft";
        return {
          id: n.id,
          midi: n.pitchMidi,
          name: midiToName(n.pitchMidi),
          startMs,
          durationMs: Math.max(50, endMs - startMs),
          lyric: lyricMap[n.id] || "",
          isDraft,
        };
      });

      // Restore user custom vocal intro and base key settings if available
      if ((chart as any).extensions?.vocalIntroMs !== undefined) {
        karaokeVocalIntroSec.value = Number(((chart as any).extensions.vocalIntroMs / 1000.0).toFixed(1));
      }
      if ((chart as any).extensions?.baseKeyMidi !== undefined) {
        karaokeBaseMidi.value = (chart as any).extensions.baseKeyMidi;
      }
    }
  } catch (e) {
    console.warn("Failed to load chart:", e);
  }
}

// Effective playback time derived directly from Audio Core packet timestamps
function getEffectiveNowMs(): number {
  if (isSyntheticRunning.value || isKaraokeRunning.value) {
    if (baseSongTimeMs > 0 && lastPacketLocalTime > 0) {
      const elapsed = performance.now() - lastPacketLocalTime;
      return baseSongTimeMs + elapsed;
    }
    return baseSongTimeMs;
  }
  if (isMicTesting.value && micTestStartLocalTime > 0) {
    return performance.now() - micTestStartLocalTime;
  }
  return 0;
}

// Update cent pitch difference (with Octave Invariant matching §8.2)
function updateCentDifference(currentPitchHz: number | null, nowMs: number) {
  if (!currentPitchHz || currentPitchHz <= 0) {
    centDiff.value = null;
    centStatus.value = "none";
    return;
  }

  const currentMidi = hzToMidi(currentPitchHz);
  const activeTargetMidi = activeTargetMidiAt(nowMs);

  if (activeTargetMidi !== null) {
    let diffSemi = currentMidi - activeTargetMidi;
    if (usesAutomaticOctaveMatching()) {
      while (diffSemi > 6.0) diffSemi -= 12.0;
      while (diffSemi < -6.0) diffSemi += 12.0;
    }
    const diff = diffSemi * 100.0;

    centDiff.value = Math.max(-50, Math.min(50, Math.round(diff)));
    if (Math.abs(diff) <= 20) {
      centStatus.value = "perfect";
    } else if (diff > 20) {
      centStatus.value = "sharp";
    } else {
      centStatus.value = "flat";
    }
  } else {
    const nearest = Math.round(currentMidi);
    const diff = (currentMidi - nearest) * 100;
    centDiff.value = Math.max(-50, Math.min(50, Math.round(diff)));
    if (Math.abs(diff) <= 20) {
      centStatus.value = "perfect";
    } else if (diff > 20) {
      centStatus.value = "sharp";
    } else {
      centStatus.value = "flat";
    }
  }
}

// Real-time Canvas 2D Piano Roll
function renderCanvas() {
  const canvas = pitchCanvas.value;
  if (!canvas) {
    animFrameId = requestAnimationFrame(renderCanvas);
    return;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    animFrameId = requestAnimationFrame(renderCanvas);
    return;
  }

  // Keep the canvas backing store matched to its responsive CSS size. Without
  // this, widening the app stretches a fixed 960 px bitmap and turns circles
  // into ellipses.
  const w = Math.max(1, Math.round(canvas.clientWidth));
  const h = Math.max(1, Math.round(canvas.clientHeight));
  const pixelRatio = Math.max(1, Math.min(2, window.devicePixelRatio || 1));
  const backingWidth = Math.round(w * pixelRatio);
  const backingHeight = Math.round(h * pixelRatio);
  if (canvas.width !== backingWidth || canvas.height !== backingHeight) {
    canvas.width = backingWidth;
    canvas.height = backingHeight;
  }
  ctx.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);

  // Background gradient
  const grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, "#090d16");
  grad.addColorStop(1, "#111827");
  ctx.fillStyle = grad;
  ctx.fillRect(0, 0, w, h);

  // Pitch range centered dynamically around user's configured karaokeBaseMidi
  const minMidi = karaokeBaseMidi.value - 16;
  const maxMidi = karaokeBaseMidi.value + 15;
  const midiRange = maxMidi - minMidi;

  // Pitch grid lines with User Base Key Highlighting (A4 fixed removed)
  for (let m = minMidi; m <= maxMidi; m++) {
    const y = h - ((m - minMidi) / midiRange) * h;
    const isOctaveC = m % 12 === 0;
    const isBase = m === karaokeBaseMidi.value;

    ctx.strokeStyle = isBase
      ? "rgba(234, 179, 8, 0.65)" // Golden line for user base pitch
      : isOctaveC
      ? "rgba(100, 116, 139, 0.4)"
      : "rgba(30, 41, 59, 0.5)";
    ctx.lineWidth = isBase ? 2 : 1;
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();

    if (isOctaveC || isBase) {
      ctx.fillStyle = isBase ? "#eab308" : "#94a3b8";
      ctx.font = isBase ? "bold 11px monospace" : "10px monospace";
      const lbl = isBase ? `★${midiToName(m)}` : midiToName(m);
      ctx.fillText(lbl, 6, y - 3);
    }
  }

  const nowMs = getEffectiveNowMs();
  // Wider windows reveal more timeline instead of stretching the same five
  // seconds across extra pixels. This keeps note-bar proportions familiar.
  const baseWindowMs = isSyntheticRunning.value ? 2500 : 5000;
  const windowMs = Math.max(baseWindowMs, baseWindowMs * (w / 960));
  const playheadX = w * 0.40;

  // 1. Draw Target Pitch Bars (Screen-clipped for max performance)
  if (activeTab.value !== "mic-diag" && targetNotes.value.length > 0) {
    for (const n of targetNotes.value) {
      const noteX = playheadX + ((n.startMs - nowMs) / windowMs) * w;
      const noteW = Math.max(12, (n.durationMs / windowMs) * w);

      // Skip notes completely offscreen
      if (noteX + noteW < -20 || noteX > w + 20) continue;

      const displayedMidi = n.midi + displayTargetSemitones();
      const noteCenterY = h - ((displayedMidi - minMidi) / midiRange) * h;
      const noteH = 20;
      const noteY = noteCenterY - noteH / 2;

      const isCurrent = nowMs >= n.startMs && nowMs <= n.startMs + n.durationMs;
      const isHit = isCurrent && centStatus.value === "perfect";

      if (n.isDraft) {
        ctx.fillStyle = "rgba(148, 163, 184, 0.25)";
        ctx.strokeStyle = "rgba(148, 163, 184, 0.6)";
        ctx.setLineDash([4, 3]);
      } else if (isHit) {
        ctx.fillStyle = "rgba(16, 185, 129, 0.9)";
        ctx.strokeStyle = "#34d399";
        ctx.setLineDash([]);
      } else if (isCurrent) {
        ctx.fillStyle = "rgba(59, 130, 246, 0.8)";
        ctx.strokeStyle = "#93c5fd";
        ctx.setLineDash([]);
      } else {
        ctx.fillStyle = "rgba(37, 99, 235, 0.45)";
        ctx.strokeStyle = "rgba(96, 165, 250, 0.8)";
        ctx.setLineDash([]);
      }

      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.roundRect(noteX, noteY, noteW, noteH, 4);
      ctx.fill();
      ctx.stroke();
      ctx.setLineDash([]);

      // Label
      ctx.fillStyle = n.isDraft ? "#94a3b8" : "#ffffff";
      ctx.font = "bold 10px sans-serif";
      const tag = n.isDraft ? "[仮]" : "";
      const shiftedName = midiToName(displayedMidi);
      const labelText = n.lyric ? `${n.lyric} (${shiftedName})${tag}` : `${shiftedName}${tag}`;
      ctx.fillText(labelText, noteX + 4, noteY + 14);
    }
  }

  // 2. Playhead line (Audio Core timestamp)
  ctx.strokeStyle = "#38bdf8";
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(playheadX, 0);
  ctx.lineTo(playheadX, h);
  ctx.stroke();

  ctx.fillStyle = "#38bdf8";
  ctx.font = "bold 10px sans-serif";
  ctx.fillText("判定線 (Playhead)", playheadX + 4, 14);

  // 3. Pitch History Trail (singing trajectory) - ULTRA-FAST 1-PASS PATH (Fixes 4s lag)
  // Prune history older than the display window to prevent unbounded memory growth
  while (pitchHistory.length > 0 && (nowMs - pitchHistory[0].timeMs) > (windowMs + 1000)) {
    pitchHistory.shift();
  }

  if (pitchHistory.length > 1) {
    ctx.lineWidth = 3;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    ctx.strokeStyle = "#38bdf8";

    ctx.beginPath();
    let isDrawing = false;

    for (let i = 0; i < pitchHistory.length; i++) {
      const p = pitchHistory[i];
      const x = playheadX - ((nowMs - p.timeMs) / windowMs) * w;
      if (x < -20 || x > w + 20) continue;

      if (p.voiced && p.midi > 0) {
        const y = h - ((p.midi - minMidi) / midiRange) * h;
        if (!isDrawing) {
          ctx.moveTo(x, y);
          isDrawing = true;
        } else {
          ctx.lineTo(x, y);
        }
      } else {
        isDrawing = false;
      }
    }
    ctx.stroke();
  }

  // 4. Live vocal cursor follows the display-filtered trail. Scoring still
  // consumes the untouched Rust PitchAnalysisFrame stream.
  const latestDisplayPitch = pitchHistory[pitchHistory.length - 1];
  if (latestDisplayPitch?.voiced && nowMs - latestDisplayPitch.timeMs < 150) {
    const currentMidi = latestDisplayPitch.midi;
    const cursorY = h - ((currentMidi - minMidi) / midiRange) * h;
    const isMatch = centStatus.value === "perfect";

    // Draw raw vocal orb
    ctx.fillStyle = isMatch ? "rgba(52, 211, 153, 0.4)" : "rgba(250, 204, 21, 0.4)";
    ctx.beginPath();
    ctx.arc(playheadX, cursorY, 12, 0, Math.PI * 2);
    ctx.fill();

    ctx.fillStyle = isMatch ? "#34d399" : "#fbbf24";
    ctx.beginPath();
    ctx.arc(playheadX, cursorY, 6, 0, Math.PI * 2);
    ctx.fill();
  }

  animFrameId = requestAnimationFrame(renderCanvas);
}

// -------------------------------------------------------------
// Mode 1: Microphone Isolated Diagnostic
// -------------------------------------------------------------
async function toggleMicTest() {
  if (isMicTesting.value) {
    try {
      await invoke("stop_mic_pipeline");
    } finally {
      isMicTesting.value = false;
      if (pollTimer) clearInterval(pollTimer);
    }
  } else {
    try {
      referenceResult.value = null;
      resetPitchDisplay();
      lastMicDisplaySequence = -1;
      const cleanMicId = selectedMicId.value.trim() ? selectedMicId.value : null;

      await invoke("start_mic_pipeline", { deviceId: cleanMicId });
      micTestStartLocalTime = performance.now();
      isMicTesting.value = true;

      pollTimer = window.setInterval(async () => {
        try {
          const [packet, diag] = await Promise.all([
            invoke<DisplayPacket | null>("poll_display_packet"),
            invoke<MicDiagnostic | null>("poll_mic_diagnostic"),
          ]);
          if (diag) {
            micDiagnostic.value = diag;
            if (diag.f0Hz && diag.midiNote) {
              updateCentDifference(diag.f0Hz, 0);
            } else {
              centDiff.value = null;
              centStatus.value = "none";
            }
          }
          if (packet && packet.sequence !== lastMicDisplaySequence) {
            lastMicDisplaySequence = packet.sequence;
            const last = packet.pitchFrames[packet.pitchFrames.length - 1];
            if (last) {
              appendDisplayPitch(last, performance.now() - micTestStartLocalTime, 600);
            }
          }
        } catch (e) {
          console.error("Mic poll error:", e);
        }
      }, 30);
    } catch (e) {
      alert("マイク単独テスト開始に失敗しました: " + e);
      isMicTesting.value = false;
    }
  }
}

// -------------------------------------------------------------
// Mode 2: Synthetic Audio & Pitch Bar Synchronization Test (2.0s)
// -------------------------------------------------------------
async function toggleSyntheticSession() {
  if (isSyntheticRunning.value) {
    try {
      const res = await invoke<any>("stop_karaoke_session");
      if (res) {
        referenceResult.value = {
          totalScore: res.total_score,
          pitchAccuracyScore: res.pitch_accuracy_score,
          voicingCoverageScore: res.voicing_coverage_score,
          status: res.status,
          isDraft: false,
        };
      }
    } finally {
      isSyntheticRunning.value = false;
      if (pollTimer) clearInterval(pollTimer);
    }
  } else {
    try {
      referenceResult.value = null;
      await loadChart(true);
      resetPitchDisplay();
      baseSongTimeMs = 0;
      lastPacketLocalTime = 0;

      const cleanMic = selectedMicId.value.trim() ? selectedMicId.value : null;
      const cleanRender = selectedRenderId.value.trim() ? selectedRenderId.value : null;

      await invoke("start_synthetic_session", {
        micDeviceId: cleanMic,
        renderDeviceId: cleanRender,
      });

      isSyntheticRunning.value = true;

      pollTimer = window.setInterval(async () => {
        try {
          const [packet, diag] = await Promise.all([
            invoke<DisplayPacket | null>("poll_display_packet"),
            invoke<MicDiagnostic | null>("poll_mic_diagnostic"),
          ]);

          if (diag) {
            micDiagnostic.value = diag;
          }

          if (packet) {
            const songMs = Math.max(0, Math.round(packet.songTimeUs / 1000));
            baseSongTimeMs = songMs;
            lastPacketLocalTime = performance.now();
            currentSongTimeMs.value = songMs;

            latestScore.value = packet.scoreSnapshot.interimScorePercent;
            pitchAccuracy.value = packet.scoreSnapshot.rawPitchAccuracy;
            voicingCoverage.value = packet.scoreSnapshot.voicingCoverage;

            if (packet.pitchFrames && packet.pitchFrames.length > 0) {
              const last = packet.pitchFrames[packet.pitchFrames.length - 1];
              appendDisplayPitch(last, songMs, 600);
              updateCentDifference(last.f0Hz, songMs);
            }

            // Auto finish when synthetic track reaches 2.0s
            if (songMs >= 2000) {
              await toggleSyntheticSession();
            }
          }
        } catch (e) {
          console.error("Synthetic poll error:", e);
        }
      }, 30);
    } catch (e) {
      alert("合成同期テスト開始に失敗しました: " + e);
      isSyntheticRunning.value = false;
    }
  }
}

// -------------------------------------------------------------
// Mode 3: Shining Star Full Karaoke Session (with 25s Intro Skip)
// -------------------------------------------------------------
async function startKaraokeWithOffset(startOffsetSecs: number) {
  if (isKaraokeRunning.value) {
    try {
      const res = await invoke<any>("stop_karaoke_session");
      lastRecording.value = await invoke<RecordingArtifact | null>("get_last_recording");
      if (res) {
        referenceResult.value = {
          totalScore: res.total_score,
          pitchAccuracyScore: res.pitch_accuracy_score,
          voicingCoverageScore: res.voicing_coverage_score,
          status: res.status,
          isDraft: true,
        };
      }
    } finally {
      isKaraokeRunning.value = false;
      if (pollTimer) clearInterval(pollTimer);
    }
  } else {
    try {
      referenceResult.value = null;
      lastRecording.value = null;
      await loadChart(false);
      resetPitchDisplay();
      baseSongTimeMs = Math.round(startOffsetSecs * 1000);
      lastPacketLocalTime = 0;

      const cleanMic = selectedMicId.value.trim() ? selectedMicId.value : null;
      const cleanRender = selectedRenderId.value.trim() ? selectedRenderId.value : null;

      isPreparingKey.value = true;
      try {
        await invoke("start_karaoke_session", {
          micDeviceId: cleanMic,
          renderDeviceId: cleanRender,
          startOffsetSecs: startOffsetSecs,
          songId: selectedSongId.value,
          keySemitones: singerKeySemitones.value,
          vocalOctaveOffset: selectedVocalOctaveOffset(),
          speedRatio: playbackSpeedRatio.value,
          octaveTolerance: usesAutomaticOctaveMatching(),
          monitorConfig: monitorConfig.value,
          recordingEnabled: recordingEnabled.value,
          mixBackingGainDb: mixBackingGainDb.value,
          mixVocalGainDb: mixVocalGainDb.value,
        });
      } finally {
        isPreparingKey.value = false;
      }

      isKaraokeRunning.value = true;

      pollTimer = window.setInterval(async () => {
        try {
          const [packet, diag] = await Promise.all([
            invoke<DisplayPacket | null>("poll_display_packet"),
            invoke<MicDiagnostic | null>("poll_mic_diagnostic"),
          ]);

          if (diag) {
            micDiagnostic.value = diag;
          }

          if (packet) {
            const songMs = Math.max(0, Math.round(packet.songTimeUs / 1000));
            baseSongTimeMs = songMs;
            lastPacketLocalTime = performance.now();
            currentSongTimeMs.value = songMs;

            latestScore.value = packet.scoreSnapshot.interimScorePercent;
            pitchAccuracy.value = packet.scoreSnapshot.rawPitchAccuracy;
            voicingCoverage.value = packet.scoreSnapshot.voicingCoverage;

            if (packet.pitchFrames && packet.pitchFrames.length > 0) {
              const last = packet.pitchFrames[packet.pitchFrames.length - 1];
              appendDisplayPitch(last, songMs, 800);
              updateCentDifference(last.f0Hz, songMs);
            }
          }
        } catch (e) {
          console.error("Karaoke poll error:", e);
        }
      }, 30);
    } catch (e) {
      alert("歌唱セッション開始に失敗しました: " + e);
      isKaraokeRunning.value = false;
      isPreparingKey.value = false;
    }
  }
}

onMounted(async () => {
  await loadAudioEndpoints();
  await loadSongs();
  await loadChart(false);
  animFrameId = requestAnimationFrame(renderCanvas);
  unlistenPackageDrop = await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "enter") {
      isPackageDragOver.value = event.payload.paths.some((path) => path.toLowerCase().endsWith(".kpk") || supportedAudioPattern.test(path));
    } else if (event.payload.type === "leave") {
      isPackageDragOver.value = false;
    } else if (event.payload.type === "drop") {
      isPackageDragOver.value = false;
      const packages = event.payload.paths.filter((path) => path.toLowerCase().endsWith(".kpk"));
      const audioFiles = event.payload.paths.filter((path) => supportedAudioPattern.test(path));
      if (packages.length === 1 && audioFiles.length === 0) void importPackagePath(packages[0]);
      else if (audioFiles.length === 1 && packages.length === 0) prepareAudioImport(audioFiles[0]);
      else if (packages.length + audioFiles.length > 1) packageStatus.value = "曲は一度に1ファイルずつ追加してください";
    }
  });
});

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer);
  if (animFrameId) cancelAnimationFrame(animFrameId);
  unlistenPackageDrop?.();
  invoke("stop_mic_pipeline");
  invoke("stop_karaoke_session");
  invoke("stop_monitor_preview");
});
</script>

<template>
  <main class="karaoke-container">
    <div v-if="pendingAudioPath" class="import-modal-backdrop">
      <section class="import-modal card" role="dialog" aria-modal="true" aria-label="新しい曲を登録">
        <h2>🎵 新しい曲を登録</h2>
        <p class="import-source">{{ pendingAudioPath.split(/[\\/]/).pop() }}</p>
        <label>
          <span>曲名</span>
          <input v-model="pendingSongTitle" class="setting-input import-text-input" maxlength="200" autofocus />
        </label>
        <label>
          <span>歌手名</span>
          <input v-model="pendingSongArtist" class="setting-input import-text-input" maxlength="200" placeholder="不明なら空欄でOK" />
        </label>
        <p>登録後、自動でボーカル分離・音程バー生成・.kpk保存を開始します。</p>
        <div class="action-bar">
          <button class="btn btn-secondary" :disabled="isPackageBusy" @click="cancelAudioImport">キャンセル</button>
          <button class="btn btn-primary" :disabled="isPackageBusy || !pendingSongTitle.trim()" @click="confirmAudioImport">
            {{ isPackageBusy ? "登録中..." : "登録してAI解析を開始" }}
          </button>
        </div>
      </section>
    </div>
    <!-- Top Header -->
    <header class="app-header">
      <div class="header-left">
        <h1>KARAOKE STUDIO <span class="badge-audit">AUDIT MODE</span></h1>
        <p class="subtitle">WASAPI Direct Low-Latency Audio Engine & Step-by-Step Diagnostic Suite</p>
      </div>
      <div class="header-right" v-if="sysInfo">
        <span class="info-pill">QPC {{ (sysInfo.qpcFrequencyHz / 1_000_000).toFixed(1) }} MHz</span>
        <span class="info-pill">Tick {{ sysInfo.qpcResolutionNs.toFixed(1) }} ns</span>
        <span class="info-pill">Protocol v{{ sysInfo.protocolVersion }}</span>
      </div>
    </header>

    <!-- Audio Device Selection (Locked during playback) -->
    <section class="device-section card">
      <div class="device-row">
        <div class="device-item">
          <label for="mic-select">🎤 入力マイク (Input):</label>
          <select
            id="mic-select"
            v-model="selectedMicId"
            :disabled="isRunningAny"
            class="device-select"
          >
            <option value="">(システム既定の録音デバイス)</option>
            <option v-for="d in captureDevices" :key="d.id" :value="d.id">
              {{ d.name }} {{ d.isDefault ? "[既定]" : "" }}
            </option>
          </select>
        </div>

        <div class="device-item">
          <label for="render-select">🎧 出力スピーカー (Output):</label>
          <select
            id="render-select"
            v-model="selectedRenderId"
            :disabled="isRunningAny"
            class="device-select"
          >
            <option value="">(システム既定の再生デバイス)</option>
            <option v-for="d in renderDevices" :key="d.id" :value="d.id">
              {{ d.name }} {{ d.isDefault ? "[既定]" : "" }}
            </option>
          </select>
        </div>

        <button
          class="btn btn-secondary refresh-btn"
          @click="loadAudioEndpoints"
          :disabled="isRunningAny"
        >
          🔄 再読込
        </button>
      </div>
      <div
        class="package-row audio-import-dropzone"
        :class="{ 'package-drag-over': isPackageDragOver }"
      >
        <div class="package-drop-copy">
          <strong>⬇ ここにMP3などの音声ファイルをドロップ</strong>
          <span>曲名・歌手名を確認するだけで、AI音程バー生成後に library/packages へ.kpk保存します（既存.kpkも対応）</span>
        </div>
        <button class="btn btn-primary package-select-btn" :disabled="isRunningAny || isPackageBusy" @click="chooseSongFile">
          📂 音声ファイルを選ぶ
        </button>
        <button
          class="btn btn-secondary package-export-btn"
          :disabled="isRunningAny || isPackageBusy || !selectedSongId"
          @click="exportSelectedPackage"
        >
          {{ isPackageBusy ? "処理中..." : "選択中の曲を.kpk書出し" }}
        </button>
        <button
          class="btn btn-danger package-delete-btn"
          :disabled="isRunningAny || isPackageBusy || !selectedSongId"
          @click="deleteSelectedSong"
        >
          🗑 選択中の曲を削除
        </button>
        <span v-if="packageStatus" class="package-status">{{ packageStatus }}</span>
      </div>
      <div v-if="isRunningAny" class="device-lock-hint">
        🔒 実行中はオーディオ設定が保護されています。変更するには停止してください。
      </div>
    </section>

    <!-- Verification & Karaoke Mode Tabs -->
    <nav class="mode-tabs">
      <button
        class="tab-btn karaoke-tab-btn"
        :class="{ active: activeTab === 'karaoke-session' }"
        @click="activeTab = 'karaoke-session'"
        :disabled="isRunningAny && !isKaraokeRunning && !isPreparingKey && activeTab !== 'karaoke-session'"
      >
        <span class="tab-step">歌唱</span>
        <span class="tab-title">🎤 カラオケ歌唱</span>
      </button>

      <button
        class="tab-btn editor-tab-btn"
        :class="{ active: activeTab === 'score-editor' }"
        @click="openScoreEditor"
        :disabled="isRunningAny || !selectedSongId"
      >
        <span class="tab-step">エディタ</span>
        <span class="tab-title">🎼 譜面エディタ (波形・F0・ノーツ編集)</span>
      </button>

      <button
        class="tab-btn analyzer-tab-btn"
        :class="{ active: activeTab === 'audio-analyzer' }"
        @click="activeTab = 'audio-analyzer'"
        :disabled="isRunningAny || !selectedSongId"
      >
        <span class="tab-step">AI解析</span>
        <span class="tab-title">🎙️ 音声再解析スタジオ</span>
      </button>

      <button
        class="tab-btn"
        :class="{ active: activeTab === 'mic-diag' }"
        @click="activeTab = 'mic-diag'"
        :disabled="isRunningAny && activeTab !== 'mic-diag'"
      >
        <span class="tab-step">診断 1</span>
        <span class="tab-title">🎤 マイク単体診断</span>
      </button>

      <button
        class="tab-btn"
        :class="{ active: activeTab === 'synthetic-sync' }"
        @click="activeTab = 'synthetic-sync'"
        :disabled="isRunningAny && activeTab !== 'synthetic-sync'"
      >
        <span class="tab-step">診断 2</span>
        <span class="tab-title">🧪 合成同期テスト</span>
      </button>
    </nav>

    <!-- Audio Analyzer Mode -->
    <div v-if="activeTab === 'audio-analyzer'" class="analyzer-view-wrapper">
      <AudioAnalyzer
        :song-id="selectedSongId"
        :song-title="songTitle"
        :song-artist="songArtist"
        @openEditor="openScoreEditor"
        @openKaraoke="activeTab = 'karaoke-session'"
      />
    </div>

    <!-- Main Score Editor Mode -->
    <div v-else-if="activeTab === 'score-editor'" class="editor-view-wrapper">
      <ScoreEditor :song-id="selectedSongId" @requestAnalysis="activeTab = 'audio-analyzer'" />
    </div>

    <!-- Main Workspace Area -->
    <div v-else class="workspace-layout">
      <!-- Left / Main Card: Interactive Canvas & Action Controls -->
      <section class="card main-view-card">
        <!-- Mode 1: Mic Isolated Diagnostics Controls -->
        <div v-if="activeTab === 'mic-diag'" class="tab-content">
          <div class="content-header">
            <h3>マイク入力の単独復旧と信号切り分け</h3>
            <p class="desc">
              音源再生を止め、マイク入力PCMの到達、無音/有声の判別、YINピッチ検出が正しく動いているか単独で確認します。
            </p>
          </div>

          <div class="action-bar">
            <button
              class="btn"
              :class="isMicTesting ? 'btn-danger' : 'btn-primary'"
              @click="toggleMicTest"
            >
              {{ isMicTesting ? "⏹ マイクテスト停止" : "🎤 マイク単体テスト開始" }}
            </button>
            <div class="setting-group mic-range-setting">
              <label class="setting-label">🎹 表示音域の中心:</label>
              <select v-model.number="karaokeBaseMidi" class="setting-select" title="マイク診断の表示範囲だけを変更します">
                <option :value="48">C3 (低い声)</option>
                <option :value="53">F3</option>
                <option :value="57">A3</option>
                <option :value="60">C4 (中央ド)</option>
                <option :value="65">F4</option>
                <option :value="69">A4 (440Hz)</option>
                <option :value="72">C5</option>
              </select>
              <button class="btn btn-secondary btn-xs" @click="shiftKaraokeView(-1)">↓ -1</button>
              <button class="btn btn-secondary btn-xs" @click="shiftKaraokeView(1)">↑ +1</button>
            </div>
          </div>
          <p class="mic-signal-summary" :class="`state-${micDiagnostic.state.toLowerCase()}`">
            {{ micSignalLabel }}
          </p>
        </div>

        <!-- Mode 2: Synthetic Sync Test Controls -->
        <div v-if="activeTab === 'synthetic-sync'" class="tab-content">
          <div class="content-header">
            <h3>既知の合成音源と音程バーの厳密な同期検証</h3>
            <p class="desc">
              440Hz(A4)の純音（0.50s〜0.55s [50ms] および 1.00s〜1.50s [500ms]）を再生し、画面上の音程バーが判定線に突入する瞬間と音が完全に一致することを確認します。
            </p>
          </div>

          <div class="action-bar">
            <button
              class="btn"
              :class="isSyntheticRunning ? 'btn-danger' : 'btn-success'"
              @click="toggleSyntheticSession"
            >
              {{ isSyntheticRunning ? "⏹ 合成テスト停止" : "▶ 合成同期テスト開始 (2.0秒)" }}
            </button>
          </div>
        </div>

        <!-- Mode 3: Selected Song Karaoke Controls -->
        <div v-if="activeTab === 'karaoke-session'" class="tab-content">
          <div class="content-header">
            <div class="song-meta">
              <h3>{{ songTitle }} - {{ songArtist }}</h3>
              <span class="playback-time">
                {{ formatTime(currentSongTimeMs) }} / {{ formatTime(songDurationMs) }}
              </span>
            </div>
            <p class="desc">
              原譜を変更せず、歌い手キーに合わせて伴奏・音程バー・採点基準を同じ半音数だけ移調します。
            </p>
          </div>

          <div v-if="songs.length === 0" class="empty-library-cta">
            <strong>まだ曲がありません</strong>
            <span>MP3・OGGなどを選ぶと、曲名登録からAI音程バー生成、.kpk保存まで自動で進みます。</span>
            <button class="btn btn-primary btn-lg" @click="chooseSongFile">
              📂 最初の音声ファイルを選ぶ
            </button>
          </div>

          <!-- User Settings Control Bar (Vocal Intro & Base Key / Transpose) -->
          <div class="user-settings-bar">
            <div class="setting-group">
              <label class="setting-label">🎵 楽曲:</label>
              <select
                v-model="selectedSongId"
                class="setting-select"
                :disabled="isRunningAny"
                @change="selectSong"
                title="登録済みの楽曲から歌唱対象を選択"
              >
                <option v-for="song in songs" :key="song.songId" :value="song.songId">
                  {{ song.title }} / {{ song.artist }}
                </option>
              </select>
            </div>

            <!-- 1. Vocal Intro Config -->
            <div class="setting-group">
              <label class="setting-label">⏱ 歌い出し秒数:</label>
              <input
                type="number"
                v-model.number="karaokeVocalIntroSec"
                step="0.5"
                min="0"
                max="600"
                class="setting-input"
                title="歌い出し秒数を自由に変更できます"
              />
              <span class="setting-unit">秒</span>
            </div>

            <div class="setting-group">
              <label class="setting-label">🎤 歌い手キー:</label>
              <div class="btn-group-key">
                <button class="btn btn-secondary btn-xs" @click="changeSingerKey(-1)" :disabled="isRunningAny || singerKeySemitones <= -SINGER_KEY_LIMIT" title="伴奏・音程バー・採点基準を半音下げる">
                  −
                </button>
                <strong class="key-value">{{ singerKeySemitones > 0 ? `+${singerKeySemitones}` : singerKeySemitones }}</strong>
                <button class="btn btn-secondary btn-xs" @click="changeSingerKey(1)" :disabled="isRunningAny || singerKeySemitones >= SINGER_KEY_LIMIT" title="伴奏・音程バー・採点基準を半音上げる">
                  ＋
                </button>
                <button class="btn btn-secondary btn-xs" @click="singerKeySemitones = 0" :disabled="isRunningAny || singerKeySemitones === 0">
                  原曲
                </button>
              </div>
              <span class="setting-unit">半音（-6〜+6／カラオケ調整）</span>
            </div>

            <div class="setting-group speed-setting">
              <label class="setting-label">⏩ 練習速度:</label>
              <input
                v-model.number="playbackSpeedRatio"
                type="range"
                min="0.7"
                max="1.3"
                step="0.05"
                :disabled="isRunningAny || isPreparingKey"
                title="音程を変えずに伴奏・音程バー・採点時刻を同じ速度へ変更します"
              />
              <strong class="speed-value">{{ playbackSpeedRatio.toFixed(2) }}×</strong>
              <button
                class="btn btn-secondary btn-xs"
                :disabled="isRunningAny || playbackSpeedRatio === 1"
                @click="playbackSpeedRatio = 1"
              >
                標準
              </button>
            </div>

            <div class="setting-group scoring-mode-setting" role="group" aria-label="歌唱オクターブ">
              <span class="setting-label">🎙 歌唱オクターブ:</span>
              <button
                class="btn btn-xs"
                :class="octaveMode === 'auto' ? 'btn-primary' : 'btn-secondary'"
                :aria-pressed="octaveMode === 'auto'"
                :disabled="isRunningAny"
                title="同じ音名なら上下2オクターブまで自動で譜面へ合わせます。伴奏と実際の声は変えません"
                @click="octaveMode = 'auto'"
              >
                自動（おすすめ）
              </button>
              <button
                class="btn btn-xs"
                :class="octaveMode === 'low' ? 'btn-primary' : 'btn-secondary'"
                :aria-pressed="octaveMode === 'low'"
                :disabled="isRunningAny"
                title="伴奏を変えず、音程バーと採点目標を1オクターブ下げます"
                @click="octaveMode = 'low'"
              >
                低い声（-1oct）
              </button>
              <button
                class="btn btn-xs"
                :class="octaveMode === 'original' ? 'btn-primary' : 'btn-secondary'"
                :aria-pressed="octaveMode === 'original'"
                :disabled="isRunningAny"
                title="原譜と同じオクターブだけを採点します"
                @click="octaveMode = 'original'"
              >原譜</button>
              <button
                class="btn btn-xs"
                :class="octaveMode === 'high' ? 'btn-primary' : 'btn-secondary'"
                :aria-pressed="octaveMode === 'high'"
                :disabled="isRunningAny"
                title="伴奏を変えず、音程バーと採点目標を1オクターブ上げます"
                @click="octaveMode = 'high'"
              >高い声（+1oct）</button>
              <span class="setting-unit">伴奏音は変えません</span>
            </div>

            <!-- 2. Base Key / Transpose Config -->
            <div class="setting-group">
              <label class="setting-label">🎹 表示音域の中心:</label>
              <select v-model.number="karaokeBaseMidi" class="setting-select" title="ピアノロールの表示中心だけを変更します。伴奏や採点キーは変わりません">
                <option :value="48">C3 (48)</option>
                <option :value="53">F3 (53)</option>
                <option :value="57">A3 (57)</option>
                <option :value="60">C4 (60 - 中央ド)</option>
                <option :value="65">F4 (65)</option>
                <option :value="69">A4 (69 - 基準440Hz)</option>
                <option :value="72">C5 (72)</option>
                <option :value="77">F5 (77)</option>
              </select>
              <div class="btn-group-key">
                <button class="btn btn-secondary btn-xs" @click="shiftKaraokeView(-1)" title="表示範囲を半音下へ移動">
                  ↓ -1
                </button>
                <button class="btn btn-secondary btn-xs" @click="shiftKaraokeView(1)" title="表示範囲を半音上へ移動">
                  ↑ +1
                </button>
              </div>
            </div>

            <!-- 3. Direct Jump to Editor -->
            <button
              class="btn btn-secondary btn-sm edit-jump-btn"
              @click="openScoreEditor"
              :disabled="isRunningAny"
              title="視覚的音程バーエディタで譜面をマウス編集"
            >
              {{ isRunningAny ? "⏹ 再生・試聴の停止後に編集" : "✏️ 音程バーを編集する" }}
            </button>
          </div>

          <section class="vocal-dsp-panel" :class="{ enabled: monitorConfig.enabled }">
            <div class="dsp-panel-header">
              <label class="monitor-toggle">
                <input v-model="monitorConfig.enabled" type="checkbox" :disabled="isRunningAny" />
                <strong>🎧 自分の声をヘッドフォンへ返す</strong>
              </label>
              <span class="dsp-safety">スピーカー使用時はOFF推奨（ハウリング防止）</span>
              <button class="btn btn-secondary btn-xs" @click="dspPanelOpen = !dspPanelOpen">
                {{ dspPanelOpen ? "詳細を閉じる" : "声のエフェクト調整" }}
              </button>
              <button
                class="btn btn-xs"
                :class="isMonitorPreviewRunning ? 'btn-danger' : 'btn-primary'"
                :disabled="isMicTesting || isSyntheticRunning || isKaraokeRunning || isPreparingKey"
                @click="toggleMonitorPreview"
              >
                {{ isMonitorPreviewRunning ? "⏹ 試聴停止" : "🎙 声だけ試聴" }}
              </button>
              <button class="btn btn-secondary btn-xs" :disabled="isRunningAny" @click="resetCleanMonitorSettings">
                原音設定へ戻す
              </button>
            </div>

            <div v-if="monitorConfig.enabled || dspPanelOpen" class="dsp-controls">
              <label class="dsp-control">
                <span>返し音量 <b>{{ monitorConfig.monitorGainDb }} dB</b></span>
                <input v-model.number="monitorConfig.monitorGainDb" type="range" min="-36" max="0" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>リバーブ <b>{{ Math.round(monitorConfig.reverbMix * 100) }}%</b></span>
                <input v-model.number="monitorConfig.reverbMix" type="range" min="0" max="0.6" step="0.01" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>エコー <b>{{ Math.round(monitorConfig.echoMix * 100) }}%</b></span>
                <input v-model.number="monitorConfig.echoMix" type="range" min="0" max="0.5" step="0.01" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>エコー間隔 <b>{{ monitorConfig.echoDelayMs }} ms</b></span>
                <input v-model.number="monitorConfig.echoDelayMs" type="range" min="40" max="500" step="10" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>EQ 低音 <b>{{ monitorConfig.eqLowDb > 0 ? "+" : "" }}{{ monitorConfig.eqLowDb }} dB</b></span>
                <input v-model.number="monitorConfig.eqLowDb" type="range" min="-12" max="12" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>EQ 中音 <b>{{ monitorConfig.eqMidDb > 0 ? "+" : "" }}{{ monitorConfig.eqMidDb }} dB</b></span>
                <input v-model.number="monitorConfig.eqMidDb" type="range" min="-12" max="12" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>EQ 高音 <b>{{ monitorConfig.eqHighDb > 0 ? "+" : "" }}{{ monitorConfig.eqHighDb }} dB</b></span>
                <input v-model.number="monitorConfig.eqHighDb" type="range" min="-12" max="12" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>コンプ開始 <b>{{ monitorConfig.compressorThresholdDb }} dB</b></span>
                <input v-model.number="monitorConfig.compressorThresholdDb" type="range" min="-40" max="0" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>コンプ比率 <b>{{ monitorConfig.compressorRatio.toFixed(1) }}:1</b></span>
                <input v-model.number="monitorConfig.compressorRatio" type="range" min="1" max="12" step="0.5" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>ノイズゲート <b>{{ monitorConfig.noiseGateThresholdDb }} dB</b></span>
                <input v-model.number="monitorConfig.noiseGateThresholdDb" type="range" min="-80" max="-20" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>リミッター <b>{{ monitorConfig.limiterCeilingDb }} dB</b></span>
                <input v-model.number="monitorConfig.limiterCeilingDb" type="range" min="-12" max="0" step="1" :disabled="isRunningAny" />
              </label>
            </div>
            <p v-if="monitorConfig.enabled" class="monitor-note">
              {{ isMonitorPreviewRunning ? "曲を流さずマイク音だけを試聴中です。設定変更は一度停止してから行ってください。" : "設定は次回の再生開始時に適用されます。採点には加工前のマイク原音を使用します。" }}
            </p>
          </section>

          <section class="recording-panel" :class="{ enabled: recordingEnabled }">
            <label class="recording-toggle">
              <input v-model="recordingEnabled" type="checkbox" :disabled="isRunningAny" />
              <strong>🎙 マイク原音を録音する</strong>
            </label>
            <span>歌唱開始前にONにした場合だけ、24bit WAVとして保存します。</span>
            <div v-if="recordingEnabled" class="mix-levels">
              <label class="dsp-control">
                <span>MIX 伴奏音量 <b>{{ mixBackingGainDb > 0 ? "+" : "" }}{{ mixBackingGainDb }} dB</b></span>
                <input v-model.number="mixBackingGainDb" type="range" min="-24" max="6" step="1" :disabled="isRunningAny" />
              </label>
              <label class="dsp-control">
                <span>MIX 歌声音量 <b>{{ mixVocalGainDb > 0 ? "+" : "" }}{{ mixVocalGainDb }} dB</b></span>
                <input v-model.number="mixVocalGainDb" type="range" min="-24" max="6" step="1" :disabled="isRunningAny" />
              </label>
              <small>master.wav だけに反映します。Raw と Wet は変えません。</small>
            </div>
            <div v-if="lastRecording" class="recording-result" :class="lastRecording.status">
              <b>{{ lastRecording.status === "complete" && lastRecording.mixStatus === "complete" ? "Raw・Wet・MIXを保存しました" : "録音またはMIXを確認してください" }}</b>
              <span>{{ lastRecording.sampleRate }} Hz / 24bit / {{ lastRecording.sampleFrames.toLocaleString() }} frames</span>
              <button class="btn btn-secondary btn-xs" @click="revealItemInDir(lastRecording.masterWavPath || lastRecording.rawWavPath)">
                保存場所を開く
              </button>
            </div>
          </section>

          <!-- Playback Action Buttons -->
          <div class="action-bar flex-gap">
            <button
              class="btn"
              :class="isKaraokeRunning ? 'btn-danger' : 'btn-primary'"
              @click="startKaraokeWithOffset(0)"
              :disabled="isPreparingKey || (!isKaraokeRunning && !selectedSongId)"
            >
              {{ isPreparingKey ? "⏳ キー音源を準備中…" : isKaraokeRunning ? "⏹ 歌唱停止" : "▶ 最初から再生 (0秒〜)" }}
            </button>
            <button
              class="btn btn-warning"
              @click="startKaraokeWithOffset(karaokeVocalIntroSec)"
              :disabled="isKaraokeRunning || isPreparingKey || !selectedSongId"
              :title="`イントロをスキップし、設定された歌い出し (${karaokeVocalIntroSec}秒) から開始`"
            >
              ⏩ 歌い出し ({{ karaokeVocalIntroSec.toFixed(1) }}秒) から開始
            </button>
          </div>
        </div>

        <!-- 60fps Real-Time Piano Roll Canvas -->
        <div class="canvas-container">
          <canvas ref="pitchCanvas" width="960" height="220" class="pitch-canvas"></canvas>
        </div>

        <!-- Cent Pitch Deviation Bar -->
        <div class="cent-bar-wrapper">
          <div class="cent-labels">
            <span>-50¢ (LOW)</span>
            <span class="cent-status" :class="centStatus">
              {{ centStatus === "perfect" ? "✨ PERFECT MATCH ✨" : centStatus === "sharp" ? "♯ HIGH" : centStatus === "flat" ? "♭ LOW" : "--" }}
            </span>
            <span>+50¢ (HIGH)</span>
          </div>
          <div class="cent-track">
            <div class="cent-zero-line"></div>
            <div
              v-if="centDiff !== null"
              class="cent-marker"
              :class="centStatus"
              :style="{ left: 50 + centDiff + '%' }"
            ></div>
          </div>
        </div>

        <!-- Reference Score Box (Non-Certified) -->
        <div v-if="referenceResult" class="reference-result-box">
          <div class="result-title">
            <span>📝 参考スコア結果 (※仮譜面・テスト検証用 参考記録)</span>
            <span class="status-tag">{{ referenceResult.status }}</span>
          </div>
          <div class="result-stats">
            <div class="stat-cell">
              <span class="stat-name">参考総合点</span>
              <span class="stat-val highlight">{{ referenceResult.totalScore.toFixed(3) }}</span>
            </div>
            <div class="stat-cell">
              <span class="stat-name">音程正確率 (A)</span>
              <span class="stat-val">{{ referenceResult.pitchAccuracyScore.toFixed(2) }}%</span>
            </div>
            <div class="stat-cell">
              <span class="stat-name">発声カバー率 (C)</span>
              <span class="stat-val">{{ referenceResult.voicingCoverageScore.toFixed(2) }}%</span>
            </div>
          </div>
          <p class="result-note">
            ※本結果は未確定の仮譜面またはテスト区間における参考評価であり、正式認定結果ではありません。
          </p>
        </div>
      </section>

      <!-- Right Card: Live Microphone Diagnostic Telemetry Panel -->
      <aside class="card telemetry-card">
        <h2>マイク入力 リアルタイム診断</h2>
        <p class="telemetry-subtitle">WASAPI Capture 内部テレメトリ (§18.2)</p>

        <div class="telemetry-grid">
          <!-- State Label Badge -->
          <div class="telemetry-row highlight-row">
            <span class="t-label">状態判別:</span>
            <span
              class="state-badge"
              :class="{
                'state-voiced': micDiagnostic.state === 'VOICED',
                'state-unvoiced': micDiagnostic.state === 'UNVOICED',
                'state-silence': micDiagnostic.state === 'SILENCE',
                'state-nopcm': micDiagnostic.state === 'NO_PCM' || micDiagnostic.state === 'STANDBY',
              }"
            >
              [{{ micDiagnostic.state }}]
            </span>
          </div>

          <!-- Open Device -->
          <div class="telemetry-row">
            <span class="t-label">開かれたデバイス:</span>
            <span class="t-val t-device" :title="micDiagnostic.deviceName">
              {{ micDiagnostic.deviceName }}
            </span>
          </div>

          <!-- Packet & Frame Count -->
          <div class="telemetry-row">
            <span class="t-label">受信パケット数:</span>
            <span class="t-val">{{ micDiagnostic.totalPackets.toLocaleString() }} pkts</span>
          </div>
          <div class="telemetry-row">
            <span class="t-label">総PCMフレーム数:</span>
            <span class="t-val">{{ micDiagnostic.totalPcmFrames.toLocaleString() }} smpls</span>
          </div>

          <!-- RMS Level -->
          <div class="telemetry-row">
            <span class="t-label">RMS 音量 (dBFS):</span>
            <span class="t-val">{{ micDiagnostic.rmsDbfs.toFixed(1) }} dBFS</span>
          </div>
          <div class="level-meter-bar">
            <div
              class="level-fill"
              :style="{ width: Math.min(100, Math.max(0, (micDiagnostic.rmsDbfs + 60) * 1.66)) + '%' }"
            ></div>
          </div>

          <!-- Peak Level -->
          <div class="telemetry-row">
            <span class="t-label">Peak 音量 (dBFS):</span>
            <span class="t-val">{{ micDiagnostic.peakDbfs.toFixed(1) }} dBFS</span>
          </div>

          <!-- Pitch F0 & MIDI -->
          <div class="telemetry-row">
            <span class="t-label">推定周波数 (F0):</span>
            <span class="t-val t-pitch" v-if="micDiagnostic.f0Hz">
              {{ micDiagnostic.f0Hz.toFixed(1) }} Hz
            </span>
            <span class="t-val muted" v-else>--</span>
          </div>
          <div class="telemetry-row">
            <span class="t-label">音階 (MIDI Note):</span>
            <span class="t-val t-pitch" v-if="micDiagnostic.midiNote">
              {{ midiToName(micDiagnostic.midiNote) }} ({{ micDiagnostic.midiNote.toFixed(1) }})
            </span>
            <span class="t-val muted" v-else>--</span>
          </div>

          <!-- Periodicity / Confidence -->
          <div class="telemetry-row">
            <span class="t-label">周期性 (Periodicity):</span>
            <span class="t-val">{{ (micDiagnostic.periodicity * 100).toFixed(1) }}%</span>
          </div>
          <div class="level-meter-bar">
            <div
              class="level-fill periodicity-fill"
              :style="{ width: micDiagnostic.periodicity * 100 + '%' }"
            ></div>
          </div>
        </div>

        <!-- Diagnostic Flow Help -->
        <div class="diag-help-box">
          <div class="help-title">💡 診断の見方</div>
          <ul>
            <li><strong>[NO_PCM]</strong>: パケットなし → デバイスが開かれていない</li>
            <li><strong>[SILENCE]</strong>: パケットありだが音量小 → マイクミュート等</li>
            <li><strong>[UNVOICED]</strong>: 音は入るがピッチなし → ノイズ・息</li>
            <li><strong>[VOICED]</strong>: 有声ピッチ検出中 → 正常動作</li>
          </ul>
        </div>
      </aside>
    </div>
  </main>
</template>

<style scoped>
.karaoke-container {
  width: 100%;
  max-width: 1680px;
  margin: 0 auto;
  padding: 16px 24px;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Hiragino Kaku Gothic ProN", sans-serif;
  color: #f1f5f9;
  background: #090e1a;
  min-height: 100vh;
  box-sizing: border-box;
}

.app-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  border-bottom: 1px solid rgba(51, 65, 85, 0.6);
  padding-bottom: 12px;
  margin-bottom: 16px;
}

.header-left h1 {
  font-size: 20px;
  margin: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  color: #f8fafc;
}

.badge-audit {
  background: #475569;
  color: #f8fafc;
  font-size: 11px;
  padding: 2px 7px;
  border-radius: 4px;
  font-weight: 700;
  letter-spacing: 0.5px;
}

.subtitle {
  font-size: 12px;
  color: #94a3b8;
  margin: 4px 0 0 0;
}

.header-right {
  display: flex;
  gap: 8px;
}

.info-pill {
  background: rgba(30, 41, 59, 0.7);
  border: 1px solid rgba(71, 85, 105, 0.5);
  font-size: 11px;
  padding: 3px 8px;
  border-radius: 12px;
  color: #cbd5e1;
  font-family: monospace;
}

.card {
  background: #111827;
  border: 1px solid rgba(51, 65, 85, 0.6);
  border-radius: 8px;
  padding: 16px;
}

/* Device Section */
.device-section {
  margin-bottom: 16px;
}

.device-row {
  display: flex;
  gap: 16px;
  align-items: flex-end;
}

.device-item {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.device-item label {
  font-size: 12px;
  font-weight: 600;
  color: #cbd5e1;
}

.device-select {
  background: #1e293b;
  border: 1px solid #475569;
  border-radius: 6px;
  color: #f8fafc;
  padding: 7px 10px;
  font-size: 13px;
  outline: none;
}

.device-select:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.refresh-btn {
  height: 35px;
  padding: 0 14px;
  white-space: nowrap;
}

.device-lock-hint {
  font-size: 11px;
  color: #f59e0b;
  margin-top: 8px;
}

.package-row {
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 36px;
  margin-top: 10px;
  padding: 7px 9px;
  background: rgba(15, 23, 42, 0.72);
  border: 1px dashed #475569;
  border-radius: 6px;
  transition: border-color 0.15s ease, background 0.15s ease;
}

.audio-import-dropzone {
  min-height: 108px;
  border: 2px dashed #38bdf8;
  background: linear-gradient(135deg, rgba(14, 165, 233, 0.13), rgba(99, 102, 241, 0.08));
}

.audio-import-dropzone .package-drop-copy strong {
  color: #7dd3fc;
  font-size: 17px;
}

.package-row.package-drag-over {
  background: rgba(8, 145, 178, 0.2);
  border-color: #22d3ee;
}

.package-drop-copy {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 240px;
}

.package-drop-copy strong {
  color: #e2e8f0;
  font-size: 12px;
}

.package-drop-copy span,
.package-status {
  color: #94a3b8;
  font-size: 10px;
}

.package-export-btn {
  padding: 7px 10px;
  white-space: nowrap;
}

.package-select-btn {
  padding: 10px 14px;
  white-space: nowrap;
}

.import-modal-backdrop {
  position: fixed;
  inset: 0;
  z-index: 10000;
  display: grid;
  place-items: center;
  padding: 20px;
  background: rgba(2, 6, 23, 0.82);
  backdrop-filter: blur(5px);
}

.import-modal {
  width: min(520px, 100%);
  display: flex;
  flex-direction: column;
  gap: 14px;
  border: 1px solid #38bdf8;
}

.import-modal h2,
.import-modal p {
  margin: 0;
}

.import-modal label {
  display: flex;
  flex-direction: column;
  gap: 6px;
  color: #cbd5e1;
  font-size: 13px;
}

.import-source {
  color: #7dd3fc;
  overflow-wrap: anywhere;
}

.import-text-input {
  width: 100%;
  box-sizing: border-box;
}

.package-status {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Mode Tabs */
.mode-tabs {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
}

.tab-btn {
  flex: 1;
  background: #1e293b;
  border: 1px solid #334155;
  border-radius: 6px;
  padding: 10px 12px;
  text-align: left;
  cursor: pointer;
  display: flex;
  flex-direction: column;
  gap: 2px;
  color: #94a3b8;
  transition: all 0.15s ease;
}

.tab-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.tab-btn.active {
  background: #0284c7;
  border-color: #38bdf8;
  color: #ffffff;
  box-shadow: 0 2px 8px rgba(2, 132, 199, 0.4);
}

.analyzer-tab-btn.active {
  background: linear-gradient(135deg, #059669, #0d9488);
  border-color: #34d399;
  color: #ffffff;
  box-shadow: 0 2px 12px rgba(16, 185, 129, 0.5);
}

.editor-tab-btn.active {
  background: linear-gradient(135deg, #7c3aed, #6366f1);
  border-color: #a855f7;
  color: #ffffff;
  box-shadow: 0 2px 12px rgba(124, 58, 237, 0.5);
}

.analyzer-view-wrapper {
  margin-top: 8px;
  width: 100%;
}

.editor-view-wrapper {
  margin-top: 8px;
  width: 100%;
}

.tab-step {
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.tab-title {
  font-size: 13px;
  font-weight: 600;
}

/* Workspace Layout */
.workspace-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) clamp(320px, 22vw, 380px);
  gap: 16px;
}

.main-view-card {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.tab-content {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.content-header h3 {
  margin: 0 0 4px 0;
  font-size: 16px;
  color: #f8fafc;
}

.content-header .desc {
  margin: 0;
  font-size: 12px;
  color: #94a3b8;
  line-height: 1.4;
}

.empty-library-cta {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 10px;
  margin: 14px 0;
  padding: 18px;
  border: 2px dashed #38bdf8;
  border-radius: 10px;
  background: rgba(14, 165, 233, 0.1);
  color: #cbd5e1;
}

.empty-library-cta strong {
  color: #7dd3fc;
  font-size: 18px;
}

.song-meta {
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.playback-time {
  font-family: monospace;
  font-size: 14px;
  font-weight: bold;
  color: #38bdf8;
}

.user-settings-bar {
  display: flex;
  align-items: center;
  gap: 12px;
  background: rgba(15, 23, 42, 0.85);
  border: 1px solid #334155;
  border-radius: 8px;
  padding: 8px 12px;
  margin: 6px 0 10px 0;
  flex-wrap: wrap;
}

.setting-group {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mic-range-setting {
  margin-left: 10px;
  padding-left: 12px;
  border-left: 1px solid #334155;
}

.mic-signal-summary {
  display: inline-flex;
  margin: 8px 0 0;
  padding: 5px 9px;
  border: 1px solid #475569;
  border-radius: 999px;
  color: #cbd5e1;
  background: rgba(15, 23, 42, 0.8);
  font-size: 12px;
}

.mic-signal-summary.state-voiced {
  border-color: #10b981;
  color: #6ee7b7;
}

.mic-signal-summary.state-unvoiced {
  border-color: #3b82f6;
  color: #93c5fd;
}

.mic-signal-summary.state-silence,
.mic-signal-summary.state-no_pcm {
  color: #94a3b8;
}

.speed-setting input[type="range"] {
  width: 110px;
  accent-color: #38bdf8;
}

.speed-value {
  min-width: 42px;
  color: #38bdf8;
  font-family: monospace;
}

.setting-label {
  font-size: 12px;
  font-weight: 600;
  color: #94a3b8;
}

.setting-input {
  width: 58px;
  background: #1e293b;
  color: #38bdf8;
  border: 1px solid #475569;
  border-radius: 4px;
  padding: 4px 6px;
  font-size: 12px;
  font-weight: bold;
  font-family: monospace;
}

.setting-unit {
  font-size: 12px;
  color: #94a3b8;
}

.setting-select {
  background: #1e293b;
  color: #e2e8f0;
  border: 1px solid #475569;
  border-radius: 4px;
  padding: 4px 6px;
  font-size: 12px;
}

.btn-group-key {
  display: flex;
  gap: 3px;
}

.btn-xs {
  padding: 2px 7px;
  font-size: 11px;
}

.edit-jump-btn {
  margin-left: auto;
  background: #1e293b;
  border: 1px solid #0284c7;
  color: #38bdf8;
}

.edit-jump-btn:hover:not(:disabled) {
  background: #0284c7;
  color: #ffffff;
}

.vocal-dsp-panel {
  margin: -3px 0 9px;
  padding: 8px 10px;
  background: rgba(15, 23, 42, 0.86);
  border: 1px solid #334155;
  border-radius: 8px;
}

.vocal-dsp-panel.enabled {
  border-color: #0891b2;
  box-shadow: inset 0 0 16px rgba(8, 145, 178, 0.08);
}

.dsp-panel-header,
.monitor-toggle {
  display: flex;
  align-items: center;
  gap: 8px;
}

.dsp-panel-header {
  flex-wrap: wrap;
}

.monitor-toggle {
  color: #e2e8f0;
  font-size: 12px;
  cursor: pointer;
}

.monitor-toggle input {
  accent-color: #06b6d4;
}

.dsp-safety {
  color: #f59e0b;
  font-size: 10px;
}

.dsp-panel-header > .btn {
  margin-left: auto;
}

.dsp-controls {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 7px 12px;
  margin-top: 9px;
  padding-top: 8px;
  border-top: 1px solid #263044;
}

.dsp-control {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
  color: #94a3b8;
  font-size: 10px;
}

.dsp-control span {
  display: flex;
  justify-content: space-between;
  gap: 6px;
}

.dsp-control b {
  color: #67e8f9;
  font-family: monospace;
}

.dsp-control input[type="range"] {
  width: 100%;
  accent-color: #06b6d4;
}

.monitor-note {
  margin: 7px 0 0;
  color: #67e8f9;
  font-size: 10px;
}

.recording-panel {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 8px 14px;
  margin: -3px 0 9px;
  padding: 8px 10px;
  color: #94a3b8;
  background: rgba(15, 23, 42, 0.86);
  border: 1px solid #334155;
  border-radius: 8px;
  font-size: 11px;
}

.recording-panel.enabled {
  border-color: #ef4444;
  box-shadow: inset 0 0 16px rgba(239, 68, 68, 0.08);
}

.recording-toggle,
.recording-result {
  display: flex;
  align-items: center;
  gap: 8px;
}

.recording-toggle {
  color: #e2e8f0;
  cursor: pointer;
}

.recording-toggle input {
  accent-color: #ef4444;
}

.mix-levels {
  display: grid;
  grid-template-columns: repeat(2, minmax(180px, 1fr));
  flex: 1 1 100%;
  gap: 8px 12px;
  padding-top: 7px;
  border-top: 1px solid #334155;
}

.mix-levels small {
  grid-column: 1 / -1;
  color: #94a3b8;
}

.recording-result {
  flex: 1 1 100%;
  padding-top: 7px;
  border-top: 1px solid #334155;
}

.recording-result.complete b {
  color: #34d399;
}

.recording-result.incomplete b,
.recording-result.empty b {
  color: #fbbf24;
}

.recording-result .btn {
  margin-left: auto;
}

.action-bar {
  display: flex;
  margin-top: 4px;
}

.flex-gap {
  gap: 10px;
}

/* Canvas */
.canvas-container {
  width: 100%;
  background: #000000;
  border: 1px solid #334155;
  border-radius: 6px;
  overflow: hidden;
}

.pitch-canvas {
  width: 100%;
  height: 220px;
  display: block;
}

/* Cent Deviation Bar */
.cent-bar-wrapper {
  background: #0f172a;
  border: 1px solid #1e293b;
  border-radius: 6px;
  padding: 8px 12px;
}

.cent-labels {
  display: flex;
  justify-content: space-between;
  font-size: 11px;
  color: #94a3b8;
  margin-bottom: 4px;
}

.cent-status {
  font-weight: bold;
}

.cent-status.perfect {
  color: #34d399;
}

.cent-status.sharp {
  color: #f59e0b;
}

.cent-status.flat {
  color: #38bdf8;
}

.cent-track {
  position: relative;
  height: 8px;
  background: #1e293b;
  border-radius: 4px;
  overflow: hidden;
}

.cent-zero-line {
  position: absolute;
  left: 50%;
  width: 2px;
  height: 100%;
  background: #64748b;
  transform: translateX(-50%);
}

.cent-marker {
  position: absolute;
  top: 0;
  width: 10px;
  height: 100%;
  border-radius: 4px;
  transform: translateX(-50%);
  background: #fbbf24;
}

.cent-marker.perfect {
  background: #34d399;
}

.cent-marker.sharp {
  background: #f59e0b;
}

.cent-marker.flat {
  background: #38bdf8;
}

/* Reference Result Box */
.reference-result-box {
  background: rgba(30, 41, 59, 0.5);
  border: 1px dashed #64748b;
  border-radius: 6px;
  padding: 12px;
}

.result-title {
  display: flex;
  justify-content: space-between;
  font-size: 13px;
  font-weight: 600;
  color: #f8fafc;
  margin-bottom: 8px;
}

.status-tag {
  background: #475569;
  font-size: 10px;
  padding: 2px 6px;
  border-radius: 4px;
}

.result-stats {
  display: grid;
  grid-template-columns: 1fr 1fr 1fr;
  gap: 8px;
  margin-bottom: 6px;
}

.stat-cell {
  background: #0f172a;
  padding: 8px;
  border-radius: 4px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.stat-name {
  font-size: 11px;
  color: #94a3b8;
}

.stat-val {
  font-size: 16px;
  font-weight: 700;
  color: #f8fafc;
}

.stat-val.highlight {
  color: #38bdf8;
}

.result-note {
  margin: 0;
  font-size: 10px;
  color: #94a3b8;
}

/* Telemetry Card */
.telemetry-card h2 {
  font-size: 15px;
  margin: 0 0 2px 0;
  color: #f8fafc;
}

.telemetry-subtitle {
  font-size: 11px;
  color: #64748b;
  margin: 0 0 12px 0;
}

.telemetry-grid {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.telemetry-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  font-size: 12px;
}

.t-label {
  color: #94a3b8;
}

.t-val {
  font-family: monospace;
  font-weight: 600;
  color: #f8fafc;
}

.t-device {
  max-width: 170px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: #cbd5e1;
}

.t-pitch {
  color: #38bdf8;
  font-size: 13px;
}

.t-val.muted {
  color: #475569;
}

.highlight-row {
  padding: 6px 8px;
  background: #0f172a;
  border-radius: 4px;
}

.state-badge {
  font-size: 11px;
  font-weight: 700;
  font-family: monospace;
  padding: 3px 8px;
  border-radius: 4px;
}

.state-voiced {
  background: #065f46;
  color: #34d399;
  border: 1px solid #059669;
}

.state-unvoiced {
  background: #1e3a8a;
  color: #93c5fd;
  border: 1px solid #2563eb;
}

.state-silence {
  background: #374151;
  color: #9ca3af;
  border: 1px solid #4b5563;
}

.state-nopcm {
  background: #450a0a;
  color: #f87171;
  border: 1px solid #b91c1c;
}

.level-meter-bar {
  height: 6px;
  background: #1e293b;
  border-radius: 3px;
  overflow: hidden;
  margin-top: -3px;
  margin-bottom: 4px;
}

.level-fill {
  height: 100%;
  background: linear-gradient(90deg, #10b981, #f59e0b, #ef4444);
  transition: width 0.05s ease;
}

.periodicity-fill {
  background: #38bdf8;
}

.diag-help-box {
  margin-top: 14px;
  padding: 10px;
  background: #0f172a;
  border: 1px solid #1e293b;
  border-radius: 6px;
  font-size: 11px;
  color: #94a3b8;
}

.help-title {
  font-weight: 600;
  color: #cbd5e1;
  margin-bottom: 6px;
}

.diag-help-box ul {
  margin: 0;
  padding-left: 14px;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

@media (max-width: 960px) {
  .karaoke-container {
    padding: 12px;
  }

  .workspace-layout {
    grid-template-columns: minmax(0, 1fr);
  }

  .telemetry-card {
    min-width: 0;
  }
}

@media (max-width: 680px) {
  .device-row,
  .package-row,
  .app-header {
    align-items: stretch;
    flex-direction: column;
  }

  .header-right {
    flex-wrap: wrap;
  }
}

/* Button Styles */
.btn {
  border: none;
  border-radius: 6px;
  font-size: 13px;
  font-weight: 600;
  padding: 9px 16px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-primary {
  background: #0284c7;
  color: #ffffff;
}

.btn-primary:hover:not(:disabled) {
  background: #0369a1;
}

.btn-success {
  background: #059669;
  color: #ffffff;
}

.btn-success:hover:not(:disabled) {
  background: #047857;
}

.btn-warning {
  background: #d97706;
  color: #ffffff;
}

.btn-warning:hover:not(:disabled) {
  background: #b45309;
}

.btn-danger {
  background: #dc2626;
  color: #ffffff;
}

.btn-danger:hover:not(:disabled) {
  background: #b91c1c;
}

.btn-secondary {
  background: #334155;
  color: #f8fafc;
}

.btn-secondary:hover:not(:disabled) {
  background: #475569;
}
</style>
