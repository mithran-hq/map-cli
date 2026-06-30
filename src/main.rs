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

/// The reusable `map-deploy.yml` workflow template. `map onboard --repo-dir` writes this
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
    /// Onboard a repo: one authenticated control-plane `/onboard` call (records the source
    /// registry binding — P2a/P2b) + a local scaffold of the deploy workflow + manifest.
    Onboard(OnboardArgs),
    /// DEPRECATED: use `map onboard`. Local workflow scaffold only (no host steps, no API call).
    Setup(SetupArgs),
    /// ADR-0019 (app access & sharing): declare who can reach a protected app as code in
    /// `access.yaml`, then reconcile it into the control-plane. `apply` takes effect hot
    /// (next deploy route push); `plan` prints the resolved policy without applying.
    Access(AccessArgs),
    /// ADR-0018 (#63): list an app's addressable internal versions, its aliases, and which
    /// internal version is currently published to the clean public URL.
    Versions(VersionsArgs),
    /// ADR-0018 (#63): publish a reviewed, succeeded internal version to the app's clean
    /// public URL (review-gated + stale-safe; pins the external published pointer).
    Publish(PublishArgs),
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

    /// Workflow file to dispatch; must already exist in the repo (added by `map onboard`).
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

/// `map setup <owner/repo>` (DEPRECATED — use `map onboard`): legacy scaffold-only shim.
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

/// `map onboard <owner/repo>` (P3a, mithran-business#531): one authenticated call to the
/// control-plane `/onboard` endpoint (records the source-registry binding — P2a/P2b) plus a
/// local scaffold of the deploy workflow + manifest. Replaces `map setup`'s host-step printing.
#[derive(Args)]
struct OnboardArgs {
    /// Repository to onboard, `owner/repo`.
    repo: String,

    /// GitHub App installation ref authorizing the repo, e.g. `github-installation://131136661`.
    /// (Auto-resolution from the caller's identity + App grant is P3b.)
    #[arg(long)]
    installation_ref: String,

    /// Tenant ref (provisioning identity); the control-plane records it on the binding.
    #[arg(long)]
    tenant_ref: Option<String>,

    /// Account ref (provisioning identity).
    #[arg(long)]
    account_ref: Option<String>,

    /// Project ref; defaults to `app:<repo-name>`.
    #[arg(long)]
    project_ref: Option<String>,

    /// Local checkout to scaffold `.github/workflows/<workflow>` + `mithran.yaml` into.
    #[arg(long)]
    repo_dir: Option<PathBuf>,

    /// Workflow filename written under `.github/workflows/`.
    #[arg(long, default_value = "map-deploy.yml")]
    workflow: String,
}

#[derive(Args)]
struct AccessArgs {
    #[command(subcommand)]
    command: AccessSubcommand,
}

#[derive(Subcommand)]
enum AccessSubcommand {
    /// Reconcile the app's `access.yaml` into the control-plane (hot — enforced on the next
    /// deploy route push, no rollout).
    Apply(AccessApplyArgs),
    /// Print the resolved access policy without applying it (no control-plane call).
    Plan(AccessApplyArgs),
}

#[derive(Args)]
struct AccessApplyArgs {
    /// Directory containing `access.yaml` (default: current directory).
    #[arg(long)]
    repo_dir: Option<PathBuf>,

    /// Explicit path to the access file (overrides `--repo-dir`/`access.yaml`).
    #[arg(long)]
    file: Option<PathBuf>,

    /// App ref to apply to; overrides `app_ref` in `access.yaml`. Required if the file omits it.
    #[arg(long)]
    app_ref: Option<String>,

    /// Tenant ref; overrides `tenant_ref` in `access.yaml`.
    #[arg(long)]
    tenant_ref: Option<String>,

    /// Account ref; overrides `account_ref` in `access.yaml`.
    #[arg(long)]
    account_ref: Option<String>,

    /// Exposure (`public` | `protected`); overrides `exposure` in `access.yaml`.
    #[arg(long)]
    exposure: Option<String>,
}

/// `map versions <app>` (ADR-0018 / #63 Phase 2c): list an app's per-version pointers,
/// its aliases (production/preview/release), and which internal version the clean public
/// URL is currently published to. Read-only — GET `/v1/map-control/routes/status`.
#[derive(Args)]
struct VersionsArgs {
    /// App name (e.g. `gtd-tracker`); normalized to `app:<app>`. Accepts a literal `app:` ref.
    app: String,
}

/// `map publish <app>` (ADR-0018 / #63 Phase 2c): pin the app's external published pointer
/// (the clean, env-bare public URL) to a chosen healthy internal version. Resolve the version
/// from `--deployment-ref`, or look one up by `--version <label>` (see `map versions`).
#[derive(Args)]
struct PublishArgs {
    /// App name (e.g. `gtd-tracker`); normalized to `app:<app>`. Accepts a literal `app:` ref.
    app: String,

    /// Internal version label to publish (resolved to a deployment_ref via `routes/status`).
    /// Mutually exclusive with `--deployment-ref`. Pick a label from `map versions <app>`.
    #[arg(long, conflicts_with = "deployment_ref")]
    version: Option<String>,

    /// Explicit deployment ref to publish (skips the `--version` lookup).
    #[arg(long = "deployment-ref")]
    deployment_ref: Option<String>,

    /// Stale-safe guard: the version must still record this exact source SHA, else the
    /// control-plane rejects with 409 (publish precisely what was reviewed).
    #[arg(long = "expected-sha")]
    expected_sha: Option<String>,

