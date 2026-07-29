use std::process::ExitCode;

use clap::Parser;

use ai_quotas::app;
use ai_quotas::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    match app::run(Cli::parse()).await {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}
