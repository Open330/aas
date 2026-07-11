//! `aas` CLI entry point. Command surface mirrors asx (see `docs/PARITY_SPEC.md` §A).
//! Includes storage, account management, same-provider exec, and the cross-provider proxy.

mod exec;
mod export;
mod login;
mod render;
mod ui;

use aas_core::model::ProfileType;
use aas_core::naming::{normalize_provider, normalize_provider_key};
use aas_core::secure_store;
use aas_core::share::{describe_share, resolve_share_selection, ShareOpts};
use aas_core::store::AccountStore;
use aas_providers::{all_providers, get_adapter, Provider};
use clap::builder::styling::{AnsiColor, Styles};
use clap::{Args, Parser, Subcommand};
use futures_util::future::join_all;
use render::UsageRow;
use std::collections::HashMap;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Cyan.on_default().bold())
    .usage(AnsiColor::Cyan.on_default().bold())
    .literal(AnsiColor::Green.on_default())
    .placeholder(AnsiColor::BrightBlack.on_default());

const EXAMPLES: &str = "\
Examples:
  aas usage                live usage for every account (fetched in parallel)
  aas login claude work    add a Claude account as an isolated profile
  aas switch codex work    make a stored account active
  aas e work               run the agent under a profile
  aas e work claude        cross-provider: run Claude's UI on this backend

Reads existing asx state directly — no migration needed.";

#[derive(Parser)]
#[command(
    name = "aas",
    version,
    about = "Agent Account Switcher — multi-account switcher for LLM coding agents",
    styles = STYLES,
    after_help = EXAMPLES,
    propagate_version = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Default, Clone)]
struct ShareArgs {
    #[arg(long)]
    isolated: bool,
    #[arg(long)]
    shared: bool,
    #[arg(long)]
    share: Option<String>,
    #[arg(long)]
    isolate: Option<String>,
}

impl ShareArgs {
    fn to_opts(&self) -> ShareOpts {
        ShareOpts {
            isolated: self.isolated,
            shared: self.shared,
            share: self.share.clone(),
            isolate: self.isolate.clone(),
        }
    }
    fn any(&self) -> bool {
        self.isolated || self.shared || self.share.is_some() || self.isolate.is_some()
    }
}

