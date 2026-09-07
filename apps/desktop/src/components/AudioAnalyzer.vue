<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";

const props = defineProps<{
  songId: string;
  songTitle: string;
  songArtist: string;
}>();

interface AnalysisProgress {
  is_running: boolean;
  progress_percent: number;
  status_text: string;
  logs: string[];
  completed: boolean;
  auto_adopted: boolean;
  error: string | null;
}

const emit = defineEmits<{
  (e: "openEditor"): void;
  (e: "openKaraoke"): void;
}>();

const isRunning = ref(false);
const progressPercent = ref(0);
const statusText = ref("待機中 - 「音声をまるまる解析」を押してください");
const logs = ref<string[]>([]);
const completed = ref(false);
const autoAdopted = ref(false);
const errorMessage = ref<string | null>(null);
const forceRecompute = ref(false);
const candidateNotes = ref<number | null>(null);
const logContainerRef = ref<HTMLDivElement | null>(null);

let pollTimer: number | null = null;

async function startAnalysis() {
  if (isRunning.value) return;
  errorMessage.value = null;
  completed.value = false;
  autoAdopted.value = false;
  progressPercent.value = 5;
  statusText.value = "AI解析プロセスを起動中...";
  logs.value = ["[*] AI音声解析プロセスを開始します..."];
  isRunning.value = true;

  try {
    await invoke("run_audio_analysis", {
      force: forceRecompute.value,
      songId: props.songId,
      autoAdopt: false,
    });
    startPolling();
  } catch (err: any) {
    errorMessage.value = `解析開始エラー: ${err}`;
    isRunning.value = false;
  }
}

function startPolling() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = window.setInterval(async () => {
    try {
      const status = await invoke<AnalysisProgress>("get_analysis_status");
      isRunning.value = status.is_running;
      progressPercent.value = status.progress_percent;
      if (status.status_text) statusText.value = status.status_text;
      logs.value = status.logs;
      completed.value = status.completed;
      autoAdopted.value = status.auto_adopted;
      errorMessage.value = status.error;

      // Auto scroll logs to bottom
      if (logContainerRef.value) {
        logContainerRef.value.scrollTop = logContainerRef.value.scrollHeight;
      }

      if (!status.is_running && (status.completed || status.error)) {
        if (status.completed) {
          try {
            const candidate = await invoke<any>("get_analysis_candidate", { songId: props.songId });
            candidateNotes.value = candidate.notes?.length ?? null;
          } catch (_) {
            candidateNotes.value = null;
          }
        }
        if (pollTimer) {
          clearInterval(pollTimer);
          pollTimer = null;
        }
      }
    } catch (e) {
      console.error("Poll status error:", e);
    }
  }, 200);
}

