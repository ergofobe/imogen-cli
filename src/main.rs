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

use std::collections::BTreeMap;

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
    let details = api_details(error);
    if out.is_json() {
        let _ = out.json(&error_json(error, details));
        return;
    }
    eprintln!("{}", out.paint(&format!("error: {error}"), RED));
    for line in details.map(detail_lines).unwrap_or_default() {
        eprintln!("  {line}");
    }
    for cause in error.chain().skip(1) {
        eprintln!("  {}", out.dim(&format!("caused by: {cause}")));
    }
}

/// The `path -> messages` map the server sent with a rejection, looked for all the way
/// down the chain: a command that added its own context leaves the SDK error underneath it.
fn api_details(error: &anyhow::Error) -> Option<&BTreeMap<String, Vec<String>>> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<imogen_sdk::Error>())
        .and_then(imogen_sdk::Error::details)
}

/// One line per complaint rather than per field, so a field the server faults twice reads
/// as two statements instead of one run-on.
fn detail_lines(details: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    details
        .iter()
        .flat_map(|(path, messages)| {
            messages
                .iter()
                .map(move |message| format!("{path}: {message}"))
        })
        .collect()
}

/// The key is absent rather than empty when the server named no fields, because `details`
/// here is the API's own optional map and not something this program invented.
fn error_json(
    error: &anyhow::Error,
    details: Option<&BTreeMap<String, Vec<String>>>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "error": error.to_string(),
        "causes": error.chain().skip(1).map(|c| c.to_string()).collect::<Vec<_>>(),
    });
    if let Some(details) = details {
        value["details"] = serde_json::json!(details);
    }
    value
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected_by_the_server() -> imogen_sdk::Error {
        let mut details = BTreeMap::new();
        details.insert("assetIds.3".to_string(), vec!["Invalid UUID".to_string()]);
        details.insert(
            "limit".to_string(),
            vec!["Too large".to_string(), "Must be an integer".to_string()],
        );
        imogen_sdk::Error::Api {
            status: 400,
            code: "validation_failed".to_string(),
            message: "The request did not match what this endpoint expects".to_string(),
            details: Some(details),
        }
    }

    #[test]
    fn a_validation_failure_names_every_field_it_rejected() {
        let error = anyhow::Error::new(rejected_by_the_server());
        let details = api_details(&error).expect("the server named the fields");
        assert_eq!(
            detail_lines(details),
            vec![
                "assetIds.3: Invalid UUID",
                "limit: Too large",
                "limit: Must be an integer",
            ]
        );
    }

    #[test]
    fn json_carries_the_map_the_server_sent_unrenamed() {
        let error = anyhow::Error::new(rejected_by_the_server());
        let value = error_json(&error, api_details(&error));
        assert_eq!(
            value["error"],
            "The request did not match what this endpoint expects (400 validation_failed)"
        );
        assert_eq!(value["details"]["assetIds.3"][0], "Invalid UUID");
        assert_eq!(value["details"]["limit"][1], "Must be an integer");
    }

    /// A command that adds its own context leaves the SDK error further down the chain,
    /// so the map has to be looked for there rather than only at the top.
    #[test]
    fn the_map_survives_a_commands_own_context() {
        let error =
            anyhow::Error::new(rejected_by_the_server()).context("could not empty the trash");
        assert!(api_details(&error).is_some());
    }

    #[test]
    fn a_failure_the_server_did_not_describe_gains_no_details_key() {
        let error = anyhow::anyhow!("the request never got an answer");
        let value = error_json(&error, api_details(&error));
        assert!(value.get("details").is_none());
        assert_eq!(value["causes"], serde_json::json!([]));
    }
}
