-- brain schema v2 -- the bitemporal fact model.
--
-- Central idea: relations ARE facts. `A depends_on B` is a fact whose object is
-- an entity, which makes the graph inherit bitemporality for free -- a
-- dependency that ended is visible as such, rather than silently deleted.
--
-- All instants are INTEGER microseconds since the Unix epoch, not RFC 3339 text.
-- Every bitemporal predicate is a comparison, and RFC 3339 only sorts
-- lexicographically when the fractional-second precision is uniform -- one
-- timestamp printed as `...:00Z` next to one printed as `...:00.5Z` silently
-- orders wrong. Integers cannot drift that way.

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

-- Nodes of the graph.
CREATE TABLE entity (
  id         INTEGER PRIMARY KEY,
  key        TEXT NOT NULL UNIQUE,  -- NFKC, non-word -> _, collapsed, casefolded
  label      TEXT NOT NULL,         -- what the user actually typed, for display
  kind       TEXT,                  -- free-form: produto, pessoa, servico...
  created_at INTEGER NOT NULL
) STRICT;

-- Alternate spellings. `source` distinguishes what a user declared from what the
-- brain learned by observing how questions get asked (Passo 7 onward).
CREATE TABLE entity_alias (
  alias_key TEXT NOT NULL,
  entity_id INTEGER NOT NULL REFERENCES entity(id),
  source    TEXT NOT NULL CHECK (source IN ('declared', 'learned')),
  weight    REAL NOT NULL DEFAULT 1.0,
  PRIMARY KEY (alias_key, entity_id)
) STRICT;

-- The learned ontology. Cardinality is the supersession rule: a `single`
-- predicate holds one value at a time, so a new value closes the old one; a
-- `multi` predicate lets values coexist.
CREATE TABLE predicate (
  key         TEXT PRIMARY KEY,
  cardinality TEXT NOT NULL CHECK (cardinality IN ('single', 'multi')),
  declared    INTEGER NOT NULL DEFAULT 0,  -- 1 = user fixed it; never auto-change
  observed_n  INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE fact (
  id        INTEGER PRIMARY KEY,
  uuid      TEXT NOT NULL UNIQUE,
  entity_id INTEGER NOT NULL REFERENCES entity(id),
  predicate TEXT NOT NULL REFERENCES predicate(key),
  -- Denormalized from `predicate.cardinality` so the partial unique index below
  -- can reference it; SQLite cannot put a subquery in an index predicate.
  is_single INTEGER NOT NULL,

  object_text      TEXT,
  object_num       REAL,
  object_entity_id INTEGER REFERENCES entity(id),  -- set => this fact is an edge
  unit             TEXT,
  statement        TEXT NOT NULL,  -- natural-language form; embedded in Passo 5
  search_text      TEXT NOT NULL,  -- label + predicate + object; the BM25 channel

  -- Valid time: when this was true in the world.
  valid_from INTEGER NOT NULL,
  valid_to   INTEGER,              -- NULL = still true
  -- Transaction time: when the brain learned it.
  recorded_at   INTEGER NOT NULL,
  retracted_at  INTEGER,           -- set when we learn it was NEVER true
  superseded_by INTEGER REFERENCES fact(id),

  confidence     REAL NOT NULL DEFAULT 1.0 CHECK (confidence BETWEEN 0.0 AND 1.0),
  reassert_count INTEGER NOT NULL DEFAULT 0,
  scope          TEXT,
  source         TEXT,          -- who asserted it: agent id, user, filename
  locator        TEXT,          -- JSON {file,line,url,sha}: index, not warehouse

  CHECK (valid_to IS NULL OR valid_from < valid_to),
  CHECK (object_text IS NOT NULL OR object_num IS NOT NULL
         OR object_entity_id IS NOT NULL)
) STRICT;

-- The central bitemporal invariant, enforced by the database rather than by
-- hope: at most one open fact per (entity, single-valued predicate).
CREATE UNIQUE INDEX fact_one_open_single ON fact(entity_id, predicate)
  WHERE valid_to IS NULL AND retracted_at IS NULL AND is_single = 1;

CREATE INDEX fact_timeline ON fact(entity_id, predicate, valid_from);
CREATE INDEX fact_open     ON fact(entity_id, predicate) WHERE valid_to IS NULL;
CREATE INDEX fact_inbound  ON fact(object_entity_id) WHERE object_entity_id IS NOT NULL;

-- The graph, as a view over the facts that point at an entity. Traversal runs
-- over this with a recursive CTE (Passo 6).
CREATE VIEW edge AS
  SELECT id, entity_id AS src, object_entity_id AS dst, predicate AS rel,
         valid_from, valid_to, retracted_at, confidence
  FROM fact
  WHERE object_entity_id IS NOT NULL;

-- The BM25 channel. External-content FTS5 plus triggers, so the index lives in
-- the same transaction as the facts and cannot drift on a crash -- the property
-- a separate search engine could not give us.
--
-- `remove_diacritics 2` makes search accent-insensitive, so a question typed
-- "preco" finds a fact recorded as "preço". Identity stays exact (see
-- norm::key); only search is forgiving.
CREATE VIRTUAL TABLE fact_fts USING fts5(
  statement,
  search_text,
  content = 'fact',
  content_rowid = 'id',
  tokenize = 'porter unicode61 remove_diacritics 2'
);

CREATE TRIGGER fact_fts_insert AFTER INSERT ON fact BEGIN
  INSERT INTO fact_fts(rowid, statement, search_text)
  VALUES (new.id, new.statement, new.search_text);
END;

CREATE TRIGGER fact_fts_delete AFTER DELETE ON fact BEGIN
  INSERT INTO fact_fts(fact_fts, rowid, statement, search_text)
  VALUES ('delete', old.id, old.statement, old.search_text);
END;

-- Guarded on the indexed columns: closing a fact rewrites valid_to and
-- superseded_by constantly, and reindexing identical text on every supersession
-- would be pure waste.
CREATE TRIGGER fact_fts_update AFTER UPDATE ON fact
WHEN old.statement IS NOT new.statement OR old.search_text IS NOT new.search_text
BEGIN
  INSERT INTO fact_fts(fact_fts, rowid, statement, search_text)
  VALUES ('delete', old.id, old.statement, old.search_text);
  INSERT INTO fact_fts(rowid, statement, search_text)
  VALUES (new.id, new.statement, new.search_text);
END;

-- The semantic channel.
--
-- `fact_embedding` is the source of truth: an ordinary table, no extension
-- required, so a brain stays readable even where `sqlite-vec` fails to build.
-- `fact_vec` below is a derived index rebuilt by `brain reindex`; sqlite-vec is
-- a 0.1.x crate with no on-disk stability guarantee, so the recovery path for a
-- format change is drop-and-rebuild rather than a migration.
CREATE TABLE fact_embedding (
  fact_id   INTEGER PRIMARY KEY REFERENCES fact(id),
  model_id  TEXT NOT NULL,   -- so a model change is detectable, not silent
  embedding BLOB NOT NULL    -- int8, one byte per dimension
) STRICT;

-- vec0 metadata filters support equality and ordering but NOT `IS NULL`, which
-- is why "currently holds" is the boolean `is_open` rather than a nullable
-- valid_to. `scope` is a partition key so a large brain shards by namespace.
CREATE VIRTUAL TABLE fact_vec USING vec0(
  fact_id   INTEGER PRIMARY KEY,
  embedding int8[256],
  is_open   INTEGER,
  scope     TEXT PARTITION KEY
);
