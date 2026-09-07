<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";

interface Note {
  id: string;
  trackId: string;
  startUs: number;
  endUs: number;
  pitchMidi: number;
  tuningCents: number;
  scorable: boolean;
}

interface LyricToken {
  id: string;
  text: string;
  ruby: string;
  noteIds: string[];
  startUs: number;
  endUs: number;
}

interface Phrase {
  id: string;
  startUs: number;
  endUs: number;
  tokenIds: string[];
}

interface Chart {
  schemaVersion: string;
  requiredFeatures: string[];
  songId: string;
  chartId: string;
  chartRevision: number;
  title: string;
  artist: string;
  durationUs: number;
  credits: any[];
  media: any[];
  tracks: any[];
  tempoMap: {
    anchorTimeUs: number;
    anchorQuarterBeat: number;
    events: Array<{ timeUs: number; bpm: number }>;
    meters: any[];
  };
  notes: Note[];
  lyricTokens: LyricToken[];
  phrases: Phrase[];
  extensions?: any;
}

interface EditorRecovery {
  songId: string;
  baseRevision: number;
  savedAtUnixMs: number;
  chart: Chart;
}

interface EditorSnapshot {
  chart: Chart;
  vocalIntroSec: number;
  baseMidi: number;
}

interface WaveformData {
  stepMs: number;
  durationS: number;
  peaks: number[];
}

interface F0Data {
  stepMs: number;
  frames: Array<[number, number, number]>; // [timeMs, hz, midi]
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
  state: string;
}

const emit = defineEmits<{
  (e: "requestAnalysis"): void;
}>();

const props = defineProps<{
  songId: string;
}>();

const canvasRef = ref<HTMLCanvasElement | null>(null);
const wrapperRef = ref<HTMLDivElement | null>(null);

// State
const chart = ref<Chart | null>(null);
const waveform = ref<WaveformData | null>(null);
const f0Contour = ref<F0Data | null>(null);
const isLoading = ref(true);
const isSaving = ref(false);
const saveMessage = ref<string>("");
const lastSavedSnapshot = ref<string>("");
const recoveryStatus = ref<string>("");
let recoveryTimer: number | null = null;
let recoveryEnabled = false;
let restoringState = false;