    /// Actor ref to attribute the publish to. The control-plane defaults one when omitted.
    #[arg(long)]
    actor: Option<String>,
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
        Command::Onboard(args) => onboard(&cli, args),
        Command::Setup(args) => setup(&cli, args),
        Command::Access(args) => match &args.command {
            AccessSubcommand::Apply(apply) => access_apply(&cli, apply),
            AccessSubcommand::Plan(plan) => access_plan(&cli, plan),
        },
        Command::Versions(args) => map_versions(&cli, args),
        Command::Publish(args) => map_publish(&cli, args),
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
            "GitHub returned 404 for workflow `{}` on {}@{} — the workflow may be missing (run `map onboard {}`, commit + push), or the repo/ref is wrong, or the token lacks access",
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

// ───────────────────────────── map onboard ─────────────────────────────

/// `map onboard <owner/repo>` (P3a). One authenticated call to the control-plane `/onboard`
/// endpoint records the source-registry binding (P2a/P2b) — so the repo passes the source
/// broker allowlist with no restart — then scaffolds the deploy workflow + a starter manifest
/// into `--repo-dir`. Supersedes `map setup`'s host-step printing.
///
/// Scope: registry binding + scaffold. Setting repo Variables (`MAP_*`) + the `MAP_CONTROL_TOKEN`
/// secret via the GitHub API is a focused follow-up (map-cli#11). Auto-resolving the installation
/// from the caller's identity + App grant is P3b (mithran-control-plane#79).
fn onboard(cli: &Cli, args: &OnboardArgs) -> Result<(), String> {
    validate_repo_slug(&args.repo)?;
    let (_owner, repo_name) = args.repo.split_once('/').expect("validated owner/repo");
    let repository_ref = format!("github://{}", args.repo);
    let project_ref = args
        .project_ref
        .clone()
        .unwrap_or_else(|| format!("app:{repo_name}"));

    let (client, state) = client(cli)?;
    let response = client
        .post(format!(
            "{}/v1/map-control/onboard",
            state.map_control_endpoint.trim_end_matches('/')
        ))
        .bearer_auth(&state.access_token)
        .json(&json!({
            "repository_ref": repository_ref,
            "installation_ref": args.installation_ref,
            "tenant_ref": args.tenant_ref,
            "account_ref": args.account_ref,
            "project_ref": project_ref,
        }))
        .send()
        .map_err(|error| format!("onboard request failed: {error}"))?;

    let status = response.status();
    let value: Value = response.json().unwrap_or_else(|_| json!({}));
    // The repo grant is missing — guide the developer to finish the GitHub App install/grant,
    // then re-run (onboard is idempotent). (Server-side grant verification lands in cp#84.)
    if status == StatusCode::CONFLICT {
        if let Some(url) = value.get("install_url").and_then(Value::as_str) {
            return Err(format!(
                "GitHub App grant required for {}: install/grant it at {} then re-run `map onboard`",
                args.repo, url
            ));
        }
        return Err(format!("onboard conflict: {}", redact(&value.to_string())));
    }
    if !status.is_success() {
        return Err(format!(
            "onboard returned {status}: {}",
            redact(&value.to_string())
        ));
    }

    let workflow_path = scaffold_deploy_workflow(args.repo_dir.as_ref(), &args.workflow)?;
    let manifest_path = scaffold_manifest(args.repo_dir.as_ref(), repo_name)?;

    // map-cli#13: set the non-secret repo Variables the OIDC map-deploy.yml reads (no secrets —
    // auth is GitHub OIDC). Best-effort: skips with a note if no GitHub token is available.
    let variables = onboard_variables(args, &project_ref);
    let variables_outcome = set_repo_variables(&args.repo, &variables);

    print_json_or_text(
        cli.json,
        json!({
            "ok": true,
            "schema_version": "map.onboard.v1",
            "repo": args.repo,
            "onboard": value,
            "workflow_written": workflow_path.as_ref().map(|p| p.display().to_string()),
            "manifest_written": manifest_path.as_ref().map(|p| p.display().to_string()),
            "variables": variables_outcome,
            "next": "commit + push the scaffolded files, then push a release/* ref (or tag) to deploy",
        }),
        &format!(
            "onboarded {} (registry binding recorded).{}{}\nnext: commit + push the scaffold, then push a release/* ref (or tag) to deploy.",
            args.repo,
            workflow_path
                .as_ref()
                .map(|p| format!("\nwrote {}", p.display()))
                .unwrap_or_else(|| "\n(no --repo-dir; skipped workflow scaffold)".to_string()),
            manifest_path
                .as_ref()
                .map(|p| format!("\nwrote {}", p.display()))
                .unwrap_or_default(),
        ),
    )
}

/// ADR-0019: the `access.yaml` schema — an app's access policy declared as code. Every field
/// is optional in the file; `app_ref` must be resolvable (file or `--app-ref`). Unknown keys
/// are rejected so a typo (e.g. `allowed_domain`) fails loudly instead of silently widening or
/// narrowing access.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessFile {
    #[serde(default)]
    app_ref: Option<String>,
    #[serde(default)]
    tenant_ref: Option<String>,
    #[serde(default)]
    account_ref: Option<String>,
    /// `public` (anyone) | `protected` (signed-in + policy). Defaults to `protected` when a
    /// policy is declared as code.
    #[serde(default)]
    exposure: Option<String>,
    /// Google-Workspace domains admitted in full (matched on the part after `@`).
    #[serde(default)]
    allowed_domains: Vec<String>,
    /// Explicitly named principals (email or `account:` ref).
    #[serde(default)]
    share: Vec<String>,
}

/// The fully resolved access policy (file merged with CLI overrides), ready to apply or print.
#[derive(Debug)]
struct ResolvedAccessPolicy {
    app_ref: String,
    body: Value,
}

