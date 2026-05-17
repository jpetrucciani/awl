use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match awl_cli::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}