function downloadChartJson() {
  if (!chart.value) return;
  const jsonStr = JSON.stringify(chart.value, null, 2);
  const blob = new Blob([jsonStr], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `${chart.value.songId || "shining_star"}_chart.json`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

// Live Mic Voice Guide State
const micGuideEnabled = ref(true);
const micDiagnostic = ref<MicDiagnostic | null>(null);
let micPollTimer: number | null = null;

// User Configurable Song Settings (Persisted to chart.json)
const vocalIntroSec = ref(25.0); // 歌い出し秒数 (User configurable)
const baseMidi = ref(69); // 基準キー / Base Pitch (Default A4 = 69, User can shift anywhere)

// Audio Preview Playback State
const isPlaying = ref(false);
const playheadMs = ref(25000); // Start at vocal intro
let playPollTimer: number | null = null;

// High-Performance rAF Render Manager (Eliminates lag, locks to 60fps/144fps monitor sync)
let animFrameId: number | null = null;
let isRedrawScheduled = false;
function requestRedraw() {
  if (!isRedrawScheduled) {
    isRedrawScheduled = true;
    animFrameId = requestAnimationFrame(() => {
      isRedrawScheduled = false;
      draw();
    });
  }
}

// Viewport / Zoom State
const pixelsPerSecond = ref(120); // Zoom: 40px/s to 400px/s
const scrollMs = ref(23000); // Viewport horizontal offset
const minMidi = computed(() => baseMidi.value - 15); // Dynamic range centered on user's base key
const maxMidi = computed(() => baseMidi.value + 15);
const rowHeight = 20; // Exact px height per semitone
const rulerHeight = 28; // Timeline ruler on top
const keyboardWidth = 60; // Sticky keyboard width
const waveformHeight = 46; // Waveform lane height

// Interaction State
const selectedNoteIds = ref<Set<string>>(new Set());
const hoverNoteId = ref<string | null>(null);
const hoverHandle = ref<"left" | "right" | "body" | null>(null);
const snapMode = ref<"off" | "16th" | "8th" | "4th">("16th");
const editPanel = ref<"lyrics" | "tempo">("lyrics");
const newLyricText = ref("");
const newLyricRuby = ref("");
const selectedLyricTokenIds = ref<Set<string>>(new Set());

// Drag state
let isDragging = false;
let dragAction: "move" | "resize-left" | "resize-right" | "scrub-ruler" | null = null;
let dragStartX = 0;
let dragStartY = 0;
let initialNoteStates: Map<string, { startUs: number; endUs: number; pitchMidi: number }> = new Map();

// Full-document Undo / Redo Stacks (Max 50)
const undoStack = ref<string[]>([]);
const redoStack = ref<string[]>([]);

// Web Audio API for interactive pitch feedback tone
let audioCtx: AudioContext | null = null;
function playTone(midi: number) {
  try {
    if (!audioCtx) {
      audioCtx = new (window.AudioContext || (window as any).webkitAudioContext)();
    }
    if (audioCtx.state === "suspended") {
      audioCtx.resume();
    }
    const osc = audioCtx.createOscillator();
    const gain = audioCtx.createGain();
    const freq = 440.0 * Math.pow(2.0, (midi - 69) / 12.0);
    osc.frequency.setValueAtTime(freq, audioCtx.currentTime);
    osc.type = "sine";

    gain.gain.setValueAtTime(0.12, audioCtx.currentTime);
    gain.gain.exponentialRampToValueAtTime(0.001, audioCtx.currentTime + 0.25);

    osc.connect(gain);
    gain.connect(audioCtx.destination);

    osc.start();
    osc.stop(audioCtx.currentTime + 0.25);
  } catch (e) {
    // Ignore audio error if user hasn't interacted
  }
}

const noteNames = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
function midiToName(midi: number): string {
  const octave = Math.floor(midi / 12) - 1;
  const name = noteNames[midi % 12];
  return `${name}${octave}`;
}

function isBlackKey(midi: number): boolean {
  const idx = midi % 12;
  return idx === 1 || idx === 3 || idx === 6 || idx === 8 || idx === 10;
}

const bpm = computed(() => {
  if (chart.value?.tempoMap?.events?.[0]?.bpm) {
    return chart.value.tempoMap.events[0].bpm;
  }
  return 158.0;
});

// Quarter beat duration in ms (at BPM 158: ~379.75ms)
const quarterBeatMs = computed(() => (60.0 / bpm.value) * 1000.0);

function snapTime(timeMs: number): number {
  if (snapMode.value === "off") return timeMs;
  let gridMs = quarterBeatMs.value;
  if (snapMode.value === "16th") gridMs /= 4.0;
  else if (snapMode.value === "8th") gridMs /= 2.0;
  return Math.round(timeMs / gridMs) * gridMs;
}

function captureSnapshot(): string {
  if (!chart.value) return "";
  return JSON.stringify({
    chart: chart.value,
    vocalIntroSec: vocalIntroSec.value,
    baseMidi: baseMidi.value,
  } satisfies EditorSnapshot);
}

function restoreSnapshot(snapshot: string) {
  const restored = JSON.parse(snapshot) as EditorSnapshot;
  restoringState = true;
  chart.value = restored.chart;
  vocalIntroSec.value = restored.vocalIntroSec;
  baseMidi.value = restored.baseMidi;
  restoringState = false;
  scheduleRecovery();
  requestRedraw();
}

function saveSnapshot() {
  const snap = captureSnapshot();
  if (!snap || undoStack.value[undoStack.value.length - 1] === snap) return;
  undoStack.value.push(snap);
  if (undoStack.value.length > 50) undoStack.value.shift();
  redoStack.value = []; // Clear redo
}

function undo() {
  if (!chart.value || undoStack.value.length === 0) return;
  const current = captureSnapshot();
  redoStack.value.push(current);
  const prev = undoStack.value.pop()!;
  restoreSnapshot(prev);
}

function redo() {
  if (!chart.value || redoStack.value.length === 0) return;
  const current = captureSnapshot();
  undoStack.value.push(current);
  const next = redoStack.value.pop()!;
  restoreSnapshot(next);
}

const isDirty = computed(() => Boolean(chart.value) && captureSnapshot() !== lastSavedSnapshot.value);

function syncSettingsIntoChart() {
  if (!chart.value) return;
  if (!chart.value.extensions) chart.value.extensions = {};
  chart.value.extensions.vocalIntroMs = Math.round(vocalIntroSec.value * 1000.0);
  chart.value.extensions.baseKeyMidi = baseMidi.value;
}

function scheduleRecovery() {
  if (!recoveryEnabled || restoringState || !chart.value) return;
  syncSettingsIntoChart();
  if (recoveryTimer !== null) window.clearTimeout(recoveryTimer);
  recoveryTimer = window.setTimeout(async () => {
    recoveryTimer = null;
    if (!chart.value || !isDirty.value) return;
    try {
      await invoke("save_editor_recovery", { chartData: chart.value });
      recoveryStatus.value = "未保存の編集を自動退避しました";
    } catch (err) {
      recoveryStatus.value = `自動退避エラー: ${err}`;
      console.error("Recovery save error:", err);
    }
  }, 750);
}

// -------------------------------------------------------------
// Data Loading & Saving
// -------------------------------------------------------------
async function loadData() {
  isLoading.value = true;
  try {
    const loadedChart = await invoke<Chart>("get_current_chart", { songId: props.songId });
    let chartToOpen = loadedChart;
    const recovery = await invoke<EditorRecovery | null>("get_editor_recovery", { songId: props.songId });
    if (recovery && JSON.stringify(recovery.chart) !== JSON.stringify(loadedChart)) {
      const savedAt = new Date(recovery.savedAtUnixMs).toLocaleString();
      if (window.confirm(`${savedAt} の未保存編集があります。復元しますか？`)) {
        chartToOpen = recovery.chart;
        recoveryStatus.value = "未保存の編集を復元しました";
      } else {
        await invoke("discard_editor_recovery", { songId: props.songId });
      }
    }
    chart.value = chartToOpen;

    // Restore user settings if present in chart extensions
    if (chartToOpen.extensions?.vocalIntroMs !== undefined) {
      vocalIntroSec.value = Number((chartToOpen.extensions.vocalIntroMs / 1000.0).toFixed(2));
    }
    if (chartToOpen.extensions?.baseKeyMidi !== undefined) {
      baseMidi.value = chartToOpen.extensions.baseKeyMidi;
    }

    // Set playhead to vocal intro position
    playheadMs.value = vocalIntroSec.value * 1000.0;
    scrollMs.value = Math.max(0, playheadMs.value - 2000.0);

    const mediaInfo = await invoke<{ waveform: WaveformData; f0: F0Data }>("get_editor_media_info", { songId: props.songId });
    waveform.value = mediaInfo.waveform;
    f0Contour.value = mediaInfo.f0;
    undoStack.value = [];
    redoStack.value = [];
    lastSavedSnapshot.value = JSON.stringify({
      chart: loadedChart,
      vocalIntroSec: loadedChart.extensions?.vocalIntroMs !== undefined
        ? Number((loadedChart.extensions.vocalIntroMs / 1000.0).toFixed(2))
        : 25.0,
      baseMidi: loadedChart.extensions?.baseKeyMidi ?? 69,
    } satisfies EditorSnapshot);
    recoveryEnabled = true;
  } catch (err) {
    console.error("Failed to load editor data:", err);
  } finally {
    isLoading.value = false;
    setTimeout(() => {
      resizeCanvas();
      requestRedraw();
    }, 50);
  }
}

async function saveChart() {
  if (!chart.value) return;
  isSaving.value = true;
  saveMessage.value = "保存中...";
  try {
    syncSettingsIntoChart();

    chart.value = await invoke<Chart>("save_chart", { chartData: chart.value });
    lastSavedSnapshot.value = captureSnapshot();
    recoveryStatus.value = "";
    saveMessage.value = `✨ 譜面を保存しました (${chart.value.songId})`;
    setTimeout(() => { saveMessage.value = ""; }, 3000);
  } catch (err: any) {
    saveMessage.value = `❌ 保存エラー: ${err}`;
    console.error("Save error:", err);
  } finally {
    isSaving.value = false;
  }
}

function setPlayheadAsVocalIntro() {
  saveSnapshot();
  vocalIntroSec.value = Number((playheadMs.value / 1000.0).toFixed(2));
  if (chart.value) {
    if (!chart.value.extensions) chart.value.extensions = {};
    chart.value.extensions.vocalIntroMs = Math.round(vocalIntroSec.value * 1000.0);
  }
  saveMessage.value = `📍 歌い出しを ${vocalIntroSec.value}s に設定しました`;
  setTimeout(() => { if (saveMessage.value.startsWith("📍")) saveMessage.value = ""; }, 3000);
}

function shiftAllNotes(semitones: number) {
  if (!chart.value || chart.value.notes.length === 0) return;
  saveSnapshot();
  for (const n of chart.value.notes) {
    n.pitchMidi = Math.max(24, Math.min(108, n.pitchMidi + semitones));
  }
  baseMidi.value = Math.max(36, Math.min(96, baseMidi.value + semitones));
  if (chart.value) {
    if (!chart.value.extensions) chart.value.extensions = {};
    chart.value.extensions.baseKeyMidi = baseMidi.value;
  }
  saveMessage.value = `🎵 全ノーツを ${semitones > 0 ? '+' : ''}${semitones} 半音シフトしました`;
  setTimeout(() => { if (saveMessage.value.startsWith("🎵")) saveMessage.value = ""; }, 2500);
  requestRedraw();
}

// -------------------------------------------------------------
// Coordinate Transformations (100% Exact & Unified)
// -------------------------------------------------------------
function resizeCanvas() {
  if (!canvasRef.value || !wrapperRef.value) return;
  const rect = wrapperRef.value.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  // Match canvas internal buffer to wrapper CSS pixels * DPR
  canvasRef.value.width = rect.width * dpr;
  canvasRef.value.height = rect.height * dpr;
}

// Y of top of row for MIDI note
function midiToY(midi: number): number {
  return rulerHeight + (maxMidi.value - midi) * rowHeight;
}

// MIDI note under Y coordinate
function yToMidi(y: number): number {
  return maxMidi.value - Math.floor((y - rulerHeight) / rowHeight);
}

function msToX(ms: number): number {
  return keyboardWidth + ((ms - scrollMs.value) / 1000.0) * pixelsPerSecond.value;
}

function xToMs(x: number): number {
  return scrollMs.value + ((x - keyboardWidth) / pixelsPerSecond.value) * 1000.0;
}

// -------------------------------------------------------------
// Canvas Rendering
// -------------------------------------------------------------
function draw() {
  if (!canvasRef.value) return;
  const ctx = canvasRef.value.getContext("2d");
  if (!ctx) return;

  const dpr = window.devicePixelRatio || 1;
  const w = canvasRef.value.width / dpr;
  const h = canvasRef.value.height / dpr;
  const totalMidiRows = maxMidi.value - minMidi.value + 1;
  const pianoBottomY = rulerHeight + totalMidiRows * rowHeight;

  ctx.save();
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);

  // 1. Background Grid (Semitone Rows) with User Base-Key Highlighting
  for (let m = minMidi.value; m <= maxMidi.value; m++) {
    const y = midiToY(m);
    const isBase = m === baseMidi.value;
    if (isBase) {
      ctx.fillStyle = "rgba(234, 179, 8, 0.16)"; // Golden highlight for user's configured Base Pitch
    } else {
      ctx.fillStyle = isBlackKey(m) ? "#121520" : "#1a1e2b";
    }
    ctx.fillRect(keyboardWidth, y, w - keyboardWidth, rowHeight);

    ctx.strokeStyle = isBase ? "rgba(234, 179, 8, 0.45)" : "rgba(255, 255, 255, 0.06)";
    ctx.lineWidth = isBase ? 1.5 : 1;
    ctx.beginPath();
    ctx.moveTo(keyboardWidth, y);
    ctx.lineTo(w, y);
    ctx.stroke();
  }

  // 2. Vertical Beat & Measure Lines
  const visibleStartMs = xToMs(keyboardWidth);
  const visibleEndMs = xToMs(w);

  const qBeatMs = quarterBeatMs.value;
  const measureMs = qBeatMs * 4.0;
  const firstMeasure = Math.floor(visibleStartMs / measureMs);
  const lastMeasure = Math.ceil(visibleEndMs / measureMs);

  for (let m = firstMeasure; m <= lastMeasure; m++) {
    const mTime = m * measureMs;
    const mX = msToX(mTime);

    // Measure line
    ctx.strokeStyle = "rgba(255, 255, 255, 0.25)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(mX, rulerHeight);
    ctx.lineTo(mX, pianoBottomY);
    ctx.stroke();

    // Beat lines
    for (let b = 1; b < 4; b++) {
      const bTime = mTime + b * qBeatMs;
      const bX = msToX(bTime);
      ctx.strokeStyle = "rgba(255, 255, 255, 0.08)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(bX, rulerHeight);
      ctx.lineTo(bX, pianoBottomY);
      ctx.stroke();
    }
  }

  // 3. Audio Waveform Lane (Bottom)
  const waveTopY = pianoBottomY;
  ctx.fillStyle = "#0c0e14";
  ctx.fillRect(keyboardWidth, waveTopY, w - keyboardWidth, waveformHeight);
  ctx.strokeStyle = "rgba(56, 189, 248, 0.3)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(keyboardWidth, waveTopY);
  ctx.lineTo(w, waveTopY);
  ctx.stroke();

  if (waveform.value && waveform.value.peaks.length > 0) {
    const stepMs = waveform.value.stepMs;
    const peaks = waveform.value.peaks;
    const midY = waveTopY + waveformHeight / 2;

    ctx.fillStyle = "rgba(56, 189, 248, 0.5)";
    ctx.beginPath();
    let started = false;

    for (let i = 0; i < peaks.length; i++) {
      const tMs = i * stepMs;
      if (tMs < visibleStartMs - 500) continue;
      if (tMs > visibleEndMs + 500) break;

      const pxX = msToX(tMs);
      const amp = peaks[i];
      const barH = amp * (waveformHeight / 2 - 2);

      if (!started) {
        ctx.moveTo(pxX, midY - barH);
        started = true;
      } else {
        ctx.lineTo(pxX, midY - barH);
      }
    }

    for (let i = peaks.length - 1; i >= 0; i--) {
      const tMs = i * stepMs;
      if (tMs < visibleStartMs - 500) continue;
      if (tMs > visibleEndMs + 500) break;
      const pxX = msToX(tMs);
      const amp = peaks[i];
      const barH = amp * (waveformHeight / 2 - 2);
      ctx.lineTo(pxX, midY + barH);
    }
    ctx.closePath();
    ctx.fill();
  }

  // Waveform label
  ctx.fillStyle = "rgba(255, 255, 255, 0.4)";
  ctx.font = "9px sans-serif";
  ctx.fillText("AUDIO WAVEFORM (DEMUCS ISOLATED VOCAL)", keyboardWidth + 8, waveTopY + 14);

  // 4. F0 Contour (RMVPE continuous pitch trajectory)
  if (f0Contour.value && f0Contour.value.frames.length > 0) {
    ctx.strokeStyle = "rgba(52, 211, 153, 0.75)"; // Emerald green
    ctx.lineWidth = 2.5;
    ctx.beginPath();
    let drawing = false;

    for (const [tMs, hz, midi] of f0Contour.value.frames) {
      if (tMs < visibleStartMs - 500) continue;
      if (tMs > visibleEndMs + 500) break;

      if (hz > 0 && midi >= minMidi.value - 1 && midi <= maxMidi.value + 1) {
        const ptX = msToX(tMs);
        const ptY = midiToY(midi) + rowHeight / 2;
        if (!drawing) {
          ctx.moveTo(ptX, ptY);
          drawing = true;
        } else {
          ctx.lineTo(ptX, ptY);
        }
      } else {
        drawing = false;
      }
    }
    ctx.stroke();
  }

  // 5. Notes (Pitch Bars) - Exactly aligned to semitone rows
  if (chart.value && chart.value.notes) {
    for (const note of chart.value.notes) {
      const startMs = note.startUs / 1000.0;
      const endMs = note.endUs / 1000.0;
      if (endMs < visibleStartMs || startMs > visibleEndMs) continue;

      const nX = msToX(startMs);
      const nW = Math.max(6, msToX(endMs) - nX);
      const nY = midiToY(note.pitchMidi) + 2;
      const nH = rowHeight - 4;

      const isSelected = selectedNoteIds.value.has(note.id);
      const isHover = hoverNoteId.value === note.id;

      // Note Body
      const gradient = ctx.createLinearGradient(nX, nY, nX, nY + nH);
      if (isSelected) {
        gradient.addColorStop(0, "#fbbf24");
        gradient.addColorStop(1, "#d97706");
      } else if (isHover) {
        gradient.addColorStop(0, "#c084fc");
        gradient.addColorStop(1, "#9333ea");
      } else {
        gradient.addColorStop(0, "#818cf8");
        gradient.addColorStop(1, "#4f46e5");
      }

      ctx.fillStyle = gradient;
      ctx.beginPath();
      ctx.roundRect(nX, nY, nW, nH, 3);
      ctx.fill();

      // Border
      ctx.strokeStyle = isSelected ? "#ffffff" : "rgba(255, 255, 255, 0.45)";
      ctx.lineWidth = isSelected ? 2 : 1;
      ctx.stroke();

      // Resize Handles
      if (isHover || isSelected) {
        ctx.fillStyle = "#ffffff";
        ctx.fillRect(nX + 1, nY + 2, 3, nH - 4);
        ctx.fillRect(nX + nW - 4, nY + 2, 3, nH - 4);
      }

      // Note Name Label
      if (nW >= 26) {
        ctx.fillStyle = isSelected ? "#000000" : "#ffffff";
        ctx.font = "bold 11px monospace";
        ctx.fillText(midiToName(note.pitchMidi), nX + 6, nY + nH - 3);
      }
    }
  }

  // 6. Timeline Ruler (Top Bar)
  ctx.fillStyle = "#11141e";
  ctx.fillRect(0, 0, w, rulerHeight);
  ctx.strokeStyle = "#2e384d";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(0, rulerHeight);
  ctx.lineTo(w, rulerHeight);
  ctx.stroke();

  // Measure and Second Ticks on Ruler
  for (let m = firstMeasure; m <= lastMeasure; m++) {
    const mTime = m * measureMs;
    const mX = msToX(mTime);

    // Measure tick
    ctx.strokeStyle = "#94a3b8";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(mX, 0);
    ctx.lineTo(mX, rulerHeight);
    ctx.stroke();

    // Measure & Time text
    ctx.fillStyle = "#e2e8f0";
    ctx.font = "bold 11px sans-serif";
    ctx.fillText(`Bar ${m + 1}`, mX + 4, 13);
    ctx.fillStyle = "#38bdf8";
    ctx.font = "9px monospace";
    ctx.fillText(`${(mTime / 1000).toFixed(1)}s`, mX + 4, 24);
  }

  // 7. Piano Keyboard (Left Sticky Column)
  ctx.fillStyle = "#0f1117";
  ctx.fillRect(0, rulerHeight, keyboardWidth, totalMidiRows * rowHeight);

  for (let m = minMidi.value; m <= maxMidi.value; m++) {
    const y = midiToY(m);
    const isBlack = isBlackKey(m);
    const isBase = m === baseMidi.value;

    ctx.fillStyle = isBase ? "#eab308" : isBlack ? "#1e2230" : "#f1f5f9";
    ctx.fillRect(0, y, keyboardWidth - 2, rowHeight - 1);

    ctx.fillStyle = isBase ? "#000000" : isBlack ? "#94a3b8" : "#0f172a";
    ctx.font = isBase ? "bold 11px sans-serif" : "bold 10px sans-serif";
    const label = isBase ? `★${midiToName(m)}` : midiToName(m);
    ctx.fillText(label, 4, y + rowHeight - 5);
  }

  // Keyboard Right Border
  ctx.strokeStyle = "#475569";
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(keyboardWidth, 0);
  ctx.lineTo(keyboardWidth, h);
  ctx.stroke();

  // Top-left Corner Cell
  ctx.fillStyle = "#0a0c12";
  ctx.fillRect(0, 0, keyboardWidth, rulerHeight);
  ctx.fillStyle = "#a855f7";
  ctx.font = "bold 11px sans-serif";
  ctx.fillText("KEY", 16, 18);

  // 8. Playhead Cursor (Red Line)
  const cursorX = msToX(playheadMs.value);
  if (cursorX >= keyboardWidth && cursorX <= w) {
    ctx.strokeStyle = "#ef4444";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(cursorX, 0);
    ctx.lineTo(cursorX, h);
    ctx.stroke();

    // Cursor Handle on Ruler
    ctx.fillStyle = "#ef4444";
    ctx.beginPath();
    ctx.moveTo(cursorX - 7, 0);
    ctx.lineTo(cursorX + 7, 0);
    ctx.lineTo(cursorX, 14);
    ctx.closePath();
    ctx.fill();

    // Time text above
    ctx.fillStyle = "#ffffff";
    ctx.font = "bold 9px monospace";
    ctx.fillText(`${(playheadMs.value / 1000).toFixed(2)}s`, cursorX + 4, 10);
  }

  // 9. Live Microphone Voice Pitch Guide (赤ライン直結・実音リアルタイム可視化 & 超高速ラグフリー)
  if (micGuideEnabled.value && micDiagnostic.value) {
    const diag = micDiagnostic.value;
    const hasVoiceInput = diag.rmsDbfs > -58.0 || diag.state === "VOICED" || (diag.f0Hz !== null && diag.f0Hz > 50);

    if (hasVoiceInput && cursorX >= keyboardWidth && cursorX <= w) {
      // 9.1 赤ライン上のマイク音量パルスインジケータ
      const normRms = Math.min(1.0, Math.max(0.1, (diag.rmsDbfs + 60.0) / 45.0)); // 0.1 to 1.0
      const pulseR = 6 + normRms * 10;
      ctx.fillStyle = "rgba(56, 189, 248, 0.3)";
      ctx.beginPath();
      ctx.arc(cursorX, rulerHeight + 14, pulseR, 0, Math.PI * 2);
      ctx.fill();

      // 9.2 ピッチが検出できている場合（実音ピッチをそのまま赤ラインの該当音高に描画）
      const liveMidi = diag.midiNote ?? (diag.f0Hz && diag.f0Hz > 50 ? (69.0 + 12.0 * Math.log2(diag.f0Hz / 440.0)) : null);

      if (liveMidi !== null && liveMidi >= minMidi.value - 2 && liveMidi <= maxMidi.value + 2) {
        const micY = midiToY(liveMidi) + rowHeight / 2;

        // 水平ネオンレーザーガイドライン（全幅）
        ctx.strokeStyle = "rgba(6, 182, 212, 0.8)";
        ctx.lineWidth = 2;
        ctx.setLineDash([6, 4]);
        ctx.beginPath();
        ctx.moveTo(keyboardWidth, micY);
        ctx.lineTo(w, micY);
        ctx.stroke();
        ctx.setLineDash([]);

        // 鍵盤側のキーハイライト
        const roundedMidi = Math.round(liveMidi);
        if (roundedMidi >= minMidi.value && roundedMidi <= maxMidi.value) {
          const keyY = midiToY(roundedMidi);
          ctx.fillStyle = "rgba(6, 182, 212, 0.75)";
          ctx.fillRect(keyboardWidth - 12, keyY + 1, 12, rowHeight - 2);
        }

        // 赤ラインと音高の交差点に、大粒の光るオーブ（shadowBlurを使わない高速多重アルファ円）
        // 外側光彩
        ctx.fillStyle = "rgba(34, 211, 238, 0.28)";
        ctx.beginPath();
        ctx.arc(cursorX, micY, 18, 0, Math.PI * 2);
        ctx.fill();

        // 中間オーブ
        ctx.fillStyle = "#38bdf8";
        ctx.beginPath();
        ctx.arc(cursorX, micY, 8, 0, Math.PI * 2);
        ctx.fill();

        // 芯ホワイト
        ctx.fillStyle = "#ffffff";
        ctx.beginPath();
        ctx.arc(cursorX, micY, 3.5, 0, Math.PI * 2);
        ctx.fill();

        // 音名バッジ（赤ラインのすぐ横にスマートにポップアップ）
        const noteName = midiToName(roundedMidi);
        const cents = Math.round((liveMidi - roundedMidi) * 100);
        const sign = cents >= 0 ? "+" : "";
        const badgeText = `🎤 ${noteName} (${sign}${cents}c)`;

        ctx.font = "bold 11px monospace";
        const textWidth = ctx.measureText(badgeText).width;
        const badgeX = Math.min(w - textWidth - 16, cursorX + 10);
        const badgeY = Math.max(rulerHeight + 2, micY - 10);

        ctx.fillStyle = "rgba(14, 116, 144, 0.95)";
        ctx.beginPath();
        ctx.roundRect(badgeX, badgeY, textWidth + 14, 20, 4);
        ctx.fill();
        ctx.strokeStyle = "#38bdf8";
        ctx.lineWidth = 1.5;
        ctx.stroke();

        ctx.fillStyle = "#ffffff";
        ctx.fillText(badgeText, badgeX + 7, badgeY + 14);
      }
    }
  }

  ctx.restore();
}

// -------------------------------------------------------------
// Mouse & Interaction Handlers (100% Accurate Hit-Testing)
// -------------------------------------------------------------
function getNoteAt(x: number, y: number): { note: Note; handle: "left" | "right" | "body" } | null {
  if (!chart.value || !canvasRef.value) return null;

  for (let i = chart.value.notes.length - 1; i >= 0; i--) {
    const n = chart.value.notes[i];
    const nX = msToX(n.startUs / 1000.0);
    const nW = Math.max(6, msToX(n.endUs / 1000.0) - nX);
    const nY = midiToY(n.pitchMidi) + 2;
    const nH = rowHeight - 4;

    if (x >= nX && x <= nX + nW && y >= nY && y <= nY + nH) {
      if (x - nX <= 8) return { note: n, handle: "left" };
      if (nX + nW - x <= 8) return { note: n, handle: "right" };
      return { note: n, handle: "body" };
    }
  }
  return null;
}

function handleMouseDown(e: MouseEvent) {
  if (!canvasRef.value) return;
  const rect = canvasRef.value.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;

  // Click on Ruler: scrub playhead
  if (y <= rulerHeight && x >= keyboardWidth) {
    dragAction = "scrub-ruler";
    isDragging = true;
    playheadMs.value = Math.max(0, snapTime(xToMs(x)));
    requestRedraw();
    return;
  }

  // Click on Keyboard: audition tone
  if (x < keyboardWidth && y > rulerHeight) {
    const midi = yToMidi(y);
    if (midi >= minMidi.value && midi <= maxMidi.value) {
      playTone(midi);
    }
    return;
  }

  // Click on Note Area
  const hit = getNoteAt(x, y);

  if (hit) {
    saveSnapshot();
    if (!e.shiftKey && !selectedNoteIds.value.has(hit.note.id)) {
      selectedNoteIds.value.clear();
    }
    selectedNoteIds.value.add(hit.note.id);
    playTone(hit.note.pitchMidi); // Audible feedback

    isDragging = true;
    dragAction = hit.handle === "left" ? "resize-left" : hit.handle === "right" ? "resize-right" : "move";
    dragStartX = x;
    dragStartY = y;

    initialNoteStates.clear();
    for (const noteId of selectedNoteIds.value) {
      const n = chart.value?.notes.find((item) => item.id === noteId);
      if (n) {
        initialNoteStates.set(n.id, { startUs: n.startUs, endUs: n.endUs, pitchMidi: n.pitchMidi });
      }
    }
  } else {
    // Click on empty grid: move playhead to here
    if (!e.shiftKey) selectedNoteIds.value.clear();
    playheadMs.value = Math.max(0, snapTime(xToMs(x)));
  }
  requestRedraw();
}

function handleMouseMove(e: MouseEvent) {
  if (!canvasRef.value) return;
  const rect = canvasRef.value.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;

  if (isDragging && chart.value) {
    if (dragAction === "scrub-ruler") {
      playheadMs.value = Math.max(0, snapTime(xToMs(x)));
      requestRedraw();
      return;
    }

    const dxPx = x - dragStartX;
    const dxMs = (dxPx / pixelsPerSecond.value) * 1000.0;
    // Difference in MIDI row
    const dyMidi = yToMidi(y) - yToMidi(dragStartY);

    for (const [id, init] of initialNoteStates.entries()) {
      const n = chart.value.notes.find((item) => item.id === id);
      if (!n) continue;

      if (dragAction === "move") {
        const newStartMs = snapTime(init.startUs / 1000.0 + dxMs);
        const durMs = (init.endUs - init.startUs) / 1000.0;
        n.startUs = Math.max(0, Math.round(newStartMs * 1000));
        n.endUs = Math.round((newStartMs + durMs) * 1000);
        const newMidi = Math.min(maxMidi.value, Math.max(minMidi.value, init.pitchMidi + dyMidi));
        if (n.pitchMidi !== newMidi) {
          n.pitchMidi = newMidi;
          playTone(newMidi); // Feedback on pitch change
        }
      } else if (dragAction === "resize-left") {
        const newStartMs = snapTime(init.startUs / 1000.0 + dxMs);
        if (newStartMs < init.endUs / 1000.0 - 40) {
          n.startUs = Math.max(0, Math.round(newStartMs * 1000));
        }
      } else if (dragAction === "resize-right") {
        const newEndMs = snapTime(init.endUs / 1000.0 + dxMs);
        if (newEndMs > init.startUs / 1000.0 + 40) {
          n.endUs = Math.round(newEndMs * 1000);
        }
      }
    }
    requestRedraw();
    return;
  }

  // Hover detection
  if (y <= rulerHeight) {
    canvasRef.value.style.cursor = "pointer";
    hoverNoteId.value = null;
    hoverHandle.value = null;
  } else if (x < keyboardWidth) {
    canvasRef.value.style.cursor = "pointer";
    hoverNoteId.value = null;
    hoverHandle.value = null;
  } else {
    const hit = getNoteAt(x, y);
    if (hit) {
      hoverNoteId.value = hit.note.id;
      hoverHandle.value = hit.handle;
      canvasRef.value.style.cursor = hit.handle === "body" ? "grab" : "ew-resize";
    } else {
      hoverNoteId.value = null;
      hoverHandle.value = null;
      canvasRef.value.style.cursor = "crosshair";
    }
  }
  requestRedraw();
}

function handleMouseUp() {
  if (isDragging) {
    isDragging = false;
    dragAction = null;
    initialNoteStates.clear();
    syncReferencedLyricTimes();
  }
}

function handleDoubleClick(e: MouseEvent) {
  if (!canvasRef.value || !chart.value) return;
  const rect = canvasRef.value.getBoundingClientRect();
  const x = e.clientX - rect.left;
  const y = e.clientY - rect.top;

  if (x < keyboardWidth || y < rulerHeight) return;

  const hit = getNoteAt(x, y);
  if (!hit) {
    // Add new note
    saveSnapshot();
    const clickMs = snapTime(xToMs(x));
    const midi = Math.min(maxMidi.value, Math.max(minMidi.value, yToMidi(y)));
    const durMs = quarterBeatMs.value; // 1 beat

    const newNote: Note = {
      id: `n_custom_${Date.now()}`,
      trackId: "lead",
      startUs: Math.round(clickMs * 1000),
      endUs: Math.round((clickMs + durMs) * 1000),
      pitchMidi: midi,
      tuningCents: 0,
      scorable: true,
    };

    chart.value.notes.push(newNote);
    chart.value.notes.sort((a, b) => a.startUs - b.startUs);
    selectedNoteIds.value.clear();
    selectedNoteIds.value.add(newNote.id);
    playTone(midi);
    requestRedraw();
  }
}

function handleWheel(e: WheelEvent) {
  e.preventDefault();
  if (e.ctrlKey) {
    // Zoom in/out
    const delta = e.deltaY < 0 ? 1.15 : 0.85;
    pixelsPerSecond.value = Math.min(400, Math.max(30, pixelsPerSecond.value * delta));
  } else {
    // Horizontal scroll
    const scrollDeltaMs = (e.deltaY / pixelsPerSecond.value) * 800;
    scrollMs.value = Math.max(0, scrollMs.value + scrollDeltaMs);
  }
  requestRedraw();
}

// -------------------------------------------------------------
// Note Operations: Split, Merge, Delete
// -------------------------------------------------------------
function splitSelectedNote() {
  if (!chart.value || selectedNoteIds.value.size !== 1) return;
  const noteId = Array.from(selectedNoteIds.value)[0];
  const n = chart.value.notes.find((item) => item.id === noteId);
  if (!n) return;

  const splitMs = playheadMs.value;
  const startMs = n.startUs / 1000.0;
  const endMs = n.endUs / 1000.0;

  if (splitMs > startMs + 40 && splitMs < endMs - 40) {
    saveSnapshot();
    const splitUs = Math.round(splitMs * 1000);
    const n2: Note = {
      ...n,
      id: `${n.id}_b`,
      startUs: splitUs,
      endUs: n.endUs,
    };
    n.endUs = splitUs;
    chart.value.notes.push(n2);
    for (const token of chart.value.lyricTokens) {
      if (token.noteIds.includes(n.id) && !token.noteIds.includes(n2.id)) {
        token.noteIds.push(n2.id);
      }
    }
    chart.value.notes.sort((a, b) => a.startUs - b.startUs);
    selectedNoteIds.value.clear();
    selectedNoteIds.value.add(n2.id);
    syncReferencedLyricTimes();
    requestRedraw();
  }
}

function mergeSelectedNotes() {
  if (!chart.value || selectedNoteIds.value.size < 2) return;
  const selectedNotes = chart.value.notes
    .filter((n) => selectedNoteIds.value.has(n.id))
    .sort((a, b) => a.startUs - b.startUs);

  saveSnapshot();
  const first = selectedNotes[0];
  const last = selectedNotes[selectedNotes.length - 1];

  first.endUs = last.endUs; // Extend first note
  const removeIds = new Set(selectedNotes.slice(1).map((n) => n.id));
  chart.value.notes = chart.value.notes.filter((n) => !removeIds.has(n.id));
  for (const token of chart.value.lyricTokens) {
    if (token.noteIds.some((id) => removeIds.has(id))) {
      token.noteIds = Array.from(new Set(token.noteIds.map((id) => removeIds.has(id) ? first.id : id)));
    }
  }

  selectedNoteIds.value.clear();
  selectedNoteIds.value.add(first.id);
  syncReferencedLyricTimes();
  requestRedraw();
}

function deleteSelectedNotes() {
  if (!chart.value || selectedNoteIds.value.size === 0) return;
  saveSnapshot();
  const deletedIds = new Set(selectedNoteIds.value);
  chart.value.notes = chart.value.notes.filter((n) => !selectedNoteIds.value.has(n.id));
  for (const token of chart.value.lyricTokens) {
    token.noteIds = token.noteIds.filter((id) => !deletedIds.has(id));
  }
  selectedNoteIds.value.clear();
  syncReferencedLyricTimes();
  requestRedraw();
}

// -------------------------------------------------------------
// Lyrics, Phrases and Tempo
// -------------------------------------------------------------
function uniqueId(prefix: string): string {
  const existing = new Set([
    ...(chart.value?.notes.map((item) => item.id) ?? []),
    ...(chart.value?.lyricTokens.map((item) => item.id) ?? []),
    ...(chart.value?.phrases.map((item) => item.id) ?? []),
  ]);
  let id = `${prefix}_${Date.now()}`;
  let suffix = 1;
  while (existing.has(id)) id = `${prefix}_${Date.now()}_${suffix++}`;
  return id;
}

function inputValue(event: Event): string {
  return (event.target as HTMLInputElement).value;
}

function notesForIds(noteIds: string[]): Note[] {
  if (!chart.value) return [];
  const wanted = new Set(noteIds);
  return chart.value.notes.filter((note) => wanted.has(note.id)).sort((a, b) => a.startUs - b.startUs);
}

function syncReferencedLyricTimes() {
  if (!chart.value) return;
  for (const token of chart.value.lyricTokens) {
    const notes = notesForIds(token.noteIds);
    if (notes.length > 0) {
      token.startUs = Math.min(...notes.map((note) => note.startUs));
      token.endUs = Math.max(...notes.map((note) => note.endUs));
    }
  }
  for (const phrase of chart.value.phrases) {
    const tokenIds = new Set(phrase.tokenIds);
    const tokens = chart.value.lyricTokens.filter((token) => tokenIds.has(token.id));
    if (tokens.length > 0) {
      phrase.startUs = Math.min(...tokens.map((token) => token.startUs));
      phrase.endUs = Math.max(...tokens.map((token) => token.endUs));
    }
  }
}

function addLyricTokenFromSelectedNotes() {
  if (!chart.value || selectedNoteIds.value.size === 0 || !newLyricText.value.trim()) return;
  const notes = notesForIds(Array.from(selectedNoteIds.value));
  if (notes.length === 0) return;
  saveSnapshot();
  const token: LyricToken = {
    id: uniqueId("lyric"),
    text: newLyricText.value.trim(),
    ruby: newLyricRuby.value.trim(),
    noteIds: notes.map((note) => note.id),
    startUs: notes[0].startUs,
    endUs: notes[notes.length - 1].endUs,
  };
  chart.value.lyricTokens.push(token);
  chart.value.lyricTokens.sort((a, b) => a.startUs - b.startUs);
  selectedLyricTokenIds.value = new Set([token.id]);
  newLyricText.value = "";
  newLyricRuby.value = "";
}

function toggleLyricToken(tokenId: string) {
  const next = new Set(selectedLyricTokenIds.value);
  if (next.has(tokenId)) next.delete(tokenId);
  else next.add(tokenId);
  selectedLyricTokenIds.value = next;
}

function updateLyricToken(tokenId: string, field: "text" | "ruby", value: string) {
  const token = chart.value?.lyricTokens.find((item) => item.id === tokenId);
  if (!token || token[field] === value) return;
  saveSnapshot();
  token[field] = value;
}

function selectTokenNotes(token: LyricToken) {
  selectedNoteIds.value = new Set(token.noteIds);
  playheadMs.value = token.startUs / 1000;
  scrollMs.value = Math.max(0, playheadMs.value - 1000);
  requestRedraw();
}

function deleteSelectedLyricTokens() {
  if (!chart.value || selectedLyricTokenIds.value.size === 0) return;
  saveSnapshot();
  const deleted = new Set(selectedLyricTokenIds.value);
  chart.value.lyricTokens = chart.value.lyricTokens.filter((token) => !deleted.has(token.id));
  for (const phrase of chart.value.phrases) {
    phrase.tokenIds = phrase.tokenIds.filter((id) => !deleted.has(id));
  }
  chart.value.phrases = chart.value.phrases.filter((phrase) => phrase.tokenIds.length > 0);
  selectedLyricTokenIds.value.clear();
  syncReferencedLyricTimes();
}

function createPhraseFromSelectedTokens() {
  if (!chart.value || selectedLyricTokenIds.value.size === 0) return;
  const wanted = selectedLyricTokenIds.value;
  const tokens = chart.value.lyricTokens
    .filter((token) => wanted.has(token.id))
    .sort((a, b) => a.startUs - b.startUs);
  if (tokens.length === 0) return;
  saveSnapshot();
  chart.value.phrases.push({
    id: uniqueId("phrase"),
    startUs: tokens[0].startUs,
    endUs: tokens[tokens.length - 1].endUs,
    tokenIds: tokens.map((token) => token.id),
  });
  chart.value.phrases.sort((a, b) => a.startUs - b.startUs);
}

function deletePhrase(phraseId: string) {
  if (!chart.value) return;
  saveSnapshot();
  chart.value.phrases = chart.value.phrases.filter((phrase) => phrase.id !== phraseId);
}

function addTempoEventAtPlayhead() {
  if (!chart.value) return;
  saveSnapshot();
  const timeUs = Math.max(1, Math.min(chart.value.durationUs - 1, Math.round(playheadMs.value * 1000)));
  const existing = chart.value.tempoMap.events.find((event) => event.timeUs === timeUs);
  if (existing) existing.bpm = bpm.value;
  else chart.value.tempoMap.events.push({ timeUs, bpm: bpm.value });
  chart.value.tempoMap.events.sort((a, b) => a.timeUs - b.timeUs);
}

function updateTempoEvent(index: number, field: "timeUs" | "bpm", rawValue: string) {
  if (!chart.value) return;
  const event = chart.value.tempoMap.events[index];
  if (!event) return;
  saveSnapshot();
  if (field === "bpm") {
    event.bpm = Math.max(20, Math.min(400, Number(rawValue) || 120));
    if (index === 0 && chart.value.extensions?.tempoEstimate) {
      chart.value.extensions.tempoEstimate.manuallyOverridden = true;
      chart.value.extensions.tempoEstimate.reviewRequired = false;
    }
  }
  else if (index === 0) event.timeUs = 0;
  else {
    const previousTime = chart.value.tempoMap.events[index - 1].timeUs;
    const nextTime = chart.value.tempoMap.events[index + 1]?.timeUs ?? chart.value.durationUs;
    const requestedTime = Math.round((Number(rawValue) || 0) * 1000);
    event.timeUs = Math.max(previousTime + 1, Math.min(nextTime - 1, requestedTime));
  }
  chart.value.tempoMap.events.sort((a, b) => a.timeUs - b.timeUs);
}

function deleteTempoEvent(index: number) {
  if (!chart.value || index === 0) return;
  saveSnapshot();
  chart.value.tempoMap.events.splice(index, 1);
}

// -------------------------------------------------------------
// Real Audio Preview Playback (WASAPI Backend Stream)
// -------------------------------------------------------------
async function togglePlay() {
  if (isPlaying.value) {
    // Stop playback
    try {
      await invoke("stop_editor_preview");
    } finally {
      isPlaying.value = false;
      if (playPollTimer) {
        clearInterval(playPollTimer);
        playPollTimer = null;
      }
    }
  } else {
    // Start real audio playback from current playhead position
    try {
      // If playhead is offscreen, jump scroll so it is visible!
      const curX = msToX(playheadMs.value);
      if (canvasRef.value && (curX < keyboardWidth || curX > canvasRef.value.width - 50)) {
        scrollMs.value = Math.max(0, playheadMs.value - 1500);
      }

      await invoke("start_editor_preview", {
        startMs: playheadMs.value,
        renderDeviceId: null,
        songId: props.songId,
      });
      isPlaying.value = true;

      // Poll actual audio hardware clock position from WASAPI
      playPollTimer = window.setInterval(async () => {
        try {
          const posMs = await invoke<number | null>("get_editor_preview_pos");
          if (posMs !== null && posMs !== undefined) {
            playheadMs.value = posMs;

            // Auto-scroll screen when playhead reaches 85% width
            const currentX = msToX(playheadMs.value);
            if (canvasRef.value && currentX > (canvasRef.value.width / (window.devicePixelRatio || 1)) * 0.85) {
              scrollMs.value = playheadMs.value - 2000;
            }
            requestRedraw();
          }
        } catch (e) {
          console.error("Poll playback error:", e);
        }
      }, 25);
    } catch (err) {
      console.error("Failed to start editor audio preview:", err);
      isPlaying.value = false;
    }
  }
}

// Quick navigation buttons
function jumpToStart() {
  playheadMs.value = 0;
  scrollMs.value = 0;
  requestRedraw();
}

function jumpToVocalIntro() {
  playheadMs.value = vocalIntroSec.value * 1000.0;
  scrollMs.value = Math.max(0, playheadMs.value - 2000.0);
  requestRedraw();
}

function jumpToPlayhead() {
  scrollMs.value = Math.max(0, playheadMs.value - 2000.0);
  requestRedraw();
}

// -------------------------------------------------------------
// Keyboard Shortcuts
// -------------------------------------------------------------
function handleKeyDown(e: KeyboardEvent) {
  if (
    e.target instanceof HTMLInputElement
    || e.target instanceof HTMLSelectElement
    || e.target instanceof HTMLTextAreaElement
  ) return;

  if (e.code === "Space") {
    e.preventDefault();
    togglePlay();
  } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z") {
    e.preventDefault();
    if (e.shiftKey) redo();
    else undo();
  } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "y") {
    e.preventDefault();
    redo();
  } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
    e.preventDefault();
    saveChart();
  } else if (e.key === "Delete" || e.key === "Backspace") {
    e.preventDefault();
    deleteSelectedNotes();
  } else if (e.key.toLowerCase() === "s") {
    splitSelectedNote();
  } else if (e.key.toLowerCase() === "m") {
    mergeSelectedNotes();
  }
}

