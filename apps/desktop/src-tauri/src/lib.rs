use karaoke_audio_engine::{
    AudioPipeline, KaraokeSession, MicMonitorSession, MonitorConfig, RecordingArtifact,
    RecordingConfig,
};
use karaoke_audio_win::{get_qpc_frequency, DeviceManager, WasapiRenderStream};
use karaoke_media::{
    stretch_preserving_pitch, transpose_preserving_duration, AudioDecoder, CanonicalAudio,
    MAX_KEY_SEMITONES, MAX_SPEED_RATIO, MIN_SPEED_RATIO,
};
use karaoke_protocol::{DisplayPacket, MicDiagnostic, PROTOCOL_VERSION};
use karaoke_scoring::ScoreResult;
use karaoke_song_format::chart::MediaRole;
use karaoke_song_format::{Chart, FileRole, PackageReader, PackageWriter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Manager, State};

struct EditorAudioPlayer {
    stream: WasapiRenderStream,
    current_frame: Arc<AtomicU64>,
    sample_rate: u32,
    is_playing: Arc<AtomicBool>,
}

impl EditorAudioPlayer {
    pub fn start(
        backing: Arc<CanonicalAudio>,
        start_ms: f64,
        render_device_id: Option<&str>,
    ) -> Result<Self, String> {
        let sample_rate = backing.sample_rate;
        let start_frame = ((start_ms / 1000.0) * sample_rate as f64).max(0.0) as usize;
        let current_frame = Arc::new(AtomicU64::new(start_frame as u64));
        let frame_clone = current_frame.clone();
        let is_playing = Arc::new(AtomicBool::new(true));
        let playing_clone = is_playing.clone();

        let num_channels = backing.channels;
        let total_frames = backing.frames;
        let backing_data = backing.clone();
        let mut source_position = start_frame as f64;

        let stream = WasapiRenderStream::start(
            render_device_id,
            move |out_buf,
                  frames_needed,
                  _clock_pos,
                  _qpc,
                  output_rate,
                  output_channels,
                  _queued_frames| {
                if !playing_clone.load(Ordering::Relaxed) {
                    out_buf.fill(0.0);
                    return;
                }
                let source_step = sample_rate as f64 / output_rate.max(1) as f64;
                for i in 0..frames_needed {
                    let frame_idx = source_position.floor() as usize;
                    let fraction = (source_position - frame_idx as f64) as f32;
                    if frame_idx < total_frames {
                        for ch in 0..output_channels {
                            let source_channel = ch.min(num_channels.saturating_sub(1));
                            let current = backing_data.data[source_channel][frame_idx];
                            let next = backing_data.data[source_channel]
                                .get(frame_idx + 1)
                                .copied()
                                .unwrap_or(current);
                            out_buf[i * output_channels + ch] =
                                (current + (next - current) * fraction) * 0.8;
                        }
                    } else {
                        for ch in 0..output_channels {
                            out_buf[i * output_channels + ch] = 0.0;
                        }
                    }
                    source_position += source_step;
                }
                frame_clone.store(source_position.floor() as u64, Ordering::Relaxed);
            },
        )
        .map_err(|e| format!("Failed to start render stream: {}", e))?;

        Ok(Self {
            stream,
            current_frame,
            sample_rate,
            is_playing,
        })
    }

    pub fn get_position_ms(&self) -> f64 {
        let frame = self.current_frame.load(Ordering::Relaxed);
        (frame as f64 / self.sample_rate as f64) * 1000.0
    }

    pub fn stop(&mut self) {
        self.is_playing.store(false, Ordering::Relaxed);
        self.stream.stop();
    }
}

#[derive(Default, Clone, serde::Serialize)]
pub struct AnalysisProgress {
    pub is_running: bool,
    pub progress_percent: u32,
    pub status_text: String,
    pub logs: Vec<String>,
    pub completed: bool,
    pub auto_adopted: bool,
    pub error: Option<String>,
}

struct AppState {
    pipeline: Mutex<Option<AudioPipeline>>,
    session: Mutex<Option<KaraokeSession>>,
    monitor_session: Mutex<Option<MicMonitorSession>>,
    last_recording: Mutex<Option<RecordingArtifact>>,
    editor_player: Mutex<Option<EditorAudioPlayer>>,
    cached_audio: Mutex<Option<(String, Arc<CanonicalAudio>)>>,
    karaoke_backing_cache: Mutex<Option<PreparedBackingCache>>,
    analysis_state: Arc<Mutex<AnalysisProgress>>,
}

struct PreparedBackingCache {
    song_id: String,
    key_semitones: i32,
    speed_percent: u16,
    audio: Arc<CanonicalAudio>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SongSummary {
    song_id: String,
    title: String,
    artist: String,
    duration_us: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SongDescriptor {
    song_id: String,
    active_chart: String,
    draft_chart: String,
    backing_audio: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditorRecovery {
    song_id: String,
    base_revision: u32,
    saved_at_unix_ms: u64,
    chart: Chart,
}

fn read_song_catalog() -> Result<Vec<SongDescriptor>, String> {
    let path = workspace_path("library/songs.json");
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read song catalog {}: {e}", path.display()))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse song catalog {}: {e}", path.display()))
}

fn library_package_path(song_id: &str) -> PathBuf {
    workspace_path(&format!("library/packages/{song_id}.kpk"))
}

fn trash_song_files(
    library_root: &Path,
    descriptor: &SongDescriptor,
    unix_ms: u128,
) -> Result<PathBuf, String> {
    let song_id = &descriptor.song_id;
    if !valid_song_id(song_id) {
        return Err("曲IDが不正です".to_string());
    }
    let package = library_root.join("packages").join(format!("{song_id}.kpk"));
    if !package.is_file() {
        return Err(format!(
            "曲パッケージが見つかりません: {}",
            package.display()
        ));
    }
    let trash_dir = library_root
        .join("trash")
        .join(format!("{song_id}-{unix_ms}"));
    fs::create_dir_all(&trash_dir)
        .map_err(|error| format!("削除済み曲の退避先を作成できません: {error}"))?;
    fs::rename(&package, trash_dir.join("package.kpk"))
        .map_err(|error| format!("曲パッケージを退避できません: {error}"))?;

    let package_backup = package.with_extension("bak");
    let backup_moved = !package_backup.is_file()
        || fs::rename(&package_backup, trash_dir.join("package.bak")).is_ok();
    let owned_project = library_root.join("imported").join(song_id);
    let project_moved =
        !owned_project.is_dir() || fs::rename(&owned_project, trash_dir.join("project")).is_ok();

    let tombstone = serde_json::json!({
        "schemaVersion": "1.0.0",
        "deletedAtUnixMs": unix_ms,
        "song": descriptor,
        "backupMoved": backup_moved,
        "projectMoved": project_moved,
        "recovery": "Move package.kpk and project back to their original library paths, then restore the songs.json entry."
    });
    let tombstone_bytes = serde_json::to_vec_pretty(&tombstone)
        .map_err(|error| format!("削除記録を作成できません: {error}"))?;
    durable_replace(&trash_dir.join("deleted.json"), &tombstone_bytes)?;
    Ok(trash_dir)
}

fn valid_song_id(song_id: &str) -> bool {
    !song_id.is_empty()
        && song_id.len() <= 96
        && song_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Only packages that still exist are part of the playable library. Missing
/// package entries are pruned so deleting a .kpk really removes the song and
/// the remaining extracted files are treated only as an orphaned work cache.
fn load_song_catalog() -> Result<Vec<SongDescriptor>, String> {
    let catalog = read_song_catalog()?;
    let retained: Vec<_> = catalog
        .iter()
        .filter(|song| {
            valid_song_id(&song.song_id) && library_package_path(&song.song_id).is_file()
        })
        .cloned()
        .collect();
    if retained.len() != catalog.len() {
        let json = serde_json::to_vec_pretty(&retained)
            .map_err(|error| format!("Failed to serialize pruned song catalog: {error}"))?;
        durable_replace(&workspace_path("library/songs.json"), &json)?;
    }
    Ok(retained)
}

fn catalog_song_descriptor(song_id: &str) -> Result<SongDescriptor, String> {
    read_song_catalog()?
        .into_iter()
        .find(|song| song.song_id == song_id)
        .ok_or_else(|| format!("Unknown songId: {song_id}"))
}

fn song_descriptor(song_id: Option<&str>) -> Result<SongDescriptor, String> {
    let wanted = song_id.unwrap_or("shining_star");
    load_song_catalog()?
        .into_iter()
        .find(|song| song.song_id == wanted)
        .ok_or_else(|| format!("Unknown songId: {wanted}"))
}

fn song_asset_path(relative: &str) -> Result<PathBuf, String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "Song catalog path must stay inside workspace: {relative}"
        ));
    }
    Ok(workspace_path(relative))
}

