//! IFilter fallback of content extraction (SPEC ch.1 §9.2, `content_ifilter`; ADR-035).
//!
//! `fennec.exe -ifilter-host` is a hidden mode of this binary: it runs
//! [`fnc_platform::ifilter::host_main`] and never opens a window. The rest of the app uses
//! [`provider`]: one shared [`IFilterClient`] (hosts start lazily on the first request,
//! so the feature costs nothing until a file without built-in text support is read),
//! plugged into `fnc_props::content::ContentExtractor::fallback` by
//! `stores::on_demand` — for the content index and for `content:` on unindexed files.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use fnc_platform::ifilter::{ClientOptions, HOST_ARG, HostCommand, HostOptions, IFilterClient};
use fnc_props::content::ContentProvider;

/// Deadline of one file in the host (Office filters can take a few seconds on large
/// documents; a hung filter is killed after this).
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// `true` if the command line (without the program name) selects the host mode.
pub fn is_host_invocation(args: &[String]) -> bool {
    args.first().is_some_and(|a| a.eq_ignore_ascii_case(HOST_ARG))
}

/// Runs the IFilter host on stdin/stdout and exits the process.
pub fn run_host() -> ! {
    std::process::exit(fnc_platform::ifilter::host_main(HostOptions::default()))
}

/// [`ContentProvider`] over the IFilter host processes.
pub struct IFilterContent(pub IFilterClient);

impl ContentProvider for IFilterContent {
    fn handles(&self, ext: &str) -> bool {
        self.0.has_filter(ext)
    }

    fn text(&self, path: &Path, _ext: &str, max_bytes: usize) -> Option<String> {
        match self.0.extract(path, max_bytes) {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "no IFilter text");
                None
            }
        }
    }
}

/// The shared IFilter provider (`None` off Windows, or if the executable path is
/// unknown).
pub fn provider() -> Option<Arc<dyn ContentProvider>> {
    static PROVIDER: OnceLock<Option<Arc<dyn ContentProvider>>> = OnceLock::new();
    if !cfg!(windows) {
        return None;
    }
    PROVIDER
        .get_or_init(|| match HostCommand::current_exe() {
            Ok(cmd) => {
                let client = IFilterClient::new(cmd, ClientOptions { timeout: TIMEOUT, ..ClientOptions::default() });
                Some(Arc::new(IFilterContent(client)) as Arc<dyn ContentProvider>)
            }
            Err(e) => {
                tracing::warn!(error = %e, "IFilter fallback unavailable: no executable path");
                None
            }
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_flag() {
        assert!(is_host_invocation(&["-ifilter-host".into()]));
        assert!(is_host_invocation(&["-IFilter-Host".into(), "x".into()]));
        assert!(!is_host_invocation(&["-s".into(), "-ifilter-host".into()]));
        assert!(!is_host_invocation(&[]));
        assert_eq!(provider().is_some(), cfg!(windows));
    }

    /// `content:` finds text that only the IFilter provides: an RTF memo through the
    /// search engine with the app's on-demand source, with and without the fallback.
    /// The host is this test binary (`host_entry`), or `FENNEC_EXE` (`fennec.exe
    /// -ifilter-host`) when set.
    #[cfg(windows)]
    #[test]
    fn content_search_reads_rtf_through_the_ifilter_host() {
        use fnc_exec::{Caches, CancelToken, ExecCtx, ExtCtx, SlowPolicy, execute_ext};
        use fnc_props::content::ContentExtractor;
        use fnc_props::ondemand::{DefaultOnDemand, OnDemand};
        use fnc_query::{ParseEnv, PlanEnv, parse, plan};

        let dir = tempfile::tempdir().unwrap();
        let rtf = dir.path().join("memo.rtf");
        std::fs::write(&rtf, br"{\rtf1\ansi\deff0{\fonttbl{\f0 Arial;}}\f0\pard Quarterly {\b zebra} plan\par}")
            .unwrap();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, "nothing here").unwrap();
        let mut efu = String::from("Filename,Size,Date Modified,Date Created,Attributes\n");
        for p in [&rtf, &txt] {
            efu.push_str(&format!("\"{}\",{},5,,32\n", p.display(), std::fs::metadata(p).unwrap().len()));
        }
        let mut s = fnc_index::IndexStore::default();
        fnc_index::efu::load_efu(&mut s, efu.as_bytes(), "t.efu").unwrap();
        s.rebuild_name_order();

        let cmd = match std::env::var_os("FENNEC_EXE") {
            Some(exe) => HostCommand { program: exe.into(), args: vec![HOST_ARG.into()], envs: Vec::new() },
            None => HostCommand {
                program: std::env::current_exe().unwrap(),
                args: ["--exact", "ifilter::tests::host_entry", "--ignored", "--quiet", "--test-threads=1"]
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                envs: vec![("FENNEC_IFILTER_APP_HOST".into(), "1".into())],
            },
        };
        let client = IFilterClient::new(cmd, ClientOptions { timeout: TIMEOUT, hosts: 1, ..ClientOptions::default() });
        if !client.has_filter("rtf") {
            eprintln!("no RTF IFilter registered; skipped");
            return;
        }
        let probe = client.request(fnc_platform::ifilter::Kind::Extract, 1 << 20, &rtf);
        assert!(matches!(&probe, Ok(fnc_platform::ifilter::Reply::Text(t)) if t.contains("zebra")), "{probe:?}");
        let search = |fallback: Option<Arc<dyn ContentProvider>>, q: &str| -> Vec<String> {
            let od: Arc<dyn OnDemand> = Arc::new(DefaultOnDemand {
                content: ContentExtractor { fallback, ..ContentExtractor::default() },
                ..DefaultOnDemand::default()
            });
            let planned = plan(&parse(q, &ParseEnv::default()).ast, &PlanEnv::default(), None);
            let ext = ExtCtx { on_demand: Some(&od), policy: SlowPolicy::default(), ..ExtCtx::default() };
            let (caches, cancel) = (Caches::new(), CancelToken::new());
            let rs =
                execute_ext(&planned, &s, &ExecCtx { cancel: &cancel, prev: None, caches: &caches }, &ext).unwrap();
            rs.ids.iter().map(|&i| s.name_string(i)).collect()
        };
        let provider: Arc<dyn ContentProvider> = Arc::new(IFilterContent(client));
        // The raw RTF has `{\b zebra} plan`: the phrase exists only in the filtered text.
        assert_eq!(search(Some(provider.clone()), "content:\"zebra plan\""), ["memo.rtf"]);
        assert!(search(None, "content:\"zebra plan\"").is_empty(), "built-in parsers alone do not see it");
        // `textplain:` never uses IFilters.
        assert!(search(Some(provider), "textplain:\"zebra plan\"").is_empty());
    }

    /// Not a test: the host process of `content_search_reads_rtf_through_the_ifilter_host`.
    #[test]
    #[ignore = "host process of the IFilter tests"]
    fn host_entry() {
        if std::env::var_os("FENNEC_IFILTER_APP_HOST").is_some() {
            std::process::exit(fnc_platform::ifilter::host_main(HostOptions::default()));
        }
    }
}
