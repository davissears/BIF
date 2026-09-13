fn main() -> std::process::ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next();
    let code = if command.as_deref() == Some(std::ffi::OsStr::new("rpc")) {
        bif::expose::run(
            arguments,
            &mut std::io::stdin(),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
    } else {
        bif::cli::run(
            command.into_iter().chain(arguments),
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        )
    };
    std::process::ExitCode::from(code as u8)
}