fn load_song_chart(song: &SongDescriptor) -> Result<Chart, String> {
    let active_path = song_asset_path(&song.active_chart)?;
    let draft_path = song_asset_path(&song.draft_chart)?;
    let chart_path = if active_path.exists() {
        active_path
    } else {
        draft_path
    };
    let content = fs::read_to_string(&chart_path)
        .map_err(|e| format!("Failed to read chart {}: {e}", chart_path.display()))?;
    let chart: Chart = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse chart {}: {e}", chart_path.display()))?;
    chart
        .validate_semantics()
        .map_err(|e| format!("Invalid chart {}: {e}", chart_path.display()))?;
    Ok(chart)
}

fn active_chart_path(song: &SongDescriptor) -> Result<PathBuf, String> {
    song_asset_path(&song.active_chart)
}

fn recovery_path(song: &SongDescriptor) -> Result<PathBuf, String> {
    Ok(active_chart_path(song)?.with_extension("recovery.json"))
}

fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("src-tauri must be inside the workspace")
        .join(relative)
}

/// Writes beside the destination, flushes the file, and keeps the previous
/// complete revision as `.bak` until the replacement succeeds.
fn durable_replace(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "保存先に親ディレクトリがありません".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("保存先作成エラー: {e}"))?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let backup = path.with_extension("bak");
    {
        let mut file = fs::File::create(&temp).map_err(|e| format!("一時保存エラー: {e}"))?;
        file.write_all(contents)
            .map_err(|e| format!("一時保存書込エラー: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("一時保存同期エラー: {e}"))?;
    }
    if path.exists() {
        let _ = fs::remove_file(&backup);
        fs::rename(path, &backup).map_err(|e| format!("旧版バックアップエラー: {e}"))?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(format!("保存置換エラー: {error}"));
    }
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn write_file_synced(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file =
        fs::File::create(path).map_err(|e| format!("Failed to create {}: {e}", path.display()))?;
    file.write_all(contents)
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("Failed to sync {}: {e}", path.display()))
}

#[tauri::command]
fn get_system_info() -> serde_json::Value {
    let freq = get_qpc_frequency();
    serde_json::json!({
        "protocolVersion": PROTOCOL_VERSION,
        "qpcFrequencyHz": freq,
        "qpcResolutionNs": 1_000_000_000.0 / freq as f64,
    })
}

#[tauri::command]
fn list_audio_devices() -> Result<Vec<serde_json::Value>, String> {
    let devices = DeviceManager::list_devices().map_err(|e| e.to_string())?;
    let res = devices
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "id": d.id,
                "name": d.name,
                "dataFlow": d.data_flow,
                "isDefault": d.is_default,
            })
        })
        .collect();
    Ok(res)
}

#[tauri::command]
fn start_mic_pipeline(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<bool, String> {
    let mut lock = state.pipeline.lock().unwrap();
    if lock.is_some() {
        return Ok(true);
    }

    let clean_device_id = device_id.filter(|s| !s.trim().is_empty());
    let pipeline =
        AudioPipeline::start(clean_device_id.as_deref(), vec![], 0).map_err(|e| e.to_string())?;
    *lock = Some(pipeline);
    Ok(true)
}

#[tauri::command]
fn poll_display_packet(state: State<'_, AppState>) -> Option<DisplayPacket> {
    // Check session first, then mic pipeline
    {
        let session_lock = state.session.lock().unwrap();
        if let Some(ref s) = *session_lock {
            if let Some(p) = s.poll_display() {
                return Some(p);
            }
        }
    }
    let lock = state.pipeline.lock().unwrap();
    if let Some(ref p) = *lock {
        p.get_latest_display()
    } else {
        None
    }
}

#[tauri::command]
fn poll_mic_diagnostic(state: State<'_, AppState>) -> Option<MicDiagnostic> {
    {
        let session_lock = state.session.lock().unwrap();
        if let Some(ref s) = *session_lock {
            if let Some(d) = s.poll_diagnostic() {
                return Some(d);
            }
        }
    }
    let lock = state.pipeline.lock().unwrap();
    (*lock).as_ref().map(|p| p.get_latest_diagnostic())
}

#[tauri::command]
fn stop_mic_pipeline(state: State<'_, AppState>) -> bool {
    let mut lock = state.pipeline.lock().unwrap();
    if let Some(mut p) = lock.take() {
        p.stop();
    }
    true
}

#[tauri::command]
fn list_songs() -> Result<Vec<SongSummary>, String> {
    load_song_catalog()?
        .into_iter()
        .map(|song| {
            let chart = load_song_chart(&song)?;
            Ok(SongSummary {
                song_id: song.song_id,
                title: chart.title,
                artist: chart.artist,
                duration_us: chart.duration_us,
            })
        })
        .collect()
}

/// Removes a song from the playable catalog without permanently destroying
/// user data. The package, package backup, and owned imported project directory
/// are moved into one timestamped library/trash entry.
#[tauri::command]
fn delete_song(state: State<'_, AppState>, song_id: String) -> Result<String, String> {
    if !valid_song_id(&song_id) {
        return Err("曲IDが不正です".to_string());
    }
    if state.session.lock().unwrap().is_some()
        || state.monitor_session.lock().unwrap().is_some()
        || state.analysis_state.lock().unwrap().is_running
    {
        return Err("歌唱・試聴・AI解析を停止してから曲を削除してください".to_string());
    }

    let mut catalog = load_song_catalog()?;
    let descriptor = catalog
        .iter()
        .find(|song| song.song_id == song_id)
        .cloned()
        .ok_or_else(|| format!("登録曲が見つかりません: {song_id}"))?;
    if let Some(mut player) = state.editor_player.lock().unwrap().take() {
        player.stop();
    }
    *state.cached_audio.lock().unwrap() = None;
    let mut backing_cache = state.karaoke_backing_cache.lock().unwrap();
    if backing_cache
        .as_ref()
        .is_some_and(|cached| cached.song_id == song_id)
    {
        *backing_cache = None;
    }

    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("System clock error: {error}"))?
        .as_millis();
    let trash_dir = trash_song_files(&workspace_path("library"), &descriptor, unix_ms)?;

    catalog.retain(|song| song.song_id != song_id);
    let catalog_bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| format!("曲一覧を更新できません: {error}"))?;
    durable_replace(&workspace_path("library/songs.json"), &catalog_bytes)?;
    Ok(trash_dir.display().to_string())
}

