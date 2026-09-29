use std::future::Future;
use std::io;
use std::process::Command;
use std::process::Stdio;

use clap::Args;
use codex_core::config::Config;
use codex_model_provider::ClaudeAccount;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;

const LOGIN_OUTPUT_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Args)]
pub(crate) struct ClaudeAccountCli {
    #[command(subcommand)]
    command: ClaudeAccountCommand,
}

#[derive(Debug, clap::Subcommand)]
enum ClaudeAccountCommand {
    /// Sign in to Claude in your browser. Press Ctrl+C to cancel.
    Add,
    /// List Claude accounts from the running owned backend; never starts it.
    List,
}

pub(super) async fn run(args: ClaudeAccountCli, config: &Config) -> anyhow::Result<()> {
    let result = match args.command {
        ClaudeAccountCommand::Add => {
            let command = codex_model_provider::prepare_cli_proxy_claude_login(
                config.codex_home.as_path(),
                config.http_client_factory(),
            ).await;
            match command {
                Ok(command) => {
                    eprintln!("Complete Claude sign-in in your browser. Press Ctrl+C to cancel.");
                    run_login(command, tokio::signal::ctrl_c(), |url| {
                        eprintln!("Open this link if your browser did not open:");
                        println!("authorization_url: {}", serde_json::to_string(url).expect("URL string"));
                    }).await.map(|()| println!("status: signed_in"))
                }
                Err(_) => Err(io::Error::other(
                    "Could not prepare Claude sign-in. Check CODEX_CLI_PROXY_EXECUTABLE or your network, then retry codex account claude add.",
                )),
            }
        }
        ClaudeAccountCommand::List => {
            codex_model_provider::list_cli_proxy_claude_accounts(
                config.codex_home.as_path(),
                config.http_client_factory(),
            ).await.map(|accounts| print!("{}", format_accounts(accounts.as_deref()))).map_err(|_| {
                io::Error::other("Could not verify Claude account status. Retry codex account claude list after checking the owned backend.")
            })
        }
    };
    if let Err(error) = result {
        println!(
            "error: {}",
            serde_json::to_string(&error.to_string()).expect("error string")
        );
        return Err(error.into());
    }
    Ok(())
}

fn format_accounts(accounts: Option<&[ClaudeAccount]>) -> String {
    let Some(accounts) = accounts else {
        return "status: not_running\nhelp: Start a session with codex -c model_provider=\"cli-proxy\", then run codex account claude list.\n".into();
    };
    let mut output = format!("accounts[{}]{{name,email,status}}:\n", accounts.len());
    for account in accounts {
        let status = if account.disabled {
            "disabled"
        } else if account.unavailable {
            "unavailable"
        } else {
            "enabled"
        };
        output.push_str(&format!(
            "  {},{},{}\n",
            serde_json::to_string(&account.name).expect("name string"),
            serde_json::to_string(&account.email).expect("email value"),
            status,
        ));
    }
    if accounts.is_empty() {
        output.push_str("help: Run codex account claude add to sign in.\n");
    }
    output
}

async fn run_login(
    command: Command,
    cancel: impl Future<Output = io::Result<()>>,
    on_authorization_url: impl FnMut(&str),
) -> io::Result<()> {
    let mut child = tokio::process::Command::from(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            io::Error::other("Could not start Claude sign-in. Retry codex account claude add.")
        })?;
    // v7.3.14 treats manual-prompt EOF as empty input and keeps waiting for the browser.
    let stdout = child.stdout.take().expect("piped login stdout");
    tokio::select! {
        result = async {
            let saved = match read_login_output(stdout, on_authorization_url).await {
                Ok(saved) => saved,
                Err(_) => {
                    child.kill().await?;
                    return Err(io::Error::other("Claude sign-in returned unexpected output. Retry codex account claude add."));
                }
            };
            let status = child.wait().await?;
            if status.code() == Some(13) {
                return Err(io::Error::other("Claude sign-in needs port 54545. Close the application using that port, then retry codex account claude add."));
            }
            if !status.success() || !saved {
                return Err(io::Error::other("Claude sign-in failed, was denied or timed out. Retry codex account claude add and complete sign-in in your browser."));
            }
            Ok(())
        } => result,
        _ = cancel => {
            child.kill().await?;
            Err(io::Error::new(io::ErrorKind::Interrupted, "Claude sign-in cancelled. Run codex account claude list to check account status before retrying."))
        }
    }
}

async fn read_login_output(
    stdout: impl AsyncRead + Unpin,
    mut on_authorization_url: impl FnMut(&str),
) -> io::Result<bool> {
    let mut reader = BufReader::new(stdout.take(LOGIN_OUTPUT_LIMIT + 1));
    let mut line = String::new();
    let mut bytes = 0;
    let mut saved = false;
    let mut next_is_url = false;
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            break;
        }
        bytes += read;
        if bytes as u64 > LOGIN_OUTPUT_LIMIT {
            return Err(io::Error::other(
                "Claude sign-in returned unexpected output. Retry codex account claude add.",
            ));
        }
        let text = line.trim_end();
        if next_is_url {
            if !authorization_url(text) {
                return Err(io::Error::other(
                    "Could not show a safe Claude sign-in link. Retry codex account claude add.",
                ));
            }
            on_authorization_url(text);
        }
        next_is_url = text == "Visit the following URL to continue authentication:";
        // v7.3.14 prints this exact marker only after Manager.Login persists the record.
        saved |= text == "Claude authentication successful!";
    }
    Ok(saved)
}

fn authorization_url(text: &str) -> bool {
    let Ok(url) = url::Url::parse(text) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("claude.ai")
        || url.port().is_some()
        || url.path() != "/oauth/authorize"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    query.len() == 8 && url.query_pairs().count() == 8
        && query.get("code").is_some_and(|value| value == "true")
        && query.get("client_id").is_some_and(|value| value == "9d1c250a-e61b-44d9-88ed-5944d1962f5e")
        && query.get("response_type").is_some_and(|value| value == "code")
        && query.get("redirect_uri").is_some_and(|value| value == "http://localhost:54545/callback")
        && query.get("code_challenge_method").is_some_and(|value| value == "S256")
        && query.get("code_challenge").is_some_and(|value| !value.is_empty())
        && query.get("state").is_some_and(|value| !value.is_empty())
        && query.get("scope").is_some_and(|value| value == "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload")
}

#[cfg(test)]
#[path = "claude_accounts_tests.rs"]
mod tests;