/// Resolve `access.yaml` (or `--file`) merged with CLI overrides into the request body the
/// control-plane's `/v1/map-control/access` endpoint expects. CLI flags win over file values.
fn resolve_access_policy(args: &AccessApplyArgs) -> Result<ResolvedAccessPolicy, String> {
    let path = args.file.clone().unwrap_or_else(|| {
        args.repo_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("access.yaml")
    });
    let raw = fs::read_to_string(&path).map_err(|error| {
        format!(
            "could not read access file {}: {error} (create access.yaml or pass --file)",
            path.display()
        )
    })?;
    let file: AccessFile = serde_yaml::from_str(&raw)
        .map_err(|error| format!("invalid {}: {error}", path.display()))?;

    let app_ref = args
        .app_ref
        .clone()
        .or(file.app_ref)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "no app_ref: set it in access.yaml or pass --app-ref app:<name>".to_string()
        })?;
    // Declaring an access policy as code implies a protected app; only an explicit `public`
    // opts back out.
    let exposure = args
        .exposure
        .clone()
        .or(file.exposure)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "protected".to_string());
    if exposure != "public" && exposure != "protected" {
        return Err(format!(
            "exposure must be 'public' or 'protected', got '{exposure}'"
        ));
    }
    let tenant_ref = args.tenant_ref.clone().or(file.tenant_ref);
    let account_ref = args.account_ref.clone().or(file.account_ref);

    let mut body = json!({
        "app_ref": app_ref,
        "exposure": exposure,
        "allowed_domains": file.allowed_domains,
        "share": file.share,
    });
    if let Some(tenant) = tenant_ref {
        body["tenant_ref"] = json!(tenant);
    }
    if let Some(account) = account_ref {
        body["account_ref"] = json!(account);
    }
    Ok(ResolvedAccessPolicy { app_ref, body })
}

/// `map access apply`: reconcile the resolved policy into the control-plane (hot).
fn access_apply(cli: &Cli, args: &AccessApplyArgs) -> Result<(), String> {
    let resolved = resolve_access_policy(args)?;
    post(cli, "/v1/map-control/access", resolved.body)
}

/// `map access plan`: print the resolved policy without applying it (no control-plane call).
fn access_plan(cli: &Cli, args: &AccessApplyArgs) -> Result<(), String> {
    let resolved = resolve_access_policy(args)?;
    let summary = format!(
        "would apply access for {}:\n  exposure: {}\n  allowed_domains: {}\n  share: {}",
        resolved.app_ref,
        resolved.body["exposure"].as_str().unwrap_or_default(),
        format_str_list(&resolved.body["allowed_domains"]),
        format_str_list(&resolved.body["share"]),
    );
    print_json_or_text(cli.json, resolved.body, &summary)
}

fn format_str_list(value: &Value) -> String {
    match value.as_array() {
        Some(items) if !items.is_empty() => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => "(none)".to_string(),
    }
}

/// The non-secret repo Variables the OIDC `map-deploy.yml` reads. Optional refs are set only
/// when present (the cp may also derive tenant/account from the edge identity — cp#86).
fn onboard_variables(args: &OnboardArgs, project_ref: &str) -> Vec<(&'static str, String)> {
    let mut vars = vec![
        ("MAP_INSTALLATION_REF", args.installation_ref.clone()),
        ("MAP_APP_REF", project_ref.to_string()),
    ];
    if let Some(tenant) = &args.tenant_ref {
        vars.push(("MAP_TENANT_REF", tenant.clone()));
    }
    if let Some(account) = &args.account_ref {
        vars.push(("MAP_ACCOUNT_REF", account.clone()));
    }
    vars
}