// Download chart as JSON file to local PC
async function downloadChart() {
  try {
    const chart = await invoke<any>(
      completed.value ? "get_analysis_candidate" : "get_current_chart",
      { songId: props.songId },
    );
    if (!chart) return;
    const jsonStr = JSON.stringify(chart, null, 2);
    const blob = new Blob([jsonStr], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${chart.songId || "shining_star"}_chart.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  } catch (e) {
    alert("ダウンロードに失敗しました: " + e);
  }
}

async function adoptCandidate() {
  if (!window.confirm("現在の譜面をバックアップし、解析候補を新しいrevisionとして採用しますか？")) return;
  try {
    const adopted = await invoke<any>("adopt_analysis_candidate", { songId: props.songId });
    candidateNotes.value = adopted.notes?.length ?? candidateNotes.value;
    statusText.value = `候補を revision ${adopted.chartRevision} として採用しました`;
    emit("openEditor");
  } catch (e) {
    errorMessage.value = `候補の採用に失敗しました: ${e}`;
  }
}

onMounted(async () => {
  try {
    const status = await invoke<AnalysisProgress>("get_analysis_status");
    isRunning.value = status.is_running;
    progressPercent.value = status.progress_percent;
    if (status.status_text) statusText.value = status.status_text;
    logs.value = status.logs;
    completed.value = status.completed;
    autoAdopted.value = status.auto_adopted;
    errorMessage.value = status.error;
    if (status.is_running) {
      startPolling();
    } else if (!status.completed) {
      // Analysis state is in-memory, while a verified candidate persists
      // across app restarts. Discover it without forcing another analysis.
      try {
        const candidate = await invoke<any>("get_analysis_candidate", { songId: props.songId });
        candidateNotes.value = candidate.notes?.length ?? null;
        completed.value = true;
        progressPercent.value = 100;
        statusText.value = `検証済みの未採用候補があります（${candidateNotes.value ?? "?"}ノーツ）`;
      } catch (_) {
        // No persisted candidate yet.
      }
    }
  } catch (e) {
    // Ignore initial poll error
  }
});

onUnmounted(() => {
  if (pollTimer) clearInterval(pollTimer);
});
</script>

<template>
  <div class="analyzer-container">
    <!-- Header Card -->
    <section class="card header-card">
      <div class="card-badge">AI AUTOMATIC TRANSCRIPTION</div>
      <h2>🎙️ 音声ファイル全編AI解析 & 自動採譜スタジオ</h2>
      <p class="desc">
        選択中の曲をDemucsでボーカル分離し、RMVPEピッチ軌跡から歌唱音程バーを自動生成します。
      </p>

      <div class="specs-grid">
        <div class="spec-item">
          <span class="spec-label">対象楽曲</span>
          <span class="spec-value">{{ songArtist || "未設定" }}「{{ songTitle }}」</span>
        </div>
        <div class="spec-item">
          <span class="spec-label">音源ファイル</span>
          <span class="spec-value font-mono">{{ songId }}</span>
        </div>
        <div class="spec-item">
          <span class="spec-label">ボーカル分離</span>
          <span class="spec-value">Demucs HTDemucs (2-stems)</span>
        </div>
        <div class="spec-item">
          <span class="spec-label">ピッチ抽出</span>
          <span class="spec-value">RMVPE (10ms解像度 / 16000Hz)</span>
        </div>
        <div class="spec-item">
          <span class="spec-label">ノート分割改善</span>
          <span class="spec-value text-emerald">ビブラート平滑化 + オンセット同音分割</span>
        </div>
      </div>
    </section>

    <!-- Execution Panel -->
    <section class="card control-card">
      <div class="control-header">
        <h3>解析の実行</h3>
        <div class="options-row">
          <label class="checkbox-label" title="既存の分離ボーカルやF0キャッシュを削除し、初めから再分離を実行します">
            <input type="checkbox" v-model="forceRecompute" :disabled="isRunning" />
            <span>キャッシュをクリアして完全再解析 (※数分かかります)</span>
          </label>
        </div>
      </div>

      <div class="action-bar">
        <button
          class="btn btn-primary btn-lg"
          @click="startAnalysis"
          :disabled="isRunning"
        >
          <span class="btn-icon">{{ isRunning ? "⏳" : "▶" }}</span>
          <span>{{ isRunning ? "音声をまるまる解析中..." : "音声をまるまる解析 (再解析) を開始" }}</span>
        </button>

        <button
          class="btn btn-secondary btn-lg"
          @click="downloadChart"
          title="現在保存されている最新譜面JSONをPCにダウンロード"
        >
          <span>📥 譜面JSONをダウンロード</span>
        </button>
      </div>

      <!-- Progress Section -->
      <div class="progress-section">
        <div class="progress-header">
          <span class="status-badge" :class="{ 'status-active': isRunning, 'status-done': completed, 'status-error': errorMessage }">
            {{ isRunning ? "実行中" : completed ? "完了" : errorMessage ? "エラー" : "待機中" }}
          </span>
          <span class="status-text">{{ statusText }}</span>
          <span class="progress-number font-mono">{{ progressPercent }}%</span>
        </div>

        <div class="progress-track">
          <div
            class="progress-bar"
            :class="{ 'bar-anim': isRunning, 'bar-done': completed, 'bar-error': errorMessage }"
            :style="{ width: `${progressPercent}%` }"
          ></div>
        </div>
      </div>

      <!-- Completion Actions Alert -->
      <div v-if="completed" class="completion-alert">
        <div class="alert-icon">✨</div>
        <div class="alert-content">
          <h4>{{ autoAdopted ? "AI音程バーの生成・保存が完了しました" : "全曲の解析候補が完了しました" }}{{ candidateNotes !== null ? ` (${candidateNotes}ノーツ)` : "" }}</h4>
          <p v-if="autoAdopted">新規曲の譜面へ自動保存済みです。すぐにエディタまたはカラオケ歌唱で確認できます。</p>
          <p v-else>安全のため現行譜面にはまだ反映していません。候補をダウンロードして確認するか、明示的に採用してください。</p>
          <div class="alert-buttons">
            <button v-if="!autoAdopted" class="btn btn-emerald" @click="adoptCandidate">
              <span>✅ 候補を採用してエディタで開く</span>
            </button>
            <button v-else class="btn btn-emerald" @click="emit('openEditor')">
              <span>🎼 保存済み譜面をエディタで開く</span>
            </button>
            <button class="btn btn-cyan" @click="downloadChart">
              <span>📥 譜面JSONをダウンロード</span>
            </button>
            <button class="btn btn-purple" @click="emit('openKaraoke')">
              <span>🌟 カラオケ歌唱へ進む</span>
            </button>
          </div>
        </div>
      </div>

      <!-- Error Alert -->
      <div v-if="errorMessage" class="error-alert">
        <div class="alert-icon">❌</div>
        <div class="alert-content">
          <h4>解析エラー</h4>
          <p>{{ errorMessage }}</p>
        </div>
      </div>
    </section>

    <!-- Real-time Log Terminal -->
    <section class="card terminal-card">
      <div class="terminal-header">
        <span class="terminal-dot red"></span>
        <span class="terminal-dot yellow"></span>
        <span class="terminal-dot green"></span>
        <span class="terminal-title">解析プロセス リアルタイム出力ログ (Python Pipeline)</span>
      </div>
      <div class="terminal-body" ref="logContainerRef">
        <div v-if="logs.length === 0" class="log-placeholder">
          解析を開始すると、ここにDemucsボーカル分離やRMVPEピッチ抽出のログがリアルタイムに表示されます。
        </div>
        <div v-for="(line, idx) in logs" :key="idx" class="log-line font-mono">
          <span class="log-prefix">&gt;</span> {{ line }}
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.analyzer-container {
  display: flex;
  flex-direction: column;
  gap: 16px;
  width: 100%;
}

.card {
  background: #131722;
  border: 1px solid #23293a;
  border-radius: 12px;
  padding: 20px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.35);
}

