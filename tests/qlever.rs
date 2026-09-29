//! End-to-end test against `QLever`.
//!
//! Builds a `QLever` index of `tests/data/small.ttl`, starts a server and runs
//! generated queries against it, with the engine itself as the source of
//! values. The binaries are taken from `$QLEVER_BIN_DIR`, or from
//! `../qlever/cmake-build-release` relative to this crate; the test is
//! skipped if they are not found.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sparqlright::Generator;
use sparqlright::engine::{ACCEPT_RESULTS, HttpEndpoint, SparqlService};
use sparqlright::fuzz::{FuzzConfig, Fuzzer};
use sparqlright::provider::{EngineProvider, EngineProviderConfig, MixProvider};

/// A running `QLever` server, killed on drop.
struct Server {
    child: Child,
    url: String,
    dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn endpoint(&self) -> HttpEndpoint {
        // QLever cancels queries after `timeout`; the client waits a bit longer.
        HttpEndpoint::new(self.url.clone(), Duration::from_secs(15)).with_param("timeout", "10s")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("server.log")).unwrap_or_default()
    }
}

fn qlever_bin_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("QLEVER_BIN_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../qlever/cmake-build-release"),
        PathBuf::from,
    );
    (dir.join("qlever-index").is_file() && dir.join("qlever-server").is_file()).then_some(dir)
}

fn start_qlever() -> Option<Server> {
    let bin = qlever_bin_dir()?;
    let dir = tempfile::tempdir().expect("temporary directory");
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/small.ttl");
    let index = Command::new(bin.join("qlever-index"))
        .current_dir(dir.path())
        .args(["-i", "small", "-F", "ttl", "-f"])
        .arg(&data)
        .output()
        .expect("qlever-index runs");
    assert!(
        index.status.success(),
        "qlever-index failed:\n{}",
        String::from_utf8_lossy(&index.stderr)
    );

    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let log = std::fs::File::create(dir.path().join("server.log")).expect("log file");
    let child = Command::new(bin.join("qlever-server"))
        .current_dir(dir.path())
        .args([
            "-i",
            "small",
            "-p",
            &port.to_string(),
            "-n",
            "--no-metrics-log",
            "--no-resource-usage-log",
        ])
        .stdout(log.try_clone().expect("log file"))
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .expect("qlever-server starts");
    let server = Server {
        child,
        url: format!("http://127.0.0.1:{port}"),
        dir,
    };
    let endpoint = server.endpoint();
    let start = Instant::now();
    while !endpoint
        .query("ASK {}", ACCEPT_RESULTS)
        .is_ok_and(|r| r.is_success())
    {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "QLever did not start:\n{}",
            server.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    Some(server)
}

#[test]
fn fuzz_qlever_end_to_end() {
    let Some(server) = start_qlever() else {
        eprintln!("skipping: QLever binaries not found (set QLEVER_BIN_DIR)");
        return;
    };
    let generator = Generator::default();
    let mut provider = MixProvider::variables_and(EngineProvider::new(
        server.endpoint(),
        EngineProviderConfig::default(),
    ));
    let findings = server.dir.path().join("findings");
    let config = FuzzConfig {
        seed: 0,
        count: 200,
        output: Some(findings.clone()),
        ..FuzzConfig::default()
    };
    let fuzzer = Fuzzer::new(&generator, server.endpoint(), config);
    let mut interesting = Vec::new();
    let report = fuzzer
        .run(&mut provider, |test| {
            if test.outcome.is_finding(true) {
                interesting.push(format!(
                    "seed {} {}: {:?}",
                    test.seed,
                    test.outcome.label(),
                    test.outcome
                ));
            }
        })
        .expect("findings can be written");
    eprintln!("{report}");
    for line in &interesting {
        eprintln!("{}", &line[..line.len().min(300)]);
    }

    let count = |label: &str| report.outcomes.get(label).copied().unwrap_or(0);
    let crashes = count("crash") + count("connection") + count("timeout:blocked");
    assert_eq!(
        crashes,
        0,
        "QLever stopped answering; see the findings above\n{}",
        server.log()
    );
    assert_eq!(
        report.validator_rejections, 0,
        "the generator produced invalid requests"
    );
    assert_eq!(
        report.generation.provider_errors, 0,
        "the engine provider failed"
    );
    // The harness works: most queries run, and many return results because
    // the values come from the data.
    let ok = count("ok") + count("ok:empty");
    assert!(
        ok * 10 >= report.tests * 7,
        "only {ok} of {} queries succeeded",
        report.tests
    );
    assert!(
        count("ok") * 10 >= report.tests * 3,
        "only {} queries returned results",
        count("ok")
    );
    // Every finding was written to disk.
    assert!(report.findings.iter().all(|seed| {
        std::fs::read_dir(&findings)
            .expect("findings directory")
            .any(|e| {
                e.is_ok_and(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{seed}-"))
                })
            })
    }));
}
