//! Command line interface of sparqlright.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use sparqlright::engine::HttpEndpoint;
use sparqlright::fuzz::{FuzzConfig, Fuzzer};
use sparqlright::provider::{
    CompletionProvider, EngineProvider, EngineProviderConfig, FixedProvider, GraphProvider,
    MixProvider,
};
use sparqlright::validate::{validate_query, validate_update};
use sparqlright::{Generator, GeneratorConfig};

/// A random SPARQL 1.1 query generator for testing SPARQL engines.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print generated queries (or updates).
    Generate(GenerateArgs),
    /// Run generated queries against an engine and report findings.
    Fuzz(FuzzArgs),
    /// Print the default generator configuration as TOML.
    Config,
}

#[derive(Debug, Args)]
struct CommonArgs {
    /// Seed of the first query; query i uses seed + i.
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Number of queries.
    #[arg(long, short = 'n', default_value_t = 10)]
    count: u64,
    /// Generator configuration (TOML); see `sparqlright config`.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Enable swarm testing (disable random features per query).
    #[arg(long)]
    swarm: bool,
    /// Where values come from: `engine` (the --endpoint), `data:FILE`
    /// (an RDF file), or `fixed` (a built-in vocabulary).
    #[arg(long)]
    provider: Option<String>,
    /// SPARQL endpoint URL.
    #[arg(long, short = 'e')]
    endpoint: Option<String>,
    /// Additional form parameter sent with every request, e.g. `timeout=5s`.
    #[arg(long = "param", value_name = "NAME=VALUE")]
    params: Vec<String>,
    /// Request timeout in seconds.
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    /// Timeout in seconds for the queries of the engine provider. They only
    /// propose values, so they should fail fast; after repeated failures the
    /// provider temporarily falls back to cheap, cached queries.
    #[arg(long, default_value_t = 5)]
    provider_timeout: u64,
}

#[derive(Debug, Args)]
struct GenerateArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Generate update requests instead of queries.
    #[arg(long)]
    update: bool,
    /// Validate each request with spargebra and report rejections.
    #[arg(long)]
    check: bool,
    /// Print generation statistics (production counts) at the end.
    #[arg(long)]
    stats: bool,
}

#[derive(Debug, Args)]
struct FuzzArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Fraction of requests that are updates (they modify the data!).
    #[arg(long, default_value_t = 0.0)]
    update_ratio: f64,
    /// Directory for findings.
    #[arg(long, short = 'o', default_value = "findings")]
    output: PathBuf,
    /// Do not validate requests with spargebra.
    #[arg(long)]
    no_validate: bool,
    /// Keep going after the engine stopped answering.
    #[arg(long)]
    keep_going: bool,
    /// Print one line per test instead of progress symbols.
    #[arg(long, short = 'v')]
    verbose: bool,
}

impl CommonArgs {
    fn generator(&self) -> Result<Generator> {
        let mut config = match &self.config {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading {}", path.display()))?;
                GeneratorConfig::from_toml(&text)
                    .with_context(|| format!("parsing {}", path.display()))?
            }
            None => GeneratorConfig::default(),
        };
        config.swarm.enabled |= self.swarm;
        Ok(Generator::new(config))
    }

    fn endpoint(&self) -> Result<Option<HttpEndpoint>> {
        self.endpoint_with_timeout(self.timeout)
    }

    fn endpoint_with_timeout(&self, timeout: u64) -> Result<Option<HttpEndpoint>> {
        let Some(url) = &self.endpoint else {
            return Ok(None);
        };
        let mut endpoint = HttpEndpoint::new(url.clone(), Duration::from_secs(timeout));
        for p in &self.params {
            let Some((name, value)) = p.split_once('=') else {
                bail!("--param expects NAME=VALUE, got {p:?}");
            };
            endpoint = endpoint.with_param(name, value);
        }
        Ok(Some(endpoint))
    }

    fn provider(&self) -> Result<MixProvider> {
        let spec = match (&self.provider, &self.endpoint) {
            (Some(p), _) => p.as_str(),
            (None, Some(_)) => "engine",
            (None, None) => "fixed",
        };
        let terms: Box<dyn CompletionProvider> = if spec == "engine" {
            let endpoint = self
                .endpoint_with_timeout(self.provider_timeout)?
                .context("--provider engine requires --endpoint")?;
            Box::new(EngineProvider::new(
                endpoint,
                EngineProviderConfig::default(),
            ))
        } else if let Some(path) = spec.strip_prefix("data:") {
            Box::new(GraphProvider::from_file(path)?)
        } else if spec == "fixed" {
            Box::new(FixedProvider::example())
        } else {
            bail!("unknown provider {spec:?}; expected engine, data:FILE or fixed");
        };
        Ok(MixProvider::variables_and(terms))
    }
}

fn generate(args: &GenerateArgs) -> Result<()> {
    let generator = args.common.generator()?;
    let mut provider = args.common.provider()?;
    let mut stats = sparqlright::generator::GenerationStats::default();
    let mut rejected = 0;
    let mut out = std::io::stdout().lock();
    for i in 0..args.common.count {
        let seed = args.common.seed.wrapping_add(i);
        let (text, s, error) = if args.update {
            let u = generator.update(seed, &mut provider);
            let e = args
                .check
                .then(|| validate_update(&u.text).error().map(str::to_owned))
                .flatten();
            (u.text, u.stats, e)
        } else {
            let q = generator.query(seed, &mut provider);
            let e = args
                .check
                .then(|| validate_query(&q.text).error().map(str::to_owned))
                .flatten();
            (q.text, q.stats, e)
        };
        stats.merge(&s);
        writeln!(out, "# seed {seed}")?;
        if let Some(e) = error {
            rejected += 1;
            writeln!(out, "# rejected by spargebra: {e}")?;
        }
        writeln!(out, "{text}\n")?;
    }
    if args.check {
        eprintln!(
            "{rejected} of {} requests rejected by spargebra",
            args.common.count
        );
    }
    if args.stats {
        eprintln!("{}", serde_json::to_string_pretty(&stats)?);
    }
    Ok(())
}

fn fuzz(args: &FuzzArgs) -> Result<()> {
    let generator = args.common.generator()?;
    let endpoint = args
        .common
        .endpoint()?
        .context("fuzz requires --endpoint")?;
    let mut provider = args.common.provider()?;
    let config = FuzzConfig {
        seed: args.common.seed,
        count: args.common.count,
        update_ratio: args.update_ratio,
        validate: !args.no_validate,
        output: Some(args.output.clone()),
        stop_on_crash: !args.keep_going,
        ..FuzzConfig::default()
    };
    let fuzzer = Fuzzer::new(&generator, endpoint, config);
    if !fuzzer.is_alive() {
        bail!("the endpoint does not answer `ASK {{}}`");
    }
    let progress = !args.verbose && std::io::stderr().is_terminal();
    let report = fuzzer.run(&mut provider, |test| {
        if args.verbose {
            eprintln!(
                "seed {} {} {:?}",
                test.seed,
                test.outcome.label(),
                test.elapsed
            );
        } else if progress {
            eprint!("{}", test.outcome.symbol());
        }
    })?;
    if progress {
        eprintln!();
    }
    println!("{report}");
    std::fs::write(
        args.output.join("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Generate(args) => generate(args),
        Command::Fuzz(args) => fuzz(args),
        Command::Config => {
            print!("{}", GeneratorConfig::default().to_toml()?);
            Ok(())
        }
    }
}
