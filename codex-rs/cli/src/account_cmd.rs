use anyhow::Context;
use clap::Args;
use clap::Parser;
use codex_cli::login_with_chatgpt;
use codex_core::config::Config;
use codex_login::AccountProfile;
use codex_login::AccountStore;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_utils_cli::CliConfigOverrides;

#[path = "codex_plus_plus/claude_accounts.rs"]
mod claude_accounts;

#[derive(Debug, Parser)]
pub(crate) struct AccountCli {
    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    pub subcommand: AccountSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub(crate) enum AccountSubcommand {
    /// Log in with ChatGPT and save it as an account.
    Add,

    /// Import the current ChatGPT login as an account.
    ImportCurrent(ImportCurrentArgs),

    /// List imported accounts.
    List,

    /// Add or list Claude subscription accounts.
    Claude(claude_accounts::ClaudeAccountCli),
}

#[derive(Debug, Args)]
pub(crate) struct ImportCurrentArgs {
    pub label: Option<String>,
}

pub(crate) async fn run_account_command(account_cli: AccountCli) -> anyhow::Result<()> {
    let config = load_config(account_cli.config_overrides).await?;
    let store = AccountStore::new(config.codex_home.to_path_buf());

    match account_cli.subcommand {
        AccountSubcommand::Add => {
            if matches!(config.forced_login_method, Some(ForcedLoginMethod::Api)) {
                anyhow::bail!("ChatGPT login is disabled. Use API key login instead.");
            }

            login_with_chatgpt(
                config.codex_home.to_path_buf(),
                config.forced_chatgpt_workspace_id.clone(),
                config.cli_auth_credentials_store_mode,
                config.auth_keyring_backend_kind(),
                config.auth_route_config(),
            )
            .await
            .context("failed to log in")?;

            let store_mode = config.cli_auth_credentials_store_mode;
            let keyring_backend_kind = config.auth_keyring_backend_kind();
            let profile = tokio::task::spawn_blocking(move || {
                store.import_current(None, store_mode, keyring_backend_kind)
            })
            .await
            .context("failed to import account")?;
            let profile = profile.context("failed to import account")?;
            println!("Added account {} ({})", profile.id, profile.label);
        }
        AccountSubcommand::ImportCurrent(args) => {
            let store_mode = config.cli_auth_credentials_store_mode;
            let keyring_backend_kind = config.auth_keyring_backend_kind();
            let profile = tokio::task::spawn_blocking(move || {
                store.import_current(args.label, store_mode, keyring_backend_kind)
            })
            .await
            .context("failed to import current account")?;
            let profile = profile.context("failed to import current account")?;
            println!("Imported account {} ({})", profile.id, profile.label);
        }
        AccountSubcommand::List => {
            print_accounts(store.list().context("failed to list accounts")?);
        }
        AccountSubcommand::Claude(args) => claude_accounts::run(args, &config).await?,
    }
    Ok(())
}

async fn load_config(config_overrides: CliConfigOverrides) -> anyhow::Result<Config> {
    let overrides = config_overrides
        .parse_overrides()
        .map_err(|err| anyhow::anyhow!("error parsing -c overrides: {err}"))?;
    Config::load_with_cli_overrides(overrides)
        .await
        .context("error loading configuration")
}

fn print_accounts(accounts: Vec<AccountProfile>) {
    if accounts.is_empty() {
        println!("No accounts imported.");
        return;
    }

    println!("ID  Label  Enabled  Login required  Priority");
    for account in accounts {
        println!(
            "{}  {}  {}  {}  {}",
            account.id, account.label, account.enabled, account.login_required, account.priority
        );
    }
}