#[tauri::command]
fn import_audio_file(
    audio_path: String,
    title: Option<String>,
    artist: Option<String>,
) -> Result<SongSummary, String> {
    let source = PathBuf::from(&audio_path)
        .canonicalize()
        .map_err(|error| format!("音声ファイルを開けません: {error}"))?;
    if !source.is_file() {
        return Err("選択されたパスはファイルではありません".to_string());
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "音声ファイルの拡張子がありません".to_string())?;
    if !matches!(
        extension.as_str(),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac"
    ) {
        return Err(format!("未対応の音声形式です: .{extension}"));
    }
    let source_size = fs::metadata(&source)
        .map_err(|error| format!("音声ファイル情報を取得できません: {error}"))?
        .len();
    if source_size > 2 * 1024 * 1024 * 1024 {
        return Err("音声ファイルは2 GiB以下にしてください".to_string());
    }

    let decoded = AudioDecoder::decode_file(&source)
        .map_err(|error| format!("音声を解析できません: {error}"))?;
    let fallback_title = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("新しい曲")
        .trim();
    let title = title
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback_title.to_string());
    let artist = artist
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "未設定".to_string());
    if title.chars().count() > 200 || artist.chars().count() > 200 {
        return Err("曲名と歌手名は200文字以内にしてください".to_string());
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("システム時計エラー: {error}"))?
        .as_millis();
    let song_id = format!("song_{stamp}");
    let imported_root = workspace_path("library/imported");
    fs::create_dir_all(&imported_root)
        .map_err(|error| format!("曲保存フォルダーを作成できません: {error}"))?;
    let final_dir = imported_root.join(&song_id);
    let staging_dir = imported_root.join(format!(".staging-{song_id}-{}", std::process::id()));
    fs::create_dir(&staging_dir)
        .map_err(|error| format!("一時曲フォルダーを作成できません: {error}"))?;

    let backing_name = format!("backing.{extension}");
    let staged_backing = staging_dir.join(&backing_name);
    fs::copy(&source, &staged_backing)
        .map_err(|error| format!("音声ファイルを保存できません: {error}"))?;
    fs::OpenOptions::new()
        .write(true)
        .open(&staged_backing)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("音声ファイルを同期できません: {error}"))?;

    let chart_value = serde_json::json!({
        "schemaVersion": "2.0.0",
        "requiredFeatures": ["single-melody", "step-tempo"],
        "songId": song_id,
        "chartId": format!("{song_id}_chart"),
        "chartRevision": 1,
        "title": title,
        "artist": artist,
        "durationUs": decoded.duration_us,
        "credits": [],
        "media": [{
            "id": "backing",
            "path": format!("media/{backing_name}"),
            "role": "backing",
            "sha256": decoded.source_sha256,
            "sampleRate": decoded.sample_rate,
            "channels": decoded.channels,
            "frames": decoded.frames,
            "mediaOffsetUs": 0
        }],
        "tracks": [{ "id": "lead", "role": "lead" }],
        "tempoMap": {
            "anchorTimeUs": 0,
            "anchorQuarterBeat": 0.0,
            "events": [{ "timeUs": 0, "bpm": 120.0 }],
            "meters": [{ "quarterBeat": 0.0, "numerator": 4, "denominator": 4 }]
        },
        "notes": [],
        "lyricTokens": [],
        "phrases": [],
        "extensions": {
            "status": "awaiting_ai_analysis",
            "importedSourceName": source.file_name().and_then(|value| value.to_str()).unwrap_or("")
        }
    });
    let chart: Chart = serde_json::from_value(chart_value)
        .map_err(|error| format!("初期譜面を作成できません: {error}"))?;
    chart
        .validate_semantics()
        .map_err(|error| format!("初期譜面が不正です: {error}"))?;
    write_file_synced(
        &staging_dir.join("chart.json"),
        &serde_json::to_vec_pretty(&chart)
            .map_err(|error| format!("初期譜面を変換できません: {error}"))?,
    )?;
    fs::rename(&staging_dir, &final_dir)
        .map_err(|error| format!("曲をライブラリへ確定できません: {error}"))?;

    let relative_base = format!("library/imported/{song_id}");
    let mut catalog = read_song_catalog()?;
    catalog.push(SongDescriptor {
        song_id: song_id.clone(),
        active_chart: format!("{relative_base}/chart.json"),
        draft_chart: format!("{relative_base}/draft.json"),
        backing_audio: format!("{relative_base}/{backing_name}"),
    });
    if let Err(error) = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| error.to_string())
        .and_then(|json| durable_replace(&workspace_path("library/songs.json"), &json))
    {
        let _ = fs::rename(&final_dir, &staging_dir);
        return Err(format!("曲一覧へ登録できません: {error}"));
    }

    save_library_package(&song_id)?;

    Ok(SongSummary {
        song_id,
        title: chart.title,
        artist: chart.artist,
        duration_us: chart.duration_us,
    })
}

#[tauri::command]
fn get_current_chart(song_id: Option<String>) -> Result<Chart, String> {
    let song = song_descriptor(song_id.as_deref())?;
    load_song_chart(&song)
}

fn analysis_candidate_path(song: &SongDescriptor) -> Result<PathBuf, String> {
    song_asset_path(&song.draft_chart)
}

fn analysis_output_dir(song: &SongDescriptor) -> PathBuf {
    if song.song_id == "shining_star" {
        workspace_path("tmpmusic/analysis_out")
    } else {
        workspace_path(&format!("library/imported/{}/analysis", song.song_id))
    }
}

#[tauri::command]
fn get_analysis_candidate(song_id: Option<String>) -> Result<Chart, String> {
    let song = song_descriptor(song_id.as_deref())?;
    let path = analysis_candidate_path(&song)?;
    let content = fs::read_to_string(&path).map_err(|e| format!("候補譜面読込エラー: {e}"))?;
    let chart: Chart =
        serde_json::from_str(&content).map_err(|e| format!("候補譜面構文エラー: {e}"))?;
    chart
        .validate_semantics()
        .map_err(|e| format!("候補譜面検証エラー: {e}"))?;
    Ok(chart)
}