#[derive(Subcommand)]
enum Command {
    /// List accounts per provider (or all). `-u` live usage table, `-d` dump credentials.
    #[command(visible_alias = "ls")]
    List {
        provider: Option<String>,
        #[arg(short, long)]
        usage: bool,
        #[arg(short, long)]
        debug: bool,
    },
    /// Live usage table for every account (shorthand for `list -u`).
    #[command(visible_alias = "u")]
    Usage {
        provider: Option<String>,
        /// Emit machine-readable JSON (for aas-bar and other integrations).
        #[arg(long)]
        json: bool,
    },
    /// Show asx-tracked active account(s).
    Status { provider: Option<String> },
    /// Print shell env for a profile (`eval "$(aas export <name>)"`), or `--all` for a
    /// portable credential bundle to migrate every account to another host.
    Export {
        /// Globally unique account name, or provider when followed by `account`.
        name: Option<String>,
        /// Optional account name for the `export <provider> <account>` form.
        account: Option<String>,
        /// Export every account + credential as a portable bundle (for `aas import`).
        #[arg(short = 'a', long)]
        all: bool,
        /// Encrypt the portable bundle with an age/scrypt passphrase.
        #[arg(long, visible_alias = "encrypted")]
        vault: bool,
        #[arg(long, value_enum, default_value_t = export::Shell::Posix)]
        shell: export::Shell,
        /// Write the bundle to a file (0600) instead of stdout.
        #[arg(short = 'o', long)]
        out: Option<std::path::PathBuf>,
    },
    /// Adopt existing asx state, or restore a JSON/encrypted vault bundle (file, or `-` for stdin).
    Import {
        /// Bundle path; encrypted age vaults are detected automatically.
        file: Option<String>,
    },
    /// Snapshot the live credential as a system profile.
    Load {
        provider: Option<String>,
        name: Option<String>,
        #[command(flatten)]
        share: ShareArgs,
    },
    /// Login and store a new isolated profile.
    Login {
        provider: Option<String>,
        name: Option<String>,
        #[arg(long)]
        long_lived: bool,
        /// Headless / no browser: use the provider's device-code flow (codex).
        #[arg(long, visible_alias = "headless")]
        device_auth: bool,
        #[command(flatten)]
        share: ShareArgs,
    },
    /// Switch the active credential.
    #[command(visible_alias = "s")]
    Switch {
        provider: String,
        name: Option<String>,
    },
    /// Rename an account.
    Rename { from: String, to: String },
    /// Remove a stored account.
    #[command(visible_alias = "rm")]
    Remove { args: Vec<String> },
    /// Show/change profile sharing.
    Sharing {
        name: String,
        #[command(flatten)]
        share: ShareArgs,
    },
    /// Refresh (rotate) a stored credential.
    Refresh {
        provider: String,
        name: Option<String>,
        #[arg(long = "no-login")]
        no_login: bool,
    },
    /// Run the native CLI under a profile. `<target>` ≠ provider → cross-provider via proxy.
    #[command(visible_alias = "e")]
    Exec {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Standalone cross-provider proxy for `<name>`'s backend, targeting a `<frontend>` agent.
    Proxy { name: String, frontend: String },
}

#[tokio::main]
async fn main() {
    // Behave like a normal Unix CLI: exit quietly on a broken pipe (e.g. `aas export … | head`)
    // instead of panicking when a downstream reader goes away.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let store = AccountStore::open_default();
    let cli = match parse_cli_with_default_exec(&store) {
        Ok(cli) => cli,
        Err(error) => {
            ui::error(error);
            std::process::exit(1);
        }
    };
    let result = match cli.command {
        Command::List {
            provider,
            usage,
            debug,
        } => cmd_list(&store, provider.as_deref(), usage, debug).await,
        Command::Usage { provider, json } => {
            if json {
                cmd_usage_json(&store, provider.as_deref()).await
            } else {
                cmd_list(&store, provider.as_deref(), true, false).await
            }
        }
        Command::Status { provider } => cmd_status(&store, provider.as_deref()),
        Command::Export {
            name,
            account,
            all,
            vault,
            shell,
            out,
        } => export::run(&store, name, account, all, vault, shell, out),
        Command::Import { file } => cmd_import(file.as_deref()),
        Command::Load {
            provider,
            name,
            share,
        } => cmd_load(&store, provider, name, &share).await,
        Command::Login {
            provider,
            name,
            long_lived,
            device_auth,
            share,
        } => cmd_login(&store, provider, name, long_lived, device_auth, &share).await,
        Command::Switch { provider, name } => cmd_switch(&store, &provider, name.as_deref()).await,
        Command::Rename { from, to } => cmd_rename(&store, &from, &to),
        Command::Remove { args } => cmd_remove(&store, &args),
        Command::Sharing { name, share } => cmd_sharing(&store, &name, &share),
        Command::Refresh {
            provider,
            name,
            no_login,
        } => cmd_refresh(&store, &provider, name.as_deref(), no_login).await,
        Command::Exec { name, rest } => exec::cmd_exec(&store, &name, &rest).await,
        Command::Proxy { name, frontend } => exec::cmd_proxy(&store, &name, &frontend).await,
    };
    if let Err(e) = result {
        ui::error(e);
        std::process::exit(1);
    }
}

// ---- helpers ----

fn parse_cli_with_default_exec(store: &AccountStore) -> anyhow::Result<Cli> {
    let args = rewrite_default_exec_args(store, std::env::args_os().collect())?;
    Ok(Cli::parse_from(args))
}

fn rewrite_default_exec_args(
    store: &AccountStore,
    mut args: Vec<std::ffi::OsString>,
) -> anyhow::Result<Vec<std::ffi::OsString>> {
    let first = args
        .get(1)
        .and_then(|value| value.to_str())
        .filter(|value| !value.starts_with('-'));
    const COMMANDS: &[&str] = &[
        "list", "ls", "usage", "u", "status", "export", "import", "load", "login", "switch", "s",
        "rename", "remove", "rm", "sharing", "refresh", "exec", "e", "proxy", "help",
    ];
    if let Some(candidate) = first {
        if !COMMANDS.contains(&candidate) && store.get_by_name(candidate)?.is_some() {
            args.insert(1, "exec".into());
        }
    }
    Ok(args)
}

fn resolve_provider_name(
    store: &AccountStore,
    a: &str,
    b: Option<&str>,
) -> anyhow::Result<(String, String)> {
    if let Some(b) = b {
        let prov = normalize_provider(a).unwrap_or_else(|| a.to_lowercase());
        return Ok((prov, b.to_string()));
    }
    match store.get_by_name(a)? {
        Some(acct) => Ok((acct.provider, a.to_string())),
        None => anyhow::bail!("Specify `<provider> <name>`, or an existing account name."),
    }
}

fn can_show_sharing(provider: &str, profile_type: Option<ProfileType>) -> bool {
    profile_type != Some(ProfileType::System)
        && matches!(
            normalize_provider_key(provider).as_str(),
            "claude" | "codex" | "grok"
        )
}

/// Fetch the live system credential for each provider concurrently.
async fn live_credentials(providers: &[Provider]) -> HashMap<String, Option<String>> {
    let futs = providers
        .iter()
        .map(|p| async move { (p.id().to_string(), p.current_credential().await) });
    join_all(futs).await.into_iter().collect()
}

// ---- commands ----

async fn cmd_list(
    store: &AccountStore,
    provider: Option<&str>,
    usage: bool,
    debug: bool,
) -> anyhow::Result<()> {
    let (provs, single_name) = aas_providers::resolve_scope(store, provider)?;
    let live = live_credentials(&provs).await;

    if usage {
        // Shared fetch path (also used by aas-bar): parallel usage for every account.
        let items = aas_providers::snapshot(store, provider).await?;
        let rows: Vec<UsageRow> = items
            .into_iter()
            .map(|it| {
                let live_cred = live.get(&it.provider).and_then(|c| c.clone());
                let stored = secure_store::get_secret(&it.provider, &it.name);
                let current = matches!((&stored, &live_cred), (Some(s), Some(l)) if s == l);
                UsageRow {
                    provider: it.provider,
                    name: it.name,
                    email: it.email,
                    active: it.active,
                    current_in_system: current,
                    usage: it.usage,
                }
            })
            .collect();
        if rows.is_empty() {
            println!("(no accounts)");
        } else {
            render::render_usage_table(&rows);
        }
        return Ok(());
    }

    // `-d` keeps a plain text dump (raw credentials don't belong in a table).
    if debug {
        for p in &provs {
            let accts: Vec<_> = store
                .list(Some(p.id()))?
                .into_iter()
                .filter(|a| single_name.as_ref().is_none_or(|n| &a.name == n))
                .collect();
            if accts.is_empty() {
                continue;
            }
            println!("{}", ui::heading(&format!("{}:", p.id())));
            for a in &accts {
                let cred =
                    secure_store::get_secret(p.id(), &a.name).unwrap_or_else(|| "(none)".into());
                println!("   {} {}", a.name, ui::dim(&format!("→ {cred}")));
            }
        }
        return Ok(());
    }

    // Build display rows for the table.
    let mut rows: Vec<render::ListRow> = Vec::new();
    let mut named_empty = false;
    for p in &provs {
        let accts: Vec<_> = store
            .list(Some(p.id()))?
            .into_iter()
            .filter(|a| single_name.as_ref().is_none_or(|n| &a.name == n))
            .collect();
        if accts.is_empty() {
            if provider.is_some() && single_name.is_none() {
                named_empty = true;
            }
            continue;
        }
        let active = store.get_active(p.id())?;
        let live_cred = live.get(p.id()).and_then(|c| c.clone());
        for a in &accts {
            let stored = secure_store::get_secret(p.id(), &a.name);
            let current = matches!((&stored, &live_cred), (Some(s), Some(l)) if s == l);
            let sharing = if current {
                render::Sharing::CurrentInSystem
            } else if !can_show_sharing(p.id(), a.profile_type) {
                render::Sharing::System
            } else {
                let txt = match a.share.as_deref() {
                    None => "shared: all".to_string(),
                    Some([]) => "isolated".to_string(),
                    Some(v) => format!("shared: {}", v.join(", ")),
                };
                render::Sharing::Categories(txt)
            };
            rows.push(render::ListRow {
                provider: p.id().to_string(),
                name: a.name.clone(),
                email: a.email.clone(),
                active: active.as_deref() == Some(a.name.as_str()),
                current_in_system: current,
                sharing,
            });
        }
    }

    if rows.is_empty() {
        let msg = if named_empty {
            "(none)"
        } else {
            "(no accounts)"
        };
        println!("{}", ui::dim(msg));
        return Ok(());
    }
    render::render_list_table(&rows);
    Ok(())
}

/// Machine-readable usage for aas-bar / integrations: the same parallel fetch as the table,
/// serialized as a typed `{"accounts":[{provider,name,email,active,plan,planLabel,headline,
/// error,notes,meters:[…]}]}` contract.
async fn cmd_usage_json(store: &AccountStore, provider: Option<&str>) -> anyhow::Result<()> {
    let items = aas_providers::snapshot(store, provider).await?;
    println!("{}", serde_json::to_string(&usage_json_response(&items))?);
    Ok(())
}

#[derive(serde::Serialize)]
struct UsageJsonResponse<'a> {
    accounts: Vec<UsageJsonAccount<'a>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageJsonAccount<'a> {
    provider: &'a str,
    name: &'a str,
    email: Option<&'a str>,
    active: bool,
    plan: Option<&'a str>,
    plan_label: Option<String>,
    headline: &'a str,
    error: Option<&'a str>,
    notes: &'a [String],
    meters: Vec<UsageJsonMeter<'a>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageJsonMeter<'a> {
    label: &'a str,
    used_pct: f64,
    reset_ms: Option<i64>,
}

fn usage_json_response(items: &[aas_providers::AccountUsage]) -> UsageJsonResponse<'_> {
    let accounts = items
        .iter()
        .map(|it| {
            let meters = it
                .usage
                .meters
                .iter()
                .map(|meter| UsageJsonMeter {
                    label: &meter.label,
                    used_pct: meter.used_pct,
                    reset_ms: meter.reset_ms,
                })
                .collect();
            UsageJsonAccount {
                provider: &it.provider,
                name: &it.name,
                email: it.email.as_deref(),
                active: it.active,
                plan: it.usage.plan.as_deref(),
                // Only a real plan gets a pretty label; long-lived tokens (plan=None) stay
                // null so the app falls back cleanly instead of chipping the raw headline.
                plan_label: it
                    .usage
                    .plan
                    .as_ref()
                    .map(|_| render::plan_label(&it.usage)),
                headline: &it.usage.headline,
                error: it.usage.error.as_deref(),
                notes: &it.usage.notes,
                meters,
            }
        })
        .collect();
    UsageJsonResponse { accounts }
}

fn cmd_status(store: &AccountStore, provider: Option<&str>) -> anyhow::Result<()> {
    let provs: Vec<String> = match provider {
        Some(p) => vec![p.to_string()],
        None => all_providers().iter().map(|p| p.id().to_string()).collect(),
    };
    let rows: Vec<(String, Option<String>)> = provs
        .into_iter()
        .map(|provider| {
            let active = store.get_active(&provider)?;
            Ok((provider, active))
        })
        .collect::<Result<_, aas_core::store::StoreError>>()?;
    render::render_status_table(&rows);
    Ok(())
}

fn cmd_import(file: Option<&str>) -> anyhow::Result<()> {
    let Some(src) = file else {
        // No file → inspect/adopt the existing (shared) asx state.
        let report = aas_import::inspect()?;
        let dir = aas_core::platform::asx_config_dir();
        println!(
            "{} {}",
            ui::heading("asx state:"),
            ui::dim(&dir.display().to_string())
        );
        println!("  {} {}", ui::dim("accounts         "), report.accounts);
        println!(
            "  {} {}",
            ui::dim("with profile home"),
            report.with_profile_home
        );
        ui::success("Adopted directly — no migration needed.");
        return Ok(());
    };

    // Restore a bundle produced by `aas export --all` (a file, or `-` for stdin).
    let data = if src == "-" {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        bytes
    } else {
        std::fs::read(src)?
    };
    let bundle: aas_import::Bundle = if aas_import::is_encrypted_bundle(&data) {
        let passphrase = match std::env::var("AAS_VAULT_PASSPHRASE") {
            Ok(value) if !value.is_empty() => value,
            Ok(_) => anyhow::bail!("AAS_VAULT_PASSPHRASE cannot be empty"),
            Err(_) => rpassword::prompt_password("Vault passphrase: ")?,
        };
        aas_import::decrypt_bundle(&data, &passphrase)?
    } else {
        serde_json::from_slice(&data)?
    };
    if bundle.version != 1 {
        anyhow::bail!("unsupported bundle version: {}", bundle.version);
    }
    let report = aas_import::import_bundle(&bundle);
    ui::success(format!(
        "Imported {} accounts, {} credentials",
        report.accounts, report.credentials
    ));
    if !report.conflicts.is_empty() {
        ui::warn(format!(
            "skipped (name already used): {}",
            report.conflicts.join(", ")
        ));
    }
    if !report.without_credential.is_empty() {
        ui::hint(format!(
            "no credential in bundle for: {}",
            report.without_credential.join(", ")
        ));
    }
    if !report.failed.is_empty() {
        ui::warn(format!(
            "could not store credential for: {}",
            report.failed.join(", ")
        ));
    }
    Ok(())
}

async fn cmd_load(
    store: &AccountStore,
    provider: Option<String>,
    name: Option<String>,
    share: &ShareArgs,
) -> anyhow::Result<()> {
    if share.any() {
        anyhow::bail!("System profiles created by `aas load` cannot use --shared/--isolated/--share/--isolate.");
    }
    let targets: Vec<(String, Option<String>)> = match &provider {
        Some(p) => vec![(p.clone(), name.clone())],
        None => all_providers()
            .iter()
            .map(|p| (p.id().to_string(), None))
            .collect(),
    };
    let mut loaded_any = false;
    for (prov, explicit) in targets {
        let Some(adapter) = get_adapter(&prov) else {
            if provider.is_some() {
                if store.get_by_name(&prov).ok().flatten().is_some() {
                    ui::error(format!("\"{prov}\" is an account, not a provider."));
                    ui::hint(format!("activate it:   aas switch {prov}"));
                    ui::hint(format!("run under it:  aas exec {prov}"));
                    ui::hint(format!("shell env:     eval \"$(aas export {prov})\""));
                } else {
                    ui::error(format!("Unknown provider: {prov}"));
                    ui::hint("providers: claude · codex · grok · zai · cursor");
                    ui::hint("`load` snapshots the currently logged-in credential, e.g.:  aas load codex");
                }
                std::process::exit(1);
            }
            continue;
        };
        let live = adapter.current_credential().await;
        if provider.is_none() && live.is_none() {
            continue;
        }
        let email = adapter.current_email().await;
        let existing = if let Some(email) = email.as_ref() {
            store.list(Some(adapter.id()))?.into_iter().find(|account| {
                account
                    .email
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(email))
            })
        } else {
            None
        };
        let final_name = explicit
            .clone()
            .or_else(|| existing.as_ref().map(|e| e.name.clone()))
            .unwrap_or_else(|| {
                aas_core::naming::derive_account_name(email.as_deref(), adapter.id())
            });
        match adapter.load_current(&final_name, None).await {
            Ok(()) => {
                store.set_profile_type(adapter.id(), &final_name, ProfileType::System)?;
                ui::success(format!("Loaded {}/{}", adapter.id(), final_name));
                loaded_any = true;
            }
            Err(e) => {
                if provider.is_some() {
                    anyhow::bail!("Failed for {prov}: {e}");
                }
            }
        }
    }
    if provider.is_none() && !loaded_any {
        ui::warn("No configured agents found to load.");
    }
    Ok(())
}

