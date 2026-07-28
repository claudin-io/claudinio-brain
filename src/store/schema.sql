-- brain schema v1
--
-- Passo 2 establishes only the identity marker. The bitemporal fact model lands
-- in Passo 3; SCHEMA_VERSION moves with it.

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
