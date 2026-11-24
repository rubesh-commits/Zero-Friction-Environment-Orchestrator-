fn main() {
    if let Err(err) = winorchestrator_mcp::run_stdio() {
        eprintln!("winorchestrator-mcp: {err}");
        std::process::exit(1);
    }
}
