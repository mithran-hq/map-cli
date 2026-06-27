use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The reusable `map-deploy.yml` workflow template. `map setup --repo-dir` writes this
/// verbatim into a repo's `.github/workflows/`; `map deploy` dispatches it. Single source
/// of truth lives at `templates/map-deploy.yml`.
const MAP_DEPLOY_WORKFLOW_TEMPLATE: &str = include_str!("../templates/map-deploy.yml");

const USER_AGENT: &str = concat!("map-cli/", env!("CARGO_PKG_VERSION"));

#[derive(Parser)]
#[command(name = "map", version, about = "Thin MAP client for Aegis.app")]
struct Cli {
    #[arg(long, global = true)]
    login_state: Option<PathBuf>,

    #[arg(long, global = true)]
    endpoint: Option<String>,

    #[arg(long, global = true)]
    token: Option<String>,

    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Login(LoginCommand),
    Whoami,
    Doctor(DoctorArgs),
    Init(InitArgs),
    Validate(DeployTarget),
    /// ADR-0016 trigger: dispatch the thin `map-deploy.yml` workflow (GitHub is the
    /// trigger + audit surface; review stays server-side).
    Deploy(DeployArgs),
    /// Host/runner-side primitive: POST a deploy request straight to the control-plane.
    DeployRequest(DeployRequestArgs),
    /// Onboard an app (allowlist + host mirror + workflow drop).
    Setup(SetupArgs),
    Status(IdArgs),
    Watch(WatchArgs),
    Logs(IdArgs),
    Evidence(IdArgs),
    Rollback(RollbackArgs),
    Version,
}

#[derive(Args)]
struct LoginCommand {
    #[command(subcommand)]
    command: LoginSubcommand,
}

#[derive(Subcommand)]
enum LoginSubcommand {
    Save(LoginSaveArgs),
    PrintToken(PrintTokenArgs),
}

#[derive(Args)]
struct LoginSaveArgs {
    #[arg(long)]
    map_control_endpoint: String,

    #[arg(long)]
    jason_controller_endpoint: Option<String>,

    #[arg(long)]
    access_token: String,

    #[arg(long, default_value = "map-control")]
    audience: String,

    #[arg(long = "scope", value_delimiter = ',')]
    scopes: Vec<String>,

    #[arg(long)]
    expires_at: Option<String>,

    #[arg(long)]
    email: Option<String>,

    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct PrintTokenArgs {
    #[arg(long)]
    audience: String,
}

#[derive(Args, Serialize)]
struct InitArgs {
    #[arg(long, default_value = "mithran.yaml")]
    manifest: PathBuf,
}

#[derive(Args, Serialize)]
struct DeployTarget {
    #[arg(long)]
    repo: String,

    #[arg(long)]
    env: Option<String>,

    #[arg(long = "ref", conflicts_with = "sha")]
    ref_name: Option<String>,

    #[arg(long)]
    sha: Option<String>,
}

/// `map deploy` (ADR-0016 / map-cli#4): dispatch the thin `map-deploy.yml` workflow via
/// the GitHub API (`workflow_dispatch`) using the user's GitHub token. This NEVER calls the
/// control-plane directly — the control-plane listens on 127.0.0.1:4260 on the host and is
/// not reachable from GitHub-hosted runners. The dispatched workflow (which runs on a
/// self-hosted runner on the host, or via a public ingress — see templates/map-deploy.yml)
/// is what POSTs `/v1/map-control/deploy/request`. For the host-local direct call use
/// `map deploy-request`.
#[derive(Args)]
struct DeployArgs {
    /// Target app env (preview | staging | production). Passed as a workflow input.
    #[arg(long)]
    env: String,

    /// Git ref or 40-hex SHA to deploy (workflow input). Defaults to `--workflow-ref`.
    #[arg(long = "ref")]
    ref_name: Option<String>,

    /// Target repository `owner/repo`. Inferred from the git `origin` remote when omitted.
    #[arg(long)]
    repo: Option<String>,

    /// Workflow file to dispatch; must already exist in the repo (added by `map setup`).
    #[arg(long, default_value = "map-deploy.yml")]
    workflow: String,

    /// Git ref the workflow file lives on (the dispatch ref).
    #[arg(long, default_value = "main")]
    workflow_ref: String,

    /// GitHub token. Falls back to $GITHUB_TOKEN, then $GH_TOKEN, then `gh auth token`.
    #[arg(long)]
    github_token: Option<String>,

    /// GitHub API base URL (GHE). Falls back to $GITHUB_API_URL, then api.github.com.
    #[arg(long)]
    github_api_base: Option<String>,
}

/// `map deploy-request`: the host/runner-side primitive that POSTs straight to the
/// control-plane `/v1/map-control/deploy/request` (what `map-deploy.yml` does via curl).
/// Only works where the control-plane endpoint is reachable (host-local :4260 or a tunnel).
/// `--repo` accepts a bare `owner/repo` (normalized to `github://owner/repo`) or a full ref.
#[derive(Args, Serialize)]
struct DeployRequestArgs {
    #[command(flatten)]
    target: DeployTarget,

    /// GitHub App installation ref. REQUIRED for a real (non-smoke) deploy — the source
    /// broker rejects a missing/unknown installation_ref at source-resolve.
    #[arg(long)]
    installation_ref: Option<String>,

    /// App ref, e.g. `app:gtd-tracker`.
    #[arg(long)]
    app_ref: Option<String>,

