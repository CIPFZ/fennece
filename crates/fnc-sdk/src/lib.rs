//! Fennec SDK: a small Rust API over the native IPC protocol (SPEC ch.4 §3.1) and a C ABI
//! mirroring the Everything SDK ([`ffi`], `include/fennec_sdk.h`).
//!
//! ```no_run
//! use fennec_sdk::{Column, Fennec};
//!
//! let results = Fennec::connect()?.search("abc ext:pdf").sort(Column::Size, false).limit(10).run()?;
//! for r in results.iter() {
//!     println!("{} {}", r.full_path, r.size);
//! }
//! # Ok::<(), fennec_sdk::Error>(())
//! ```

pub mod ffi;

use std::time::Duration;

pub use fnc_ipc::proto::{BookmarkEntry, Column, FilterEntry, IndexStatus, Mods, Query, VersionInfo, VolumeEntry};
pub use fnc_ipc::rows::Row;
pub use fnc_ipc::{Client, ClientError, Status};

/// SDK errors.
pub type Error = ClientError;

/// Default time to wait for the instance when connecting.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// A connection to a running Fennec.
pub struct Fennec {
    client: Client,
}

impl Fennec {
    /// Connects to the default instance.
    pub fn connect() -> Result<Self, Error> {
        Self::connect_instance("", CONNECT_TIMEOUT)
    }

    /// Connects to a named instance (`-instance <name>`), waiting up to `timeout`.
    pub fn connect_instance(instance: &str, timeout: Duration) -> Result<Self, Error> {
        let mut client = Client::connect(instance, timeout)?;
        client.set_timeout(Some(Duration::from_secs(120)));
        Ok(Self { client })
    }

    /// The underlying protocol client.
    pub fn client(&mut self) -> &mut Client {
        &mut self.client
    }

    /// Starts a search.
    pub fn search(&mut self, text: &str) -> SearchBuilder<'_> {
        SearchBuilder {
            fennec: self,
            query: Query { search: text.to_string(), columns: Vec::new(), ..Query::default() },
        }
    }

    /// Number of results of `text`.
    pub fn result_count(&mut self, text: &str) -> Result<u64, Error> {
        self.client.result_count(&fnc_ipc::SearchSpec { search: text.to_string(), mods: Mods::default() })
    }

    /// Total size of the files matching `text`.
    pub fn total_size(&mut self, text: &str) -> Result<u64, Error> {
        self.client.total_size(&fnc_ipc::SearchSpec { search: text.to_string(), mods: Mods::default() })
    }

    /// Versions.
    pub fn version(&mut self) -> Result<VersionInfo, Error> {
        self.client.version()
    }

    /// Whether the database is loaded.
    pub fn is_db_loaded(&mut self) -> Result<bool, Error> {
        self.client.is_db_loaded()
    }

    /// Index status.
    pub fn index_status(&mut self) -> Result<IndexStatus, Error> {
        self.client.index_status()
    }

    /// Rebuilds the index.
    pub fn rebuild(&mut self) -> Result<(), Error> {
        self.client.rebuild()
    }

    /// Rescans folder indexes.
    pub fn update(&mut self) -> Result<(), Error> {
        self.client.update()
    }

    /// Saves the database.
    pub fn save_db(&mut self) -> Result<(), Error> {
        self.client.save_db()
    }

    /// Exits Fennec.
    pub fn exit(&mut self) -> Result<(), Error> {
        self.client.exit()
    }

    /// Run count of a file.
    pub fn run_count(&mut self, path: &str) -> Result<u32, Error> {
        self.client.run_count(path)
    }

    /// Increments the run count of a file.
    pub fn inc_run_count(&mut self, path: &str) -> Result<u32, Error> {
        self.client.inc_run_count(path)
    }

    /// Sets the run count of a file.
    pub fn set_run_count(&mut self, path: &str, count: u32) -> Result<(), Error> {
        self.client.set_run_count(path, count)
    }

    /// Indexed volumes.
    pub fn volumes(&mut self) -> Result<Vec<VolumeEntry>, Error> {
        self.client.volumes()
    }

    /// Filters.
    pub fn filters(&mut self) -> Result<Vec<FilterEntry>, Error> {
        self.client.filters()
    }

    /// Bookmarks.
    pub fn bookmarks(&mut self) -> Result<Vec<BookmarkEntry>, Error> {
        self.client.bookmarks()
    }
}