#[tauri::command]
fn adopt_analysis_candidate(song_id: Option<String>) -> Result<Chart, String> {
    let song = song_descriptor(song_id.as_deref())?;
    let mut candidate = get_analysis_candidate(Some(song.song_id.clone()))?;
    let active_path = active_chart_path(&song)?;
    if active_path.exists() {
        let active_content =
            fs::read_to_string(&active_path).map_err(|e| format!("現行譜面読込エラー: {e}"))?;
        let active: Chart = serde_json::from_str(&active_content)
            .map_err(|e| format!("現行譜面構文エラー: {e}"))?;
        candidate.chart_revision = active.chart_revision.saturating_add(1);
    }
    candidate
        .extensions
        .insert("status".into(), serde_json::json!("adopted_ai_draft"));
    candidate
        .validate_semantics()
        .map_err(|e| format!("採用前検証エラー: {e}"))?;
    let json = serde_json::to_vec_pretty(&candidate).map_err(|e| format!("譜面変換エラー: {e}"))?;
    durable_replace(&active_path, &json)?;
    Ok(candidate)
}

#[tauri::command]
fn get_editor_media_info(song_id: Option<String>) -> Result<serde_json::Value, String> {
    let selected_song = song_id.unwrap_or_else(|| "shining_star".to_string());
    let (waveform_path, f0_path) = if selected_song == "shining_star" {
        (
            workspace_path("tmpmusic/analysis_out/waveform.json"),
            workspace_path("tmpmusic/analysis_out/f0_contour.json"),
        )
    } else {
        let analysis_dir = workspace_path(&format!("library/imported/{selected_song}/analysis"));
        (
            analysis_dir.join("waveform.json"),
            analysis_dir.join("f0_contour.json"),
        )
    };

    let waveform: serde_json::Value = if waveform_path.exists() {
        let content = fs::read_to_string(waveform_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({ "peaks": [] }))
    } else {
        serde_json::json!({ "peaks": [] })
    };

    let f0: serde_json::Value = if f0_path.exists() {
        let content = fs::read_to_string(f0_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({ "frames": [] }))
    } else {
        serde_json::json!({ "frames": [] })
    };

    Ok(serde_json::json!({
        "waveform": waveform,
        "f0": f0
    }))
}

#[tauri::command]
fn save_chart(chart_data: serde_json::Value) -> Result<Chart, String> {
    let mut chart: Chart = serde_json::from_value(chart_data)
        .map_err(|e| format!("Invalid chart structure: {}", e))?;

    // Semantic validation
    chart
        .validate_semantics()
        .map_err(|e| format!("Validation error: {}", e))?;

    let song = song_descriptor(Some(&chart.song_id))?;
    let target_path = active_chart_path(&song)?;
    if target_path.exists() {
        let current: Chart = serde_json::from_str(
            &fs::read_to_string(&target_path)
                .map_err(|e| format!("Current chart read error: {e}"))?,
        )
        .map_err(|e| format!("Current chart parse error: {e}"))?;
        if current.chart_revision != chart.chart_revision {
            return Err(format!(
                "RevisionConflict: 現在 revision={} / 編集画面 revision={}",
                current.chart_revision, chart.chart_revision
            ));
        }
        chart.chart_revision = chart.chart_revision.saturating_add(1);
    }
    let json =
        serde_json::to_vec_pretty(&chart).map_err(|e| format!("Serialization error: {e}"))?;
    durable_replace(&target_path, &json)?;
    let recovery = recovery_path(&song)?;
    if recovery.exists() {
        fs::remove_file(&recovery)
            .map_err(|e| format!("Saved chart, but failed to clear recovery data: {e}"))?;
    }
    Ok(chart)
}

#[tauri::command]
fn save_editor_recovery(chart_data: serde_json::Value) -> Result<EditorRecovery, String> {
    let chart: Chart = serde_json::from_value(chart_data)
        .map_err(|e| format!("Invalid recovery chart structure: {e}"))?;
    chart
        .validate_semantics()
        .map_err(|e| format!("Recovery validation error: {e}"))?;

    let song = song_descriptor(Some(&chart.song_id))?;
    let current = load_song_chart(&song)?;
    if current.chart_revision != chart.chart_revision {
        return Err(format!(
            "RevisionConflict: current revision={} / recovery revision={}",
            current.chart_revision, chart.chart_revision
        ));
    }

    let saved_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System clock error: {e}"))?
        .as_millis() as u64;
    let recovery = EditorRecovery {
        song_id: chart.song_id.clone(),
        base_revision: chart.chart_revision,
        saved_at_unix_ms,
        chart,
    };
    let json = serde_json::to_vec_pretty(&recovery)
        .map_err(|e| format!("Recovery serialization error: {e}"))?;
    durable_replace(&recovery_path(&song)?, &json)?;
    Ok(recovery)
}

#[tauri::command]
fn get_editor_recovery(song_id: String) -> Result<Option<EditorRecovery>, String> {
    let song = song_descriptor(Some(&song_id))?;
    let path = recovery_path(&song)?;
    if !path.exists() {
        return Ok(None);
    }
    let recovery: EditorRecovery = serde_json::from_str(
        &fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read editor recovery {}: {e}", path.display()))?,
    )
    .map_err(|e| format!("Failed to parse editor recovery {}: {e}", path.display()))?;
    recovery
        .chart
        .validate_semantics()
        .map_err(|e| format!("Invalid editor recovery {}: {e}", path.display()))?;
    let current = load_song_chart(&song)?;
    if recovery.song_id != song_id || recovery.base_revision != current.chart_revision {
        return Ok(None);
    }
    Ok(Some(recovery))
}

#[tauri::command]
fn discard_editor_recovery(song_id: String) -> Result<bool, String> {
    let song = song_descriptor(Some(&song_id))?;
    let path = recovery_path(&song)?;
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| format!("Failed to discard editor recovery {}: {e}", path.display()))?;
    }
    Ok(true)
}