    #[arg(long)]
    tenant_ref: Option<String>,

    #[arg(long)]
    account_ref: Option<String>,

    /// Platform env, e.g. `sandbox`.
    #[arg(long)]
    platform_env: Option<String>,

    /// Explicit deployment ref; the control-plane mints one when omitted.
    #[arg(long)]
    deployment_ref: Option<String>,

    #[arg(long)]
    evidence_ref: Option<String>,
}

/// `map doctor` (map-cli#6): readiness checks against the saved `map-control` endpoint.
#[derive(Args)]
struct DoctorArgs {
    /// Also diagnose a specific app `owner/repo` (allowlist + alias/recent deployment).
    #[arg(long)]
    app: Option<String>,
}

/// `map setup <owner/repo>` (map-cli#5): onboard an app.
#[derive(Args)]
struct SetupArgs {
    /// Repository to onboard, `owner/repo`.
    repo: String,

    /// Local checkout to write `.github/workflows/<workflow>` into (idempotent).
    #[arg(long)]
    repo_dir: Option<PathBuf>,

    /// Workflow filename written under `.github/workflows/`.
    #[arg(long, default_value = "map-deploy.yml")]
    workflow: String,
}

#[derive(Args)]
struct IdArgs {
    id: String,
}

#[derive(Args)]
struct WatchArgs {
    id: String,

    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,

    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
}

#[derive(Args, Serialize)]
struct RollbackArgs {
    id: String,

    #[arg(long)]
    evidence_ref: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct LoginState {
    map_control_endpoint: String,
    jason_controller_endpoint: Option<String>,
    access_token: String,
    expires_at: Option<String>,
    audience: Option<String>,
    #[serde(default)]
    scopes: Vec<String>,
    principal: Option<Principal>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Principal {
    email: Option<String>,
    name: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        eprintln!("map: {}", redact(&error));
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match &cli.command {
        Command::Login(LoginCommand {
            command: LoginSubcommand::Save(args),
        }) => {
            let state = LoginState {
                map_control_endpoint: args.map_control_endpoint.clone(),
                jason_controller_endpoint: args.jason_controller_endpoint.clone(),
                access_token: args.access_token.clone(),
                expires_at: args.expires_at.clone(),
                audience: Some(args.audience.clone()),
                scopes: args.scopes.clone(),
                principal: Some(Principal {
                    email: args.email.clone(),
                    name: args.name.clone(),
                }),
            };
            let path = login_state_path(cli.login_state.as_ref())?;
            write_login_state(&path, &state)?;
            print_json_or_text(cli.json, json!({ "ok": true, "path": path }), "login saved")
        }
        Command::Login(LoginCommand {
            command: LoginSubcommand::PrintToken(args),
        }) => {
            let state = resolve_state(&cli)?;
            if !audience_allowed(&state, &args.audience) {
                return Err(format!(
                    "login state is not valid for audience `{}`; run `map login`",
                    args.audience
                ));
            }
            println!("{}", state.access_token);
            Ok(())
        }
        Command::Whoami => {
            let state = resolve_state(&cli)?;
            let principal = state.principal.unwrap_or(Principal {
                email: None,
                name: None,
            });
            print_json_or_text(
                cli.json,
                json!({
                    "ok": true,
                    "endpoint": state.map_control_endpoint,
                    "email": principal.email,
                    "name": principal.name,
                    "audience": state.audience,
                    "scopes": state.scopes,
                }),
                "logged in",
            )
        }
        Command::Doctor(args) => doctor(&cli, args),
        Command::Init(args) => {
            if args.manifest.exists() {
                return Err(format!("{} already exists", args.manifest.display()));
            }
            fs::write(
                &args.manifest,
                "schema_version: mithran.map.v1\nname: example\n",
            )
            .map_err(|error| format!("write {}: {error}", args.manifest.display()))?;
            print_json_or_text(
                cli.json,
                json!({ "ok": true, "manifest": args.manifest }),
                "created mithran.yaml",
            )
        }
        Command::Validate(target) => {
            validate_target(target)?;
            print_json_or_text(cli.json, json!({ "ok": true }), "target is valid")
        }
        Command::Deploy(args) => deploy_dispatch(&cli, args),
        Command::DeployRequest(args) => {
            validate_target(&args.target)?;
            // The control-plane matches the allowlist on `github://owner/repo`; accept a bare
            // `owner/repo` and normalize it (a caller may also pass a full ref verbatim).
            let repository_ref = if args.target.repo.contains("://") {
                args.target.repo.clone()
            } else {
                format!("github://{}", args.target.repo)
            };
            post(
                &cli,
                "/v1/map-control/deploy/request",
                json!({
                    "deployment_ref": args.deployment_ref,
                    "repository_ref": repository_ref,
                    "installation_ref": args.installation_ref,
                    "app_ref": args.app_ref,
                    "app_env": args.target.env,
                    "tenant_ref": args.tenant_ref,
                    "account_ref": args.account_ref,
                    "platform_env": args.platform_env,
                    "requested_ref": args.target.ref_name,
                    "source_sha": args.target.sha,
                    "authority_evidence_ref": args.evidence_ref,
                }),
            )
        }
        Command::Setup(args) => setup(&cli, args),
        Command::Status(args) => get(
            &cli,
            "/v1/map-control/deploy/status",
            &[("deployment_ref", args.id.as_str())],
        ),
        Command::Watch(args) => watch(&cli, args),
        Command::Logs(_) => Err(
            "the live control plane exposes no deploy logs route; use `map status` or `map evidence`"
                .to_string(),
        ),
        Command::Evidence(args) => get(
            &cli,
            "/v1/map-control/deploy/evidence",
            &[("deployment_ref", args.id.as_str())],
        ),
        Command::Rollback(args) => post(
            &cli,
            "/v1/map-control/deploy/rollback",
            json!({
                "deployment_ref": args.id,
                "authority_evidence_ref": args.evidence_ref,
            }),
        ),
        Command::Version => print_json_or_text(
            cli.json,
            json!({ "name": "map-cli", "binary": "map", "version": VERSION }),
            VERSION,
        ),
    }
}

fn resolve_state(cli: &Cli) -> Result<LoginState, String> {
    if let (Some(endpoint), Some(token)) = (&cli.endpoint, &cli.token) {
        return Ok(LoginState {
            map_control_endpoint: endpoint.clone(),
            jason_controller_endpoint: None,
            access_token: token.clone(),
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec![],
            principal: None,
        });
    }
    let path = login_state_path(cli.login_state.as_ref())?;
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "read login state {}: {error}; run `map login`",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn login_state_path(override_path: Option<&PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        return Ok(path.clone());
    }
    if let Ok(path) = env::var("MITHRAN_LOGIN_STATE") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = env::var("AEGIS_LOGIN_STATE") {
        return Ok(PathBuf::from(path));
    }
    let config_home = env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map_err(|_| "HOME or XDG_CONFIG_HOME is required".to_string())?;
    Ok(config_home.join("mithran").join("login.json"))
}

fn write_login_state(path: &PathBuf, state: &LoginState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(state).expect("login state serializes");
    fs::write(path, text).map_err(|error| format!("write {}: {error}", path.display()))
}

fn audience_allowed(state: &LoginState, audience: &str) -> bool {
    state.audience.as_deref() == Some(audience)
        || state.scopes.iter().any(|scope| scope == "map:*")
        || state
            .scopes
            .iter()
            .any(|scope| scope == &format!("audience:{audience}"))
}

fn validate_target(target: &DeployTarget) -> Result<(), String> {
    if target.ref_name.is_none() && target.sha.is_none() {
        return Err("deploy target requires --ref or --sha".to_string());
    }
    if let Some(sha) = &target.sha {
        if sha.len() != 40 || !sha.chars().all(|char| char.is_ascii_hexdigit()) {
            return Err("--sha must be a 40-character Git SHA".to_string());
        }
    }
    Ok(())
}

fn build_client() -> Result<Client, String> {
    Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|error| format!("build HTTP client: {error}"))
}

fn client(cli: &Cli) -> Result<(Client, LoginState), String> {
    let state = resolve_state(cli)?;
    Ok((build_client()?, state))
}

fn get(cli: &Cli, path: &str, query: &[(&str, &str)]) -> Result<(), String> {
    let (client, state) = client(cli)?;
    let response = client
        .get(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            path
        ))
        .query(query)
        .bearer_auth(&state.access_token)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;
    print_response(cli.json, response)
}

fn post(cli: &Cli, path: &str, body: Value) -> Result<(), String> {
    let (client, state) = client(cli)?;
    let response = client
        .post(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            path
        ))
        .bearer_auth(&state.access_token)
        .json(&body)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;
    print_response(cli.json, response)
}