// -------------------------------------------------------------
// Live Mic Voice Guide Polling & Auto-Start
// -------------------------------------------------------------
async function pollMicGuide() {
  if (!micGuideEnabled.value) return;
  try {
    const diag = await invoke<MicDiagnostic>("poll_mic_diagnostic");
    const prevHadSignal = micDiagnostic.value && (micDiagnostic.value.rmsDbfs > -58.0 || micDiagnostic.value.state === "VOICED");
    micDiagnostic.value = diag;

    const curHadSignal = diag && (diag.rmsDbfs > -58.0 || diag.state === "VOICED" || (diag.f0Hz !== null && diag.f0Hz > 50));
    if (curHadSignal || prevHadSignal) {
      requestRedraw();
    }
  } catch (e) {
    // Ignore if audio engine is idle
  }
}

onMounted(async () => {
  window.addEventListener("keydown", handleKeyDown);
  window.addEventListener("resize", () => {
    resizeCanvas();
    requestRedraw();
  });
  loadData();

  // Automatically start mic capture pipeline so user voice is instantly visible in editor!
  try {
    await invoke("start_mic_pipeline", { deviceId: null });
  } catch (e) {
    console.warn("Could not start mic pipeline in editor:", e);
  }

  // Poll live mic diagnostic every 25ms for seamless real-time voice pitch feedback
  micPollTimer = window.setInterval(pollMicGuide, 25);
});