#[tauri::command]
fn export_song_package(song_id: String) -> Result<String, String> {
    let song = catalog_song_descriptor(&song_id)?;
    let mut chart = load_song_chart(&song)?;
    let backing_source = song_asset_path(&song.backing_audio)?;
    let backing_bytes = fs::read(&backing_source)
        .map_err(|e| format!("Failed to read backing {}: {e}", backing_source.display()))?;
    let extension = backing_source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("bin")
        .to_ascii_lowercase();
    let package_backing_path = format!("media/backing.{extension}");
    let backing_media = chart
        .media
        .iter_mut()
        .find(|media| media.role == MediaRole::Backing)
        .ok_or_else(|| "Chart has no backing media".to_string())?;
    backing_media.path = package_backing_path.clone();
    chart
        .validate_semantics()
        .map_err(|e| format!("Chart validation failed before export: {e}"))?;
    let chart_bytes = serde_json::to_vec_pretty(&chart)
        .map_err(|e| format!("Chart serialization failed: {e}"))?;

    let export_dir = workspace_path("exports");
    fs::create_dir_all(&export_dir)
        .map_err(|e| format!("Failed to create export directory: {e}"))?;
    let target = export_dir.join(format!("{}.kpk", song.song_id));
    let temp = export_dir.join(format!(".{}-{}.tmp", song.song_id, std::process::id()));
    let file = fs::File::create(&temp)
        .map_err(|e| format!("Failed to create package staging file: {e}"))?;
    let file = PackageWriter::write_package(
        file,
        &song.song_id,
        "KARAOKE STUDIO PRO",
        &chart_bytes,
        &package_backing_path,
        &backing_bytes,
    )
    .map_err(|e| format!("Package export failed: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("Package sync failed: {e}"))?;
    PackageReader::inspect_package(
        fs::File::open(&temp).map_err(|e| format!("Package verification open failed: {e}"))?,
    )
    .map_err(|e| format!("Package verification failed: {e}"))?;
    let package_bytes = fs::read(&temp)
        .map_err(|e| format!("Failed to read verified package staging file: {e}"))?;
    durable_replace(&target, &package_bytes)?;
    let _ = fs::remove_file(&temp);
    Ok(target.display().to_string())
}

fn save_library_package(song_id: &str) -> Result<PathBuf, String> {
    let exported = PathBuf::from(export_song_package(song_id.to_string())?);
    let package_dir = workspace_path("library/packages");
    fs::create_dir_all(&package_dir)
        .map_err(|error| format!("ライブラリパッケージ保存先を作成できません: {error}"))?;
    let target = library_package_path(song_id);
    let bytes = fs::read(&exported)
        .map_err(|error| format!("生成済みパッケージを読み直せません: {error}"))?;
    durable_replace(&target, &bytes)?;
    PackageReader::inspect_package(
        fs::File::open(&target)
            .map_err(|error| format!("保存済みパッケージを開けません: {error}"))?,
    )
    .map_err(|error| format!("保存済みパッケージの再検証に失敗しました: {error}"))?;
    Ok(target)
}

#[tauri::command]
fn import_song_package(package_path: String) -> Result<SongSummary, String> {
    let source = PathBuf::from(&package_path)
        .canonicalize()
        .map_err(|e| format!("Package path is not readable: {e}"))?;
    if !source
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("kpk"))
    {
        return Err("Only .kpk files can be imported".to_string());
    }
    let package_size = fs::metadata(&source)
        .map_err(|e| format!("Failed to inspect package: {e}"))?
        .len();
    if package_size > 2 * 1024 * 1024 * 1024 {
        return Err("Package exceeds the 2 GiB limit".to_string());
    }

    let (manifest, chart_json) = PackageReader::inspect_package(
        fs::File::open(&source).map_err(|e| format!("Failed to open package: {e}"))?,
    )
    .map_err(|e| format!("Package validation failed: {e}"))?;
    let mut chart: Chart = serde_json::from_str(&chart_json)
        .map_err(|e| format!("Package chart parse failed: {e}"))?;
    if chart.song_id != manifest.package_id {
        return Err("packageId and chart songId must match".to_string());
    }
    if chart.duration_us > 600_000_000
        || chart.notes.len() > 100_000
        || chart.lyric_tokens.len() > 50_000
    {
        return Err("Package exceeds the v1 chart duration or item limits".to_string());
    }
    chart
        .validate_semantics()
        .map_err(|e| format!("Package chart validation failed: {e}"))?;
    let backing_entry = manifest
        .files
        .iter()
        .find(|entry| entry.role == FileRole::Backing)
        .ok_or_else(|| "Package has no backing entry".to_string())?;
    let chart_backing = chart
        .media
        .iter_mut()
        .find(|media| media.role == MediaRole::Backing)
        .ok_or_else(|| "Package chart has no backing media".to_string())?;
    if chart_backing.path != backing_entry.path {
        return Err("Chart backing path does not match the package manifest".to_string());
    }
    let backing_bytes = PackageReader::extract_file(
        fs::File::open(&source).map_err(|e| format!("Failed to reopen package: {e}"))?,
        &backing_entry.path,
    )
    .map_err(|e| format!("Failed to extract backing: {e}"))?;

    let mut catalog = read_song_catalog()?;
    if catalog.iter().any(|entry| entry.song_id == chart.song_id) {
        return Err(format!("Song '{}' is already registered", chart.song_id));
    }
    let imported_root = workspace_path("library/imported");
    fs::create_dir_all(&imported_root)
        .map_err(|e| format!("Failed to create imported-song directory: {e}"))?;
    let final_dir = imported_root.join(&chart.song_id);
    if final_dir.exists() {
        return Err(format!(
            "Import destination already exists: {}",
            final_dir.display()
        ));
    }

    // Keep the verified package itself as the durable library artifact. The
    // extracted files below are the editable/runtime working copy.
    let package_dir = workspace_path("library/packages");
    fs::create_dir_all(&package_dir)
        .map_err(|error| format!("Failed to create package library: {error}"))?;
    let library_package = package_dir.join(format!("{}.kpk", chart.song_id));
    let already_in_library = library_package
        .canonicalize()
        .ok()
        .is_some_and(|path| path == source);
    if !already_in_library {
        let package_bytes = fs::read(&source)
            .map_err(|error| format!("Failed to retain package in library: {error}"))?;
        durable_replace(&library_package, &package_bytes)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System clock error: {e}"))?
        .as_millis();
    let staging_dir = imported_root.join(format!(
        ".staging-{}-{}-{stamp}",
        chart.song_id,
        std::process::id()
    ));
    fs::create_dir(&staging_dir)
        .map_err(|e| format!("Failed to create import staging directory: {e}"))?;
    let backing_name = Path::new(&backing_entry.path)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "Invalid backing filename".to_string())?;
    let staged_backing = staging_dir.join(backing_name);
    write_file_synced(&staged_backing, &backing_bytes)?;
    let decoded = AudioDecoder::decode_file(&staged_backing)
        .map_err(|e| format!("Imported backing decode failed: {e}"))?;
    if decoded.duration_us.abs_diff(chart.duration_us) > 150_000 {
        return Err(format!(
            "Backing duration does not match chart: audio={}us chart={}us",
            decoded.duration_us, chart.duration_us
        ));
    }
    chart_backing.sample_rate = decoded.sample_rate;
    chart_backing.channels = decoded.channels as u8;
    chart_backing.frames = decoded.frames as u64;
    chart_backing.sha256 = decoded.source_sha256;
    chart
        .validate_semantics()
        .map_err(|e| format!("Prepared imported chart is invalid: {e}"))?;
    let staged_chart = staging_dir.join("chart.json");
    write_file_synced(
        &staged_chart,
        &serde_json::to_vec_pretty(&chart)
            .map_err(|e| format!("Imported chart serialization failed: {e}"))?,
    )?;
    fs::rename(&staging_dir, &final_dir)
        .map_err(|e| format!("Failed to commit imported song: {e}"))?;

    let relative_base = format!("library/imported/{}", chart.song_id);
    catalog.push(SongDescriptor {
        song_id: chart.song_id.clone(),
        active_chart: format!("{relative_base}/chart.json"),
        draft_chart: format!("{relative_base}/chart.json"),
        backing_audio: format!("{relative_base}/{backing_name}"),
    });
    let catalog_json = serde_json::to_vec_pretty(&catalog)
        .map_err(|e| format!("Song catalog serialization failed: {e}"))?;
    if let Err(error) = durable_replace(&workspace_path("library/songs.json"), &catalog_json) {
        let _ = fs::rename(&final_dir, &staging_dir);
        return Err(format!("Song import registration failed: {error}"));
    }

    Ok(SongSummary {
        song_id: chart.song_id,
        title: chart.title,
        artist: chart.artist,
        duration_us: chart.duration_us,
    })
}

#[tauri::command]
fn get_synthetic_chart() -> Result<Chart, String> {
    let chart_path = workspace_path("tests/fixtures/synthetic_chart.json");
    let chart_content = fs::read_to_string(chart_path)
        .map_err(|e| format!("Failed to read synthetic chart: {}", e))?;
    let chart: Chart = serde_json::from_str(&chart_content)
        .map_err(|e| format!("Failed to parse synthetic chart: {}", e))?;
    Ok(chart)
}

fn generate_synthetic_backing() -> CanonicalAudio {
    let sample_rate = 48000;
    let total_frames = 48000 * 2; // 2.0s
    let mut data = vec![0.0f32; total_frames];
    // n1: 0.50s - 0.55s (500ms - 550ms)
    let n1_start = (0.50 * sample_rate as f64) as usize;
    let n1_end = (0.55 * sample_rate as f64) as usize;
    for (i, sample) in data.iter_mut().enumerate().take(n1_end).skip(n1_start) {
        let t = i as f32 / sample_rate as f32;
        *sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
    }
    // n2: 1.00s - 1.50s (1000ms - 1500ms)
    let n2_start = (1.00 * sample_rate as f64) as usize;
    let n2_end = (1.50 * sample_rate as f64) as usize;
    for (i, sample) in data.iter_mut().enumerate().take(n2_end).skip(n2_start) {
        let t = i as f32 / sample_rate as f32;
        *sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
    }
    CanonicalAudio {
        sample_rate,
        channels: 1,
        frames: total_frames,
        data: vec![data],
        duration_us: 2_000_000,
        pcm_sha256: "synthetic_backing_pcm".to_string(),
        source_sha256: "synthetic_backing_source".to_string(),
    }
}

#[tauri::command]
fn start_synthetic_session(
    state: State<'_, AppState>,
    mic_device_id: Option<String>,
    render_device_id: Option<String>,
) -> Result<bool, String> {
    let mut lock = state.session.lock().unwrap();
    if lock.is_some() {
        return Ok(true);
    }
    let chart = get_synthetic_chart()?;
    let backing = generate_synthetic_backing();
    let clean_mic_id = mic_device_id.filter(|s| !s.trim().is_empty());
    let clean_render_id = render_device_id.filter(|s| !s.trim().is_empty());

    let session = KaraokeSession::start(
        chart,
        backing,
        clean_mic_id.as_deref(),
        clean_render_id.as_deref(),
        0,
        0,
        0,
        0,
        1.0,
        true,
        MonitorConfig::default(),
        None,
    )
    .map_err(|e| format!("Failed to start synthetic session: {}", e))?;

    *lock = Some(session);
    Ok(true)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_karaoke_session(
    state: State<'_, AppState>,
    mic_device_id: Option<String>,
    render_device_id: Option<String>,
    start_offset_secs: Option<f64>,
    song_id: Option<String>,
    key_semitones: Option<i32>,
    vocal_octave_offset: Option<i32>,
    speed_ratio: Option<f32>,
    octave_tolerance: Option<bool>,
    monitor_config: Option<MonitorConfig>,
    recording_enabled: Option<bool>,
    mix_backing_gain_db: Option<f32>,
    mix_vocal_gain_db: Option<f32>,
) -> Result<bool, String> {
    if state.monitor_session.lock().unwrap().is_some() {
        return Err("マイク試聴を停止してから歌唱を開始してください".to_string());
    }
    let mut lock = state.session.lock().unwrap();
    if lock.is_some() {
        return Ok(true);
    }

    let song = song_descriptor(song_id.as_deref())?;
    let chart = load_song_chart(&song)?;
    let key = key_semitones.unwrap_or(0);
    if !(-MAX_KEY_SEMITONES..=MAX_KEY_SEMITONES).contains(&key) {
        return Err(format!(
            "keySemitones must be between -{MAX_KEY_SEMITONES} and +{MAX_KEY_SEMITONES}"
        ));
    }
    let vocal_octave = vocal_octave_offset.unwrap_or(0);
    if !(-1..=1).contains(&vocal_octave) {
        return Err("vocalOctaveOffset must be -1, 0, or +1".to_string());
    }
    let speed = speed_ratio.unwrap_or(1.0);
    if !(MIN_SPEED_RATIO..=MAX_SPEED_RATIO).contains(&speed) {
        return Err(format!(
            "speedRatio must be between {MIN_SPEED_RATIO:.2} and {MAX_SPEED_RATIO:.2}"
        ));
    }
    let speed_percent = (speed * 100.0).round() as u16;

    let cache_key = song.song_id.clone();
    let backing = {
        let cached = state.karaoke_backing_cache.lock().unwrap();
        cached
            .as_ref()
            .filter(|cached| {
                cached.song_id == cache_key
                    && cached.key_semitones == key
                    && cached.speed_percent == speed_percent
            })
            .map(|cached| cached.audio.as_ref().clone())
    };
    let backing = match backing {
        Some(audio) => audio,
        None => {
            let decoded = AudioDecoder::decode_file(song_asset_path(&song.backing_audio)?)
                .map_err(|e| format!("Failed to decode backing: {e}"))?;
            let transposed = transpose_preserving_duration(&decoded, key)
                .map_err(|e| format!("Failed to prepare key {key:+}: {e}"))?;
            let prepared = stretch_preserving_pitch(&transposed, speed)
                .map_err(|e| format!("Failed to prepare speed {speed:.2}x: {e}"))?;
            *state.karaoke_backing_cache.lock().unwrap() = Some(PreparedBackingCache {
                song_id: cache_key,
                key_semitones: key,
                speed_percent,
                audio: Arc::new(prepared.clone()),
            });
            prepared
        }
    };

    let clean_mic_id = mic_device_id.filter(|s| !s.trim().is_empty());
    let clean_render_id = render_device_id.filter(|s| !s.trim().is_empty());
    let offset_us = (start_offset_secs.unwrap_or(0.0).max(0.0) * 1_000_000.0) as u64;
    let recording_config = if recording_enabled.unwrap_or(false) {
        let unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("System clock error: {error}"))?
            .as_millis();
        let session_id = format!("{}-{unix_ms}", song.song_id);
        Some(RecordingConfig {
            output_dir: workspace_path(&format!("recordings/{}/{session_id}", song.song_id)),
            session_id,
            song_id: song.song_id.clone(),
            start_offset_us: offset_us,
            backing_gain_db: mix_backing_gain_db.unwrap_or(-6.0).clamp(-24.0, 6.0),
            vocal_gain_db: mix_vocal_gain_db.unwrap_or(0.0).clamp(-24.0, 6.0),
        })
    } else {
        None
    };
    *state.last_recording.lock().unwrap() = None;

    let session = KaraokeSession::start(
        chart,
        backing,
        clean_mic_id.as_deref(),
        clean_render_id.as_deref(),
        0,
        offset_us,
        key,
        vocal_octave,
        speed as f64,
        octave_tolerance.unwrap_or(true),
        monitor_config.unwrap_or_default(),
        recording_config,
    )
    .map_err(|e| format!("Failed to start session: {}", e))?;

    *lock = Some(session);
    Ok(true)
}

#[tauri::command]
fn start_monitor_preview(
    state: State<'_, AppState>,
    mic_device_id: Option<String>,
    render_device_id: Option<String>,
    monitor_config: Option<MonitorConfig>,
) -> Result<bool, String> {
    if state.session.lock().unwrap().is_some() {
        return Err("歌唱を停止してからマイク試聴を開始してください".to_string());
    }
    let mut lock = state.monitor_session.lock().unwrap();
    if lock.is_some() {
        return Ok(true);
    }

    let clean_mic_id = mic_device_id.filter(|value| !value.trim().is_empty());
    let clean_render_id = render_device_id.filter(|value| !value.trim().is_empty());
    let session = MicMonitorSession::start(
        clean_mic_id.as_deref(),
        clean_render_id.as_deref(),
        monitor_config.unwrap_or_default(),
    )
    .map_err(|error| format!("マイク試聴を開始できません: {error}"))?;
    *lock = Some(session);
    Ok(true)
}

#[tauri::command]
fn stop_monitor_preview(state: State<'_, AppState>) -> bool {
    if let Some(mut session) = state.monitor_session.lock().unwrap().take() {
        session.stop();
        true
    } else {
        false
    }
}

#[tauri::command]
fn stop_karaoke_session(state: State<'_, AppState>) -> Option<ScoreResult> {
    let mut lock = state.session.lock().unwrap();
    if let Some(mut s) = lock.take() {
        let result = s.stop();
        *state.last_recording.lock().unwrap() = s.take_recording_artifact();
        result
    } else {
        None
    }
}

#[tauri::command]
fn get_last_recording(state: State<'_, AppState>) -> Option<RecordingArtifact> {
    state.last_recording.lock().ok()?.clone()
}

#[tauri::command]
fn start_editor_preview(
    state: State<'_, AppState>,
    start_ms: f64,
    render_device_id: Option<String>,
    song_id: Option<String>,
) -> Result<bool, String> {
    let mut player_lock = state.editor_player.lock().unwrap();
    if let Some(mut p) = player_lock.take() {
        p.stop();
    }

    // Get or load audio
    let song = song_descriptor(song_id.as_deref())?;
    let cache_key = song.song_id.clone();
    let audio_arc = {
        let mut audio_lock = state.cached_audio.lock().unwrap();
        if let Some((cached_song_id, audio)) = audio_lock.as_ref() {
            if cached_song_id == &cache_key {
                audio.clone()
            } else {
                let decoded = AudioDecoder::decode_file(song_asset_path(&song.backing_audio)?)
                    .map_err(|e| format!("Decode error: {e}"))?;
                let arc = Arc::new(decoded);
                *audio_lock = Some((cache_key, arc.clone()));
                arc
            }
        } else {
            let decoded = AudioDecoder::decode_file(song_asset_path(&song.backing_audio)?)
                .map_err(|e| format!("Decode error: {e}"))?;
            let arc = Arc::new(decoded);
            *audio_lock = Some((cache_key, arc.clone()));
            arc
        }
    };

    let clean_render_id = render_device_id.filter(|s| !s.trim().is_empty());
    let player = EditorAudioPlayer::start(audio_arc, start_ms, clean_render_id.as_deref())?;
    *player_lock = Some(player);
    Ok(true)
}

#[tauri::command]
fn stop_editor_preview(state: State<'_, AppState>) -> bool {
    let mut player_lock = state.editor_player.lock().unwrap();
    if let Some(mut p) = player_lock.take() {
        p.stop();
        true
    } else {
        false
    }
}

#[tauri::command]
fn get_editor_preview_pos(state: State<'_, AppState>) -> Option<f64> {
    let player_lock = state.editor_player.lock().unwrap();
    player_lock.as_ref().map(|p| p.get_position_ms())
}

#[tauri::command]
fn run_audio_analysis(
    state: State<'_, AppState>,
    force: bool,
    song_id: Option<String>,
    auto_adopt: Option<bool>,
) -> Result<bool, String> {
    use std::io::BufRead;
    let song = song_descriptor(song_id.as_deref())?;
    let analysis_song_id = song.song_id.clone();
    let input_path = song_asset_path(&song.backing_audio)?;
    let output_chart_path = analysis_candidate_path(&song)?;
    let output_dir = analysis_output_dir(&song);
    let base_chart_path = active_chart_path(&song)?;
    let auto_adopt = auto_adopt.unwrap_or(false);
    let state_clone = state.analysis_state.clone();
    {
        let mut cur = state_clone.lock().unwrap();
        if cur.is_running {
            return Err("解析が既に実行中です".to_string());
        }
        *cur = AnalysisProgress {
            is_running: true,
            progress_percent: 5,
            status_text: "AI解析プロセスを起動中...".to_string(),
            logs: vec!["[*] AI音声解析プロセスを開始します...".to_string()],
            completed: false,
            auto_adopted: false,
            error: None,
        };
    }

    std::thread::spawn(move || {
        let workspace = workspace_path("");
        let python_path = workspace_path("services/analyzer/.venv/Scripts/python.exe");
        let script_path = workspace_path("services/analyzer/src/pipeline.py");

        let mut cmd = std::process::Command::new(&python_path);
        cmd.arg(&script_path)
            .arg("--input")
            .arg(&input_path)
            .arg("--output-chart")
            .arg(&output_chart_path)
            .arg("--output-dir")
            .arg(&output_dir)
            .arg("--base-chart")
            .arg(&base_chart_path)
            .current_dir(&workspace);
        // The GUI process has no console code page. Force one encoding on both
        // sides of the pipe so Japanese progress output cannot make Rust close
        // stdout after a UTF-8 decoding error (which surfaced in Python as EINVAL).
        cmd.env("PYTHONUTF8", "1");
        cmd.env("PYTHONIOENCODING", "utf-8");
        cmd.env("PYTHONUNBUFFERED", "1");
        if force {
            cmd.arg("--force");
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        match cmd.spawn() {
            Ok(mut child) => {
                let stderr_thread = child.stderr.take().map(|stderr| {
                    let error_state = state_clone.clone();
                    std::thread::spawn(move || {
                        for line in std::io::BufReader::new(stderr)
                            .lines()
                            .map_while(Result::ok)
                        {
                            let mut lock = error_state.lock().unwrap();
                            lock.logs.push(format!("[stderr] {line}"));
                            if lock.logs.len() > 100 {
                                lock.logs.remove(0);
                            }
                        }
                    })
                });
                if let Some(stdout) = child.stdout.take() {
                    let reader = std::io::BufReader::new(stdout);
                    for line in reader.lines().map_while(Result::ok) {
                        let mut lock = state_clone.lock().unwrap();
                        if line.starts_with("PROGRESS:") {
                            if let Some(pct_end) = line.find('%') {
                                if let Ok(pct) = line[9..pct_end].trim().parse::<u32>() {
                                    lock.progress_percent = pct;
                                }
                                lock.status_text = line[pct_end + 1..].trim().to_string();
                            }
                        }
                        lock.logs.push(line);
                        if lock.logs.len() > 100 {
                            lock.logs.remove(0);
                        }
                    }
                }

                let status = child.wait();
                if let Some(thread) = stderr_thread {
                    let _ = thread.join();
                }
                let mut lock = state_clone.lock().unwrap();
                lock.is_running = false;
                match status {
                    Ok(s) if s.success() => {
                        match get_analysis_candidate(Some(analysis_song_id.clone())) {
                            Ok(candidate) => {
                                if auto_adopt {
                                    match adopt_analysis_candidate(Some(analysis_song_id.clone())) {
                                        Ok(adopted) => {
                                            match save_library_package(&analysis_song_id) {
                                                Ok(path) => {
                                                    lock.completed = true;
                                                    lock.auto_adopted = true;
                                                    lock.progress_percent = 100;
                                                    lock.status_text = format!(
                                                        "AI音程バーと.kpkを保存しました（{}ノーツ）",
                                                        adopted.notes.len()
                                                    );
                                                    lock.logs.push(format!(
                                                        "[+] 再生パッケージを保存しました: {}",
                                                        path.display()
                                                    ));
                                                }
                                                Err(error) => {
                                                    lock.error = Some(error);
                                                    lock.status_text =
                                                        ".kpkの保存に失敗しました".to_string();
                                                }
                                            }
                                        }
                                        Err(error) => {
                                            lock.error = Some(error);
                                            lock.status_text =
                                                "解析候補の自動保存に失敗しました".to_string();
                                        }
                                    }
                                } else {
                                    lock.completed = true;
                                    lock.progress_percent = 100;
                                    lock.status_text = format!(
                                        "AI解析候補が完了しました（{}ノーツ・未採用）",
                                        candidate.notes.len()
                                    );
                                    lock.logs.push(
                                        "[+] 候補を検証しました。現行譜面は変更していません。"
                                            .to_string(),
                                    );
                                }
                            }
                            Err(error) => {
                                lock.error = Some(error);
                                lock.status_text = "解析出力の検証に失敗しました".to_string();
                            }
                        }
                    }
                    Ok(s) => {
                        lock.error = Some(format!(
                            "プロセスが終了コード {:?} で失敗しました",
                            s.code()
                        ));
                        lock.status_text = "エラーが発生しました".to_string();
                    }
                    Err(e) => {
                        lock.error = Some(format!("待機エラー: {}", e));
                        lock.status_text = "エラーが発生しました".to_string();
                    }
                }
            }
            Err(e) => {
                let mut lock = state_clone.lock().unwrap();
                lock.is_running = false;
                lock.error = Some(format!("Python起動エラー: {}", e));
                lock.status_text = "Pythonプロセスの起動に失敗しました".to_string();
            }
        }
    });

    Ok(true)
}

#[tauri::command]
fn get_analysis_status(state: State<'_, AppState>) -> AnalysisProgress {
    let lock = state.analysis_state.lock().unwrap();
    lock.clone()
}

#[tauri::command]
fn export_chart_file(
    chart_data: serde_json::Value,
    filename: Option<String>,
) -> Result<String, String> {
    let fname = filename.unwrap_or_else(|| "shining_star_chart.json".to_string());
    let safe_name = Path::new(&fname)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.ends_with(".json"))
        .ok_or_else(|| "Invalid export filename".to_string())?;
    let chart: Chart =
        serde_json::from_value(chart_data).map_err(|e| format!("Invalid chart: {e}"))?;
    chart
        .validate_semantics()
        .map_err(|e| format!("Validation error: {e}"))?;
    let export_path = workspace_path("exports").join(safe_name);
    let json = serde_json::to_vec_pretty(&chart).map_err(|e| e.to_string())?;
    durable_replace(&export_path, &json)?;
    Ok(export_path.display().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            println!("[*] Tauri setup hook executed.");
            if let Some(win) = app.get_webview_window("main") {
                println!("[+] Window 'main' found! Forcing center, unminimize, show, and focus...");
                if let Err(e) = win.center() {
                    eprintln!("[!] center error: {:?}", e);
                }
                if let Err(e) = win.unminimize() {
                    eprintln!("[!] unminimize error: {:?}", e);
                }
                if let Err(e) = win.show() {
                    eprintln!("[!] show error: {:?}", e);
                }
                if let Err(e) = win.set_focus() {
                    eprintln!("[!] set_focus error: {:?}", e);
                }
                println!("[+] Window operations completed.");
            } else {
                eprintln!("[!] Warning: 'main' window not found!");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            println!("[*] WindowEvent on {:?}: {:?}", window.label(), event);
        })
        .manage(AppState {
            pipeline: Mutex::new(None),
            session: Mutex::new(None),
            monitor_session: Mutex::new(None),
            last_recording: Mutex::new(None),
            editor_player: Mutex::new(None),
            cached_audio: Mutex::new(None),
            karaoke_backing_cache: Mutex::new(None),
            analysis_state: Arc::new(Mutex::new(AnalysisProgress::default())),
        })
        .invoke_handler(tauri::generate_handler![
            get_system_info,
            list_audio_devices,
            get_current_chart,
            list_songs,
            delete_song,
            import_audio_file,
            get_analysis_candidate,
            adopt_analysis_candidate,
            get_synthetic_chart,
            get_editor_media_info,
            save_chart,
            save_editor_recovery,
            get_editor_recovery,
            discard_editor_recovery,
            export_song_package,
            import_song_package,
            start_mic_pipeline,
            poll_display_packet,
            poll_mic_diagnostic,
            stop_mic_pipeline,
            start_synthetic_session,
            start_karaoke_session,
            stop_karaoke_session,
            get_last_recording,
            start_monitor_preview,
            stop_monitor_preview,
            start_editor_preview,
            stop_editor_preview,
            get_editor_preview_pos,
            run_audio_analysis,
            get_analysis_status,
            export_chart_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_delete_moves_package_backup_and_project_to_trash() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "karaoke-delete-test-{}-{unique}",
            std::process::id()
        ));
        let song_id = "song_delete_fixture";
        let package_dir = root.join("packages");
        let project_dir = root.join("imported").join(song_id);
        fs::create_dir_all(&package_dir).unwrap();
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(package_dir.join(format!("{song_id}.kpk")), b"package").unwrap();
        fs::write(package_dir.join(format!("{song_id}.bak")), b"backup").unwrap();
        fs::write(project_dir.join("chart.json"), b"{}").unwrap();
        let descriptor = SongDescriptor {
            song_id: song_id.into(),
            active_chart: format!("library/imported/{song_id}/chart.json"),
            draft_chart: format!("library/imported/{song_id}/draft.json"),
            backing_audio: format!("library/imported/{song_id}/backing.wav"),
        };

        let trash = trash_song_files(&root, &descriptor, 1234).unwrap();
        assert!(!package_dir.join(format!("{song_id}.kpk")).exists());
        assert!(!package_dir.join(format!("{song_id}.bak")).exists());
        assert!(!project_dir.exists());
        assert_eq!(fs::read(trash.join("package.kpk")).unwrap(), b"package");
        assert_eq!(fs::read(trash.join("package.bak")).unwrap(), b"backup");
        assert!(trash.join("project/chart.json").is_file());
        assert!(trash.join("deleted.json").is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