/// Best-effort: set repo Actions Variables via the GitHub API using the dev's token. No
/// secrets (auth is GitHub OIDC — ADR-0023). Returns a JSON summary; never fails onboard.
fn set_repo_variables(repo: &str, vars: &[(&'static str, String)]) -> Value {
    let token = match resolve_github_token(None) {
        Ok(token) => token,
        Err(_) => {
            return json!({
                "set": false,
                "reason": "no GitHub token (set $GITHUB_TOKEN/$GH_TOKEN or run `gh auth login`); set the MAP_* Variables manually",
            })
        }
    };
    let client = match build_client() {
        Ok(client) => client,
        Err(error) => return json!({ "set": false, "reason": redact(&error) }),
    };
    let api_base =
        env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".to_string());
    let mut set: Vec<&str> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for (name, value) in vars {
        match set_one_repo_variable(&client, &token, &api_base, repo, name, value) {
            Ok(()) => set.push(name),
            Err(error) => failed.push(json!({ "name": name, "error": redact(&error) })),
        }
    }
    json!({ "set": set, "failed": failed })
}

/// Create-or-update a single repo Actions Variable (POST to create; PATCH on 409-exists).
fn set_one_repo_variable(
    client: &Client,
    token: &str,
    api_base: &str,
    repo: &str,
    name: &str,
    value: &str,
) -> Result<(), String> {
    let base = api_base.trim_end_matches('/');
    let create = client
        .post(format!("{base}/repos/{repo}/actions/variables"))
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header("user-agent", "map-cli")
        .bearer_auth(token)
        .json(&json!({ "name": name, "value": value }))
        .send()
        .map_err(|error| format!("create variable {name}: {error}"))?;
    if create.status().is_success() {
        return Ok(());
    }
    if create.status() == StatusCode::CONFLICT {
        let update = client
            .patch(format!("{base}/repos/{repo}/actions/variables/{name}"))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", "map-cli")
            .bearer_auth(token)
            .json(&json!({ "name": name, "value": value }))
            .send()
            .map_err(|error| format!("update variable {name}: {error}"))?;
        if update.status().is_success() {
            return Ok(());
        }
        return Err(format!("update variable {name}: HTTP {}", update.status()));
    }
    Err(format!("create variable {name}: HTTP {}", create.status()))
}

/// Drop the deploy workflow into `<repo_dir>/.github/workflows/<workflow>` (idempotent).
fn scaffold_deploy_workflow(
    repo_dir: Option<&PathBuf>,
    workflow: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(repo_dir) = repo_dir else {
        return Ok(None);
    };
    let dir = repo_dir.join(".github").join("workflows");
    fs::create_dir_all(&dir).map_err(|error| format!("create {}: {error}", dir.display()))?;
    let path = dir.join(workflow);
    fs::write(&path, MAP_DEPLOY_WORKFLOW_TEMPLATE)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(Some(path))
}

/// Write a starter `<repo_dir>/mithran.yaml` if one is not already present (never clobbers).
fn scaffold_manifest(repo_dir: Option<&PathBuf>, name: &str) -> Result<Option<PathBuf>, String> {
    let Some(repo_dir) = repo_dir else {
        return Ok(None);
    };
    let path = repo_dir.join("mithran.yaml");
    if path.exists() {
        return Ok(None);
    }
    fs::write(
        &path,
        format!("schema_version: mithran.map.v1\nname: {name}\n"),
    )
    .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(Some(path))
}

/// DEPRECATED (`map setup`): use `map onboard`. Kept as a local scaffold-only shim — it writes
/// the deploy workflow but does NOT call the control-plane (it cannot supply the installation
/// ref) and no longer prints the host onboarding wall (superseded by the `/onboard` endpoint).
fn setup(cli: &Cli, args: &SetupArgs) -> Result<(), String> {
    validate_repo_slug(&args.repo)?;
    eprintln!(
        "map: `map setup` is deprecated; use `map onboard <owner/repo> --installation-ref <ref>`."
    );
    let workflow_path = scaffold_deploy_workflow(args.repo_dir.as_ref(), &args.workflow)?;
    print_json_or_text(
        cli.json,
        json!({
            "ok": true,
            "schema_version": "map.setup.v1",
            "deprecated": "use `map onboard`",
            "repo": args.repo,
            "workflow_written": workflow_path.as_ref().map(|p| p.display().to_string()),
        }),
        &match &workflow_path {
            Some(path) => format!(
                "wrote {} ({}). Deprecated: run `map onboard {} --installation-ref <ref>` to record the registry binding.",
                path.display(),
                args.workflow,
                args.repo
            ),
            None => format!(
                "(no --repo-dir; nothing written). Deprecated: use `map onboard {} --installation-ref <ref>`.",
                args.repo
            ),
        },
    )
}

// ───────────────────────── map versions / map publish (ADR-0018 #63) ─────────────────────────

/// The control-plane keys every route pointer by `app:<name>`; `map versions`/`map publish` take
/// the bare app name and normalize it (a caller may also pass a literal `app:` ref verbatim).
fn normalize_app_ref(app: &str) -> String {
    if app.starts_with("app:") {
        app.to_string()
    } else {
        format!("app:{app}")
    }
}

fn fetch_routes_status(cli: &Cli) -> Result<Value, String> {
    let (http, state) = client(cli)?;
    fetch_json(&http, &state, "/v1/map-control/routes/status")
}

/// `map versions <app>`: classify the app's route pointers from `routes/status` into addressable
/// internal versions (`route_pointer_ref` containing `/version/<label>`), aliases (the
/// production/preview/release `(app,env)` pointers), and the external published pointer
/// (`published-external://…`) — the clean public URL and which internal version it serves.
fn map_versions(cli: &Cli, args: &VersionsArgs) -> Result<(), String> {
    let app_ref = normalize_app_ref(&args.app);
    let routes = fetch_routes_status(cli)?;
    let payload = versions_payload(&routes, &app_ref);
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    } else {
        print!("{}", render_versions_text(&payload));
    }
    Ok(())
}

/// Pure shape of `map versions`: filter `routes/status` `aliases` to this app and split into
/// `versions` / `aliases` / `published`. Field names mirror the control-plane `RoutePointerRecord`
/// (`app_ref`, `route_pointer_ref`, `current_deployment_ref`, `hostname`, `app_env`, `pinned`,
/// `updated_from_action`). `published` is `null` when the app has never been published.
fn versions_payload(routes: &Value, app_ref: &str) -> Value {
    let mut versions: Vec<Value> = Vec::new();
    let mut aliases: Vec<Value> = Vec::new();
    let mut published = Value::Null;

    if let Some(pointers) = routes.get("aliases").and_then(Value::as_object) {
        for pointer in pointers.values() {
            if pointer.get("app_ref").and_then(Value::as_str) != Some(app_ref) {
                continue;
            }
            let pointer_ref = pointer
                .get("route_pointer_ref")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let deployment_ref = pointer
                .get("current_deployment_ref")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let hostname = pointer
                .get("hostname")
                .and_then(Value::as_str)
                .unwrap_or_default();

            if pointer_ref.starts_with("published-external://") {
                published = json!({
                    "deployment_ref": deployment_ref,
                    "hostname": hostname,
                    "route_pointer_ref": pointer_ref,
                });
            } else if let Some(label) = version_label_from_ref(pointer_ref) {
                versions.push(json!({
                    "label": label,
                    "deployment_ref": deployment_ref,
                    "hostname": hostname,
                    "app_env": pointer.get("app_env"),
                    "platform_env": pointer.get("platform_env"),
                    "route_pointer_ref": pointer_ref,
                }));
            } else {
                aliases.push(json!({
                    "app_env": pointer.get("app_env"),
                    "updated_from_action": pointer.get("updated_from_action"),
                    "deployment_ref": deployment_ref,
                    "hostname": hostname,
                    "pinned": pointer.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                    "route_pointer_ref": pointer_ref,
                }));
            }
        }
    }

    // Stable, deterministic output regardless of the BTreeMap iteration the server happens to send.
    versions.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
    aliases.sort_by(|a, b| a["app_env"].as_str().cmp(&b["app_env"].as_str()));

    json!({
        "app": app_ref.trim_start_matches("app:"),
        "app_ref": app_ref,
        "versions": versions,
        "aliases": aliases,
        "published": published,
    })
}

/// The per-version label embedded in an immutable version pointer ref
/// (`route-pointer://<penv>/<aenv>/<app_ref>/version/<label>`); `None` for non-version pointers.
fn version_label_from_ref(pointer_ref: &str) -> Option<&str> {
    pointer_ref
        .split_once("/version/")
        .map(|(_, label)| label)
        .filter(|label| !label.is_empty())
}

fn render_versions_text(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "app: {}\n",
        payload["app"].as_str().unwrap_or_default()
    ));

    out.push_str("\ninternal versions:\n");
    match payload["versions"].as_array() {
        Some(versions) if !versions.is_empty() => {
            for version in versions {
                out.push_str(&format!(
                    "  {}  {}  {}\n",
                    version["label"].as_str().unwrap_or_default(),
                    version["deployment_ref"].as_str().unwrap_or_default(),
                    version["hostname"].as_str().unwrap_or_default(),
                ));
            }
        }
        _ => out.push_str("  (none)\n"),
    }

    out.push_str("\naliases:\n");
    match payload["aliases"].as_array() {
        Some(aliases) if !aliases.is_empty() => {
            for alias in aliases {
                let pinned = if alias["pinned"].as_bool().unwrap_or(false) {
                    " [pinned]"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "  {} ({}){}  ->  {}  {}\n",
                    alias["app_env"].as_str().unwrap_or_default(),
                    alias["updated_from_action"].as_str().unwrap_or_default(),
                    pinned,
                    alias["deployment_ref"].as_str().unwrap_or_default(),
                    alias["hostname"].as_str().unwrap_or_default(),
                ));
            }
        }
        _ => out.push_str("  (none)\n"),
    }

    out.push_str("\npublished: ");
    if payload["published"].is_null() {
        out.push_str("(not published)\n");
    } else {
        out.push_str(&format!(
            "{}  https://{}\n",
            payload["published"]["deployment_ref"]
                .as_str()
                .unwrap_or_default(),
            payload["published"]["hostname"]
                .as_str()
                .unwrap_or_default(),
        ));
    }
    out
}