async fn cmd_login(
    store: &AccountStore,
    provider: Option<String>,
    name: Option<String>,
    long_lived: bool,
    device_auth: bool,
    share: &ShareArgs,
) -> anyhow::Result<()> {
    // Resolve provider (explicit, or inferred from an existing account name).
    let (prov, name) = match (&provider, &name) {
        (Some(p), n) => (p.clone(), n.clone()),
        (None, _) => {
            ui::error("Specify a provider to log in.");
            ui::hint("providers:  claude · codex · grok · zai");
            ui::hint("example:    aas login claude work");
            std::process::exit(2);
        }
    };
    let Some(adapter) = get_adapter(&prov) else {
        anyhow::bail!("Unknown provider: {prov}");
    };
    let share_sel = resolve_share_selection(&share.to_opts(), Some(adapter.id()))?;
    let system_home = match name.as_deref() {
        Some(account_name) => {
            store
                .get(adapter.id(), account_name)?
                .and_then(|account| account.profile_type)
                == Some(ProfileType::System)
        }
        None => false,
    };
    if system_home && share_sel.provided {
        anyhow::bail!(
            "System profiles cannot use --shared/--isolated/--share/--isolate during login."
        );
    }
    let final_name = login::run_login_flow(
        adapter,
        name.as_deref(),
        long_lived,
        device_auth,
        system_home,
    )
    .await?;
    if let Some(final_name) = final_name {
        store.set_profile_type(
            adapter.id(),
            &final_name,
            if system_home {
                ProfileType::System
            } else {
                ProfileType::Isolated
            },
        )?;
        if share_sel.provided {
            store.set_share(adapter.id(), &final_name, share_sel.value)?;
        }
        ui::success(format!("Logged in: {}/{}", adapter.id(), final_name));
    }
    Ok(())
}

