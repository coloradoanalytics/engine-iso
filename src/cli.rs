//! The engine-iso command line, shared by every build of the program.
use clap::{CommandFactory, FromArgMatches, Parser};
use std::{io::IsTerminal, sync::Arc};

/// engine-iso: the Acalc ISO 9613-2:2024 calculation engine.
///
/// acalc-engine starts and controls it over standard input and output with
/// the engine protocol (acalc-engine docs/ENGINE_PROTOCOL.md); it has no
/// network interface. Clients, keys and farms are acalc-engine's.
#[derive(Parser, Debug)]
#[command(name = "engine-iso", about)]
struct Args {
    /// CPU workers; default uses every processor available to this process.
    #[arg(long)]
    threads: Option<usize>,
    /// Prepared farm plans kept for running work items.
    #[arg(long, default_value_t = 4)]
    plan_cache: usize,
    /// Print the engine's identity (its `hello` reply) and exit.
    #[arg(long)]
    describe: bool,
    /// Print the capability document and exit.
    #[arg(long)]
    capabilities: bool,
}

/// Runs the program with the process arguments. A build that installs
/// kernels (`crate::kernels::install`) or sets its version
/// (`crate::engine::set_version`) does so before calling this.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let matches = Args::command()
        .version(crate::engine::version())
        .get_matches();
    let args = Args::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let threads = match args.threads {
        Some(0) => return Err("--threads must be positive".into()),
        Some(n) => n,
        None => std::thread::available_parallelism()?.get(),
    };
    if args.describe {
        println!("{:#}", crate::engine::hello(threads));
        return Ok(());
    }
    if args.capabilities {
        println!("{:#}", crate::capabilities());
        return Ok(());
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!(
            "engine-iso is started by acalc-engine and reads engine protocol frames on \
             standard input; see --help."
        );
    }
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("iso-cpu-{i}"))
            .build()?,
    );
    eprintln!(
        "engine-iso {} ({}) using {threads} CPU workers, {} kernels",
        crate::engine::version(),
        crate::capabilities::build_revision(),
        crate::kernels::name()
    );
    let output = std::io::BufWriter::with_capacity(1 << 16, std::io::stdout());
    crate::engine::serve(pool, args.plan_cache, stdin.lock(), output)?;
    Ok(())
}
