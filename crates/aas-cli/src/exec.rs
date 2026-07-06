//! `exec`/`e` and `proxy` commands. Mirrors asx cli.ts exec (735-957) + proxy (677-729).
//! Same-provider runs inject the profile home + shared-state symlinks; cross-provider runs
//! spin up the local ASX Proxy and point the agent binary at it.

use aas_core::execargs::parse_exec_args;
use aas_core::model::ProfileType;
use aas_core::naming::{
    is_known_provider, normalize_provider, normalize_provider_key, profile_home, safe_profile_dir_name,
};
use aas_core::store::AccountStore;
use aas_core::{platform, secure_store, share};
use aas_providers::get_adapter;
use aas_proxy::{inject_proxy_endpoint, start_proxy, Credential, ProxyStartOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct AgentSpec {
    bin: &'static str,
    home_env: &'static str,
    file: &'static str,
    bypass: &'static [&'static str],
    stub: Option<&'static str>,
}

fn agent_spec(provider: &str) -> Option<AgentSpec> {
    let key = if provider.contains("claude") { "claude" } else { provider };
    match key {
        "codex" => Some(AgentSpec {
            bin: "codex",
            home_env: "CODEX_HOME",
            file: "auth.json",
            bypass: &["--dangerously-bypass-approvals-and-sandbox", "--dangerously-bypass-hook-trust"],
            stub: None,
        }),
        "claude" => Some(AgentSpec {
            bin: "claude",
            home_env: "CLAUDE_CONFIG_DIR",
            file: ".credentials.json",
            bypass: &["--dangerously-skip-permissions"],
            stub: Some("{\"claudeAiOauth\":{\"accessToken\":\"asx-proxy-dummy\"}}"),
        }),
        "grok" => Some(AgentSpec {
            bin: "grok",
            home_env: "GROK_HOME",
            file: "auth.json",
            bypass: &["--dangerously-skip-permissions"],
            stub: None,
        }),
        _ => None,
    }
}

fn agents_dir() -> PathBuf {
    platform::profiles_dir().join(".agents")
}

fn agent_scratch_home(provider: &str, name: &str) -> PathBuf {
    agents_dir().join(safe_profile_dir_name(provider, name))
}

fn cross_session_home(provider: &str, name: &str) -> PathBuf {
    let id = uuid::Uuid::new_v4();
    agents_dir()
        .join("sessions")
        .join(format!("{}-{}", safe_profile_dir_name(provider, name), id))
}

