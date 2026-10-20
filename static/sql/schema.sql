-- name: pragma
-- Concurrency (minimal write concern)
PRAGMA journal_mode       = WAL;
PRAGMA busy_timeout       = 1000;
PRAGMA wal_autocheckpoint = 0;          -- Disable auto-checkpoint; do it manually during maintenance
PRAGMA cache_size         = -8192;      -- 8 MiB per connection
PRAGMA temp_store         = FILE;       -- Allow large sorts to spill to disk
PRAGMA mmap_size          = 67108864;   -- Map at most 64 MiB
PRAGMA foreign_keys       = ON;
PRAGMA query_only         = OFF;
PRAGMA analysis_limit     = 1000;


-- name: schema
CREATE TABLE IF NOT EXISTS repos (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    slug              TEXT    NOT NULL,           -- 'nixpkgs-unstable', 'arch-extra', 'aur'
    name              TEXT    NOT NULL,           -- 'Nixpkgs (unstable)'
    family            TEXT    NOT NULL,           -- 'debuntu | fedora ...'
    distro            TEXT,                       -- 'nixos', 'arch', 'debian' (NULL for distro-agnostic)
    manager           TEXT    NOT NULL,           -- 'nix' | 'pacman' | 'aur' | 'apt' | 'apk' | ...
    branch            TEXT,                       -- 'unstable', '25.05', 'extra', 'trixie/main'
    homepage_url      TEXT,                       -- repo homepage

    pkg_url_template    TEXT,                     -- package landing page template, eg: https://site.com/{name}
    source_url_template TEXT,                     -- package recipe/sources template (PKGBUILD, .spec, ebuild ...), eg: https://site.com/{pkg_base}/src

    links             TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(links)), -- additional repository links, preserving upstream labels/types
    meta              TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(meta)),

    brand_color       TEXT,

    score             REAL    NOT NULL DEFAULT 0, -- for ranking in cross-repo search
    package_count     INTEGER NOT NULL DEFAULT 0,
    num_packages      INTEGER NOT NULL DEFAULT 0,
    num_maintainers   INTEGER NOT NULL DEFAULT 0,

    -- These timestamps reflect the upstream repo, not the local state.
    -- created_at is repology's repositories.first_seen.
    created_at        TEXT    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- updated_at is the last time the repo received an update to any of its packages.
    updated_at        TEXT    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (slug)
) STRICT;

-- packages
CREATE TABLE IF NOT EXISTS packages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_id           INTEGER NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    slug              TEXT    NOT NULL,           -- unique string slug that the repo uses. eg: nix (something.name), pacman (pkg), aur (name) etc.
    name              TEXT    NOT NULL,
    package           TEXT    NOT NULL,           -- canonical upstream identifier, before slug escaping
    name_norm         TEXT    NOT NULL,           -- normalized name for cross-repo search. eg: 'foo-bar' -> 'foobar', 'FooBar' -> 'foobar'
    project_name      TEXT,                       -- cross-repository project name (`effname`)
    binary_names      TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(binary_names)), -- installable package names
    excerpt           TEXT,
    description       TEXT,

    -- Source/base package that several binary packages are built from.
    -- eg: pacman -> pkgbase, aur -> PackageBase, deb -> Source. NULL for nix.
    pkg_base          TEXT,
    subrepo           TEXT,                       -- repository component of the selected version, eg: extra, bookworm/main

    version           TEXT,                       -- eg: 5.0.1, 125.0, 1.2.1-1, unstable-2024-01-05 etc.
    version_norm      TEXT,                       -- 0 padded semver form for lexicographic search, eg: 2.1.1 = 00002.00001.00001

    homepage_url      TEXT,                       -- upstream project URL

    licenses          TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(licenses)),  -- eg: ["GPL-3.0-or-later"]
    is_nonfree        INTEGER CHECK (is_nonfree IN (0, 1)),
    platforms         TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(platforms)), -- eg: ["x86_64-linux","aarch64-darwin"]

    "groups"          TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid("groups")),
    keywords          TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),

    -- Remove duplicate search tokens from each field before saving.
    identity_tokens   TEXT,
    keyword_tokens    TEXT,
    body_tokens       TEXT,

    status            TEXT    NOT NULL DEFAULT 'unprocessed' CHECK (status IN ('unprocessed', 'newest', 'outdated', 'ignored', 'unique', 'devel', 'legacy', 'incorrect', 'untrusted', 'noscheme', 'rolling')),
    score             REAL    NOT NULL DEFAULT 0,

    meta              TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(meta)),

    -- These timestamps refelect upstream packages, not the local state.
    -- created_at is repology's first_seen.
    created_at        TEXT    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- updated_at is when repology detected a version change in the upstrea,
    updated_at        TEXT    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (repo_id, slug)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_packages_name      ON packages (name);
