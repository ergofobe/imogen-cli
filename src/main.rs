//! imogen — a command-line client and terminal browser for an imogen photo library.

mod auth;
mod cli;
mod commands;
mod config;
mod context;
mod dates;
mod media;
mod output;
mod tui;

use anyhow::Result;
use clap::{CommandFactory, Parser};

use crate::cli::{Cli, Command};
use crate::context::Context;
use crate::output::{Output, RED};

fn main() {
    restore_sigpipe();
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Could not start: {error}");
            std::process::exit(1);
        }
    };

    let out = Output::new(cli.global.json, cli.global.no_color, cli.global.quiet);
    if let Err(error) = runtime.block_on(run(&cli)) {
        report(&out, &error);
        std::process::exit(1);
    }
}

/// Rust ignores SIGPIPE so that a write to a closed pipe surfaces as an error rather than
/// killing the process. For a tool built to be piped — `imogen ls --ids | head` — that
/// turns an ordinary early exit into a panic, so the default is put back.
#[cfg(unix)]
fn restore_sigpipe() {
    // Safety: setting a signal disposition before any thread has started is sound, and
    // SIG_DFL is what every other command-line program runs with.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_sigpipe() {}

/// Failures are reported the same way the command would have answered, so a script running
/// with `--json` gets an error it can read rather than prose on stderr.
fn report(out: &Output, error: &anyhow::Error) {
    if out.is_json() {
        let _ = out.json(&serde_json::json!({
            "error": error.to_string(),
            "causes": error.chain().skip(1).map(|c| c.to_string()).collect::<Vec<_>>(),
        }));
        return;
    }
    eprintln!("{}", out.paint(&format!("error: {error}"), RED));
    for cause in error.chain().skip(1) {
        eprintln!("  {}", out.dim(&format!("caused by: {cause}")));
    }
}

async fn run(cli: &Cli) -> Result<()> {
    let global = &cli.global;

    // The commands that manage credentials build their own client, because they have to
    // work when there is no saved login at all.
    match &cli.command {
        Some(Command::Login(args)) => return commands::session::login(global, args).await,
        Some(Command::Logout { revoke }) => {
            return commands::session::logout(global, *revoke).await
        }
        Some(Command::Profiles(args)) => return commands::session::profiles(global, args),
        Some(Command::Completions { shell }) => {
            let mut command = Cli::command();
            let name = command.get_name().to_string();
            clap_complete::generate(*shell, &mut command, name, &mut std::io::stdout());
            return Ok(());
        }
        _ => {}
    }

    let ctx = Context::build(global)?;

    match &cli.command {
        None => tui::run(&ctx).await,
        Some(Command::Tui) => tui::run(&ctx).await,

        Some(Command::Whoami) => commands::account::whoami(&ctx).await,
        Some(Command::Status) => commands::account::status(&ctx).await,

        Some(Command::List(args)) => commands::assets::list(&ctx, args).await,
        Some(Command::Search(args)) => commands::assets::search(&ctx, args).await,
        Some(Command::Show(args)) => commands::assets::show(&ctx, args).await,
        Some(Command::Stats) => commands::assets::stats(&ctx).await,
        Some(Command::Timeline { after, before }) => {
            commands::assets::timeline(&ctx, after.as_deref(), before.as_deref()).await
        }

        Some(Command::Upload(args)) => commands::upload::upload(&ctx, args).await,
        Some(Command::Download(args)) => commands::download::download(&ctx, args).await,
        Some(Command::Edit(args)) => commands::assets::edit(&ctx, args).await,
        Some(Command::Trash(args)) => commands::assets::trash(&ctx, args).await,
        Some(Command::Restore(args)) => commands::assets::restore(&ctx, args).await,

        Some(Command::Album(command)) => commands::albums::run(&ctx, command).await,
        Some(Command::Share(command)) => commands::share::run(&ctx, command).await,
        Some(Command::People(command)) => commands::people::run(&ctx, command).await,
        Some(Command::Account(command)) => commands::account::run(&ctx, command).await,
        Some(Command::Admin(command)) => commands::admin::run(&ctx, command).await,

        // Handled above, before the client was built.
        Some(Command::Login(_))
        | Some(Command::Logout { .. })
        | Some(Command::Profiles(_))
        | Some(Command::Completions { .. }) => unreachable!(),
    }
}
