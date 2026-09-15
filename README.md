
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="site/assets/static/logo-dark.svg">
  <img alt="pkg.bot" src="site/assets/static/logo.svg" width="275">
</picture>

# pkg.bot

[pkg.bot](https://pkg.bot) is a fast search engine for Linux packages across distributions and package repositories. It uses the package data dump from [Repology](https://repology.org).

In early September 2026, [I](https://nadh.in) had just switched to NixOS. In the process of setting it up, I was frequently searching for packages on the official package site, but was quite annoyed by its clunky UX. No way to sort by date or versions, no "permalink" to share a package's page even.

I remembered that pretty much most distros have clunky, annoying package sites. So I built this.

The search engine is a statically compiled Rust program which uses a single SQLite DB as its data source. It has a built-in Repology Postgres dump importer.

### Features

- Super-fast search, filtering, and querying
- Good UX (subjective) and accessibility
- Sort and filter across various parameters
- Filter on recency, eg: `version > 2.0` and `updated > 2026-01-01`
- RSS feeds for search, filtering, and individual packages

-------------

### Usage

- Download the latest compiled binary from the releases page or clone this repo and run `make build`.
- `./pkgbot new-config` to generate a new config file (`config.toml` by default)
- Edit `config.toml`
- `./pkgbot install` to generate a new SQLite DB (`data.db` by default)
- `./pkgbot --site=./site` to run the web search engine. `./site` is the theme directory cloned from this repo.

### Importing data from repology
- Download the latest dump from `https://dumps.repology.org`
- Uncompress it and restore it to a Postgres database. Use the `docker-compose.yml` file in this repository and do `docker compose up db` to quickly run a Postgres instance.
- `./pkgbot import --db=data.db` (pass Postgres connection DSN `--db`) to import data from the Postgres DB into the pkgbot SQLite DB.

License: AGPL