async fn cmd_switch(store: &AccountStore, a: &str, b: Option<&str>) -> anyhow::Result<()> {
    let (prov, name) = resolve_provider_name(store, a, b)?;
    let Some(adapter) = get_adapter(&prov) else {
        anyhow::bail!("Unknown provider: {prov}");
    };
    adapter.switch_to(&name).await?;
    ui::success(format!("Switched {} → {}", adapter.id(), name));
    Ok(())
}

fn cmd_rename(store: &AccountStore, from: &str, to: &str) -> anyhow::Result<()> {
    let Some(_account) = store.get_by_name(from)? else {
        anyhow::bail!("Account not found: {from}");
    };
    store.rename(from, to)?;
    ui::success(format!("Renamed {from} → {to}"));
    Ok(())
}

fn cmd_remove(store: &AccountStore, args: &[String]) -> anyhow::Result<()> {
    let (prov, name) = match args {
        [name] => {
            let Some(acct) = store.get_by_name(name)? else {
                anyhow::bail!("Account not found: {name}");
            };
            (acct.provider, name.clone())
        }
        [prov, name] => (prov.clone(), name.clone()),
        _ => anyhow::bail!("Usage: aas remove [provider] <name>"),
    };
    let account = store.get(&prov, &name)?;
    let active_before = store.get_active(&prov)?;
    let secret_before = secure_store::get_secret(&prov, &name);
    if store.remove(&prov, &name)? {
        if let Err(delete_error) = secure_store::delete_secret(&prov, &name) {
            let mut rollback_errors = Vec::new();
            if let Some(account) = account {
                if let Err(error) = store.add(account) {
                    rollback_errors.push(format!("account={error}"));
                }
            }
            if let Some(secret) = secret_before {
                if let Err(error) = secure_store::set_secret(&prov, &name, &secret) {
                    rollback_errors.push(format!("credential={error}"));
                }
            }
            if active_before.as_deref() == Some(name.as_str()) {
                if let Err(error) = store.set_active(&prov, &name) {
                    rollback_errors.push(format!("active={error}"));
                }
            }
            anyhow::bail!(
                "Could not remove credential for {prov}/{name}: {delete_error}; rollback: {}",
                if rollback_errors.is_empty() {
                    "completed".to_string()
                } else {
                    rollback_errors.join(", ")
                }
            );
        }
        ui::success(format!("Removed {prov}/{name}"));
    } else {
        ui::warn(format!("Not found: {prov}/{name}"));
    }
    Ok(())
}