watch([chart, vocalIntroSec, baseMidi], scheduleRecovery, { deep: true });

onUnmounted(() => {
  window.removeEventListener("keydown", handleKeyDown);
  if (animFrameId) cancelAnimationFrame(animFrameId);
  if (playPollTimer) clearInterval(playPollTimer);
  if (micPollTimer) clearInterval(micPollTimer);
  if (recoveryTimer !== null) clearTimeout(recoveryTimer);
  invoke("stop_editor_preview").catch(() => {});
  invoke("stop_mic_pipeline").catch(() => {});
});
</script>

<template>
  <div class="score-editor-container">
    <!-- Top Toolbar -->
    <header class="editor-toolbar">
      <!-- Playback Controls -->
      <div class="toolbar-group">
        <button class="btn btn-primary play-btn" @click="togglePlay">
          <span class="btn-icon">{{ isPlaying ? "⏸" : "▶" }}</span>
          <span>{{ isPlaying ? "停止 (Space)" : "音を鳴らして再生 (Space)" }}</span>
        </button>
        <span class="time-display" title="現在の再生位置">
          {{ (playheadMs / 1000).toFixed(2) }}s
        </span>
      </div>

      <!-- Quick Navigation -->
      <div class="toolbar-group">
        <button class="btn btn-secondary btn-sm" @click="jumpToStart" title="曲の最初 (0秒) へジャンプ">
          ⏮ 0秒
        </button>
        <button class="btn btn-secondary btn-sm" @click="jumpToVocalIntro" :title="`歌い出し (${vocalIntroSec.toFixed(1)}秒) へジャンプ`">
          ⏩ 歌い出し({{ vocalIntroSec.toFixed(1) }}s)
        </button>
        <button class="btn btn-secondary btn-sm" @click="jumpToPlayhead" title="再生ヘッドが見える位置へ画面をスクロール">
          📍 赤バーへ
        </button>
      </div>

      <!-- Vocal Intro Config (User Configurable & Persisted to JSON) -->
      <div class="toolbar-group highlight-box" title="歌い出し秒数の設定（JSONに保存されます）">
        <label class="toolbar-label">歌い出し:</label>
        <input
          type="number"
          v-model.number="vocalIntroSec"
          step="0.5"
          min="0"
          max="600"
          class="toolbar-number-input"
          title="歌い出し秒数（直接入力・矢印で変更可能）"
          @focus="saveSnapshot"
          @change="requestRedraw"
        />
        <span class="toolbar-unit">s</span>
        <button
          class="btn btn-secondary btn-xs"
          @click="setPlayheadAsVocalIntro"
          title="現在の赤ライン位置を歌い出し秒数にワンクリック設定"
        >
          📍 現在地セット
        </button>
      </div>

      <!-- Base Key / Transpose Config (User Configurable & Centered Graph) -->
      <div class="toolbar-group highlight-box" title="グラフの基準キー設定（A4固定を解除、自由に変更可能）">
        <label class="toolbar-label">基準キー:</label>
        <select v-model.number="baseMidi" class="toolbar-select" @focus="saveSnapshot" @change="requestRedraw" title="グラフの中心・基準音を設定">
          <option :value="48">C3 (48)</option>
          <option :value="53">F3 (53)</option>
          <option :value="57">A3 (57)</option>
          <option :value="60">C4 (60 - 中央ド)</option>
          <option :value="65">F4 (65)</option>
          <option :value="69">A4 (69 - 基準440Hz)</option>
          <option :value="72">C5 (72)</option>
          <option :value="77">F5 (77)</option>
        </select>
        <button class="btn btn-secondary btn-xs" @click="shiftAllNotes(-1)" title="全ノーツと基準キーを半音下げる (Key -1)">
          ♭ -1
        </button>
        <button class="btn btn-secondary btn-xs" @click="shiftAllNotes(1)" title="全ノーツと基準キーを半音上げる (Key +1)">
          ♯ +1
        </button>
      </div>

      <!-- History Controls -->
      <div class="toolbar-group">
        <button class="btn btn-secondary btn-sm" @click="undo" :disabled="undoStack.length === 0" title="元に戻す (Ctrl+Z)">
          ↶ Undo
        </button>
        <button class="btn btn-secondary btn-sm" @click="redo" :disabled="redoStack.length === 0" title="やり直す (Ctrl+Y)">
          ↷ Redo
        </button>
      </div>

      <!-- Note Editing Tools -->
      <div class="toolbar-group">
        <button class="btn btn-secondary btn-sm" @click="splitSelectedNote" :disabled="selectedNoteIds.size !== 1" title="選択ノーツを再生ヘッドで2分割 (Sキー)">
          ✂ 分割(S)
        </button>
        <button class="btn btn-secondary btn-sm" @click="mergeSelectedNotes" :disabled="selectedNoteIds.size < 2" title="選択ノーツを結合 (Mキー)">
          🔗 結合(M)
        </button>
        <button class="btn btn-danger btn-sm" @click="deleteSelectedNotes" :disabled="selectedNoteIds.size === 0" title="選択ノーツを削除 (Delキー)">
          🗑 削除(Del)
        </button>
      </div>

      <!-- Snap Selection -->
      <div class="toolbar-group">
        <label class="toolbar-label">スナップ:</label>
        <select v-model="snapMode" class="toolbar-select">
          <option value="off">OFF (ミリ秒直接)</option>
          <option value="16th">16分音符 (1/4拍)</option>
          <option value="8th">8分音符 (1/2拍)</option>
          <option value="4th">4分音符 (1拍)</option>
        </select>
      </div>

      <!-- Live Mic Voice Guide Toggle -->
      <div class="toolbar-group">
        <button
          class="btn btn-sm"
          :class="micGuideEnabled ? 'btn-mic-active' : 'btn-secondary'"
          @click="micGuideEnabled = !micGuideEnabled"
          title="マイクで声を出した時のピッチをキャンバス上の赤ラインにリアルタイム可視化（自分の声とバーを照合）"
        >
          <span>🎤 声ガイド: {{ micGuideEnabled ? 'ON' : 'OFF' }}</span>
          <span v-if="micGuideEnabled && micDiagnostic?.midiNote" class="mic-pitch-badge">
            {{ midiToName(Math.round(micDiagnostic.midiNote)) }}
          </span>
        </button>
      </div>

      <!-- Zoom Slider -->
      <div class="toolbar-group">
        <label class="toolbar-label">ズーム:</label>
        <input type="range" v-model.number="pixelsPerSecond" min="30" max="350" class="zoom-slider" title="横軸の拡大・縮小" />
      </div>

      <!-- Save & Status -->
      <div class="toolbar-group right-group">
        <span v-if="saveMessage" class="save-status">{{ saveMessage }}</span>
        <span v-else-if="recoveryStatus" class="save-status">{{ recoveryStatus }}</span>
        <span v-else-if="isDirty" class="save-status">未保存</span>
        <button class="btn btn-secondary btn-sm" @click="emit('requestAnalysis')" title="音声ファイルをまるまる再解析する画面へ移動">
          🔄 音声再解析
        </button>
        <button class="btn btn-secondary btn-sm" @click="downloadChartJson" title="現在の譜面をJSONファイルとしてPCにダウンロード">
          📥 ダウンロード
        </button>
        <button class="btn btn-success" @click="saveChart" :disabled="isSaving" title="譜面を保存 (Ctrl+S)">
          💾 保存 (Ctrl+S)
        </button>
      </div>
    </header>

    <!-- Canvas Area (Wrapper ensures exact 1:1 pixel dimensions) -->
    <div class="canvas-wrapper" ref="wrapperRef">
      <div v-if="isLoading" class="loading-overlay">
        <div class="loading-spinner"></div>
        <p>全曲下書き譜面 & 波形・F0データを読み込み中...</p>
      </div>

      <canvas
        ref="canvasRef"
        class="editor-canvas"
        @mousedown="handleMouseDown"
        @mousemove="handleMouseMove"
        @mouseup="handleMouseUp"
        @dblclick="handleDoubleClick"
        @wheel="handleWheel"
      ></canvas>
    </div>

    <section class="metadata-editor">
      <div class="metadata-tabs">
        <button class="metadata-tab" :class="{ active: editPanel === 'lyrics' }" @click="editPanel = 'lyrics'">
          歌詞・フレーズ ({{ chart?.lyricTokens.length ?? 0 }})
        </button>
        <button class="metadata-tab" :class="{ active: editPanel === 'tempo' }" @click="editPanel = 'tempo'">
          BPM・テンポ ({{ chart?.tempoMap.events.length ?? 0 }})
        </button>
      </div>

      <div v-if="editPanel === 'lyrics'" class="metadata-content lyrics-editor">
        <div class="lyric-compose">
          <strong>選択ノーツへ歌詞を割り当て</strong>
          <input v-model="newLyricText" class="metadata-input lyric-text-input" placeholder="歌詞（例：シャイニング）" />
          <input v-model="newLyricRuby" class="metadata-input" placeholder="ルビ（任意）" />
          <button
            class="btn btn-primary btn-sm"
            :disabled="selectedNoteIds.size === 0 || !newLyricText.trim()"
            @click="addLyricTokenFromSelectedNotes"
          >
            選択{{ selectedNoteIds.size }}ノーツへ追加
          </button>
          <button
            class="btn btn-secondary btn-sm"
            :disabled="selectedLyricTokenIds.size === 0"
            @click="createPhraseFromSelectedTokens"
          >
            選択歌詞をフレーズ化
          </button>
          <button
            class="btn btn-danger btn-sm"
            :disabled="selectedLyricTokenIds.size === 0"
            @click="deleteSelectedLyricTokens"
          >
            歌詞を削除
          </button>
        </div>

        <div class="metadata-scroll">
          <table class="metadata-table">
            <thead>
              <tr><th>選択</th><th>開始–終了</th><th>歌詞</th><th>ルビ</th><th>ノーツ</th><th>移動</th></tr>
            </thead>
            <tbody>
              <tr v-if="(chart?.lyricTokens.length ?? 0) === 0">
                <td colspan="6" class="empty-metadata">ノーツを選択し、歌詞とルビを入力して追加します。</td>
              </tr>
              <tr v-for="token in chart?.lyricTokens ?? []" :key="token.id" :class="{ selected: selectedLyricTokenIds.has(token.id) }">
                <td><input type="checkbox" :checked="selectedLyricTokenIds.has(token.id)" @change="toggleLyricToken(token.id)" /></td>
                <td class="mono">{{ (token.startUs / 1e6).toFixed(2) }}–{{ (token.endUs / 1e6).toFixed(2) }}s</td>
                <td><input class="metadata-input" :value="token.text" @change="updateLyricToken(token.id, 'text', inputValue($event))" /></td>
                <td><input class="metadata-input" :value="token.ruby" @change="updateLyricToken(token.id, 'ruby', inputValue($event))" /></td>
                <td class="mono">{{ token.noteIds.length }}</td>
                <td><button class="btn btn-secondary btn-xs" @click="selectTokenNotes(token)">表示</button></td>
              </tr>
            </tbody>
          </table>
        </div>

        <div class="phrase-list">
          <span class="section-label">フレーズ:</span>
          <span v-if="(chart?.phrases.length ?? 0) === 0" class="empty-inline">未設定</span>
          <span v-for="phrase in chart?.phrases ?? []" :key="phrase.id" class="phrase-chip">
            {{ (phrase.startUs / 1e6).toFixed(1) }}–{{ (phrase.endUs / 1e6).toFixed(1) }}s / {{ phrase.tokenIds.length }}語
            <button @click="deletePhrase(phrase.id)" title="フレーズだけ削除">×</button>
          </span>
        </div>
      </div>

      <div v-else class="metadata-content tempo-editor">
        <div class="tempo-help">
          <strong>テンポマップ</strong>
          <span>赤い再生ヘッド位置へテンポ変更点を追加できます。先頭イベントは必ず0秒です。</span>
          <span v-if="chart?.extensions?.tempoEstimate" class="tempo-estimate-badge">
            {{ chart.extensions.tempoEstimate.manuallyOverridden
              ? "先頭BPMは手動修正済み"
              : chart.extensions.tempoEstimate.usedFallback
                ? "自動推定できず仮BPM（要確認）"
                : `自動推定・要確認（信頼度 ${Math.round(chart.extensions.tempoEstimate.confidence * 100)}%）` }}
          </span>
          <button class="btn btn-primary btn-sm" @click="addTempoEventAtPlayhead">
            現在地 {{ (playheadMs / 1000).toFixed(2) }}s に追加
          </button>
        </div>
        <div class="tempo-events">
          <div v-for="(event, index) in chart?.tempoMap.events ?? []" :key="`${event.timeUs}-${index}`" class="tempo-row">
            <label>時刻</label>
            <input
              class="metadata-input number-input"
              type="number"
              step="0.01"
              :disabled="index === 0"
              :value="(event.timeUs / 1000).toFixed(2)"
              @change="updateTempoEvent(index, 'timeUs', inputValue($event))"
            />
            <span>ms</span>
            <label>BPM</label>
            <input
              class="metadata-input number-input"
              type="number"
              min="20"
              max="400"
              step="0.01"
              :value="event.bpm"
              @change="updateTempoEvent(index, 'bpm', inputValue($event))"
            />
            <button class="btn btn-danger btn-xs" :disabled="index === 0" @click="deleteTempoEvent(index)">削除</button>
          </div>
        </div>
      </div>
    </section>

    <!-- Bottom Status & Hints -->
    <footer class="editor-footer">
      <div class="footer-stats">
        <span>ノーツ数: <strong>{{ chart?.notes.length ?? 0 }}</strong> 個</span>
        <span>選択中: <strong>{{ selectedNoteIds.size }}</strong> 個</span>
        <span>BPM: <strong>{{ bpm }}</strong> (1拍 = {{ quarterBeatMs.toFixed(1) }}ms)</span>
      </div>
      <div class="footer-hints">
        💡 <strong>操作方法</strong>: 上のタイムルーラーをクリックして赤い再生ヘッド移動 | ノーツをドラッグして音高・時間変更（音が鳴ります） | 空白ダブルクリックで追加 | Spaceで実音再生
      </div>
    </footer>
  </div>