/// `map publish <app>`: resolve the chosen internal version to a deployment_ref, then POST
/// `/v1/map-control/deploy/publish` to pin the app's clean public URL to it. The control-plane is
/// review-gated (400 unless the version is a reviewed, succeeded deploy) and stale-safe (409 when
/// `--expected-sha` no longer matches the version's recorded source SHA).
fn map_publish(cli: &Cli, args: &PublishArgs) -> Result<(), String> {
    let app_ref = normalize_app_ref(&args.app);
    let deployment_ref = match &args.deployment_ref {
        Some(deployment_ref) => deployment_ref.clone(),
        None => {
            let label = args.version.as_deref().ok_or_else(|| {
                format!(
                    "pick a version to publish: pass --version <label> or --deployment-ref <ref> (run `map versions {}` to list)",
                    app_ref.trim_start_matches("app:")
                )
            })?;
            let routes = fetch_routes_status(cli)?;
            resolve_version_deployment_ref(&routes, &app_ref, label)?
        }
    };

    let body = build_publish_body(
        &app_ref,
        &deployment_ref,
        args.actor.as_deref(),
        args.expected_sha.as_deref(),
    );

    let (http, state) = client(cli)?;
    let response = http
        .post(format!(
            "{}/v1/map-control/deploy/publish",
            state.map_control_endpoint.trim_end_matches('/'),
        ))
        .bearer_auth(&state.access_token)
        .json(&body)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;

    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("read MAP response: {error}"))?;
    match status {
        StatusCode::OK | StatusCode::CREATED | StatusCode::ACCEPTED => {}
        StatusCode::BAD_REQUEST => {
            return Err(format!(
                "version not publishable: must be a reviewed, succeeded deploy ({})",
                redact(&text)
            ));
        }
        StatusCode::CONFLICT => {
            return Err(format!(
                "stale: the reviewed source moved; re-check `map versions` ({})",
                redact(&text)
            ));
        }
        _ => return Err(format!("MAP returned {status}: {}", redact(&text))),
    }

    if cli.json {
        println!("{text}");
        return Ok(());
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|error| format!("parse publish response: {error}"))?;
    match value
        .get("published")
        .and_then(|published| published.get("hostname"))
        .and_then(Value::as_str)
    {
        Some(hostname) => println!("published https://{hostname}"),
        None => println!("ok"),
    }
    Ok(())
}

/// Look up the deployment_ref behind an internal version `label` from `routes/status` (the
/// `current_deployment_ref` of this app's `…/version/<label>` pointer).
fn resolve_version_deployment_ref(
    routes: &Value,
    app_ref: &str,
    label: &str,
) -> Result<String, String> {
    if let Some(pointers) = routes.get("aliases").and_then(Value::as_object) {
        for pointer in pointers.values() {
            if pointer.get("app_ref").and_then(Value::as_str) != Some(app_ref) {
                continue;
            }
            let pointer_ref = pointer
                .get("route_pointer_ref")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if version_label_from_ref(pointer_ref) == Some(label) {
                return pointer
                    .get("current_deployment_ref")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        format!("version `{label}` has no current_deployment_ref in routes/status")
                    });
            }
        }
    }
    Err(format!(
        "no internal version labeled `{label}` for {app_ref}; run `map versions {}` to list",
        app_ref.trim_start_matches("app:")
    ))
}

