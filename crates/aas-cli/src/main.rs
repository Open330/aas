//! `aas` CLI entry point. Command surface mirrors asx (see `docs/PARITY_SPEC.md` §A).
//! P1/P2 wired: list (+ parallel `-u` table), load, login, switch, status, rename, remove,
//! sharing, refresh, import. exec (P3) / proxy (P4) land next.

mod exec;
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
    Usage { provider: Option<String> },
    /// Show asx-tracked active account(s).
    Status { provider: Option<String> },
    /// Adopt / inspect existing asx state.
    Import,
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
        #[command(flatten)]
        share: ShareArgs,
    },
    /// Switch the active credential.
    #[command(visible_alias = "s")]
    Switch { provider: String, name: Option<String> },
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
    let cli = Cli::parse();
    let store = AccountStore::open_default();
    let result = match cli.command {
        Command::List { provider, usage, debug } => cmd_list(&store, provider.as_deref(), usage, debug).await,
        Command::Usage { provider } => cmd_list(&store, provider.as_deref(), true, false).await,
        Command::Status { provider } => cmd_status(&store, provider.as_deref()),
        Command::Import => cmd_import(),
        Command::Load { provider, name, share } => cmd_load(&store, provider, name, &share).await,
        Command::Login { provider, name, long_lived, share } => {
            cmd_login(&store, provider, name, long_lived, &share).await
        }
        Command::Switch { provider, name } => cmd_switch(&store, &provider, name.as_deref()).await,
        Command::Rename { from, to } => cmd_rename(&store, &from, &to),
        Command::Remove { args } => cmd_remove(&store, &args),
        Command::Sharing { name, share } => cmd_sharing(&store, &name, &share),
        Command::Refresh { provider, name, no_login } => {
            cmd_refresh(&store, &provider, name.as_deref(), no_login).await
        }
        Command::Exec { name, rest } => exec::cmd_exec(&store, &name, &rest).await,
        Command::Proxy { name, frontend } => exec::cmd_proxy(&store, &name, &frontend).await,
    };
    if let Err(e) = result {
        ui::error(e);
        std::process::exit(1);
    }
}

// ---- helpers ----

fn resolve_provider_name(store: &AccountStore, a: &str, b: Option<&str>) -> anyhow::Result<(String, String)> {
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
        && matches!(normalize_provider_key(provider).as_str(), "claude" | "codex" | "grok")
}

/// Fetch the live system credential for each provider concurrently.
async fn live_credentials(providers: &[Provider]) -> HashMap<String, Option<String>> {
    let futs = providers.iter().map(|p| async move { (p.id().to_string(), p.current_credential().await) });
    join_all(futs).await.into_iter().collect()
}

async fn ensure_fresh(p: Provider, name: &str) {
    if p.is_expired(name).await {
        let _ = p.refresh(name).await;
    }
}

/// Resolve which providers to show and an optional single-account filter.
fn resolve_list_scope(store: &AccountStore, provider: Option<&str>) -> anyhow::Result<(Vec<Provider>, Option<String>)> {
    match provider {
        Some(p) => {
            if let Some(adapter) = get_adapter(p) {
                Ok((vec![adapter], None))
            } else if let Some(acct) = store.get_by_name(p)? {
                match get_adapter(&acct.provider) {
                    Some(a) => Ok((vec![a], Some(p.to_string()))),
                    None => anyhow::bail!("Unknown provider or name: {p}"),
                }
            } else {
                anyhow::bail!("Unknown provider or name: {p}")
            }
        }
        None => Ok((all_providers().to_vec(), None)),
    }
}

// ---- commands ----