</template>

<style scoped>
.score-editor-container {
  display: flex;
  flex-direction: column;
  height: calc(100vh - 130px);
  background: #0b0c10;
  border-radius: 10px;
  overflow: hidden;
  border: 1px solid #1f2430;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.45);
}

.editor-toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 14px;
  background: #141721;
  border-bottom: 1px solid #252b3b;
  flex-wrap: wrap;
}

.toolbar-group {
  display: flex;
  align-items: center;
  gap: 6px;
}

.right-group {
  margin-left: auto;
}

.toolbar-label {
  font-size: 11px;
  color: #94a3b8;
}

.toolbar-select {
  background: #1e2333;
  color: #e2e8f0;
  border: 1px solid #334155;
  border-radius: 5px;
  padding: 4px 6px;
  font-size: 11px;
}

.toolbar-number-input {
  width: 54px;
  background: #1e2333;
  color: #38bdf8;
  border: 1px solid #334155;
  border-radius: 4px;
  padding: 3px 4px;
  font-size: 11px;
  font-weight: bold;
  font-family: monospace;
}

.toolbar-unit {
  font-size: 11px;
  color: #94a3b8;
  margin-left: -2px;
}

.highlight-box {
  background: rgba(30, 41, 59, 0.7);
  padding: 3px 8px;
  border-radius: 6px;
  border: 1px solid rgba(56, 189, 248, 0.25);
}