fn watch(cli: &Cli, args: &WatchArgs) -> Result<(), String> {
    let mut elapsed = 0;
    loop {
        let (client, state) = client(cli)?;
        let response = client
            .get(format!(
                "{}/v1/map-control/deploy/status",
                state.map_control_endpoint.trim_end_matches('/'),
            ))
            .query(&[("deployment_ref", args.id.as_str())])
            .bearer_auth(&state.access_token)
            .send()
            .map_err(|error| format!("MAP watch failed: {error}"))?;
        let value: Value = response
            .json()
            .map_err(|error| format!("read MAP watch response: {error}"))?;
        let phase = value
            .get("deployment")
            .and_then(|deployment| deployment.get("status"))
            .and_then(|status| status.get("status"))
            .and_then(|phase| phase.as_str())
            .unwrap_or("unknown");
        if cli.json {
            println!("{}", serde_json::to_string(&value).unwrap());
        } else {
            println!("{phase}");
        }
        if is_terminal_phase(phase) {
            return if phase == "Succeeded" {
                Ok(())
            } else {
                Err(format!("deploy reached terminal state {phase}"))
            };
        }
        if elapsed >= args.timeout_seconds {
            return Err("watch timed out".to_string());
        }
        thread::sleep(Duration::from_secs(args.interval_seconds));
        elapsed += args.interval_seconds;
    }
}

/// Terminal deployment phases reported by the control-plane deploy state
/// machine (see `mithran-control-plane` `DeploymentStatus`). Every phase here
/// stops the watch loop; only `Succeeded` is a success, so callers treat any
/// other terminal phase as a failure.
fn is_terminal_phase(phase: &str) -> bool {
    matches!(
        phase,
        "Succeeded"
            | "Failed"
            | "Superseded"
            | "RolledBack"
            | "ReviewBlocked"
            | "BuildFailed"
            | "RuntimeFailed"
            | "RouteFailed"
    )
}