/// Query builder returned by [`Fennec::search`].
pub struct SearchBuilder<'a> {
    fennec: &'a mut Fennec,
    query: Query,
}

impl SearchBuilder<'_> {
    /// Match case.
    pub fn case(mut self, on: bool) -> Self {
        self.query.mods.case = on;
        self
    }

    /// Match whole words.
    pub fn whole_word(mut self, on: bool) -> Self {
        self.query.mods.whole_word = on;
        self
    }

    /// Match the full path.
    pub fn match_path(mut self, on: bool) -> Self {
        self.query.mods.path = on;
        self
    }

    /// Treat the search as one regular expression.
    pub fn regex(mut self, on: bool) -> Self {
        self.query.mods.regex = on;
        self
    }

    /// Match diacritics.
    pub fn diacritics(mut self, on: bool) -> Self {
        self.query.mods.diacritics = on;
        self
    }

    /// All options at once.
    pub fn mods(mut self, mods: Mods) -> Self {
        self.query.mods = mods;
        self
    }

    /// Primary sort (replaces earlier sort keys).
    pub fn sort(mut self, column: Column, ascending: bool) -> Self {
        self.query.sort = vec![(column, ascending)];
        self
    }

    /// Adds a tie-breaking sort key (at most three keys in total).
    pub fn then_sort(mut self, column: Column, ascending: bool) -> Self {
        if self.query.sort.is_empty() {
            self.query.sort.push((Column::Name, true));
        }
        if self.query.sort.len() < fnc_ipc::proto::MAX_SORT_KEYS {
            self.query.sort.push((column, ascending));
        }
        self
    }

    /// First result.
    pub fn offset(mut self, offset: u64) -> Self {
        self.query.offset = offset;
        self
    }

    /// Most results (default: all, fetched page by page).
    pub fn limit(mut self, limit: u64) -> Self {
        self.query.limit = limit;
        self
    }

    /// Adds a column (default: name, path, full path, size, date modified, attributes).
    pub fn column(mut self, c: Column) -> Self {
        if !self.query.columns.contains(&c) {
            self.query.columns.push(c);
        }
        self
    }

    /// Sets the columns.
    pub fn columns(mut self, cols: &[Column]) -> Self {
        self.query.columns = cols.to_vec();
        self
    }

    /// Also return `*`-highlighted name/path/full path.
    pub fn highlight(mut self, on: bool) -> Self {
        self.query.highlight = on;
        self
    }

    /// Server-side time limit.
    pub fn timeout(mut self, t: Duration) -> Self {
        self.query.timeout_ms = u32::try_from(t.as_millis()).unwrap_or(u32::MAX);
        self
    }

    /// The query this builder sends.
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// Runs the query.
    pub fn run(mut self) -> Result<SearchResults, Error> {
        if self.query.columns.is_empty() {
            self.query.columns = vec![
                Column::Name,
                Column::Path,
                Column::FullPath,
                Column::Size,
                Column::DateModified,
                Column::Attributes,
            ];
        }
        let r = self.fennec.client.query_all(&self.query)?;
        let rows = (0..r.len()).map(|i| r.row(i)).collect();
        Ok(SearchResults { total: r.total, offset: r.offset, rows })
    }
}

/// Results of a search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResults {
    /// Number of results of the whole search.
    pub total: u64,
    /// Offset of the first row.
    pub offset: u64,
    rows: Vec<Row>,
}

impl SearchResults {
    /// Returned rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` if nothing was returned.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row `i`.
    pub fn get(&self, i: usize) -> Option<&Row> {
        self.rows.get(i)
    }

    /// Rows in order.
    pub fn iter(&self) -> std::slice::Iter<'_, Row> {
        self.rows.iter()
    }

    /// The rows.
    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }
}

impl<'a> IntoIterator for &'a SearchResults {
    type Item = &'a Row;
    type IntoIter = std::slice::Iter<'a, Row>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.iter()
    }
}