async fn cmd_list(store: &AccountStore, provider: Option<&str>, usage: bool, debug: bool) -> anyhow::Result<()> {
    let (provs, single_name) = resolve_list_scope(store, provider)?;
    let live = live_credentials(&provs).await;

    if usage {
        // Fan out ensure_fresh + usage across every account, then render once.
        let mut jobs = Vec::new();
        for p in &provs {
            let accts: Vec<_> = store
                .list(Some(p.id()))
                .into_iter()
                .filter(|a| single_name.as_ref().is_none_or(|n| &a.name == n))
                .collect();
            for a in accts {
                let p = *p;
                jobs.push(async move {
                    ensure_fresh(p, &a.name).await;
                    let u = p.usage(&a.name).await;
                    (p, a, u)
                });
            }
        }
        let done = join_all(jobs).await;
        let rows: Vec<UsageRow> = done
            .into_iter()
            .map(|(p, a, usage)| {
                let live_cred = live.get(p.id()).and_then(|c| c.clone());
                let stored = secure_store::get_secret(p.id(), &a.name);
                let current = matches!((&stored, &live_cred), (Some(s), Some(l)) if s == l);
                UsageRow {
                    provider: p.id().to_string(),
                    name: a.name.clone(),
                    email: a.email.clone(),
                    active: store.get_active(p.id()).as_deref() == Some(a.name.as_str()),
                    current_in_system: current,
                    usage,
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
                .list(Some(p.id()))
                .into_iter()
                .filter(|a| single_name.as_ref().is_none_or(|n| &a.name == n))
                .collect();
            if accts.is_empty() {
                continue;
            }
            println!("{}", ui::heading(&format!("{}:", p.id())));
            for a in &accts {
                let cred = secure_store::get_secret(p.id(), &a.name).unwrap_or_else(|| "(none)".into());
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
            .list(Some(p.id()))
            .into_iter()
            .filter(|a| single_name.as_ref().is_none_or(|n| &a.name == n))
            .collect();
        if accts.is_empty() {
            if provider.is_some() && single_name.is_none() {
                named_empty = true;
            }
            continue;
        }
        let active = store.get_active(p.id());
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
                    Some(v) if v.is_empty() => "isolated".to_string(),
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
        let msg = if named_empty { "(none)" } else { "(no accounts)" };
        println!("{}", ui::dim(msg));
        return Ok(());
    }
    render::render_list_table(&rows);
    Ok(())
}

fn cmd_status(store: &AccountStore, provider: Option<&str>) -> anyhow::Result<()> {
    let provs: Vec<String> = match provider {
        Some(p) => vec![p.to_string()],
        None => all_providers().iter().map(|p| p.id().to_string()).collect(),
    };
    let rows: Vec<(String, Option<String>)> =
        provs.into_iter().map(|p| { let a = store.get_active(&p); (p, a) }).collect();
    render::render_status_table(&rows);
    Ok(())
}

fn cmd_import() -> anyhow::Result<()> {
    let report = aas_import::inspect()?;
    let dir = aas_core::platform::asx_config_dir();
    println!("{} {}", ui::heading("asx state:"), ui::dim(&dir.display().to_string()));
    println!("  {} {}", ui::dim("accounts         "), report.accounts);
    println!("  {} {}", ui::dim("with profile home"), report.with_profile_home);
    ui::success("Adopted directly — no migration needed.");
    Ok(())
}

async fn cmd_load(store: &AccountStore, provider: Option<String>, name: Option<String>, share: &ShareArgs) -> anyhow::Result<()> {
    if share.any() {
        anyhow::bail!("System profiles created by `aas load` cannot use --shared/--isolated/--share/--isolate.");
    }
    let targets: Vec<(String, Option<String>)> = match &provider {
        Some(p) => vec![(p.clone(), name.clone())],
        None => all_providers().iter().map(|p| (p.id().to_string(), None)).collect(),
    };
    let mut loaded_any = false;
    for (prov, explicit) in targets {
        let Some(adapter) = get_adapter(&prov) else {
            if provider.is_some() {
                eprintln!("Unknown provider: {prov}");
            }
            continue;
        };
        let live = adapter.current_credential().await;
        if provider.is_none() && live.is_none() {
            continue;
        }
        let email = adapter.current_email().await;
        let existing = email.as_ref().and_then(|e| {
            store
                .list(Some(adapter.id()))
                .into_iter()
                .find(|a| a.email.as_deref().is_some_and(|ae| ae.eq_ignore_ascii_case(e)))
        });
        let final_name = explicit
            .clone()
            .or_else(|| existing.as_ref().map(|e| e.name.clone()))
            .unwrap_or_else(|| aas_core::naming::derive_account_name(email.as_deref(), adapter.id()));
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

async fn cmd_login(store: &AccountStore, provider: Option<String>, name: Option<String>, long_lived: bool, share: &ShareArgs) -> anyhow::Result<()> {
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
    let system_home = false; // system-profile re-login detection lands with exec/P3
    let final_name = login::run_login_flow(adapter, name.as_deref(), long_lived, system_home).await?;
    if let Some(final_name) = final_name {
        store.set_profile_type(adapter.id(), &final_name, ProfileType::Isolated)?;
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
    let Some(acct) = store.get_by_name(from)? else {
        anyhow::bail!("Account not found: {from}");
    };
    secure_store::rename_secret(&acct.provider, from, to)?;
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
    if store.remove(&prov, &name)? {
        secure_store::delete_secret(&prov, &name);
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
    if !matches!(normalize_provider_key(&acct.provider).as_str(), "claude" | "codex" | "grok") {
        anyhow::bail!("Sharing is only available for agent profiles (claude, codex, grok).");
    }
    if acct.profile_type == Some(ProfileType::System) {
        anyhow::bail!("Sharing is not available for a system profile ({}/{name}).", acct.provider);
    }
    let sel = resolve_share_selection(&share.to_opts(), Some(&acct.provider))?;
    if sel.provided {
        store.set_share(&acct.provider, name, sel.value)?;
        ui::success(format!("Updated sharing for {}/{name}", acct.provider));
    }
    let cur = store.get(&acct.provider, name);
    println!(
        "{}/{name}: {}",
        acct.provider,
        describe_share(cur.as_ref().and_then(|a| a.share.as_deref()), Some(&acct.provider))
    );
    Ok(())
}

async fn cmd_refresh(store: &AccountStore, a: &str, b: Option<&str>, no_login: bool) -> anyhow::Result<()> {
    let (prov, name) = resolve_provider_name(store, a, b)?;
    let Some(adapter) = get_adapter(&prov) else {
        anyhow::bail!("Unknown provider: {prov}");
    };
    if store.get(&prov, &name).is_none() {
        anyhow::bail!("Account not found: {prov}/{name}");
    }
    let r = adapter.refresh(&name).await;
    if r.ok {
        ui::success(format!("{name}: {}", r.message));
        return Ok(());
    }
    ui::error(format!("{name}: {}", r.message));
    if r.needs_relogin && !no_login {
        let acct = store.get(&prov, &name);
        let system_home = acct.and_then(|a| a.profile_type) == Some(ProfileType::System);
        login::run_login_flow(adapter, Some(&name), false, system_home).await?;
        return Ok(());
    }
    anyhow::bail!("refresh failed");
}