fn print_response(json_output: bool, response: reqwest::blocking::Response) -> Result<(), String> {
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("read MAP response: {error}"))?;
    if status != StatusCode::OK && status != StatusCode::CREATED && status != StatusCode::ACCEPTED {
        return Err(format!("MAP returned {status}: {}", redact(&text)));
    }
    if json_output {
        println!("{text}");
    } else if let Some(deployment_ref) = serde_json::from_str::<Value>(&text)
        .ok()
        .as_ref()
        .and_then(|value| value.get("deployment_ref"))
        .and_then(|value| value.as_str())
    {
        // Surface the server-generated ref so the user can run `map status`,
        // `map watch`, or `map evidence` against it.
        println!("{deployment_ref}");
    } else {
        println!("ok");
    }
    Ok(())
}

fn print_json_or_text(json_output: bool, payload: Value, text: &str) -> Result<(), String> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        println!("{text}");
    }
    Ok(())
}

fn redact(text: &str) -> String {
    let mut redacted = text.to_string();
    for marker in ["access_token", "Authorization", "Bearer"] {
        if redacted.contains(marker) {
            redacted = redacted.replace(marker, "[REDACTED]");
        }
    }
    redacted
}

// ───────────────────────────── map deploy (workflow_dispatch) ─────────────────────────────

/// Dispatch the thin `map-deploy.yml` workflow (ADR-0016 / map-cli#4). We POST a
/// `workflow_dispatch` to the GitHub API with the user's GitHub token; the deployed ref +
/// env ride along as workflow inputs. The control-plane is host-local (127.0.0.1:4260) and
/// unreachable from Actions, so this command deliberately does NOT talk to it — the
/// dispatched workflow does, from a self-hosted runner on the host (or a public ingress).
fn deploy_dispatch(cli: &Cli, args: &DeployArgs) -> Result<(), String> {
    let repo = match &args.repo {
        Some(repo) => repo.clone(),
        None => infer_repo_from_git()
            .ok_or("could not infer --repo from git `origin`; pass --repo <owner/repo>")?,
    };
    validate_repo_slug(&repo)?;

    // Empty when --ref is omitted: the workflow input defaults to "" and the template falls
    // back to $GITHUB_REF (the dispatch ref), rather than a bare branch name.
    let deploy_ref = args.ref_name.clone().unwrap_or_default();
    let token = resolve_github_token(args.github_token.as_deref())?;
    let api_base = args
        .github_api_base
        .clone()
        .or_else(|| env::var("GITHUB_API_URL").ok())
        .unwrap_or_else(|| "https://api.github.com".to_string());

    let url = format!(
        "{}/repos/{}/actions/workflows/{}/dispatches",
        api_base.trim_end_matches('/'),
        repo,
        args.workflow,
    );
    let body = json!({
        "ref": args.workflow_ref,
        "inputs": { "env": args.env, "ref": deploy_ref },
    });

    let response = build_client()?
        .post(&url)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .bearer_auth(&token)
        .json(&body)
        .send()
        .map_err(|error| format!("workflow_dispatch failed: {error}"))?;

    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        let runs_url = format!(
            "https://github.com/{}/actions/workflows/{}",
            repo, args.workflow
        );
        return print_json_or_text(
            cli.json,
            json!({
                "ok": true,
                "dispatched": true,
                "repo": repo,
                "workflow": args.workflow,
                "workflow_ref": args.workflow_ref,
                "env": args.env,
                "ref": deploy_ref,
                "runs_url": runs_url,
            }),
            &format!(
                "dispatched {} on {} (env={}, ref={}); watch the run at {}",
                args.workflow,
                repo,
                args.env,
                if deploy_ref.is_empty() {
                    "(dispatch ref)"
                } else {
                    &deploy_ref
                },
                runs_url
            ),
        );
    }
    let text = response.text().unwrap_or_default();
    if status == StatusCode::NOT_FOUND {
        return Err(format!(
            "GitHub returned 404 for workflow `{}` on {}@{} — the workflow may be missing (run `map setup {}`, commit + push), or the repo/ref is wrong, or the token lacks access",
            args.workflow, repo, args.workflow_ref, repo
        ));
    }
    Err(format!(
        "GitHub workflow_dispatch returned {status}: {}",
        redact(&text)
    ))
}

/// Resolve a GitHub token from (in order): the flag, $GITHUB_TOKEN, $GH_TOKEN, `gh auth token`.
fn resolve_github_token(explicit: Option<&str>) -> Result<String, String> {
    if let Some(token) = explicit {
        if !token.is_empty() {
            return Ok(token.to_string());
        }
    }
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(token) = env::var(var) {
            if !token.is_empty() {
                return Ok(token);
            }
        }
    }
    if let Ok(output) = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
    {
        if output.status.success() {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                return Ok(token);
            }
        }
    }
    Err(
        "no GitHub token: pass --github-token, set $GITHUB_TOKEN/$GH_TOKEN, or run `gh auth login`"
            .to_string(),
    )
}

/// Best-effort `owner/repo` from the local git `origin` remote.
fn infer_repo_from_git() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_repo_slug(&String::from_utf8_lossy(&output.stdout))
}

/// Extract `owner/repo` from a GitHub remote URL (ssh, https, or scp-like), trimming `.git`.
fn parse_repo_slug(url: &str) -> Option<String> {
    let after = url.trim().rsplit_once("github.com")?.1;
    let path = after.trim_start_matches([':', '/']);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.trim_end_matches('/').split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn validate_repo_slug(repo: &str) -> Result<(), String> {
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
        return Err(format!("`{repo}` is not a valid `owner/repo`"));
    }
    Ok(())
}