fn remove_cross_home(dir: &Path) {
    // Refuse to delete anything not strictly under <profiles>/.agents/sessions/.
    let sessions = agents_dir().join("sessions");
    let under = match (dir.canonicalize(), sessions.canonicalize()) {
        (Ok(d), Ok(s)) => d.starts_with(&s) && d != s,
        _ => dir.starts_with(&sessions),
    };
    if under {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(unix)]
fn ensure_700(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn ensure_700(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
}

#[cfg(unix)]
fn set_0600(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_0600(_p: &Path) {}

fn seed_agent_home(provider_key: &str, dir: &Path) {
    if provider_key != "claude" {
        return;
    }
    let p = dir.join(".claude.json");
    let mut json = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = json.as_object_mut() {
        obj.insert("hasCompletedOnboarding".into(), serde_json::Value::Bool(true));
    }
    if std::fs::write(&p, serde_json::to_string(&json).unwrap_or_default()).is_ok() {
        set_0600(&p);
    }
}

fn claude_long_lived_token(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if v.get("type").and_then(|t| t.as_str()) == Some("claude-code-oauth-token") {
        return v.get("token").and_then(|t| t.as_str()).map(String::from);
    }
    None
}

async fn live_credential(provider: &str) -> Option<String> {
    match get_adapter(provider) {
        Some(p) => p.current_credential().await,
        None => None,
    }
}

async fn is_current_system_profile(provider: &str, name: &str) -> bool {
    let stored = secure_store::get_secret(provider, name);
    let live = live_credential(provider).await;
    matches!((stored, live), (Some(s), Some(l)) if s == l)
}

pub async fn cmd_exec(store: &AccountStore, name: &str, rest: &[String]) -> anyhow::Result<()> {
    let Some(acct) = store.get_by_name(name)? else {
        anyhow::bail!("Account not found: {name}");
    };
    let profile_provider = acct.provider.clone();
    let account_name = acct.name.clone();

    // Optional <target> positional (a known provider that isn't a flag).
    let mut idx = 0;
    let mut specified: Option<String> = None;
    if let Some(first) = rest.first() {
        if !first.starts_with('-') && is_known_provider(first) {
            specified = normalize_provider(first);
            idx = 1;
        }
    }
    let after = &rest[idx..];
    let agent_provider = specified.clone().unwrap_or_else(|| profile_provider.clone());
    let is_cross = specified
        .as_ref()
        .map(|s| normalize_provider_key(s) != normalize_provider_key(&profile_provider))
        .unwrap_or(false);
    let Some(spec) = agent_spec(&agent_provider) else {
        anyhow::bail!("Exec is not supported for provider '{agent_provider}'.");
    };

    let exec_args = parse_exec_args(after, is_cross, Some(&agent_provider))?;

    // Auto-refresh an expired credential before launch.
    if let Some(p) = get_adapter(&profile_provider) {
        if p.is_expired(&account_name).await {
            let _ = p.refresh(&account_name).await;
        }
    }

    let mut env: HashMap<String, String> = std::env::vars().collect();
    let secret = secure_store::get_secret(&profile_provider, &account_name);

    // Claude long-lived token → env auth (same-provider claude only).
    if !is_cross && normalize_provider_key(&agent_provider) == "claude" {
        if let Some(tok) = secret.as_deref().and_then(claude_long_lived_token) {
            env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), tok);
        }
    }

    let system_profile = acct.profile_type == Some(ProfileType::System)
        || is_current_system_profile(&profile_provider, &account_name).await;

    let mut proxy_handle = None;
    let mut cross_home: Option<PathBuf> = None;

    if !is_cross {
        if system_profile {
            if let (Some(stored), Some(live)) = (&secret, live_credential(&profile_provider).await) {
                if stored != &live {
                    anyhow::bail!(
                        "{profile_provider}/{account_name} is a system profile but is not current in system. Run: aas switch {account_name}"
                    );
                }
            }
        } else {
            let home = profile_home(&profile_provider, &account_name);
            ensure_700(&home);
            env.insert(spec.home_env.into(), home.display().to_string());
            seed_agent_home(&normalize_provider_key(&agent_provider), &home);
            share::link_shared_state(&profile_provider, &home, false, acct.share.as_deref());
        }
    } else {
        let home = cross_session_home(&agent_provider, &account_name);
        ensure_700(&home);
        cross_home = Some(home.clone());
        env.insert(spec.home_env.into(), home.display().to_string());
        seed_agent_home(&normalize_provider_key(&agent_provider), &home);
        let cats = if exec_args.share.provided {
            exec_args.share.value.as_deref()
        } else {
            None
        };
        share::link_shared_state(&agent_provider, &home, true, cats);
        if let Some(stub) = spec.stub {
            let f = home.join(spec.file);
            let _ = std::fs::write(&f, stub);
            set_0600(&f);
        }
        let Some(backend_cred) = secret.clone() else {
            anyhow::bail!("No stored credential for {profile_provider}/{account_name}");
        };
        let handle = start_proxy(ProxyStartOptions {
            source_provider: agent_provider.clone(),
            target_provider: profile_provider.clone(),
            target_credential: Credential { raw: Some(backend_cred), api_key: None },
            tmp_dir: Some(home.clone()),
            port: None,
        })
        .await?;
        inject_proxy_endpoint(&agent_provider, &mut env, &handle.url, Some(&home), Some(&profile_provider))?;
        proxy_handle = Some(handle);
    }

    // Forward args + bypass + debug.
    let mut forward = exec_args.forward_args.clone();
    if exec_args.bypass {
        let mut b: Vec<String> = spec.bypass.iter().map(|s| s.to_string()).collect();
        b.extend(forward);
        forward = b;
    }
    if exec_args.debug {
        env.insert("ASX_DEBUG".into(), "1".into());
    }

    // Spawn the native binary (interactive; stdio inherited).
    let mut cmd = tokio::process::Command::new(spec.bin);
    cmd.env_clear().envs(&env).args(&forward);
    let code = match cmd.spawn() {
        Ok(mut child) => {
            tokio::select! {
                s = child.wait() => s.ok().and_then(|s| s.code()).unwrap_or(0),
                _ = tokio::signal::ctrl_c() => { let _ = child.wait().await; 130 }
            }
        }
        Err(e) => {
            cleanup(proxy_handle, &cross_home, exec_args.keep_context).await;
            anyhow::bail!("Failed to launch {}: {e}", spec.bin);
        }
    };

    cleanup(proxy_handle, &cross_home, exec_args.keep_context).await;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

async fn cleanup(proxy: Option<aas_proxy::ProxyHandle>, cross_home: &Option<PathBuf>, keep: bool) {
    if let Some(h) = proxy {
        h.stop().await;
    }
    if let Some(dir) = cross_home {
        let keep = keep || std::env::var("ASX_KEEP_CONTEXT").ok().as_deref() == Some("1");
        if keep {
            eprintln!("[aas] keeping cross context: {}", dir.display());
        } else {
            remove_cross_home(dir);
        }
    }
}

pub async fn cmd_proxy(store: &AccountStore, name: &str, frontend: &str) -> anyhow::Result<()> {
    let Some(acct) = store.get_by_name(name)? else {
        anyhow::bail!("Account not found: {name}");
    };
    let backend_provider = acct.provider.clone();
    let frontend_norm = normalize_provider(frontend).unwrap_or_else(|| frontend.to_lowercase());
    if !matches!(frontend_norm.as_str(), "claude" | "codex" | "grok") {
        anyhow::bail!("Frontend must be one of claude, codex, grok (got {frontend}).");
    }

    if let Some(p) = get_adapter(&backend_provider) {
        if p.is_expired(&acct.name).await {
            let _ = p.refresh(&acct.name).await;
        }
    }
    let Some(backend_cred) = secure_store::get_secret(&backend_provider, &acct.name) else {
        anyhow::bail!("No stored credential for {backend_provider}/{}", acct.name);
    };

    let handle = start_proxy(ProxyStartOptions {
        source_provider: frontend_norm.clone(),
        target_provider: backend_provider.clone(),
        target_credential: Credential { raw: Some(backend_cred), api_key: None },
        tmp_dir: None,
        port: None,
    })
    .await?;

    let dir = agent_scratch_home(&frontend_norm, &acct.name);
    ensure_700(&dir);
    let mut injected: HashMap<String, String> = std::env::vars().collect();
    let before = injected.clone();
    inject_proxy_endpoint(&frontend_norm, &mut injected, &handle.url, Some(&dir), Some(&backend_provider))?;

    println!("ASX Proxy: {}", handle.url);
    println!("  backend:  {backend_provider}/{}", acct.name);
    println!("  frontend: {frontend_norm}");
    let mut keys: Vec<&String> = injected.keys().filter(|k| before.get(*k) != injected.get(*k)).collect();
    keys.sort();
    for k in keys {
        println!("  export {k}=\"{}\"", injected[k]);
    }
    println!("Press Ctrl+C to stop.");
    let _ = tokio::signal::ctrl_c().await;
    handle.stop().await;
    Ok(())
}
