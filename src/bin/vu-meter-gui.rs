//! vu-meter-gui — the sound meter in a window; the same as `vu-meter --gui`.

use clap::{CommandFactory, FromArgMatches};
use vu::cli::Cli;

fn main() {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let res = if cli.list_devices {
        vu::source::list_devices()
    } else {
        let explicit = vu::gui::explicit_args(&matches);
        cli.validate().and_then(|_| {
            if cli.foreground {
                vu::gui::run(cli, explicit)
            } else {
                vu::gui::detach()
            }
        })
    };
    if let Err(e) = res {
        eprintln!("vu-meter: {e:#}");
        std::process::exit(1);
    }
}
