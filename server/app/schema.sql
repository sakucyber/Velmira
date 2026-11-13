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