fn cmd_sharing(store: &AccountStore, name: &str, share: &ShareArgs) -> anyhow::Result<()> {
    let Some(acct) = store.get_by_name(name)? else {
        anyhow::bail!("Account not found: {name}");
    };
    if !matches!(
        normalize_provider_key(&acct.provider).as_str(),
        "claude" | "codex" | "grok"
    ) {
        anyhow::bail!("Sharing is only available for agent profiles (claude, codex, grok).");
    }
    if acct.profile_type == Some(ProfileType::System) {
        anyhow::bail!(
            "Sharing is not available for a system profile ({}/{name}).",
            acct.provider
        );
    }
    let sel = resolve_share_selection(&share.to_opts(), Some(&acct.provider))?;
    if sel.provided {
        store.set_share(&acct.provider, name, sel.value)?;
        ui::success(format!("Updated sharing for {}/{name}", acct.provider));
    }
    let cur = store.get(&acct.provider, name)?;
    println!(
        "{}/{name}: {}",
        acct.provider,
        describe_share(
            cur.as_ref().and_then(|account| account.share.as_deref()),
            Some(&acct.provider),
        )
    );
    Ok(())
}

async fn cmd_refresh(
    store: &AccountStore,
    a: &str,
    b: Option<&str>,
    no_login: bool,
) -> anyhow::Result<()> {
    let (prov, name) = resolve_provider_name(store, a, b)?;
    let Some(adapter) = get_adapter(&prov) else {
        anyhow::bail!("Unknown provider: {prov}");
    };
    if store.get(&prov, &name)?.is_none() {
        anyhow::bail!("Account not found: {prov}/{name}");
    }
    let r = adapter.refresh(&name).await;
    if r.ok {
        ui::success(format!("{name}: {}", r.message));
        return Ok(());
    }
    ui::error(format!("{name}: {}", r.message));
    if r.needs_relogin && !no_login {
        let account = store.get(&prov, &name)?;
        let system_home = account.and_then(|item| item.profile_type) == Some(ProfileType::System);
        let relogged =
            login::run_login_flow(adapter, Some(&name), false, false, system_home).await?;
        if relogged.is_some() {
            return Ok(());
        }
        anyhow::bail!("refresh failed and re-login was not completed");
    }
    anyhow::bail!("refresh failed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use aas_core::model::AccountRecord;

    fn test_store() -> (AccountStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("aas-cli-test-{}", uuid::Uuid::new_v4()));
        (AccountStore::at(&dir), dir)
    }

    #[test]
    fn existing_account_is_rewritten_to_default_exec() {
        let (store, dir) = test_store();
        store.add(AccountRecord::new("codex", "work")).unwrap();
        let args = vec!["aas".into(), "work".into(), "--".into(), "--version".into()];

        let rewritten = rewrite_default_exec_args(&store, args).unwrap();
        assert_eq!(rewritten[1], "exec");
        assert_eq!(rewritten[2], "work");
        let parsed = Cli::try_parse_from(rewritten).unwrap();
        match parsed.command {
            Command::Exec { name, rest } => {
                assert_eq!(name, "work");
                assert_eq!(rest, ["--version"]);
            }
            _ => panic!("expected exec command"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn known_commands_and_unknown_names_are_not_rewritten() {
        let (store, dir) = test_store();
        for candidate in ["list", "unknown"] {
            let args = vec!["aas".into(), candidate.into()];
            let rewritten = rewrite_default_exec_args(&store, args).unwrap();
            assert_eq!(rewritten.len(), 2);
            assert_eq!(rewritten[1], candidate);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn export_accepts_provider_and_account_positionals() {
        let parsed =
            Cli::try_parse_from(["aas", "export", "zai", "work", "--shell", "fish"]).unwrap();
        match parsed.command {
            Command::Export {
                name,
                account,
                shell: export::Shell::Fish,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("zai"));
                assert_eq!(account.as_deref(), Some("work"));
            }
            _ => panic!("expected two-positional export"),
        }
    }

    #[test]
    fn usage_json_schema_includes_notes_and_camel_case_meters() {
        let items = vec![aas_providers::AccountUsage {
            provider: "grok".into(),
            name: "work".into(),
            email: None,
            active: true,
            usage: aas_core::usage::Usage {
                headline: "Grok team".into(),
                plan: Some("team".into()),
                meters: vec![aas_core::usage::Meter::new("credits", 25.0, Some(123))],
                notes: vec!["rate remaining req=3 tok=9".into()],
                error: None,
            },
        }];
        let value = serde_json::to_value(usage_json_response(&items)).unwrap();
        let account = &value["accounts"][0];
        assert_eq!(account["notes"][0], "rate remaining req=3 tok=9");
        assert_eq!(account["meters"][0]["usedPct"], 25.0);
        assert_eq!(account["meters"][0]["resetMs"], 123);
        assert!(account["meters"][0].get("used_pct").is_none());
    }
}