.zoom-slider {
  width: 75px;
  accent-color: #8b5cf6;
}

.time-display {
  font-family: monospace;
  font-size: 14px;
  font-weight: bold;
  color: #38bdf8;
  background: #0f172a;
  padding: 4px 8px;
  border-radius: 5px;
  border: 1px solid #1e293b;
}

.btn {
  padding: 5px 10px;
  font-size: 12px;
  font-weight: 600;
  border-radius: 5px;
  border: none;
  cursor: pointer;
  transition: all 0.15s ease;
  display: flex;
  align-items: center;
  gap: 5px;
}

.btn-sm {
  padding: 4px 8px;
  font-size: 11px;
}

.btn-xs {
  padding: 2px 6px;
  font-size: 10px;
}

.btn:disabled {
  opacity: 0.35;
  cursor: not-allowed;
}

.play-btn {
  background: linear-gradient(135deg, #10b981, #059669);
  color: #ffffff;
  font-size: 12px;
  padding: 6px 12px;
}
.play-btn:hover:not(:disabled) {
  filter: brightness(1.2);
}

.btn-primary {
  background: linear-gradient(135deg, #6366f1, #8b5cf6);
  color: #ffffff;
}

.btn-secondary {
  background: #23293a;
  color: #e2e8f0;
  border: 1px solid #38425e;
}
.btn-secondary:hover:not(:disabled) {
  background: #2e364c;
}

.btn-danger {
  background: rgba(239, 68, 68, 0.2);
  color: #fca5a5;
  border: 1px solid rgba(239, 68, 68, 0.4);
}
.btn-danger:hover:not(:disabled) {
  background: rgba(239, 68, 68, 0.35);
}

.btn-success {
  background: linear-gradient(135deg, #8b5cf6, #7c3aed);
  color: #ffffff;
}
.btn-success:hover:not(:disabled) {
  filter: brightness(1.2);
}

.btn-mic-on {
  background: rgba(6, 182, 212, 0.22);
  color: #67e8f9;
  border: 1px solid rgba(6, 182, 212, 0.5);
}
.btn-mic-on:hover:not(:disabled) {
  background: rgba(6, 182, 212, 0.35);
}

.btn-mic-active {
  background: linear-gradient(135deg, #06b6d4, #0284c7);
  color: #ffffff;
  border: 1px solid #38bdf8;
  box-shadow: 0 0 10px rgba(6, 182, 212, 0.6);
}

.mic-pitch-badge {
  background: #0f172a;
  color: #38bdf8;
  padding: 1px 5px;
  border-radius: 4px;
  font-family: monospace;
  font-size: 10px;
  font-weight: bold;
  border: 1px solid #0284c7;
}

.save-status {
  font-size: 12px;
  color: #34d399;
  font-weight: 600;
  margin-right: 6px;
}

.canvas-wrapper {
  position: relative;
  flex: 1;
  min-height: 220px;
  overflow: hidden;
  background: #0d0f15;
  width: 100%;
  height: 100%;
}

.metadata-editor {
  flex: 0 0 220px;
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: #10131c;
  border-top: 1px solid #293044;
}

.metadata-tabs {
  display: flex;
  gap: 2px;
  padding: 5px 10px 0;
  background: #141721;
}

.metadata-tab {
  padding: 6px 12px;
  color: #94a3b8;
  background: #1a1f2c;
  border: 1px solid #30384c;
  border-bottom: none;
  border-radius: 5px 5px 0 0;
  cursor: pointer;
  font-size: 11px;
  font-weight: 700;
}

.metadata-tab.active {
  color: #67e8f9;
  background: #202737;
  border-color: #0e7490;
}

.metadata-content {
  flex: 1;
  min-height: 0;
  padding: 8px 10px;
  color: #cbd5e1;
  font-size: 11px;
}

.lyrics-editor {
  display: grid;
  grid-template-columns: 1fr;
  grid-template-rows: auto minmax(70px, 1fr) auto;
  gap: 7px;
}

.lyric-compose,
.tempo-help,
.tempo-row,
.phrase-list {
  display: flex;
  align-items: center;
  gap: 7px;
}

.lyric-text-input {
  min-width: 180px;
}

.metadata-input {
  min-width: 90px;
  padding: 4px 6px;
  color: #e2e8f0;
  background: #171c28;
  border: 1px solid #35405a;
  border-radius: 4px;
  font-size: 11px;
}

.metadata-input:focus {
  outline: 1px solid #06b6d4;
  border-color: #06b6d4;
}

.metadata-scroll,
.tempo-events {
  min-height: 0;
  overflow: auto;
}

.metadata-table {
  width: 100%;
  border-collapse: collapse;
}

.metadata-table th,
.metadata-table td {
  padding: 3px 6px;
  text-align: left;
  border-bottom: 1px solid #252c3c;
}

.metadata-table th {
  position: sticky;
  top: 0;
  z-index: 1;
  color: #94a3b8;
  background: #161b27;
}

.metadata-table tr.selected td {
  background: rgba(14, 116, 144, 0.18);
}

.metadata-table .metadata-input {
  width: 100%;
  box-sizing: border-box;
}

.mono {
  white-space: nowrap;
  font-family: monospace;
}

.empty-metadata,
.empty-inline {
  color: #64748b;
}

.phrase-list {
  overflow-x: auto;
  white-space: nowrap;
}

.section-label {
  color: #94a3b8;
  font-weight: 700;
}

.phrase-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 6px;
  color: #c4b5fd;
  background: rgba(124, 58, 237, 0.16);
  border: 1px solid rgba(139, 92, 246, 0.4);
  border-radius: 999px;
}

.phrase-chip button {
  color: #fca5a5;
  background: transparent;
  border: 0;
  cursor: pointer;
}

.tempo-editor {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tempo-help span {
  color: #94a3b8;
}

.tempo-help .tempo-estimate-badge {
  padding: 3px 7px;
  color: #fde68a;
  white-space: nowrap;
  background: rgba(161, 98, 7, 0.2);
  border: 1px solid rgba(245, 158, 11, 0.45);
  border-radius: 999px;
}

.tempo-events {
  display: flex;
  flex-wrap: wrap;
  align-content: flex-start;
  gap: 6px;
}

.tempo-row {
  padding: 5px 7px;
  background: #171c28;
  border: 1px solid #30384c;
  border-radius: 5px;
}

.number-input {
  width: 80px;
  min-width: 80px;
  font-family: monospace;
}

.editor-canvas {
  width: 100%;
  height: 100%;
  display: block;
}

.loading-overlay {
  position: absolute;
  inset: 0;
  background: rgba(11, 12, 16, 0.88);
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 14px;
  color: #38bdf8;
  font-size: 14px;
  z-index: 10;
}

.loading-spinner {
  width: 32px;
  height: 32px;
  border: 3px solid rgba(56, 189, 248, 0.2);
  border-top-color: #38bdf8;
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.editor-footer {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 14px;
  background: #12151f;
  border-top: 1px solid #202636;
  font-size: 11px;
  color: #94a3b8;
}

.footer-stats {
  display: flex;
  gap: 14px;
}

.footer-stats strong {
  color: #38bdf8;
}

.footer-hints {
  color: #64748b;
}
.footer-hints strong {
  color: #c084fc;
}
</style>