// ───────────────────────────── map setup (onboarding) ─────────────────────────────

/// Onboard an app (map-cli#5). Two parts: (a) drop the `map-deploy.yml` workflow into a
/// local checkout when `--repo-dir` is given; (b) print the host-local onboarding steps
/// (allowlist + bare-mirror create + `map-mirror-sync`) the operator/automation must run.
///
/// DEFERRED: the host steps exist only because there is no control-plane `onboard` endpoint
/// yet — the CLI has no host access, so it cannot create the mirror or edit the allowlist
/// itself. The proper fix (flagged on map-cli#5) is a single authenticated `onboard` API
/// call (allowlist via durable drop-in + broker-driven mirror create + installation
/// allowlist), so `map setup` stops printing host commands and just calls one endpoint.
fn setup(cli: &Cli, args: &SetupArgs) -> Result<(), String> {
    validate_repo_slug(&args.repo)?;
    let (owner, repo_name) = args.repo.split_once('/').expect("validated owner/repo");

    let mut workflow_path: Option<PathBuf> = None;
    if let Some(repo_dir) = &args.repo_dir {
        let dir = repo_dir.join(".github").join("workflows");
        fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
        let path = dir.join(&args.workflow);
        fs::write(&path, MAP_DEPLOY_WORKFLOW_TEMPLATE)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        workflow_path = Some(path);
    }

    let steps = host_onboarding_steps(owner, repo_name);

    if cli.json {
        let payload = json!({
            "ok": true,
            "schema_version": "map.setup.v1",
            "repo": args.repo,
            "workflow_written": workflow_path.as_ref().map(|path| path.display().to_string()),
            "host_steps": steps,
            "deferred": {
                "onboard_endpoint": "no control-plane `onboard` endpoint yet; host steps are printed for an operator/automation to run (map-cli#5)",
                "broker_mirror_create": "broker should create the bare mirror in the source phase; `map-mirror-sync` only REFRESHES an existing one (map-cli#5)",
                "durable_allowlist": "prefer a systemd drop-in / provisioning template over a manual service.env edit (ADR-0016, mithran-auth#93)"
            }
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return Ok(());
    }

    match &workflow_path {
        Some(path) => println!("wrote {} ({})", path.display(), args.workflow),
        None => println!(
            "(no --repo-dir given; skipped workflow drop — pass --repo-dir <checkout> to write .github/workflows/{})",
            args.workflow
        ),
    }
    println!();
    println!(
        "Host onboarding for {} — run on the control-plane host (no `onboard` API yet):",
        args.repo
    );
    for (index, step) in steps.iter().enumerate() {
        println!(
            "  {}. {}",
            index + 1,
            step["title"].as_str().unwrap_or_default()
        );
        if let Some(command) = step["command"].as_str() {
            println!("       {command}");
        }
    }
    println!();
    println!("DEFERRED (map-cli#5): replace the host steps above with a control-plane `onboard` endpoint");
    println!("  so `map setup` is one authenticated API call — durable allowlist (drop-in, not service.env),");
    println!("  broker-driven bare-mirror create (today `map-mirror-sync` only REFRESHES), installation allowlist.");
    Ok(())
}

/// The host-local onboarding commands, faithful to the clean-room runbook §2.
fn host_onboarding_steps(owner: &str, repo: &str) -> Vec<Value> {
    let slug = format!("{owner}/{repo}");
    let mirror = format!("/var/lib/map-source-repos/{owner}/{repo}.git");
    vec![
        json!({
            "title": "Allowlist the repo (durable: prefer a systemd drop-in over editing service.env)",
            "command": format!(
                "sudo sed -i \"s#\\(MAP_LIVE_SOURCE_ALLOWED_REPOSITORIES=.*\\)#\\1,github://{slug}#\" /etc/mithran-control-plane/service.env"
            )
        }),
        json!({
            "title": "Create the host-local bare source mirror (first-time; map-mirror-sync only REFRESHES)",
            "command": format!("sudo -u mithran-control-plane git init --bare -q {mirror}")
        }),
        json!({
            "title": "Point the mirror at GitHub (map-mirror-sync's `git remote set-url` needs an existing origin)",
            "command": format!("sudo -u mithran-control-plane git -C {mirror} remote add origin https://github.com/{slug}.git")
        }),
        json!({
            "title": "Sync the mirror with a minted GitHub-App installation token",
            "command": format!("sudo map-mirror-sync {slug}")
        }),
        json!({
            "title": "Restart the control-plane so it reloads the allowlist",
            "command": "sudo systemctl restart mithran-control-plane"
        }),
        json!({
            "title": "Verify the repo is allowlisted (the count should increase)",
            "command": "curl -s :4260/v1/map-control/config | jq .source_snapshot_storage.live_source_broker.allowed_repository_count"
        }),
        json!({
            "title": "installation_ref must be in MAP_LIVE_SOURCE_ALLOWED_INSTALLATIONS",
            "command": "# reuse the org-wide github-installation://131136661 — usually no change needed"
        }),
    ]
}

// ───────────────────────────── map doctor (readiness) ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

struct Check {
    level: Level,
    name: String,
    detail: String,
    remediation: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Check {
            level: Level::Ok,
            name: name.to_string(),
            detail: detail.into(),
            remediation: None,
        }
    }

    fn warn(name: &str, detail: impl Into<String>, remediation: impl Into<String>) -> Self {
        Check {
            level: Level::Warn,
            name: name.to_string(),
            detail: detail.into(),
            remediation: Some(remediation.into()),
        }
    }

    fn fail(name: &str, detail: impl Into<String>, remediation: impl Into<String>) -> Self {
        Check {
            level: Level::Fail,
            name: name.to_string(),
            detail: detail.into(),
            remediation: Some(remediation.into()),
        }
    }
}

fn format_check(check: &Check) -> String {
    let base = format!(
        "[{:>4}] {} — {}",
        check.level.label(),
        check.name,
        check.detail
    );
    match &check.remediation {
        Some(remediation) => format!("{base}\n         ↳ {remediation}"),
        None => base,
    }
}

fn check_value(check: &Check) -> Value {
    json!({
        "level": check.level.label(),
        "name": check.name,
        "detail": check.detail,
        "remediation": check.remediation,
    })
}

/// Readiness diagnostics against the saved `map-control` endpoint (map-cli#6).
///
/// NOTE / DEFERRED: this implements only the checks reachable through the public
/// `map-control` API (`/v1/map-control/config` + `/v1/map-control/routes/status`). The
/// deeper host-side checks listed on map-cli#6 — other service ports (runtime-control,
/// deploy-review, llm-proxy, auth, sidecar, D3, edge), the mirror HEAD vs GitHub, Ed25519
/// attestation wiring, the M2M token mint, the smoke config — need host access or a
/// control-plane diagnostics endpoint and are deferred to that endpoint.
fn doctor(cli: &Cli, args: &DoctorArgs) -> Result<(), String> {
    if let Some(app) = &args.app {
        validate_repo_slug(app)?;
    }
    let mut checks: Vec<Check> = Vec::new();

    let state = match resolve_state(cli) {
        Ok(state) => state,
        Err(error) => {
            checks.push(Check::fail(
                "control-plane configured",
                redact(&error),
                "run `map login save --map-control-endpoint <url> --access-token <token>`",
            ));
            return emit_doctor(cli, &checks);
        }
    };
    checks.push(Check::ok(
        "control-plane configured",
        state.map_control_endpoint.clone(),
    ));

    let http = build_client()?;

    let config = match fetch_json(&http, &state, "/v1/map-control/config") {
        Ok(value) => {
            checks.push(Check::ok(
                "control-plane reachable",
                "GET /v1/map-control/config 200",
            ));
            Some(value)
        }
        Err(error) => {
            checks.push(Check::fail(
                "control-plane reachable",
                redact(&error),
                "ensure the control-plane (:4260) is running and the saved endpoint is reachable (host-local, or via a tunnel/ingress)",
            ));
            None
        }
    };

    if let Some(config) = &config {
        checks.push(adapter_check(config));
        checks.push(allowlist_check(config));
    }

    let routes = fetch_json(&http, &state, "/v1/map-control/routes/status").ok();

    if let Some(app) = &args.app {
        // routes/status serializes DeploymentStatusView, which has NO repository_ref field; the
        // owner/repo slug appears via source_snapshot_ref (".../<owner>/<repo>/source-…tar.gz"),
        // so a recursive match finds it wherever it nests. Limitation: a deploy that failed
        // BEFORE the source snapshot has source_snapshot_ref=null and won't be detected here.
        let has_deployment = json_mentions(routes.as_ref().and_then(|r| r.get("deployments")), app);
        let app_name = app.rsplit('/').next().unwrap_or(app);
        let has_alias = json_mentions(routes.as_ref().and_then(|r| r.get("aliases")), app_name);
        checks.push(app_allowlist_check(
            config.as_ref().and_then(allowlist_count),
            has_deployment,
            app,
        ));
        checks.push(app_route_check(has_alias, has_deployment, app));
    }

    emit_doctor(cli, &checks)
}

fn adapter_check(config: &Value) -> Check {
    match config.get("adapter_mode").and_then(Value::as_str) {
        Some("sandbox-live") => Check::ok("adapter mode", "sandbox-live"),
        Some(other) => Check::warn(
            "adapter mode",
            format!("adapter_mode={other} (not sandbox-live)"),
            "set the control-plane adapter to sandbox-live for live deploys",
        ),
        None => Check::warn(
            "adapter mode",
            "adapter_mode missing from config",
            "control-plane config did not report adapter_mode; check the service version",
        ),
    }
}

fn allowlist_count(config: &Value) -> Option<u64> {
    config
        .get("source_snapshot_storage")?
        .get("live_source_broker")?
        .get("allowed_repository_count")?
        .as_u64()
}

fn allowlist_check(config: &Value) -> Check {
    match allowlist_count(config) {
        Some(0) => Check::fail(
            "source allowlist",
            "0 repositories allowlisted",
            "onboard a repo with `map setup <owner/repo>` (allowlist + mirror)",
        ),
        Some(count) => Check::ok(
            "source allowlist",
            format!(
                "{count} repositor{} allowlisted",
                if count == 1 { "y" } else { "ies" }
            ),
        ),
        None => Check::warn(
            "source allowlist",
            "allowed_repository_count missing from config",
            "control-plane config did not report the broker allowlist; check the service version",
        ),
    }
}

/// Per-app allowlist signal. The config endpoint exposes only a COUNT, not the allowed
/// repository list, so membership can't be read directly. A repo with a recorded deployment
/// was provably allowlisted at deploy time — use that as the positive signal.
fn app_allowlist_check(allowlist_count: Option<u64>, has_deployment: bool, app: &str) -> Check {
    if has_deployment {
        return Check::ok(
            "app allowlisted",
            format!("{app} has a recorded deployment (implies it was allowlisted)"),
        );
    }
    match allowlist_count {
        Some(0) => Check::fail(
            "app allowlisted",
            format!("no repositories allowlisted — {app} cannot deploy"),
            format!("run `map setup {app}`"),
        ),
        _ => Check::warn(
            "app allowlisted",
            format!("cannot confirm {app} is allowlisted (the config endpoint exposes only a count, not the list)"),
            format!("run `map setup {app}` or verify the host allowlist; a control-plane allowlist-membership query would let doctor check this directly"),
        ),
    }
}

fn app_route_check(has_alias: bool, has_deployment: bool, app: &str) -> Check {
    let name = app.rsplit('/').next().unwrap_or(app);
    if has_alias {
        Check::ok(
            "app route/alias",
            format!("a route alias references `{name}`"),
        )
    } else if has_deployment {
        Check::warn(
            "app route/alias",
            format!("{app} has a deployment but no live route alias was found"),
            "promote/pin the app's production alias (control-plane routes/alias)",
        )
    } else {
        Check::warn(
            "app route/alias",
            format!("no deployment or route alias found for {app}"),
            format!("after `map setup {app}`, deploy with `map deploy --env preview --repo {app}`"),
        )
    }
}

/// Recursively test whether any string value within `value` contains `needle`.
fn json_mentions(value: Option<&Value>, needle: &str) -> bool {
    match value {
        Some(Value::String(text)) => text.contains(needle),
        Some(Value::Array(items)) => items.iter().any(|item| json_mentions(Some(item), needle)),
        Some(Value::Object(map)) => map.values().any(|item| json_mentions(Some(item), needle)),
        _ => false,
    }
}

fn fetch_json(client: &Client, state: &LoginState, path: &str) -> Result<Value, String> {
    let response = client
        .get(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            path
        ))
        .bearer_auth(&state.access_token)
        .send()
        .map_err(|error| format!("request {path} failed: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("read {path}: {error}"))?;
    if status != StatusCode::OK {
        return Err(format!("{path} returned {status}: {}", redact(&text)));
    }
    serde_json::from_str(&text).map_err(|error| format!("parse {path}: {error}"))
}

fn emit_doctor(cli: &Cli, checks: &[Check]) -> Result<(), String> {
    let any_fail = checks.iter().any(|check| check.level == Level::Fail);
    if cli.json {
        let payload = json!({
            "ok": !any_fail,
            "schema_version": "map.doctor.v2",
            "checks": checks.iter().map(check_value).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        for check in checks {
            println!("{}", format_check(check));
        }
    }
    if any_fail {
        Err("doctor found failing checks".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_every_control_plane_terminal_phase() {
        for phase in [
            "Succeeded",
            "Failed",
            "Superseded",
            "RolledBack",
            "ReviewBlocked",
            "BuildFailed",
            "RuntimeFailed",
            "RouteFailed",
        ] {
            assert!(is_terminal_phase(phase), "{phase} must be terminal");
        }
        for phase in ["IntentReceived", "BuildPending", "RuntimeReady", "unknown"] {
            assert!(!is_terminal_phase(phase), "{phase} must not be terminal");
        }
    }

    #[test]
    fn validates_git_sha() {
        let target = DeployTarget {
            repo: "mithran-hq/demo".to_string(),
            env: None,
            ref_name: None,
            sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        };
        assert!(validate_target(&target).is_ok());
    }

    #[test]
    fn rejects_missing_ref_and_sha() {
        let target = DeployTarget {
            repo: "mithran-hq/demo".to_string(),
            env: None,
            ref_name: None,
            sha: None,
        };
        assert!(validate_target(&target).is_err());
    }

    #[test]
    fn allows_audience_by_scope() {
        let state = LoginState {
            map_control_endpoint: "https://map.example".to_string(),
            jason_controller_endpoint: None,
            access_token: "secret".to_string(),
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec!["audience:jason-controller".to_string()],
            principal: None,
        };
        assert!(audience_allowed(&state, "jason-controller"));
    }

    #[test]
    fn deploy_parses_dispatch_args_with_defaults() {
        let cli = Cli::try_parse_from([
            "map",
            "deploy",
            "--env",
            "staging",
            "--repo",
            "mithran-hq/demo",
            "--ref",
            "refs/heads/release/1.2",
        ])
        .expect("parses");
        match cli.command {
            Command::Deploy(args) => {
                assert_eq!(args.env, "staging");
                assert_eq!(args.repo.as_deref(), Some("mithran-hq/demo"));
                assert_eq!(args.ref_name.as_deref(), Some("refs/heads/release/1.2"));
                assert_eq!(args.workflow, "map-deploy.yml");
                assert_eq!(args.workflow_ref, "main");
            }
            _ => panic!("expected deploy"),
        }
    }

    #[test]
    fn deploy_request_is_a_distinct_subcommand() {
        let cli = Cli::try_parse_from([
            "map",
            "deploy-request",
            "--repo",
            "github://mithran-hq/demo",
            "--ref",
            "refs/heads/main",
        ])
        .expect("parses");
        assert!(matches!(cli.command, Command::DeployRequest(_)));
    }

    #[test]
    fn setup_parses_repo_and_repo_dir() {
        let cli = Cli::try_parse_from(["map", "setup", "mithran-hq/demo", "--repo-dir", "/tmp/x"])
            .expect("parses");
        match cli.command {
            Command::Setup(args) => {
                assert_eq!(args.repo, "mithran-hq/demo");
                assert_eq!(args.repo_dir, Some(PathBuf::from("/tmp/x")));
                assert_eq!(args.workflow, "map-deploy.yml");
            }
            _ => panic!("expected setup"),
        }
    }

    #[test]
    fn doctor_parses_optional_app() {
        let cli =
            Cli::try_parse_from(["map", "doctor", "--app", "mithran-hq/demo"]).expect("parses");
        match cli.command {
            Command::Doctor(args) => assert_eq!(args.app.as_deref(), Some("mithran-hq/demo")),
            _ => panic!("expected doctor"),
        }
    }

    #[test]
    fn parses_repo_slug_from_ssh_https_and_scp() {
        for url in [
            "git@github.com:mithran-hq/demo.git",
            "https://github.com/mithran-hq/demo.git",
            "https://github.com/mithran-hq/demo",
            "ssh://git@github.com/mithran-hq/demo.git\n",
        ] {
            assert_eq!(
                parse_repo_slug(url).as_deref(),
                Some("mithran-hq/demo"),
                "{url}"
            );
        }
        assert_eq!(parse_repo_slug("https://gitlab.com/x/y.git"), None);
    }

    #[test]
    fn validates_repo_slug() {
        assert!(validate_repo_slug("mithran-hq/demo").is_ok());
        assert!(validate_repo_slug("demo").is_err());
        assert!(validate_repo_slug("a/b/c").is_err());
        assert!(validate_repo_slug("/demo").is_err());
    }

    #[test]
    fn template_is_a_thin_dispatch_to_the_control_plane() {
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("workflow_dispatch"));
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("/v1/map-control/deploy/request"));
        // Records the ingress gap: must run host-local (self-hosted) to reach :4260.
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("self-hosted"));
    }

    #[test]
    fn format_check_renders_level_and_remediation() {
        let ok = format_check(&Check::ok("control-plane reachable", "200"));
        assert_eq!(ok, "[  ok] control-plane reachable — 200");
        let fail = format_check(&Check::fail(
            "source allowlist",
            "0 repositories",
            "run setup",
        ));
        assert!(fail.starts_with("[fail] source allowlist — 0 repositories"));
        assert!(fail.contains("↳ run setup"));
    }

    #[test]
    fn adapter_check_classifies_sandbox_live() {
        assert_eq!(
            adapter_check(&json!({ "adapter_mode": "sandbox-live" })).level,
            Level::Ok
        );
        assert_eq!(
            adapter_check(&json!({ "adapter_mode": "stub" })).level,
            Level::Warn
        );
        assert_eq!(adapter_check(&json!({})).level, Level::Warn);
    }

    #[test]
    fn allowlist_check_classifies_count() {
        let config = |count: u64| {
            json!({
                "source_snapshot_storage": {
                    "live_source_broker": { "allowed_repository_count": count }
                }
            })
        };
        assert_eq!(allowlist_check(&config(0)).level, Level::Fail);
        let one = allowlist_check(&config(1));
        assert_eq!(one.level, Level::Ok);
        assert!(one.detail.contains("1 repository"));
        let two = allowlist_check(&config(2));
        assert_eq!(two.level, Level::Ok);
        assert!(two.detail.contains("2 repositories"));
        assert_eq!(allowlist_check(&json!({})).level, Level::Warn);
    }

    #[test]
    fn app_checks_use_deployment_and_alias_signals() {
        // A recorded deployment implies allowlisted, even though config exposes only a count.
        assert_eq!(
            app_allowlist_check(Some(2), true, "mithran-hq/demo").level,
            Level::Ok
        );
        // No deployment + nothing allowlisted is a hard fail.
        assert_eq!(
            app_allowlist_check(Some(0), false, "mithran-hq/demo").level,
            Level::Fail
        );
        // Allowlisted-count > 0 but no deployment for this repo: can't confirm → warn.
        assert_eq!(
            app_allowlist_check(Some(2), false, "mithran-hq/demo").level,
            Level::Warn
        );
        assert_eq!(
            app_route_check(true, true, "mithran-hq/demo").level,
            Level::Ok
        );
        assert_eq!(
            app_route_check(false, false, "mithran-hq/demo").level,
            Level::Warn
        );
    }

    #[test]
    fn json_mentions_finds_slug_via_source_snapshot_ref() {
        // Real shape: routes/status deployments are DeploymentStatusView (no repository_ref);
        // the owner/repo slug surfaces through source_snapshot_ref.
        let routes = json!({
            "deployments": {
                "deployment://sandbox/production/demo-1": {
                    "deployment_ref": "deployment://sandbox/production/demo-1",
                    "status": "Succeeded",
                    "source_snapshot_ref": "gs://map-source-snapshots/mithran-hq/demo/source-cb2ab44-abcd.tar.gz"
                }
            }
        });
        assert!(json_mentions(routes.get("deployments"), "mithran-hq/demo"));
        assert!(!json_mentions(
            routes.get("deployments"),
            "mithran-hq/other"
        ));
    }

    #[test]
    fn emit_doctor_fails_when_any_check_fails() {
        let any_fail = [Check::ok("a", "x"), Check::fail("b", "y", "fix")]
            .iter()
            .any(|check| check.level == Level::Fail);
        assert!(any_fail);
    }
}