.header-card {
  position: relative;
  background: linear-gradient(135deg, #181d2e 0%, #111420 100%);
  border-left: 4px solid #8b5cf6;
}

.card-badge {
  display: inline-block;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.08em;
  color: #a78bfa;
  background: rgba(139, 92, 246, 0.15);
  padding: 3px 8px;
  border-radius: 4px;
  margin-bottom: 8px;
}

.header-card h2 {
  font-size: 20px;
  font-weight: 700;
  color: #f1f5f9;
  margin: 0 0 6px 0;
}

.desc {
  font-size: 13px;
  color: #94a3b8;
  margin: 0 0 16px 0;
  line-height: 1.5;
}

.specs-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 10px;
  padding-top: 12px;
  border-top: 1px solid rgba(255, 255, 255, 0.08);
}

.spec-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.spec-label {
  font-size: 11px;
  color: #64748b;
}

.spec-value {
  font-size: 13px;
  font-weight: 600;
  color: #e2e8f0;
}

.font-mono {
  font-family: monospace;
}

.text-emerald {
  color: #34d399;
}

.control-card {
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.control-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
}

.control-header h3 {
  font-size: 16px;
  font-weight: 600;
  color: #f1f5f9;
  margin: 0;
}

.checkbox-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: #cbd5e1;
  cursor: pointer;
}

.action-bar {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
}

.btn {
  padding: 8px 16px;
  font-size: 13px;
  font-weight: 600;
  border-radius: 6px;
  border: none;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 8px;
  transition: all 0.2s ease;
}

.btn-lg {
  padding: 12px 22px;
  font-size: 14px;
}

.btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.btn-primary {
  background: linear-gradient(135deg, #6366f1, #8b5cf6);
  color: #ffffff;
  box-shadow: 0 4px 14px rgba(99, 102, 241, 0.35);
}
.btn-primary:hover:not(:disabled) {
  filter: brightness(1.15);
  transform: translateY(-1px);
}

.btn-secondary {
  background: #23293a;
  color: #e2e8f0;
  border: 1px solid #38425e;
}
.btn-secondary:hover:not(:disabled) {
  background: #2e364c;
}

.btn-emerald {
  background: linear-gradient(135deg, #10b981, #059669);
  color: #ffffff;
}
.btn-emerald:hover {
  filter: brightness(1.15);
}

.btn-cyan {
  background: linear-gradient(135deg, #06b6d4, #0284c7);
  color: #ffffff;
}
.btn-cyan:hover {
  filter: brightness(1.15);
}

.btn-purple {
  background: linear-gradient(135deg, #a855f7, #7e22ce);
  color: #ffffff;
}
.btn-purple:hover {
  filter: brightness(1.15);
}

.progress-section {
  display: flex;
  flex-direction: column;
  gap: 8px;
  background: #0f121a;
  padding: 14px 18px;
  border-radius: 8px;
  border: 1px solid #1f2533;
}

.progress-header {
  display: flex;
  align-items: center;
  gap: 12px;
}

.status-badge {
  font-size: 10px;
  font-weight: 700;
  padding: 2px 6px;
  border-radius: 4px;
  background: #334155;
  color: #94a3b8;
}
.status-active {
  background: #8b5cf6;
  color: #ffffff;
  animation: pulse 1.2s infinite alternate;
}
.status-done {
  background: #10b981;
  color: #ffffff;
}
.status-error {
  background: #ef4444;
  color: #ffffff;
}

.status-text {
  font-size: 13px;
  color: #e2e8f0;
  font-weight: 500;
  flex: 1;
}

.progress-number {
  font-size: 15px;
  font-weight: 700;
  color: #8b5cf6;
}

.progress-track {
  height: 8px;
  background: #1e2433;
  border-radius: 4px;
  overflow: hidden;
}

.progress-bar {
  height: 100%;
  background: linear-gradient(90deg, #6366f1, #a855f7);
  border-radius: 4px;
  transition: width 0.3s ease;
}
.bar-anim {
  background: linear-gradient(90deg, #6366f1, #ec4899, #a855f7);
  background-size: 200% 100%;
  animation: shimmer 1.5s infinite linear;
}
.bar-done {
  background: linear-gradient(90deg, #10b981, #34d399);
}
.bar-error {
  background: #ef4444;
}

@keyframes shimmer {
  0% { background-position: 200% 0; }
  100% { background-position: -200% 0; }
}

@keyframes pulse {
  0% { opacity: 0.7; }
  100% { opacity: 1; }
}

.completion-alert {
  display: flex;
  gap: 14px;
  background: rgba(16, 185, 129, 0.12);
  border: 1px solid rgba(16, 185, 129, 0.3);
  padding: 16px;
  border-radius: 8px;
}
.completion-alert .alert-icon {
  font-size: 24px;
}
.completion-alert h4 {
  margin: 0 0 4px 0;
  color: #34d399;
  font-size: 15px;
}
.completion-alert p {
  margin: 0 0 12px 0;
  font-size: 12px;
  color: #cbd5e1;
}
.alert-buttons {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
}

.error-alert {
  display: flex;
  gap: 12px;
  background: rgba(239, 68, 68, 0.12);
  border: 1px solid rgba(239, 68, 68, 0.3);
  padding: 14px;
  border-radius: 8px;
}
.error-alert h4 {
  margin: 0 0 4px 0;
  color: #f87171;
  font-size: 14px;
}
.error-alert p {
  margin: 0;
  font-size: 12px;
  color: #fca5a5;
}

.terminal-card {
  padding: 0;
  overflow: hidden;
  background: #0b0d13;
  border: 1px solid #1a1f2e;
}

.terminal-header {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 10px 14px;
  background: #141824;
  border-bottom: 1px solid #1e2536;
}

.terminal-dot {
  width: 10px;
  height: 10px;
  border-radius: 50%;
}
.terminal-dot.red { background: #ef4444; }
.terminal-dot.yellow { background: #f59e0b; }
.terminal-dot.green { background: #10b981; }

.terminal-title {
  font-size: 11px;
  color: #94a3b8;
  margin-left: 8px;
  font-family: monospace;
}

.terminal-body {
  padding: 12px 16px;
  height: 200px;
  overflow-y: auto;
  font-size: 11px;
  line-height: 1.6;
}

.log-placeholder {
  color: #475569;
  font-style: italic;
}

.log-line {
  color: #94a3b8;
  word-break: break-all;
}

.log-prefix {
  color: #8b5cf6;
  margin-right: 4px;
}
</style>
