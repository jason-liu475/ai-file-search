fn main() {
    let result = ai_file_search_cli::run(std::env::args().skip(1));

    print!("{}", result.stdout);
    eprint!("{}", result.stderr);

    std::process::exit(result.exit_code);
}
