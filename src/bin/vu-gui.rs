//! vu-gui — the vu sound meter in a window; the same as `vu --gui`.

use clap::{CommandFactory, FromArgMatches};
use vu::cli::Cli;

fn main() {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let res = if cli.list_devices {
        vu::source::list_devices()
    } else {
        let explicit = vu::gui::explicit_args(&matches);
        cli.validate().and_then(|_| vu::gui::run(cli, explicit))
    };
    if let Err(e) = res {
        eprintln!("vu: {e:#}");
        std::process::exit(1);
    }
}
