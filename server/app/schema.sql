-- M0 数据库 DDL（权威：docs/API-CONTRACT.md 第 2 节，字段名不得改动）
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS books (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  author TEXT,
  format TEXT NOT NULL DEFAULT 'txt',
  file_path TEXT NOT NULL,
  sha256 TEXT NOT NULL UNIQUE,
  encoding TEXT,
  total_chars INTEGER NOT NULL DEFAULT 0,
  chapter_count INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chapters (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  idx INTEGER NOT NULL,
  title TEXT NOT NULL,
  chars INTEGER NOT NULL,
  paras_json TEXT NOT NULL,
  UNIQUE(book_id, idx)
);

-- EPUB 图片资源：仅保存 EPUB 内被正文引用的图片，正文端通过受保护 API 读取。
CREATE TABLE IF NOT EXISTS epub_assets (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL DEFAULT 0,
  path TEXT NOT NULL,
  media_type TEXT NOT NULL,
  image_bytes BLOB NOT NULL,
  bytes INTEGER NOT NULL,
  UNIQUE(book_id, path)
);
CREATE INDEX IF NOT EXISTS idx_epub_assets_book ON epub_assets(book_id, chapter_idx, path);

-- A resource may occur repeatedly within/across chapters. Anchors count plain
-- text paragraphs, so book progress and annotation locators remain unchanged.
CREATE TABLE IF NOT EXISTS epub_illustrations (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  before_para INTEGER NOT NULL,
  asset_id TEXT NOT NULL REFERENCES epub_assets(id) ON DELETE CASCADE,
  alt TEXT NOT NULL DEFAULT '',
  PRIMARY KEY(book_id, chapter_idx, ordinal)
);

CREATE TABLE IF NOT EXISTS epub_paragraph_formats (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL,
  para_idx INTEGER NOT NULL,
  formats_json TEXT NOT NULL,
  PRIMARY KEY(book_id, chapter_idx, para_idx)
);

-- 可恢复的书架移除状态；不删除书籍或其派生数据，兼容已有数据库。
CREATE TABLE IF NOT EXISTS library_state (
  book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
  archived_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chapters_book ON chapters(book_id, idx);

CREATE TABLE IF NOT EXISTS progress (
  book_id TEXT PRIMARY KEY,
  chapter INTEGER NOT NULL DEFAULT 0,
  para INTEGER NOT NULL DEFAULT 0,
  offset INTEGER NOT NULL DEFAULT 0,
  percent REAL NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

-- 唯一位置索引由 db.init_db 在旧库去重后建立。
CREATE TABLE IF NOT EXISTS bookmarks (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  para INTEGER NOT NULL,
  offset INTEGER NOT NULL DEFAULT 0,
  label TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS segments (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  idx INTEGER NOT NULL,
  strategy TEXT NOT NULL,
  params_hash TEXT NOT NULL,
  start_para INTEGER NOT NULL,
  end_para INTEGER NOT NULL,
  text TEXT NOT NULL,
  features_json TEXT NOT NULL,
  score REAL NOT NULL,          -- 语料内百分位 0..1
  raw REAL NOT NULL DEFAULT 0,  -- 原始加权分
  selected INTEGER NOT NULL DEFAULT 0,
  UNIQUE(book_id, chapter, idx, params_hash)
);
CREATE INDEX IF NOT EXISTS idx_segments_lookup ON segments(book_id, params_hash, chapter, score);

CREATE TABLE IF NOT EXISTS search_index_state (
  book_id TEXT PRIMARY KEY,
  chapters_indexed INTEGER NOT NULL DEFAULT 0,
  degraded INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS para_fts USING fts5(
  text,
  book_id UNINDEXED,
  chapter_idx UNINDEXED,
  para_idx UNINDEXED,
  tokenize='trigram'
);
-- ==========================================================================
-- M1 增量 DDL（契约 7.2，幂等；M0 老库直接升级）
-- ==========================================================================

CREATE TABLE IF NOT EXISTS book_pages (          -- 仅 cbz 使用
  book_id TEXT NOT NULL,
  idx INTEGER NOT NULL,
  path TEXT NOT NULL,                             -- 相对 DATA_DIR 的路径
  width INTEGER NOT NULL DEFAULT 0,
  height INTEGER NOT NULL DEFAULT 0,
  bytes INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (book_id, idx)
);

CREATE TABLE IF NOT EXISTS annotations (          -- 高亮/笔记
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  para INTEGER NOT NULL,
  start_offset INTEGER NOT NULL,
  end_offset INTEGER NOT NULL,
  text TEXT NOT NULL,
  note TEXT,
  color TEXT NOT NULL DEFAULT 'yellow',          -- yellow | green | blue | pink
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_annotations_book ON annotations(book_id, chapter, para);

-- Local-first synchronization. Operations are immutable and globally ordered by
-- rowid so a client can resume after a network interruption with one cursor.
CREATE TABLE IF NOT EXISTS sync_devices (
  id TEXT PRIMARY KEY,
  label TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  revoked_at INTEGER
);
CREATE TABLE IF NOT EXISTS sync_operations (
  cursor INTEGER PRIMARY KEY AUTOINCREMENT,
  op_id TEXT NOT NULL UNIQUE,
  device_id TEXT NOT NULL,
  seq INTEGER NOT NULL,
  entity TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  action TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  base_revision INTEGER,
  created_at INTEGER NOT NULL,
  FOREIGN KEY(device_id) REFERENCES sync_devices(id) ON DELETE CASCADE,
  UNIQUE(device_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_sync_operations_cursor ON sync_operations(cursor);
CREATE INDEX IF NOT EXISTS idx_sync_operations_entity ON sync_operations(entity, entity_id, cursor);
CREATE TABLE IF NOT EXISTS sync_entity_states (
  entity TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  payload_json TEXT,
  revision INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(entity, entity_id)
);
CREATE TABLE IF NOT EXISTS sync_device_sessions (
  device_id TEXT PRIMARY KEY REFERENCES sync_devices(id) ON DELETE CASCADE,
  session_hash TEXT NOT NULL
);

-- ============================================================================
-- M2 AI 增量 DDL（文本抽取、人物卡、scene；幂等，M0/M1 老库可直接升级）
-- ============================================================================

CREATE TABLE IF NOT EXISTS ai_provider_configs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  book_id TEXT NOT NULL UNIQUE,
  provider TEXT NOT NULL DEFAULT 'mock',
  base_url TEXT,
  model TEXT,
  api_key_env TEXT,
  temperature REAL NOT NULL DEFAULT 0.2,
  prompt_version INTEGER NOT NULL DEFAULT 1,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(book_id) REFERENCES books(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS ai_runs (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  provider TEXT NOT NULL,
  config_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'running',
  stats_json TEXT NOT NULL DEFAULT '{}',
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(book_id) REFERENCES books(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ai_runs_book ON ai_runs(book_id, created_at DESC);

CREATE TABLE IF NOT EXISTS ai_extractions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  book_id TEXT NOT NULL,
  segment_id TEXT NOT NULL,
  prompt_version INTEGER NOT NULL,
  input_hash TEXT NOT NULL,
  result_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'done',
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(book_id, segment_id, prompt_version, input_hash),
  FOREIGN KEY(book_id) REFERENCES books(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ai_extractions_book ON ai_extractions(book_id, segment_id);

CREATE TABLE IF NOT EXISTS characters (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  name TEXT NOT NULL,
  aliases_json TEXT NOT NULL DEFAULT '[]',
  card_json TEXT NOT NULL DEFAULT '{}',
  frozen INTEGER NOT NULL DEFAULT 0,
  identity_block TEXT,
  identity_hash TEXT,
  version INTEGER NOT NULL DEFAULT 1,
  first_chapter INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(book_id, name),
  FOREIGN KEY(book_id) REFERENCES books(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_characters_book ON characters(book_id, name);

CREATE TABLE IF NOT EXISTS scenes (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL,
  chapter_id INTEGER,
  segment_id TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'draft',
  prompt_version INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(book_id, segment_id, prompt_version),
  FOREIGN KEY(book_id) REFERENCES books(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_scenes_book ON scenes(book_id, chapter_id, segment_id);

-- Text-model director plans contain summaries/prompts, never API credentials.
CREATE TABLE IF NOT EXISTS manga_text_plans (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  cache_key TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(book_id, cache_key)
);

-- Resumable whole-book reading. Cache keys include source, model and prompt version.
CREATE TABLE IF NOT EXISTS book_analysis (
  book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL,
  status TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  config_json TEXT NOT NULL,
  total INTEGER NOT NULL,
  completed INTEGER NOT NULL DEFAULT 0,
  current_chapter INTEGER,
  current_part INTEGER NOT NULL DEFAULT 0,
  total_parts INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS analysis_chunks (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  cache_key TEXT NOT NULL,
  result_json TEXT NOT NULL,
  PRIMARY KEY(book_id, cache_key)
);
CREATE TABLE IF NOT EXISTS chapter_summaries (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter INTEGER NOT NULL,
  source_hash TEXT NOT NULL,
  cache_key TEXT NOT NULL,
  summary_json TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(book_id, chapter)
);

-- Bounded chapter continuity memory. Capsules are navigation aids; quoted
-- facts remain separately auditable and are filtered by source hash/position.
CREATE TABLE IF NOT EXISTS chapter_memory_capsules (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter INTEGER NOT NULL,
  source_hash TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  text TEXT NOT NULL,
  tokens INTEGER NOT NULL,
  token_method TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'ok',
  version INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(book_id, chapter)
);
CREATE TABLE IF NOT EXISTS chapter_memory_facts (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT 'state',
  character_id TEXT NOT NULL,
  field TEXT NOT NULL,
  value TEXT NOT NULL,
  quote TEXT NOT NULL,
  start_offset INTEGER NOT NULL,
  end_offset INTEGER NOT NULL,
  source_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chapter_memory_facts_position
  ON chapter_memory_facts(book_id, chapter, start_offset, character_id, field);
CREATE TABLE IF NOT EXISTS chapter_memory_history (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter INTEGER NOT NULL,
  revision INTEGER NOT NULL,
  source_hash TEXT NOT NULL,
  memory_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY(book_id, chapter, revision)
);

-- M3: versioned identities, reviewed references, reproducible single-panel jobs.
CREATE TABLE IF NOT EXISTS render_configs (
  book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
  config_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS character_forms (
  id TEXT PRIMARY KEY,
  character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
  form_key TEXT NOT NULL,
  label TEXT NOT NULL,
  card_json TEXT NOT NULL,
  identity_block TEXT NOT NULL,
  identity_hash TEXT NOT NULL,
  version INTEGER NOT NULL,
  locked INTEGER NOT NULL DEFAULT 0,
  start_ch INTEGER NOT NULL DEFAULT 0,
  start_para INTEGER NOT NULL DEFAULT 0,
  reversible INTEGER NOT NULL DEFAULT 1,
  UNIQUE(character_id, form_key)
);
CREATE TABLE IF NOT EXISTS form_refs (
  id TEXT PRIMARY KEY,
  form_id TEXT NOT NULL REFERENCES character_forms(id) ON DELETE CASCADE,
  form_version INTEGER NOT NULL,
  identity_hash TEXT NOT NULL,
  path TEXT NOT NULL,
  accepted INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS render_jobs (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  form_id TEXT REFERENCES character_forms(id),
  scene_id TEXT REFERENCES scenes(id),
  snapshot_json TEXT NOT NULL,
  config_hash TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'queued',
  attempts INTEGER NOT NULL DEFAULT 0,
  image_path TEXT,
  face_score REAL,
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_render_jobs_queue ON render_jobs(status, created_at);
CREATE TABLE IF NOT EXISTS render_attempts (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES render_jobs(id) ON DELETE CASCADE,
  idx INTEGER NOT NULL,
  seed INTEGER NOT NULL,
  prompt_id TEXT,
  workflow_json TEXT,
  image_path TEXT,
  result_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'running',
  error TEXT,
  UNIQUE(job_id, idx)
);
CREATE TABLE IF NOT EXISTS panel_forms (
  panel_id TEXT PRIMARY KEY REFERENCES render_jobs(id) ON DELETE CASCADE,
  form_id TEXT NOT NULL REFERENCES character_forms(id),
  source TEXT NOT NULL,
  flashback INTEGER NOT NULL DEFAULT 0
);

-- M4: editable pages point at immutable compositions; published editions never change.
CREATE TABLE IF NOT EXISTS manga_pages (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  spec_json TEXT NOT NULL,
  sources_json TEXT NOT NULL,
  image_path TEXT NOT NULL,
  image_hash TEXT NOT NULL,
  deleted INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_manga_pages ON manga_pages(book_id,deleted,position,id);
CREATE TABLE IF NOT EXISTS manga_exports (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id),
  fingerprint TEXT NOT NULL,
  manifest_json TEXT NOT NULL,
  archive_path TEXT NOT NULL,
  comic_book_id TEXT NOT NULL REFERENCES books(id),
  created_at INTEGER NOT NULL,
  UNIQUE(book_id,fingerprint)
);

-- Opt-in automatic comics: resumable orchestration and separately labelled drafts.
CREATE TABLE IF NOT EXISTS auto_manga (
  book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 0,
  epoch INTEGER NOT NULL DEFAULT 0,
  start_chapter INTEGER NOT NULL,
  chapter INTEGER NOT NULL,
  status TEXT NOT NULL,
  message TEXT NOT NULL DEFAULT '',
  source_hash TEXT NOT NULL,
  state_json TEXT NOT NULL DEFAULT '{}',
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS auto_manga_refs (
  ref_id TEXT PRIMARY KEY REFERENCES form_refs(id) ON DELETE CASCADE,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS auto_manga_pages (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL,
  scene_id TEXT NOT NULL,
  chapter INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  spec_json TEXT NOT NULL,
  sources_json TEXT NOT NULL,
  image_path TEXT NOT NULL,
  image_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE(book_id,run_id,scene_id)
);

CREATE TABLE IF NOT EXISTS task_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  level TEXT NOT NULL,
  message TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_logs_run ON task_logs(book_id,run_id,id);

-- Editable, source-bound storyboards. One text request describes a whole page.
CREATE TABLE IF NOT EXISTS storyboards (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  scene_id TEXT NOT NULL REFERENCES scenes(id),
  cache_key TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1,
  plan_json TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(book_id,cache_key)
);
CREATE TABLE IF NOT EXISTS render_cast (
  job_id TEXT NOT NULL REFERENCES render_jobs(id) ON DELETE CASCADE,
  form_id TEXT NOT NULL REFERENCES character_forms(id),
  PRIMARY KEY(job_id,form_id)
);

CREATE TABLE IF NOT EXISTS ai_memories (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  kind TEXT NOT NULL,
  content TEXT NOT NULL,
  tags_json TEXT NOT NULL DEFAULT '[]',
  book_id TEXT REFERENCES books(id) ON DELETE CASCADE,
  character_id TEXT REFERENCES characters(id) ON DELETE CASCADE,
  enabled INTEGER NOT NULL DEFAULT 1,
  revision INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ai_memories_scope ON ai_memories(book_id,character_id,enabled);
CREATE TABLE IF NOT EXISTS ai_web_cache (
  cache_key TEXT PRIMARY KEY,
  results_json TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