CREATE INDEX IF NOT EXISTS idx_packages_repo_name ON packages (repo_id, name);
CREATE INDEX IF NOT EXISTS idx_packages_repo_version ON packages (repo_id, version_norm);
CREATE INDEX IF NOT EXISTS idx_packages_version ON packages (version_norm);
CREATE INDEX IF NOT EXISTS idx_packages_repo_updated ON packages (repo_id, substr(updated_at, 1, 10));
CREATE INDEX IF NOT EXISTS idx_packages_updated ON packages (substr(updated_at, 1, 10));

-- maintainers
CREATE TABLE IF NOT EXISTS maintainers (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    slug              TEXT    NOT NULL,           -- email address or upstream handle
    handle            TEXT    NOT NULL,           -- as displayed upstream. eg: 'UserName' or @username
    name              TEXT,                       -- display name. eg: 'User Name'
    email             TEXT,
    url               TEXT,

    package_count     INTEGER NOT NULL DEFAULT 0, -- recomputed after every sync
    created_at        TEXT    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at        TEXT    DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    UNIQUE (slug)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_maintainers_slug ON maintainers (slug);


CREATE TABLE IF NOT EXISTS package_maintainers (
    package_id        INTEGER NOT NULL REFERENCES packages(id)    ON DELETE CASCADE,
    maintainer_id     INTEGER NOT NULL REFERENCES maintainers(id) ON DELETE CASCADE,

    PRIMARY KEY (package_id, maintainer_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_pkg_maintainers ON package_maintainers (maintainer_id, package_id);

-- Store common filter values as separate rows for faster lookups.
-- Rebuild these in the same transaction as package updates.
CREATE TABLE IF NOT EXISTS package_facets (
    repo_id           INTEGER NOT NULL,
    kind              TEXT    NOT NULL, -- 'license', 'tag', 'maintainer', 'group', 'status', 'is_nonfree'
    value             TEXT    NOT NULL,
    name              TEXT    NOT NULL, -- Copy of packages.name for sorting
    package_id        INTEGER NOT NULL REFERENCES packages(id) ON DELETE CASCADE,

    PRIMARY KEY (repo_id, kind, value, name, package_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_facets_pkg ON package_facets (package_id, kind, value);

-- Package count per facet value.
-- Also holds kind = 'platform', which has no package_facets rows: a row per
-- (package, platform) would run into millions, and platform is filtered against
CREATE TABLE IF NOT EXISTS facet_counts (
    repo_id           INTEGER NOT NULL,
    kind              TEXT    NOT NULL,
    value             TEXT    NOT NULL,
    package_count     INTEGER NOT NULL,

    PRIMARY KEY (repo_id, kind, value)
) STRICT, WITHOUT ROWID;

-- FTS.
-- packages_fts
CREATE VIRTUAL TABLE IF NOT EXISTS packages_fts USING fts5 (
    -- SQLite's built in rowid is used as the primary key to reference packages.id
    identity,
    keywords,
    body,
    repo,

    -- This is sqlite's `content` keyword. Setting this to ''
    -- avoids text content being duplicated in the fts table.
    -- contentless_delete allows DELETE/UPDATE on a contentless table (SQLite >= 3.43).
    content='',
    contentless_delete=1
);

-- Keep the fts table in sync with rows in packages.
CREATE TRIGGER IF NOT EXISTS trg_packages_after_insert AFTER INSERT ON packages
BEGIN
    INSERT INTO packages_fts (rowid, identity, keywords, body, repo)
    VALUES (NEW.id, NEW.identity_tokens, NEW.keyword_tokens, NEW.body_tokens, NEW.repo_id);
END;

CREATE TRIGGER IF NOT EXISTS trg_packages_after_delete AFTER DELETE ON packages
BEGIN
    DELETE FROM packages_fts WHERE rowid = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS trg_packages_after_update
AFTER UPDATE OF identity_tokens, keyword_tokens, body_tokens, repo_id ON packages
BEGIN
    DELETE FROM packages_fts WHERE rowid = OLD.id;

    INSERT INTO packages_fts (rowid, identity, keywords, body, repo)
    VALUES (NEW.id, NEW.identity_tokens, NEW.keyword_tokens, NEW.body_tokens, NEW.repo_id);
END;
