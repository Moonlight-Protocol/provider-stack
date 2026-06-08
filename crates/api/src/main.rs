use std::process::ExitCode;

#[actix_web::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        return provider_stack_api::cli::run(&args[1]);
    }

    if let Err(e) = provider_stack_api::run_server().await {
        eprintln!("fatal: {e:?}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