/// The `/v1/map-control/deploy/publish` request body (a control-plane `ActionInput`). `app_ref` is
/// carried for symmetry/audit; the handler authoritatively derives the app from the deployment.
/// `actor_ref`/`expected_source_sha` are sent only when supplied.
fn build_publish_body(
    app_ref: &str,
    deployment_ref: &str,
    actor_ref: Option<&str>,
    expected_source_sha: Option<&str>,
) -> Value {
    let mut body = json!({
        "app_ref": app_ref,
        "deployment_ref": deployment_ref,
    });
    if let Some(actor_ref) = actor_ref {
        body["actor_ref"] = json!(actor_ref);
    }
    if let Some(expected_source_sha) = expected_source_sha {
        body["expected_source_sha"] = json!(expected_source_sha);
    }
    body
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
    let broker = config
        .get("source_snapshot_storage")?
        .get("live_source_broker")?;
    let env_count = broker.get("allowed_repository_count")?.as_u64()?;
    // P2a: the hot registry is the live authority for onboarded repos; count its bindings too
    // so doctor reflects repos onboarded via `map onboard` (not just the env bootstrap seed).
    let registry_count = broker
        .get("registry_binding_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(env_count + registry_count)
}

fn allowlist_check(config: &Value) -> Check {
    match allowlist_count(config) {
        Some(0) => Check::fail(
            "source allowlist",
            "0 repositories allowlisted",
            "onboard a repo with `map onboard <owner/repo> --installation-ref <ref>`",
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
            format!("run `map onboard {app} --installation-ref <ref>`"),
        ),
        _ => Check::warn(
            "app allowlisted",
            format!("cannot confirm {app} is allowlisted (the config endpoint exposes only a count, not the list)"),
            format!("run `map onboard {app} --installation-ref <ref>`; doctor counts registry bindings (P2a) so an onboarded repo shows here"),
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
            format!("after `map onboard {app} --installation-ref <ref>`, deploy with `map deploy --env preview --repo {app}`"),
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

    fn access_args_for(file: &PathBuf) -> AccessApplyArgs {
        AccessApplyArgs {
            repo_dir: None,
            file: Some(file.clone()),
            app_ref: None,
            tenant_ref: None,
            account_ref: None,
            exposure: None,
        }
    }

    fn write_access_file(name: &str, body: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("map-access-{}-{}.yaml", std::process::id(), name));
        fs::write(&path, body).unwrap();
        path
    }

    // ADR-0019: access.yaml resolves into the control-plane request body; a declared policy
    // defaults to protected and carries the domains + share verbatim.
    #[test]
    fn resolve_access_policy_reads_file_and_defaults_protected() {
        let path = write_access_file(
            "basic",
            "app_ref: app:developer-portal\nallowed_domains:\n  - mithran.ai\nshare:\n  - guest@partner.com\n",
        );
        let resolved = resolve_access_policy(&access_args_for(&path)).unwrap();
        assert_eq!(resolved.app_ref, "app:developer-portal");
        assert_eq!(resolved.body["exposure"], "protected");
        assert_eq!(resolved.body["allowed_domains"][0], "mithran.ai");
        assert_eq!(resolved.body["share"][0], "guest@partner.com");
        fs::remove_file(&path).ok();
    }

    // CLI flags override file values.
    #[test]
    fn resolve_access_policy_cli_overrides_file() {
        let path = write_access_file("override", "app_ref: app:from-file\nexposure: protected\n");
        let mut args = access_args_for(&path);
        args.app_ref = Some("app:from-flag".to_string());
        args.exposure = Some("public".to_string());
        let resolved = resolve_access_policy(&args).unwrap();
        assert_eq!(resolved.app_ref, "app:from-flag");
        assert_eq!(resolved.body["exposure"], "public");
        fs::remove_file(&path).ok();
    }

    // A typo'd key fails loudly instead of silently dropping a restriction.
    #[test]
    fn resolve_access_policy_rejects_unknown_field() {
        let path = write_access_file("typo", "app_ref: app:x\nallowed_domain:\n  - mithran.ai\n");
        let err = resolve_access_policy(&access_args_for(&path)).unwrap_err();
        assert!(err.contains("invalid"), "got: {err}");
        fs::remove_file(&path).ok();
    }

    // No resolvable app_ref is an error, not a silent no-op.
    #[test]
    fn resolve_access_policy_requires_app_ref() {
        let path = write_access_file("noapp", "allowed_domains:\n  - mithran.ai\n");
        let err = resolve_access_policy(&access_args_for(&path)).unwrap_err();
        assert!(err.contains("app_ref"), "got: {err}");
        fs::remove_file(&path).ok();
    }

    // A bad exposure is rejected before any control-plane call.
    #[test]
    fn resolve_access_policy_rejects_bad_exposure() {
        let path = write_access_file("badexp", "app_ref: app:x\nexposure: internal\n");
        let err = resolve_access_policy(&access_args_for(&path)).unwrap_err();
        assert!(err.contains("exposure"), "got: {err}");
        fs::remove_file(&path).ok();
    }

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
    fn onboard_parses_repo_and_installation_with_optionals() {
        let cli = Cli::try_parse_from([
            "map",
            "onboard",
            "john-smith/my-app",
            "--installation-ref",
            "github-installation://131136661",
            "--tenant-ref",
            "tenant:john-smith",
            "--repo-dir",
            "/tmp/x",
        ])
        .expect("parses");
        match cli.command {
            Command::Onboard(args) => {
                assert_eq!(args.repo, "john-smith/my-app");
                assert_eq!(args.installation_ref, "github-installation://131136661");
                assert_eq!(args.tenant_ref.as_deref(), Some("tenant:john-smith"));
                assert_eq!(args.project_ref, None);
                assert_eq!(args.repo_dir, Some(PathBuf::from("/tmp/x")));
                assert_eq!(args.workflow, "map-deploy.yml");
            }
            _ => panic!("expected onboard"),
        }
    }

    #[test]
    fn onboard_requires_installation_ref() {
        assert!(Cli::try_parse_from(["map", "onboard", "john-smith/my-app"]).is_err());
    }

    #[test]
    fn onboard_variables_set_required_and_present_optionals_only() {
        let args = OnboardArgs {
            repo: "john-smith/my-app".to_string(),
            installation_ref: "github-installation://131136661".to_string(),
            tenant_ref: Some("tenant:john-smith".to_string()),
            account_ref: None,
            project_ref: None,
            repo_dir: None,
            workflow: "map-deploy.yml".to_string(),
        };
        let vars: std::collections::HashMap<&str, String> =
            onboard_variables(&args, "app:my-app").into_iter().collect();
        assert_eq!(
            vars.get("MAP_INSTALLATION_REF").map(String::as_str),
            Some("github-installation://131136661")
        );
        assert_eq!(
            vars.get("MAP_APP_REF").map(String::as_str),
            Some("app:my-app")
        );
        assert_eq!(
            vars.get("MAP_TENANT_REF").map(String::as_str),
            Some("tenant:john-smith")
        );
        // an absent optional ref is omitted (not set to empty).
        assert!(!vars.contains_key("MAP_ACCOUNT_REF"));
    }

    #[test]
    fn scaffold_helpers_write_and_do_not_clobber() {
        let dir = env::temp_dir().join(format!("map-onboard-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let wf = scaffold_deploy_workflow(Some(&dir), "map-deploy.yml")
            .expect("ok")
            .expect("path");
        assert!(wf.ends_with("map-deploy.yml"));
        assert_eq!(
            fs::read_to_string(&wf).unwrap(),
            MAP_DEPLOY_WORKFLOW_TEMPLATE
        );

        let manifest = scaffold_manifest(Some(&dir), "my-app")
            .expect("ok")
            .expect("path");
        let body = fs::read_to_string(&manifest).unwrap();
        assert!(body.contains("schema_version: mithran.map.v1"));
        assert!(body.contains("name: my-app"));

        // existing manifest is never clobbered.
        fs::write(&manifest, "name: edited-by-user\n").unwrap();
        assert_eq!(scaffold_manifest(Some(&dir), "my-app").expect("ok"), None);
        assert_eq!(
            fs::read_to_string(&manifest).unwrap(),
            "name: edited-by-user\n"
        );

        // no --repo-dir → no-op.
        assert_eq!(
            scaffold_deploy_workflow(None, "map-deploy.yml").unwrap(),
            None
        );
        assert_eq!(scaffold_manifest(None, "my-app").unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allowlist_count_sums_env_and_registry_bindings() {
        let config = json!({
            "source_snapshot_storage": {
                "live_source_broker": {
                    "allowed_repository_count": 1,
                    "registry_binding_count": 2
                }
            }
        });
        assert_eq!(allowlist_count(&config), Some(3));
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
        // ADR-0023: keyless auth via GitHub OIDC federation (no static deploy secret).
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("id-token: write"));
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("/v1/auth/github-oidc/exchange"));
        // Runs on a GitHub-hosted runner against the public edge — no self-hosted/localhost.
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("runs-on: ubuntu-latest"));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("self-hosted"));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("MAP_CONTROL_TOKEN"));
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

    // ── map versions / map publish (ADR-0018 #63) ──

    /// A representative `routes/status` payload grounded in the control-plane `RoutePointerRecord`
    /// shape (`aliases` is a map keyed by `route_pointer_ref`): a production alias, two immutable
    /// per-version pointers, the env-bare published-external pointer, and an unrelated app that must
    /// be filtered out. Pointer refs/hostnames mirror the cp builders (`route_pointer_ref_for*`,
    /// `published_external_route_pointer_ref`, `hostname_for_published_external`).
    fn sample_routes_status() -> Value {
        json!({
            "status": "ok",
            "deployments": {},
            "aliases": {
                "route-pointer://sandbox/production/app:gtd-tracker": {
                    "route_pointer_ref": "route-pointer://sandbox/production/app:gtd-tracker",
                    "app_ref": "app:gtd-tracker",
                    "app_env": "production",
                    "platform_env": "sandbox",
                    "hostname": "gtd-tracker-production.sandbox.apps.mithran.cloud",
                    "current_deployment_ref": "deployment://sandbox/production/gtd-2",
                    "updated_from_action": "ProductionPromote",
                    "pinned": false
                },
                "route-pointer://sandbox/production/app:gtd-tracker/version/gtd-2": {
                    "route_pointer_ref": "route-pointer://sandbox/production/app:gtd-tracker/version/gtd-2",
                    "app_ref": "app:gtd-tracker",
                    "app_env": "production",
                    "platform_env": "sandbox",
                    "hostname": "gtd-tracker-gtd-2.sandbox.apps.mithran.cloud",
                    "current_deployment_ref": "deployment://sandbox/production/gtd-2",
                    "updated_from_action": "PreviewUpdate",
                    "pinned": false
                },
                "route-pointer://sandbox/production/app:gtd-tracker/version/gtd-1": {
                    "route_pointer_ref": "route-pointer://sandbox/production/app:gtd-tracker/version/gtd-1",
                    "app_ref": "app:gtd-tracker",
                    "app_env": "production",
                    "platform_env": "sandbox",
                    "hostname": "gtd-tracker-gtd-1.sandbox.apps.mithran.cloud",
                    "current_deployment_ref": "deployment://sandbox/production/gtd-1",
                    "updated_from_action": "PreviewUpdate",
                    "pinned": false
                },
                "published-external://sandbox/app:gtd-tracker": {
                    "route_pointer_ref": "published-external://sandbox/app:gtd-tracker",
                    "app_ref": "app:gtd-tracker",
                    "app_env": "production",
                    "platform_env": "sandbox",
                    "hostname": "gtd-tracker.apps.mithran.cloud",
                    "current_deployment_ref": "deployment://sandbox/production/gtd-1",
                    "updated_from_action": "PublishedExternal",
                    "pinned": true
                },
                "published-external://sandbox/app:other-app": {
                    "route_pointer_ref": "published-external://sandbox/app:other-app",
                    "app_ref": "app:other-app",
                    "app_env": "production",
                    "platform_env": "sandbox",
                    "hostname": "other-app.apps.mithran.cloud",
                    "current_deployment_ref": "deployment://sandbox/production/other-9",
                    "updated_from_action": "PublishedExternal",
                    "pinned": true
                }
            }
        })
    }

    #[test]
    fn normalizes_app_ref() {
        assert_eq!(normalize_app_ref("gtd-tracker"), "app:gtd-tracker");
        assert_eq!(normalize_app_ref("app:gtd-tracker"), "app:gtd-tracker");
    }

    #[test]
    fn versions_payload_splits_versions_published_and_filters_other_apps() {
        let payload = versions_payload(&sample_routes_status(), "app:gtd-tracker");

        assert_eq!(payload["app"], "gtd-tracker");
        assert_eq!(payload["app_ref"], "app:gtd-tracker");

        // Both per-version pointers are surfaced, sorted by label (gtd-1 before gtd-2).
        let versions = payload["versions"].as_array().expect("versions array");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0]["label"], "gtd-1");
        assert_eq!(
            versions[0]["deployment_ref"],
            "deployment://sandbox/production/gtd-1"
        );
        assert_eq!(
            versions[0]["hostname"],
            "gtd-tracker-gtd-1.sandbox.apps.mithran.cloud"
        );
        assert_eq!(versions[1]["label"], "gtd-2");

        // The production alias is an alias, not a version.
        let aliases = payload["aliases"].as_array().expect("aliases array");
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0]["app_env"], "production");
        assert_eq!(aliases[0]["updated_from_action"], "ProductionPromote");

        // The published-external pointer surfaces the env-bare hostname + the published version.
        assert_eq!(
            payload["published"]["deployment_ref"],
            "deployment://sandbox/production/gtd-1"
        );
        assert_eq!(
            payload["published"]["hostname"],
            "gtd-tracker.apps.mithran.cloud"
        );

        // The unrelated app's published pointer must not leak in.
        let rendered = render_versions_text(&payload);
        assert!(!rendered.contains("other-app"), "must filter other apps");
        assert!(rendered.contains("https://gtd-tracker.apps.mithran.cloud"));
    }

    #[test]
    fn versions_payload_reports_not_published_when_no_published_pointer() {
        let routes = json!({
            "aliases": {
                "route-pointer://sandbox/production/app:fresh/version/v1": {
                    "route_pointer_ref": "route-pointer://sandbox/production/app:fresh/version/v1",
                    "app_ref": "app:fresh",
                    "app_env": "production",
                    "platform_env": "sandbox",
                    "hostname": "fresh-v1.sandbox.apps.mithran.cloud",
                    "current_deployment_ref": "deployment://sandbox/production/v1",
                    "updated_from_action": "PreviewUpdate",
                    "pinned": false
                }
            }
        });
        let payload = versions_payload(&routes, "app:fresh");
        assert!(payload["published"].is_null());
        assert!(render_versions_text(&payload).contains("(not published)"));
    }

    #[test]
    fn resolves_version_label_to_deployment_ref() {
        let routes = sample_routes_status();
        assert_eq!(
            resolve_version_deployment_ref(&routes, "app:gtd-tracker", "gtd-2").unwrap(),
            "deployment://sandbox/production/gtd-2"
        );
        // Unknown labels and other apps' labels both fail with a guiding message.
        assert!(resolve_version_deployment_ref(&routes, "app:gtd-tracker", "nope").is_err());
        assert!(resolve_version_deployment_ref(&routes, "app:other-app", "gtd-1").is_err());
    }

    #[test]
    fn publish_body_has_app_ref_and_deployment_ref_and_omits_optionals() {
        let body = build_publish_body(
            "app:gtd-tracker",
            "deployment://sandbox/production/gtd-1",
            None,
            None,
        );
        assert_eq!(body["app_ref"], "app:gtd-tracker");
        assert_eq!(
            body["deployment_ref"],
            "deployment://sandbox/production/gtd-1"
        );
        // Optionals are absent (not null) so the cp's serde defaults apply.
        assert!(body.get("expected_source_sha").is_none());
        assert!(body.get("actor_ref").is_none());
    }

    #[test]
    fn publish_body_includes_expected_sha_and_actor_when_given() {
        let body = build_publish_body(
            "app:gtd-tracker",
            "deployment://sandbox/production/gtd-1",
            Some("actor://user/b@mithran.ai"),
            Some("0123456789abcdef0123456789abcdef01234567"),
        );
        assert_eq!(
            body["expected_source_sha"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(body["actor_ref"], "actor://user/b@mithran.ai");
    }

    #[test]
    fn publish_parses_version_and_expected_sha_flags() {
        let cli = Cli::try_parse_from([
            "map",
            "publish",
            "gtd-tracker",
            "--version",
            "gtd-2",
            "--expected-sha",
            "0123456789abcdef0123456789abcdef01234567",
        ])
        .expect("parses");
        match cli.command {
            Command::Publish(args) => {
                assert_eq!(args.app, "gtd-tracker");
                assert_eq!(args.version.as_deref(), Some("gtd-2"));
                assert_eq!(
                    args.expected_sha.as_deref(),
                    Some("0123456789abcdef0123456789abcdef01234567")
                );
                assert!(args.deployment_ref.is_none());
            }
            _ => panic!("expected publish"),
        }
    }

    #[test]
    fn publish_rejects_version_and_deployment_ref_together() {
        // The two version selectors are mutually exclusive.
        assert!(Cli::try_parse_from([
            "map",
            "publish",
            "gtd-tracker",
            "--version",
            "gtd-2",
            "--deployment-ref",
            "deployment://sandbox/production/gtd-2",
        ])
        .is_err());
    }

    #[test]
    fn versions_parses_app_arg() {
        let cli = Cli::try_parse_from(["map", "versions", "gtd-tracker"]).expect("parses");
        match cli.command {
            Command::Versions(args) => assert_eq!(args.app, "gtd-tracker"),
            _ => panic!("expected versions"),
        }
    }
}
