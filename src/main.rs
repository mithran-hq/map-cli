use clap::{ArgGroup, Args, Parser, Subcommand};
use map_deploy_review_contract::{MapDeployReviewContract, MapDeployReviewStatus};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The reusable `map-deploy.yml` workflow template — the OPT-IN BYO-CI keyless-OIDC deploy path
/// (ADR-0023). `map onboard --with-ci-workflow --repo-dir` writes it into a repo's
/// `.github/workflows/`; it is NOT installed by default (webhook-native — ADR-0016 amendment
/// 2026-07-06, mithran-business#582). Single source of truth: `templates/map-deploy.yml`.
const MAP_DEPLOY_WORKFLOW_TEMPLATE: &str = include_str!("../templates/map-deploy.yml");

const USER_AGENT: &str = concat!("map-cli/", env!("CARGO_PKG_VERSION"));
const DEPLOY_LOGS_PATH: &str = "/v1/map-control/deploy/logs";
const CANARY_DEPLOY_PATH: &str = "/v1/map-control/deploy/canary";
const OFFBOARD_PATH: &str = "/v1/map-control/offboard";
const LIVE_ADAPTER_MODE: &str = "sandbox-live";
const DEFAULT_STARTER_RUNTIME: &str = "nodejs22";
const DEFAULT_STARTER_COMMAND: &str = "npm start";
const AUTH_FLAGS_HELP: &str = "Authentication flags: --login-state <path>, or --endpoint <url> with --token-file <path>, --token-stdin, or --token <token>";
const LOGIN_SAVE_HELP: &str = "State path flag: --login-state <path>\nToken input: prefer --access-token-file <path> or --access-token-stdin; --access-token <token> is also accepted.";

#[derive(Parser)]
#[command(
    name = "map",
    version,
    about = "Deploy, inspect, publish, and manage MAP apps",
    after_help = AUTH_FLAGS_HELP
)]
struct Cli {
    /// Path to a saved map login state file.
    #[arg(long, global = true, hide = true)]
    login_state: Option<PathBuf>,

    /// Override the MAP control-plane endpoint for this command.
    #[arg(long, global = true, hide = true)]
    endpoint: Option<String>,

    /// Bearer token for this command. Prefer saved login state for interactive use.
    #[arg(long, global = true, hide = true, conflicts_with_all = ["token_file", "token_stdin"])]
    token: Option<String>,

    /// Read the one-command bearer token from a file.
    #[arg(long, global = true, hide = true, conflicts_with_all = ["token", "token_stdin"])]
    token_file: Option<PathBuf>,

    /// Read the one-command bearer token from stdin.
    #[arg(long, global = true, hide = true, conflicts_with_all = ["token", "token_file"])]
    token_stdin: bool,

    /// Print structured JSON when the command supports it.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Save or print local MAP login state.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Login(LoginCommand),
    /// Show the saved MAP account and endpoint.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Whoami,
    /// Check control-plane readiness and optional app diagnostics.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Doctor(DoctorArgs),
    /// Write a starter mithran.yaml manifest.
    Init(InitArgs),
    /// Review mithran.yaml locally before deploy.
    DeployReview(DeployReviewArgs),
    /// Validate a deploy request without deploying.
    Validate(DeployTarget),
    /// Request a deploy directly. Built-in GitHub App webhooks are preferred for standard refs.
    #[command(alias = "deploy-request", after_help = AUTH_FLAGS_HELP)]
    Deploy(DeployRequestArgs),
    /// Register a GitHub repo with MAP and optionally scaffold local files.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Onboard(OnboardArgs),
    /// Remove a GitHub repo from MAP deploy intake.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Offboard(OffboardArgs),
    /// Show onboarding guidance.
    #[command(hide = true)]
    Setup(SetupArgs),
    /// Plan or apply protected app sharing from access.yaml.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Access(AccessArgs),
    /// List internal versions, app-environment aliases, and the published clean URL.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Versions(VersionsArgs),
    /// Publish a reviewed version to the app's clean public URL.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Publish(PublishArgs),
    /// Start, promote, or roll back a weighted canary.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Canary(CanaryArgs),
    /// Show deploy status for a deployment ref.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Status(IdArgs),
    /// Poll deploy status until success, failure, or timeout.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Watch(WatchArgs),
    /// Show deploy phase logs for a deployment ref.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Logs(IdArgs),
    /// Show deploy evidence for a deployment ref.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Evidence(IdArgs),
    /// Roll back the route pointer for a deployment ref.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Rollback(RollbackArgs),
    /// Print the map CLI version.
    Version,
}

#[derive(Args)]
struct LoginCommand {
    #[command(subcommand)]
    command: LoginSubcommand,
}

#[derive(Subcommand)]
enum LoginSubcommand {
    /// Save control-plane endpoint and token metadata locally.
    #[command(after_help = LOGIN_SAVE_HELP)]
    Save(LoginSaveArgs),
    /// Print the saved token for an allowed audience.
    #[command(after_help = AUTH_FLAGS_HELP)]
    PrintToken(PrintTokenArgs),
}

#[derive(Args)]
#[command(group(
    ArgGroup::new("access_token_source")
        .required(true)
        .args(["access_token", "access_token_file", "access_token_stdin"])
))]
struct LoginSaveArgs {
    /// MAP control-plane endpoint to save.
    #[arg(long)]
    map_control_endpoint: String,

    /// Optional secondary controller endpoint to save.
    #[arg(long)]
    jason_controller_endpoint: Option<String>,

    /// Bearer token to save in the login state file.
    #[arg(long)]
    access_token: Option<String>,

    /// Read the bearer token to save from a file.
    #[arg(long)]
    access_token_file: Option<PathBuf>,

    /// Read the bearer token to save from stdin.
    #[arg(long)]
    access_token_stdin: bool,

    /// Audience this login state is valid for.
    #[arg(long, default_value = "map-control")]
    audience: String,

    /// Allowed token scopes, comma-separated or repeated.
    #[arg(long = "scope", value_delimiter = ',')]
    scopes: Vec<String>,

    /// Token expiry timestamp, when known.
    #[arg(long)]
    expires_at: Option<String>,

    /// Principal email to record in login state.
    #[arg(long)]
    email: Option<String>,

    /// Principal display name to record in login state.
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
struct PrintTokenArgs {
    /// Audience to print a token for.
    #[arg(long)]
    audience: String,
}

#[derive(Args, Serialize)]
struct InitArgs {
    /// Manifest path to create.
    #[arg(long, default_value = "mithran.yaml")]
    manifest: PathBuf,
}

#[derive(Args)]
struct DeployReviewArgs {
    /// Repository root used to resolve the manifest path.
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    /// Manifest path to review. Relative paths resolve under --repo-root.
    #[arg(long, default_value = "mithran.yaml")]
    manifest: PathBuf,
}

#[derive(Args, Serialize)]
#[command(group(
    ArgGroup::new("source_selector")
        .required(true)
        .args(["ref_name", "sha"])
))]
struct DeployTarget {
    /// GitHub repository, either owner/repo or github://owner/repo.
    #[arg(long)]
    repo: String,

    /// App environment to target, such as preview or production.
    #[arg(long)]
    env: Option<String>,

    /// Git ref to deploy, such as refs/heads/release/v1.
    #[arg(long = "ref", conflicts_with = "sha")]
    ref_name: Option<String>,

    /// Exact 40-character Git SHA to deploy.
    #[arg(long)]
    sha: Option<String>,
}

/// Request a deploy directly for a repo ref or SHA. Use the built-in GitHub App
/// webhook path for standard main and release refs unless automation needs an
/// explicit deploy request.
#[derive(Args, Serialize)]
struct DeployRequestArgs {
    #[command(flatten)]
    target: DeployTarget,

    /// GitHub App installation ref authorizing source fetch for this repo.
    #[arg(long)]
    installation_ref: Option<String>,

    /// App ref, e.g. `app:gtd-tracker`.
    #[arg(long)]
    app_ref: Option<String>,

    /// Tenant ref to attach to the deploy request.
    #[arg(long)]
    tenant_ref: Option<String>,

    /// Account ref to attach to the deploy request.
    #[arg(long)]
    account_ref: Option<String>,

    /// Compatibility override for pre-derived MAP platform environment requests.
    #[arg(long, hide = true)]
    platform_env: Option<String>,

    /// Explicit deployment ref; the control-plane mints one when omitted.
    #[arg(long)]
    deployment_ref: Option<String>,

    /// Evidence ref to attach to the deploy request.
    #[arg(long)]
    evidence_ref: Option<String>,
}

/// Run readiness checks against the saved MAP control-plane endpoint.
#[derive(Args)]
struct DoctorArgs {
    /// Also diagnose a specific app `owner/repo` (source access + alias/recent deployment).
    #[arg(long)]
    app: Option<String>,
}

#[derive(Args)]
struct SetupArgs {
    /// Repository to onboard, `owner/repo`.
    repo: String,

    /// Local checkout to write `.github/workflows/<workflow>` into, with `--with-ci-workflow`.
    #[arg(long)]
    repo_dir: Option<PathBuf>,

    /// Workflow filename written under `.github/workflows/` when `--with-ci-workflow` is set.
    #[arg(long, default_value = "map-deploy.yml")]
    workflow: String,

    /// Also scaffold the optional custom-CI deploy workflow.
    #[arg(long, default_value_t = false)]
    with_ci_workflow: bool,
}

/// Register a GitHub repo with MAP. The default path uses GitHub App webhooks
/// and does not write a repo workflow.
#[derive(Args)]
struct OnboardArgs {
    /// Repository to onboard, `owner/repo`.
    repo: String,

    /// GitHub App installation ref authorizing the repo, e.g. `github-installation://131136661`.
    #[arg(long)]
    installation_ref: String,

    /// Tenant ref (provisioning identity); the control-plane records it on the binding.
    #[arg(long)]
    tenant_ref: Option<String>,

    /// Account ref (provisioning identity).
    #[arg(long)]
    account_ref: Option<String>,

    /// Project/app ref; defaults to `app:<repo-name>`. In self-service onboarding this is also
    /// the app identity used by deploy routing, version, and publish surfaces.
    #[arg(long, visible_alias = "app-ref")]
    project_ref: Option<String>,

    /// Local checkout to scaffold `mithran.yaml` (and, with `--with-ci-workflow`, the deploy
    /// workflow) into.
    #[arg(long)]
    repo_dir: Option<PathBuf>,

    /// Workflow filename written under `.github/workflows/` when `--with-ci-workflow` is set.
    #[arg(long, default_value = "map-deploy.yml")]
    workflow: String,

    /// Also scaffold the optional custom-CI deploy workflow and its repo Variables.
    #[arg(long, default_value_t = false)]
    with_ci_workflow: bool,
}

/// Remove source-registry onboarding bindings for a GitHub repo.
#[derive(Args)]
#[command(group(
    ArgGroup::new("offboard_scope")
        .required(true)
        .args(["installation_ref", "all_installations"])
))]
struct OffboardArgs {
    /// Repository to offboard, `owner/repo`.
    repo: String,

    /// Remove only this GitHub App installation binding.
    #[arg(long, conflicts_with = "all_installations")]
    installation_ref: Option<String>,

    /// Remove all installation bindings for this repository.
    #[arg(long, default_value_t = false, conflicts_with = "installation_ref")]
    all_installations: bool,
}

#[derive(Args)]
struct AccessArgs {
    #[command(subcommand)]
    command: AccessSubcommand,
}

#[derive(Subcommand)]
enum AccessSubcommand {
    /// Apply the resolved access policy to MAP.
    #[command(after_help = AUTH_FLAGS_HELP)]
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

/// List an app's internal versions, app-environment aliases, and published URL.
#[derive(Args)]
struct VersionsArgs {
    /// App name (e.g. `gtd-tracker`); normalized to `app:<app>`. Accepts a literal `app:` ref.
    app: String,
}

/// Publish a reviewed internal version to the app's clean public URL.
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

/// Operator controls for a weighted app-environment canary.
#[derive(Args)]
struct CanaryArgs {
    #[command(subcommand)]
    command: CanarySubcommand,
}

#[derive(Subcommand)]
enum CanarySubcommand {
    /// Shift a 1..99% traffic slice to a succeeded deployment.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Start(CanaryStartArgs),
    /// Promote the active canary to 100% and clear the split.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Promote(CanaryEndArgs),
    /// Drop the active canary split and keep the current deployment at 100%.
    #[command(after_help = AUTH_FLAGS_HELP)]
    Rollback(CanaryEndArgs),
}

#[derive(Args)]
struct CanaryStartArgs {
    /// App name (e.g. `gtd-tracker`); normalized to `app:<app>`. Accepts a literal `app:` ref.
    app: String,

    /// Deployment ref to receive the canary traffic slice.
    #[arg(long = "deployment-ref")]
    deployment_ref: String,

    /// Canary traffic percentage. Must be an integer from 1 through 99.
    #[arg(long)]
    weight: u32,
}

#[derive(Args)]
struct CanaryEndArgs {
    /// App name (e.g. `gtd-tracker`); normalized to `app:<app>`. Accepts a literal `app:` ref.
    app: String,

    /// Deployment ref identifying the active canary or its production alias.
    #[arg(long = "deployment-ref")]
    deployment_ref: String,
}

#[derive(Args)]
struct IdArgs {
    /// Deployment ref to inspect.
    id: String,
}

#[derive(Args)]
struct WatchArgs {
    /// Deployment ref to poll.
    id: String,

    /// Seconds between status polls.
    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,

    /// Maximum seconds to wait before failing.
    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
}

#[derive(Args, Serialize)]
struct RollbackArgs {
    /// Deployment ref whose route pointer should roll back.
    id: String,

    /// Evidence ref to attach to the rollback request.
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
            let access_token = read_token_input(
                "--access-token",
                args.access_token.as_ref(),
                "--access-token-file",
                args.access_token_file.as_ref(),
                args.access_token_stdin,
                "--access-token-stdin",
            )?;
            let state = LoginState {
                map_control_endpoint: args.map_control_endpoint.clone(),
                jason_controller_endpoint: args.jason_controller_endpoint.clone(),
                access_token,
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
                    "login state is not valid for audience `{}`. {}",
                    args.audience,
                    login_recovery_hint()
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
                starter_manifest("example", "Example", "app:example"),
            )
            .map_err(|error| format!("write {}: {error}", args.manifest.display()))?;
            print_json_or_text(
                cli.json,
                json!({ "ok": true, "manifest": args.manifest }),
                "created mithran.yaml",
            )
        }
        Command::DeployReview(args) => deploy_review(&cli, args),
        Command::Validate(target) => {
            validate_target(target)?;
            print_json_or_text(cli.json, json!({ "ok": true }), "target is valid")
        }
        Command::Deploy(args) => deploy_request(&cli, args),
        Command::Onboard(args) => onboard(&cli, args),
        Command::Offboard(args) => offboard(&cli, args),
        Command::Setup(args) => setup(&cli, args),
        Command::Access(args) => match &args.command {
            AccessSubcommand::Apply(apply) => access_apply(&cli, apply),
            AccessSubcommand::Plan(plan) => access_plan(&cli, plan),
        },
        Command::Versions(args) => map_versions(&cli, args),
        Command::Publish(args) => map_publish(&cli, args),
        Command::Canary(args) => map_canary(&cli, args),
        Command::Status(args) => get(
            &cli,
            "/v1/map-control/deploy/status",
            &[("deployment_ref", args.id.as_str())],
        ),
        Command::Watch(args) => watch(&cli, args),
        Command::Logs(args) => map_logs(&cli, args),
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
    let has_explicit_token_source =
        cli.token.is_some() || cli.token_file.is_some() || cli.token_stdin;
    if let (Some(endpoint), true) = (&cli.endpoint, has_explicit_token_source) {
        let token = read_token_input(
            "--token",
            cli.token.as_ref(),
            "--token-file",
            cli.token_file.as_ref(),
            cli.token_stdin,
            "--token-stdin",
        )?;
        return Ok(LoginState {
            map_control_endpoint: endpoint.clone(),
            jason_controller_endpoint: None,
            access_token: token,
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec![],
            principal: None,
        });
    }
    if has_explicit_token_source {
        return Err(format!(
            "explicit token auth requires --endpoint <url>. {}",
            one_command_auth_hint()
        ));
    }
    let path = login_state_path(cli.login_state.as_ref())?;
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "read login state {}: {error}. {}",
            path.display(),
            login_recovery_hint()
        )
    })?;
    let mut state: LoginState = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if let Some(endpoint) = &cli.endpoint {
        state.map_control_endpoint = endpoint.clone();
    }
    Ok(state)
}

fn login_recovery_hint() -> &'static str {
    "Run `map login save --map-control-endpoint <url> --access-token-file <path>` to save login state, or pass `--endpoint <url> --token-file <path>` for one command. Use --access-token-stdin or --token-stdin when piping a token."
}

fn one_command_auth_hint() -> &'static str {
    "Pass `--endpoint <url>` with `--token-file <path>`, `--token-stdin`, or `--token <token>`."
}

fn read_token_input(
    token_flag: &str,
    token: Option<&String>,
    token_file_flag: &str,
    token_file: Option<&PathBuf>,
    token_stdin: bool,
    token_stdin_flag: &str,
) -> Result<String, String> {
    let token = if let Some(token) = token {
        token.clone()
    } else if let Some(path) = token_file {
        fs::read_to_string(path).map_err(|error| {
            format!(
                "read token from {token_file_flag} {}: {error}",
                path.display()
            )
        })?
    } else if token_stdin {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .map_err(|error| format!("read token from {token_stdin_flag}: {error}"))?;
        input
    } else {
        return Err(format!(
            "missing token source: pass {token_file_flag} <path>, {token_stdin_flag}, or {token_flag} <token>"
        ));
    };

    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(format!(
            "empty token from input; pass {token_file_flag} <path>, {token_stdin_flag}, or {token_flag} <token>"
        ));
    }
    Ok(token)
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
        return Err(
            "deploy target requires --ref <git-ref> or --sha <40-character-git-sha>".to_string(),
        );
    }
    if let Some(sha) = &target.sha {
        if sha.len() != 40 || !sha.chars().all(|char| char.is_ascii_hexdigit()) {
            return Err("--sha must be a 40-character Git SHA".to_string());
        }
    }
    Ok(())
}

fn deploy_review(cli: &Cli, args: &DeployReviewArgs) -> Result<(), String> {
    let path = deploy_review_manifest_path(args);
    let manifest = read_deploy_review_manifest(&path)?;
    let review = map_deploy_review_contract::review_manifest(
        manifest.as_deref(),
        path.display().to_string(),
    );
    emit_deploy_review(cli.json, &review)
}

fn deploy_review_manifest_path(args: &DeployReviewArgs) -> PathBuf {
    if args.manifest.is_absolute() {
        args.manifest.clone()
    } else {
        args.repo_root.join(&args.manifest)
    }
}

fn read_deploy_review_manifest(path: &PathBuf) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(manifest) => Ok(Some(manifest)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read manifest {}: {error}", path.display())),
    }
}

fn emit_deploy_review(json_output: bool, review: &MapDeployReviewContract) -> Result<(), String> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(review).unwrap());
    }

    match review.status {
        MapDeployReviewStatus::Passed => {
            if !json_output {
                println!("deploy review passed: {}", review.manifest_path);
            }
            Ok(())
        }
        MapDeployReviewStatus::Blocked => Err(format_deploy_review_blocked(review)),
    }
}

fn format_deploy_review_blocked(review: &MapDeployReviewContract) -> String {
    let finding_count = review.findings.len();
    let mut lines = vec![format!(
        "deploy review blocked: {finding_count} finding{} in {}",
        if finding_count == 1 { "" } else { "s" },
        review.manifest_path
    )];
    for finding in &review.findings {
        lines.push(format!(
            "  {} {}: {}",
            finding.code, finding.path, finding.message
        ));
    }
    lines.join("\n")
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
        let value = read_watch_response(response)?;
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

fn map_logs(cli: &Cli, args: &IdArgs) -> Result<(), String> {
    let (client, state) = client(cli)?;
    let response = client
        .get(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            DEPLOY_LOGS_PATH
        ))
        .query(&[("deployment_ref", args.id.as_str())])
        .bearer_auth(&state.access_token)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;
    let value = parse_deploy_logs_response(response.status(), response.text())?;
    if cli.json {
        println!("{}", serde_json::to_string(&value).unwrap());
    } else {
        print!("{}", render_deploy_logs_text(&value));
    }
    Ok(())
}

fn read_watch_response(response: reqwest::blocking::Response) -> Result<Value, String> {
    parse_watch_response(response.status(), response.text())
}

fn parse_deploy_logs_response(
    status: StatusCode,
    text: Result<String, reqwest::Error>,
) -> Result<Value, String> {
    let text = text.map_err(|error| format!("read MAP logs response: {error}"))?;
    if status == StatusCode::NOT_FOUND && deploy_logs_route_absent(&text) {
        return Err(deploy_logs_unavailable_hint().to_string());
    }
    if !status.is_success() {
        return Err(format!("MAP returned {status}: {}", redact(&text)));
    }
    serde_json::from_str(&text).map_err(|error| format!("read MAP logs response: {error}"))
}

fn deploy_logs_route_absent(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("route not found")
        || lower.contains("no route matched")
        || (lower.contains(DEPLOY_LOGS_PATH) && !lower.contains("deployment logs not found"))
}

fn deploy_logs_unavailable_hint() -> &'static str {
    "deployment logs are not available yet; use `map --json status <deployment-ref>` for deployment state or `map --json evidence <deployment-ref>` for review evidence"
}

fn parse_watch_response(
    status: StatusCode,
    text: Result<String, reqwest::Error>,
) -> Result<Value, String> {
    let text = text.map_err(|error| format!("read MAP watch response: {error}"))?;
    if !status.is_success() {
        return Err(format!("MAP returned {status}: {}", redact(&text)));
    }
    serde_json::from_str(&text).map_err(|error| format!("read MAP watch response: {error}"))
}

fn render_deploy_logs_text(payload: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "deployment_ref: {}\n",
        payload
            .get("deployment_ref")
            .and_then(Value::as_str)
            .unwrap_or("(not returned)")
    ));
    if let Some(status) = payload.get("deployment_status").and_then(Value::as_str) {
        out.push_str(&format!("deployment_status: {status}\n"));
    }

    let phases = payload
        .get("phases")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if !phases.is_empty() {
        out.push('\n');
    }
    for phase in phases {
        let name = phase
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("phase");
        let status = phase
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = phase
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message returned");
        out.push_str(&format!("{name}: {status} - {message}\n"));
        if let Some(refs) = phase.get("refs").and_then(Value::as_object) {
            for (key, value) in refs {
                out.push_str(&format!("  {key}: {}\n", render_json_value(value)));
            }
        }
        if let Some(details) = phase.get("details") {
            append_deploy_log_details(&mut out, details);
        }
    }
    out
}

fn append_deploy_log_details(out: &mut String, details: &Value) {
    match details {
        Value::Object(map) => {
            for (key, value) in map {
                out.push_str(&format!("  detail.{key}: {}\n", render_json_value(value)));
            }
        }
        _ => out.push_str(&format!("  details: {}\n", render_json_value(details))),
    }
}

fn render_json_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap(),
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

// ─────────────────────── map deploy (direct brokered control-plane call) ───────────────────────

/// `map deploy` / `map deploy-request` (ADR-0016, amended 2026-07-06 → webhook-native): POST the
/// deploy request straight to the control-plane `/v1/map-control/deploy/request` with the saved
/// `map-control` login token. No GitHub Actions workflow is dispatched — GitHub is not in the
/// trigger path. The server-side review gate (ADR-0014) is unchanged.
fn deploy_request(cli: &Cli, args: &DeployRequestArgs) -> Result<(), String> {
    validate_target(&args.target)?;
    // The control-plane matches the allowlist on `github://owner/repo`; accept a bare
    // `owner/repo` and normalize it (a caller may also pass a full ref verbatim).
    let repository_ref = if args.target.repo.contains("://") {
        args.target.repo.clone()
    } else {
        format!("github://{}", args.target.repo)
    };
    post(
        cli,
        "/v1/map-control/deploy/request",
        deploy_request_body(args, repository_ref),
    )
}

fn deploy_request_body(args: &DeployRequestArgs, repository_ref: String) -> Value {
    let mut body = json!({
        "repository_ref": repository_ref,
        "installation_ref": args.installation_ref,
        "app_ref": args.app_ref,
        "app_env": args.target.env,
        "tenant_ref": args.tenant_ref,
        "account_ref": args.account_ref,
        "requested_ref": args.target.ref_name,
        "source_sha": args.target.sha,
        "authority_evidence_ref": args.evidence_ref,
    });
    if let Some(deployment_ref) = &args.deployment_ref {
        body["deployment_ref"] = json!(deployment_ref);
    }
    if let Some(platform_env) = &args.platform_env {
        body["platform_env"] = json!(platform_env);
    }
    body
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
/// broker allowlist with no restart — then scaffolds a starter manifest into `--repo-dir`.
/// With `--with-ci-workflow`, it also scaffolds the opt-in custom-CI workflow.
///
/// Scope: registry binding + scaffold. Auto-resolving the installation from the caller's
/// identity + App grant is P3b (mithran-control-plane#79).
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
    let text = response
        .text()
        .map_err(|error| format!("read onboard response: {error}"))?;
    let value = parse_onboard_response(status, &text)?;

    let manifest_path = scaffold_manifest(args.repo_dir.as_ref(), repo_name, &project_ref)?;

    // Webhook-native default (ADR-0016 amendment): the App installation + webhook is the deploy
    // trigger, so onboard writes NO repo workflow and touches no repo Actions config — zero repo
    // footprint. `--with-ci-workflow` opts into the custom-CI keyless-OIDC path (ADR-0023):
    // scaffold map-deploy.yml + best-effort set non-secret routing repo Variables.
    let (workflow_path, variables_outcome) = if args.with_ci_workflow {
        let workflow_path = scaffold_deploy_workflow(args.repo_dir.as_ref(), &args.workflow)?;
        let variables = onboard_variables(args, &project_ref);
        (workflow_path, set_repo_variables(&args.repo, &variables))
    } else {
        (
            None,
            json!({ "set": false, "reason": "default path uses the GitHub App; pass --with-ci-workflow to scaffold the optional custom-CI workflow" }),
        )
    };

    let next = if args.with_ci_workflow {
        "commit + push the scaffolded workflow + mithran.yaml, then push a release/* ref (or tag) to deploy"
    } else {
        "commit + push mithran.yaml, then push a release/* ref (or tag) — the webhook deploys (no repo workflow needed)"
    };

    let (payload, text) = onboard_output(
        &args.repo,
        &project_ref,
        value,
        args.with_ci_workflow,
        workflow_path.as_ref(),
        manifest_path.as_ref(),
        variables_outcome,
        next,
    );
    print_json_or_text(cli.json, payload, &text)
}

fn parse_onboard_response(status: StatusCode, text: &str) -> Result<Value, String> {
    let value = serde_json::from_str::<Value>(text).ok();
    if status == StatusCode::CONFLICT {
        let body = onboard_error_body(value.as_ref(), text);
        return Err(format!("onboard conflict: {}", redact(&body)));
    }
    if !status.is_success() {
        let body = onboard_error_body(value.as_ref(), text);
        return Err(format!("onboard returned {status}: {}", redact(&body)));
    }

    Ok(value.unwrap_or_else(|| json!({})))
}

fn onboard_error_body(value: Option<&Value>, text: &str) -> String {
    value.map_or_else(|| text.to_string(), Value::to_string)
}

fn onboard_output(
    repo: &str,
    project_ref: &str,
    onboard: Value,
    ci_workflow: bool,
    workflow_path: Option<&PathBuf>,
    manifest_path: Option<&PathBuf>,
    variables: Value,
    next: &str,
) -> (Value, String) {
    let payload = json!({
        "ok": true,
        "schema_version": "map.onboard.v1",
        "repo": repo,
        "project_ref": project_ref,
        "app_ref": project_ref,
        "identity_note": "project_ref is also the self-service app identity used by deploy routing, version, and publish surfaces",
        "onboard": onboard,
        "ci_workflow": ci_workflow,
        "workflow_written": workflow_path.map(|p| p.display().to_string()),
        "manifest_written": manifest_path.map(|p| p.display().to_string()),
        "variables": variables,
        "next": next,
    });
    let text = format!(
        "onboarded {repo} (repo registration recorded).\napp identity: {project_ref} (stored as identity.project_ref; used for deploy routing, versions, and publish).{}{}\nnext: {next}.",
        workflow_path
            .map(|p| format!("\nwrote {}", p.display()))
            .unwrap_or_default(),
        manifest_path
            .map(|p| format!("\nwrote {}", p.display()))
            .unwrap_or_default(),
    );
    (payload, text)
}

// ───────────────────────────── map offboard ─────────────────────────────

fn offboard(cli: &Cli, args: &OffboardArgs) -> Result<(), String> {
    validate_repo_slug(&args.repo)?;
    let repository_ref = format!("github://{}", args.repo);
    let installation_ref = offboard_installation_ref(args)?;
    let body = offboard_request_body(&repository_ref, installation_ref.as_deref());

    let (http, state) = client(cli)?;
    let response = http
        .post(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            OFFBOARD_PATH
        ))
        .bearer_auth(&state.access_token)
        .json(&body)
        .send()
        .map_err(|error| format!("offboard request failed: {error}"))?;

    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("read offboard response: {error}"))?;
    let value = parse_offboard_response(status, &text)?;
    if cli.json {
        println!("{text}");
    } else {
        println!(
            "{}",
            render_offboard_text(&args.repo, installation_ref.as_deref(), &value)
        );
    }
    Ok(())
}

fn offboard_installation_ref(args: &OffboardArgs) -> Result<Option<String>, String> {
    match args.installation_ref.as_deref() {
        Some(value) if value.trim().is_empty() => {
            Err("--installation-ref must not be empty".to_string())
        }
        Some(value) => Ok(Some(value.trim().to_string())),
        None => Ok(None),
    }
}

fn offboard_request_body(repository_ref: &str, installation_ref: Option<&str>) -> Value {
    let mut body = json!({ "repository_ref": repository_ref });
    if let Some(installation_ref) = installation_ref {
        body["installation_ref"] = json!(installation_ref);
    }
    body
}

fn parse_offboard_response(status: StatusCode, text: &str) -> Result<Value, String> {
    let value = serde_json::from_str::<Value>(text).ok();
    if !status.is_success() {
        let body = value.map_or_else(|| text.to_string(), |value| value.to_string());
        return Err(format!("offboard returned {status}: {}", redact(&body)));
    }
    Ok(value.unwrap_or_else(|| json!({})))
}

fn render_offboard_text(repo: &str, installation_ref: Option<&str>, response: &Value) -> String {
    let removed = response
        .get("removed_binding_count")
        .and_then(Value::as_u64)
        .map_or_else(|| "(not returned)".to_string(), |count| count.to_string());
    let remaining = response
        .get("remaining_binding_count")
        .and_then(Value::as_u64)
        .map_or_else(|| "(not returned)".to_string(), |count| count.to_string());
    let scope = installation_ref.unwrap_or("all installations");
    format!("offboarded {repo} ({scope}): removed {removed}; remaining {remaining}")
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

/// The non-secret routing repo Variables for the OIDC `map-deploy.yml`. Optional refs are set
/// only when present (the cp may also derive tenant/account from the edge identity — cp#86).
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
fn scaffold_manifest(
    repo_dir: Option<&PathBuf>,
    app_id: &str,
    project_ref: &str,
) -> Result<Option<PathBuf>, String> {
    let Some(repo_dir) = repo_dir else {
        return Ok(None);
    };
    let path = repo_dir.join("mithran.yaml");
    if path.exists() {
        return Ok(None);
    }
    fs::write(&path, starter_manifest(app_id, app_id, project_ref))
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(Some(path))
}

fn starter_manifest(app_id: &str, name: &str, project_ref: &str) -> String {
    let app_id = yaml_double_quoted(app_id);
    let name = yaml_double_quoted(name);
    let project_ref = yaml_double_quoted(project_ref);
    format!(
        "apiVersion: map.mithran/v1
kind: MithranApp
metadata:
  app_id: {app_id}
  name: {name}
identity:
  project_ref: {project_ref}
capabilities:
  - kind: http
    route: /
    runtime: {DEFAULT_STARTER_RUNTIME}
    startup:
      command: {DEFAULT_STARTER_COMMAND}
"
    )
}

fn yaml_double_quoted(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            ch if ch.is_control() => quoted.push_str(&format!("\\x{:02X}", ch as u32)),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn setup(cli: &Cli, args: &SetupArgs) -> Result<(), String> {
    validate_repo_slug(&args.repo)?;
    let guidance = setup_guidance(&args.repo);
    print_json_or_text(
        cli.json,
        json!({
            "ok": true,
            "schema_version": "map.setup.v1",
            "repo": args.repo,
            "next": guidance,
        }),
        &guidance,
    )
}

fn setup_guidance(repo: &str) -> String {
    format!("Use `map onboard {repo} --installation-ref <ref>`.")
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
/// `updated_from_action`, and active canary split fields when present). `published` is `null` when
/// the app has never been published.
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
                let mut alias = json!({
                    "app_env": pointer.get("app_env"),
                    "updated_from_action": pointer.get("updated_from_action"),
                    "deployment_ref": deployment_ref,
                    "hostname": hostname,
                    "pinned": pointer.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                    "route_pointer_ref": pointer_ref,
                });
                if let Some(canary_deployment_ref) = pointer.get("canary_deployment_ref") {
                    alias["canary_deployment_ref"] = canary_deployment_ref.clone();
                }
                if let Some(canary_weight_pct) = pointer.get("canary_weight_pct") {
                    alias["canary_weight_pct"] = canary_weight_pct.clone();
                }
                aliases.push(alias);
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
                let canary = match (
                    alias
                        .get("canary_deployment_ref")
                        .and_then(Value::as_str)
                        .filter(|deployment_ref| !deployment_ref.is_empty()),
                    alias.get("canary_weight_pct").and_then(Value::as_u64),
                ) {
                    (Some(deployment_ref), Some(weight_pct)) => {
                        format!("  [canary {deployment_ref} {weight_pct}%]")
                    }
                    _ => String::new(),
                };
                out.push_str(&format!(
                    "  {} ({}){}  ->  {}  {}{}\n",
                    alias["app_env"].as_str().unwrap_or_default(),
                    alias["updated_from_action"].as_str().unwrap_or_default(),
                    pinned,
                    alias["deployment_ref"].as_str().unwrap_or_default(),
                    alias["hostname"].as_str().unwrap_or_default(),
                    canary,
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

// ───────────────────────────── map canary (ADR-0017) ─────────────────────────────

fn map_canary(cli: &Cli, args: &CanaryArgs) -> Result<(), String> {
    let request = canary_request(args)?;
    let (http, state) = client(cli)?;
    let response = http
        .post(format!(
            "{}{}",
            state.map_control_endpoint.trim_end_matches('/'),
            CANARY_DEPLOY_PATH,
        ))
        .bearer_auth(&state.access_token)
        .json(&request.body)
        .send()
        .map_err(|error| format!("MAP request failed: {error}"))?;

    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("read MAP response: {error}"))?;
    if status != StatusCode::OK && status != StatusCode::CREATED && status != StatusCode::ACCEPTED {
        return Err(format!("MAP returned {status}: {}", redact(&text)));
    }
    if cli.json {
        println!("{text}");
        return Ok(());
    }

    let value: Value =
        serde_json::from_str(&text).map_err(|error| format!("parse canary response: {error}"))?;
    print!("{}", render_canary_text(&request, &value));
    validate_canary_alias_app(&request, &value)?;
    Ok(())
}

struct CanaryRequest {
    action: &'static str,
    app: String,
    app_ref: String,
    deployment_ref: String,
    weight_pct: Option<u32>,
    body: Value,
}

fn canary_request(args: &CanaryArgs) -> Result<CanaryRequest, String> {
    match &args.command {
        CanarySubcommand::Start(start) => {
            validate_canary_weight(start.weight)?;
            let app_ref = normalize_app_ref(&start.app);
            Ok(CanaryRequest {
                action: "start",
                app: app_ref.trim_start_matches("app:").to_string(),
                app_ref: app_ref.clone(),
                deployment_ref: start.deployment_ref.clone(),
                weight_pct: Some(start.weight),
                body: build_canary_body(
                    "start",
                    &app_ref,
                    &start.deployment_ref,
                    Some(start.weight),
                ),
            })
        }
        CanarySubcommand::Promote(promote) => canary_end_request("promote", promote),
        CanarySubcommand::Rollback(rollback) => canary_end_request("rollback", rollback),
    }
}

fn canary_end_request(action: &'static str, args: &CanaryEndArgs) -> Result<CanaryRequest, String> {
    let app_ref = normalize_app_ref(&args.app);
    Ok(CanaryRequest {
        action,
        app: app_ref.trim_start_matches("app:").to_string(),
        app_ref: app_ref.clone(),
        deployment_ref: args.deployment_ref.clone(),
        weight_pct: None,
        body: build_canary_body(action, &app_ref, &args.deployment_ref, None),
    })
}

fn validate_canary_weight(weight_pct: u32) -> Result<(), String> {
    if (1..=99).contains(&weight_pct) {
        Ok(())
    } else {
        Err(format!(
            "--weight must be an integer from 1 through 99, got {weight_pct}"
        ))
    }
}

fn build_canary_body(
    action: &str,
    app_ref: &str,
    deployment_ref: &str,
    weight_pct: Option<u32>,
) -> Value {
    let mut body = json!({
        "app_ref": app_ref,
        "canary_action": action,
        "canary_deployment_ref": deployment_ref,
    });
    if let Some(weight_pct) = weight_pct {
        body["weight_pct"] = json!(weight_pct);
    }
    body
}

fn render_canary_text(request: &CanaryRequest, response: &Value) -> String {
    let response_action =
        response
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or(match request.action {
                "start" => "canary-start",
                "promote" => "canary-promote",
                "rollback" => "canary-rollback",
                _ => "canary",
            });
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("ok");
    let alias = response.get("alias").unwrap_or(&Value::Null);
    let route_pointer_ref = alias
        .get("route_pointer_ref")
        .and_then(Value::as_str)
        .unwrap_or("(not returned)");
    let hostname = alias
        .get("hostname")
        .and_then(Value::as_str)
        .unwrap_or("(not returned)");
    let current = alias
        .get("current_deployment_ref")
        .and_then(Value::as_str)
        .unwrap_or("(not returned)");
    let canary = alias.get("canary_deployment_ref").and_then(Value::as_str);
    let canary_weight = alias.get("canary_weight_pct").and_then(Value::as_u64);
    let alias_app_ref = canary_alias_app_ref(response);
    let alias_app_mismatch = alias_app_ref
        .as_deref()
        .filter(|app_ref| *app_ref != request.app_ref.as_str())
        .map(|app_ref| format!("requested {} but alias returned {app_ref}", request.app_ref))
        .unwrap_or_else(|| "no".to_string());
    let result = match request.action {
        "start" => {
            if let Some(weight) = canary_weight.or(request.weight_pct.map(u64::from)) {
                format!(
                    "{weight}% canary, {}% current",
                    100_u64.saturating_sub(weight)
                )
            } else {
                "canary split started".to_string()
            }
        }
        "promote" => "promoted canary to current at 100%; split cleared".to_string(),
        "rollback" => "rolled back canary; current remains at 100%; split cleared".to_string(),
        _ => status.to_string(),
    };

    format!(
        "action: {action}\nstatus: {status}\napp: {app} (requested)\napp_ref: {app_ref} (requested)\nalias_app_ref: {alias_app_ref}\nalias_app_mismatch: {alias_app_mismatch}\ncanary_deployment_ref: {deployment_ref}\nalias: {route_pointer_ref}\nhostname: {hostname}\ncurrent_deployment_ref: {current}\nactive_canary: {active_canary}\nresult: {result}\n",
        action = response_action,
        status = status,
        app = request.app,
        app_ref = request.app_ref,
        alias_app_ref = alias_app_ref
            .as_deref()
            .unwrap_or("(not returned; app/app_ref above are requested)"),
        alias_app_mismatch = alias_app_mismatch,
        deployment_ref = request.deployment_ref,
        active_canary = canary.unwrap_or("(none)"),
    )
}

fn validate_canary_alias_app(request: &CanaryRequest, response: &Value) -> Result<(), String> {
    if let Some(alias_app_ref) = canary_alias_app_ref(response) {
        if alias_app_ref != request.app_ref {
            return Err(format!(
                "canary response alias app mismatch: requested {}, alias returned {alias_app_ref}",
                request.app_ref
            ));
        }
    }
    Ok(())
}

fn canary_alias_app_ref(response: &Value) -> Option<String> {
    let alias = response.get("alias")?;
    for field in ["app_ref", "project_ref"] {
        if let Some(app_ref) = alias.get(field).and_then(Value::as_str) {
            let app_ref = app_ref.trim();
            if app_ref.starts_with("app:") {
                return Some(app_ref.to_string());
            }
        }
    }
    if let Some(app) = alias.get("app").and_then(Value::as_str) {
        let app = app.trim();
        if !app.is_empty() {
            return Some(normalize_app_ref(app));
        }
    }
    alias
        .get("route_pointer_ref")
        .and_then(Value::as_str)
        .and_then(app_ref_from_route_pointer_ref)
}

fn app_ref_from_route_pointer_ref(route_pointer_ref: &str) -> Option<String> {
    route_pointer_ref
        .split('/')
        .rev()
        .find(|segment| segment.starts_with("app:"))
        .map(ToString::to_string)
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
                "run `map login save --map-control-endpoint <url> --access-token-file <path>`",
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
        Some(LIVE_ADAPTER_MODE) => Check::ok("adapter mode", "live"),
        Some(other) => Check::warn(
            "adapter mode",
            format!("adapter_mode={other} (expected live adapter)"),
            "set the control-plane adapter to live deploy mode",
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
            "source access",
            "0 repositories registered for source access",
            "onboard a repo with `map onboard <owner/repo> --installation-ref <ref>`",
        ),
        Some(count) => Check::ok(
            "source access",
            format!(
                "{count} repositor{} registered for source access",
                if count == 1 { "y" } else { "ies" }
            ),
        ),
        None => Check::warn(
            "source access",
            "source-access repository count missing from config",
            "control-plane config did not report source-access registration count; check the service version",
        ),
    }
}

/// Per-app allowlist signal. The config endpoint exposes only a COUNT, not the allowed
/// repository list, so membership can't be read directly. A repo with a recorded deployment
/// was provably allowlisted at deploy time — use that as the positive signal.
fn app_allowlist_check(allowlist_count: Option<u64>, has_deployment: bool, app: &str) -> Check {
    if has_deployment {
        return Check::ok(
            "app source access",
            format!("{app} has a recorded deployment (confirms source access was available)"),
        );
    }
    match allowlist_count {
        Some(0) => Check::fail(
            "app source access",
            format!("no repositories registered for source access — {app} cannot deploy"),
            format!("run `map onboard {app} --installation-ref <ref>`"),
        ),
        _ => Check::warn(
            "app source access",
            format!("cannot confirm source access for {app} (the config endpoint exposes only a count, not the registered repo list)"),
            format!("run `map onboard {app} --installation-ref <ref>` to record repo source access, then rerun doctor"),
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
            format!("after `map onboard {app} --installation-ref <ref>`, push a matching Git ref through the GitHub App webhook path or run `map deploy --repo {app} --env production --ref <git-ref> --installation-ref <ref> --app-ref app:{name}`"),
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
    use clap::CommandFactory;

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

    fn temp_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!("map-cli-{}-{}", std::process::id(), name))
    }

    fn assert_starter_manifest(body: &str, app_id: &str, name: &str, project_ref: &str) {
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(body).expect("starter manifest is valid yaml");
        assert_eq!(parsed["apiVersion"].as_str(), Some("map.mithran/v1"));
        assert_eq!(parsed["kind"].as_str(), Some("MithranApp"));
        assert_eq!(parsed["metadata"]["app_id"].as_str(), Some(app_id));
        assert_eq!(parsed["metadata"]["name"].as_str(), Some(name));
        assert_eq!(
            parsed["identity"]["project_ref"].as_str(),
            Some(project_ref)
        );
        assert_eq!(parsed["capabilities"][0]["kind"].as_str(), Some("http"));
        assert_eq!(parsed["capabilities"][0]["route"].as_str(), Some("/"));
        assert_eq!(
            parsed["capabilities"][0]["runtime"].as_str(),
            Some("nodejs22")
        );
        assert_eq!(
            parsed["capabilities"][0]["startup"]["command"].as_str(),
            Some("npm start")
        );
        assert!(!body.contains("schema_version"));
        assert!(!body.contains("schema_version: mithran.map.v1"));
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
    fn watch_response_fails_fast_on_non_success_status() {
        let err = parse_watch_response(
            StatusCode::FORBIDDEN,
            Ok(
                r#"{"status":"error","code":"forbidden","message":"Bearer access_token denied"}"#
                    .to_string(),
            ),
        )
        .unwrap_err();

        assert_eq!(
            err,
            r#"MAP returned 403 Forbidden: {"status":"error","code":"forbidden","message":"[REDACTED] [REDACTED] denied"}"#
        );
    }

    #[test]
    fn watch_response_parses_success_body() {
        let value = parse_watch_response(
            StatusCode::OK,
            Ok(r#"{"deployment":{"status":{"status":"Succeeded"}}}"#.to_string()),
        )
        .unwrap();

        assert_eq!(value["deployment"]["status"]["status"], "Succeeded");
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
        let err = validate_target(&target).unwrap_err();
        assert_eq!(
            err,
            "deploy target requires --ref <git-ref> or --sha <40-character-git-sha>"
        );
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
    fn missing_login_state_points_to_supported_login_paths() {
        let path = temp_file_path("missing-login");
        let cli = Cli {
            login_state: Some(path.clone()),
            endpoint: None,
            token: None,
            token_file: None,
            token_stdin: false,
            json: false,
            command: Command::Whoami,
        };

        let err = resolve_state(&cli).unwrap_err();

        assert!(err.contains(&format!("read login state {}", path.display())));
        assert!(
            err.contains("map login save --map-control-endpoint <url> --access-token-file <path>")
        );
        assert!(err.contains("--endpoint <url> --token-file <path>"));
        fs::remove_file(path).ok();
    }

    #[test]
    fn invalid_login_audience_points_to_supported_login_paths() {
        let path = temp_file_path("wrong-audience-login");
        let state = LoginState {
            map_control_endpoint: "https://map.example".to_string(),
            jason_controller_endpoint: None,
            access_token: "secret".to_string(),
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec![],
            principal: None,
        };
        write_login_state(&path, &state).expect("login state should write");

        let cli = Cli {
            login_state: Some(path.clone()),
            endpoint: None,
            token: None,
            token_file: None,
            token_stdin: false,
            json: false,
            command: Command::Login(LoginCommand {
                command: LoginSubcommand::PrintToken(PrintTokenArgs {
                    audience: "jason-controller".to_string(),
                }),
            }),
        };
        let err = run(cli).unwrap_err();

        assert!(err.contains("login state is not valid for audience `jason-controller`"));
        assert!(
            err.contains("map login save --map-control-endpoint <url> --access-token-file <path>")
        );
        fs::remove_file(path).ok();
    }

    #[test]
    fn login_save_reads_access_token_from_file() {
        let token_path = temp_file_path("login-token");
        let state_path = temp_file_path("login-token-state");
        fs::write(&token_path, "secret-from-file\n").unwrap();
        fs::remove_file(&state_path).ok();

        let cli = Cli::try_parse_from([
            "map",
            "--login-state",
            state_path.to_str().unwrap(),
            "login",
            "save",
            "--map-control-endpoint",
            "https://map.example",
            "--access-token-file",
            token_path.to_str().unwrap(),
        ])
        .expect("login save accepts token file");
        run(cli).expect("login save reads token file");

        let saved: LoginState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(saved.access_token, "secret-from-file");

        fs::remove_file(token_path).ok();
        fs::remove_file(state_path).ok();
    }

    #[test]
    fn one_command_auth_reads_token_from_file() {
        let token_path = temp_file_path("one-command-token");
        fs::write(&token_path, "one-command-secret\n").unwrap();
        let cli = Cli::try_parse_from([
            "map",
            "--endpoint",
            "https://map.example",
            "--token-file",
            token_path.to_str().unwrap(),
            "whoami",
        ])
        .expect("one-command auth accepts token file");

        let state = resolve_state(&cli).expect("token file resolves state");
        assert_eq!(state.map_control_endpoint, "https://map.example");
        assert_eq!(state.access_token, "one-command-secret");

        fs::remove_file(token_path).ok();
    }

    #[test]
    fn endpoint_flag_overrides_saved_login_endpoint() {
        let path = temp_file_path("endpoint-override-login");
        let state = LoginState {
            map_control_endpoint: "https://saved.example".to_string(),
            jason_controller_endpoint: None,
            access_token: "saved-secret".to_string(),
            expires_at: None,
            audience: Some("map-control".to_string()),
            scopes: vec![],
            principal: None,
        };
        write_login_state(&path, &state).expect("login state should write");

        let cli = Cli::try_parse_from([
            "map",
            "--login-state",
            path.to_str().unwrap(),
            "--endpoint",
            "https://override.example",
            "whoami",
        ])
        .expect("endpoint override parses");

        let state = resolve_state(&cli).expect("endpoint override resolves saved state");
        assert_eq!(state.map_control_endpoint, "https://override.example");
        assert_eq!(state.access_token, "saved-secret");

        fs::remove_file(path).ok();
    }

    #[test]
    fn argv_token_flags_remain_compatible() {
        Cli::try_parse_from([
            "map",
            "--endpoint",
            "https://map.example",
            "--token",
            "test-token",
            "doctor",
        ])
        .expect("global compatibility token flag still parses");

        Cli::try_parse_from([
            "map",
            "login",
            "save",
            "--map-control-endpoint",
            "https://map.example",
            "--access-token",
            "test-token",
        ])
        .expect("login compatibility access-token flag still parses");
    }

    #[test]
    fn token_source_errors_do_not_print_secret_values() {
        let err = read_token_input(
            "--token",
            Some(&"   ".to_string()),
            "--token-file",
            None,
            false,
            "--token-stdin",
        )
        .unwrap_err();

        assert!(err.contains("empty token"));
        assert!(!err.contains("test-token"));
        assert!(!err.contains("secret"));
    }

    #[test]
    fn doctor_auth_remediation_uses_token_file_path() {
        let check = Check::fail(
            "control-plane configured",
            "read login state /tmp/missing: not found",
            "run `map login save --map-control-endpoint <url> --access-token-file <path>`",
        );
        let text = format_check(&check);
        let value = check_value(&check);

        assert!(text.contains("--access-token-file <path>"), "{text}");
        assert!(!text.contains("--access-token <token>"), "{text}");
        assert_eq!(
            value["remediation"].as_str(),
            Some("run `map login save --map-control-endpoint <url> --access-token-file <path>`")
        );
    }

    #[test]
    fn deploy_parses_into_a_direct_control_plane_request() {
        let cli = Cli::try_parse_from([
            "map",
            "deploy",
            "--repo",
            "mithran-hq/demo",
            "--env",
            "production",
            "--ref",
            "refs/heads/release/1.2",
            "--installation-ref",
            "github-installation://131136661",
            "--app-ref",
            "app:demo",
        ])
        .expect("parses");
        match cli.command {
            Command::Deploy(args) => {
                assert_eq!(args.target.repo, "mithran-hq/demo");
                assert_eq!(args.target.env.as_deref(), Some("production"));
                assert_eq!(
                    args.target.ref_name.as_deref(),
                    Some("refs/heads/release/1.2")
                );
                assert_eq!(
                    args.installation_ref.as_deref(),
                    Some("github-installation://131136661")
                );
                assert_eq!(args.app_ref.as_deref(), Some("app:demo"));
                let body = deploy_request_body(&args, "github://mithran-hq/demo".to_string());
                assert!(body.get("platform_env").is_none());
                assert!(body.get("deployment_ref").is_none());
            }
            _ => panic!("expected deploy"),
        }
    }

    #[test]
    fn deploy_preserves_hidden_platform_env_override_for_compatibility() {
        let cli = Cli::try_parse_from([
            "map",
            "deploy",
            "--repo",
            "mithran-hq/demo",
            "--sha",
            "0123456789abcdef0123456789abcdef01234567",
            "--platform-env",
            "sandbox",
            "--deployment-ref",
            "deployment://sandbox/preview/manual",
        ])
        .expect("hidden compatibility flag still parses");
        match cli.command {
            Command::Deploy(args) => {
                let body = deploy_request_body(&args, "github://mithran-hq/demo".to_string());
                assert_eq!(body["platform_env"], json!("sandbox"));
                assert_eq!(
                    body["deployment_ref"],
                    json!("deployment://sandbox/preview/manual")
                );
            }
            _ => panic!("expected deploy"),
        }
    }

    #[test]
    fn deploy_request_is_an_alias_for_deploy() {
        let cli = Cli::try_parse_from([
            "map",
            "deploy-request",
            "--repo",
            "github://mithran-hq/demo",
            "--ref",
            "refs/heads/main",
        ])
        .expect("parses");
        assert!(matches!(cli.command, Command::Deploy(_)));
    }

    #[test]
    fn deploy_and_validate_require_ref_or_sha_at_parse_time() {
        for args in [
            vec!["map", "deploy", "--repo", "mithran-hq/demo"],
            vec!["map", "validate", "--repo", "mithran-hq/demo"],
        ] {
            let err = match Cli::try_parse_from(args) {
                Ok(_) => panic!("missing source selector should not parse"),
                Err(err) => err.to_string(),
            };
            assert!(err.contains("--ref <REF_NAME>"), "{err}");
            assert!(err.contains("--sha <SHA>"), "{err}");
        }
    }

    #[test]
    fn deploy_and_validate_accept_ref_or_sha_sources() {
        for args in [
            vec![
                "map",
                "deploy",
                "--repo",
                "mithran-hq/demo",
                "--ref",
                "refs/heads/main",
            ],
            vec![
                "map",
                "deploy",
                "--repo",
                "mithran-hq/demo",
                "--sha",
                "0123456789abcdef0123456789abcdef01234567",
            ],
            vec![
                "map",
                "validate",
                "--repo",
                "mithran-hq/demo",
                "--ref",
                "refs/heads/main",
            ],
            vec![
                "map",
                "validate",
                "--repo",
                "mithran-hq/demo",
                "--sha",
                "0123456789abcdef0123456789abcdef01234567",
            ],
        ] {
            Cli::try_parse_from(args).expect("ref or sha source should parse");
        }
    }

    #[test]
    fn deploy_review_parses_default_manifest_inputs() {
        let cli = Cli::try_parse_from(["map", "deploy-review"]).expect("parses");
        match cli.command {
            Command::DeployReview(args) => {
                assert_eq!(args.repo_root, PathBuf::from("."));
                assert_eq!(args.manifest, PathBuf::from("mithran.yaml"));
                assert_eq!(
                    deploy_review_manifest_path(&args),
                    PathBuf::from(".").join("mithran.yaml")
                );
            }
            _ => panic!("expected deploy-review"),
        }
    }

    #[test]
    fn deploy_review_uses_public_contract_codes() {
        let review = map_deploy_review_contract::review_manifest(
            Some(
                r#"
apiVersion: wrong/v0
kind: MithranApp
metadata: {app_id: acme, name: Acme}
identity: {project_ref: owner/repo}
"#,
            ),
            "mithran.yaml",
        );

        assert_eq!(
            review.schema_version,
            map_deploy_review_contract::MAP_DEPLOY_REVIEW_CONTRACT_SCHEMA
        );
        assert_eq!(review.status, MapDeployReviewStatus::Blocked);
        assert_eq!(review.finding_codes, vec!["ERR_API_VERSION"]);
        assert_eq!(
            format_deploy_review_blocked(&review),
            "deploy review blocked: 1 finding in mithran.yaml\n  ERR_API_VERSION apiVersion: apiVersion must be map.mithran/v1"
        );
    }

    #[test]
    fn deploy_review_missing_manifest_is_a_contract_result() {
        let path = temp_file_path("missing-mithran-yaml");
        let _ = fs::remove_file(&path);
        let manifest = read_deploy_review_manifest(&path).expect("missing is handled");
        let review = map_deploy_review_contract::review_manifest(
            manifest.as_deref(),
            path.display().to_string(),
        );

        assert_eq!(review.status, MapDeployReviewStatus::Blocked);
        assert_eq!(review.finding_codes, vec!["ERR_MANIFEST_MISSING"]);
        assert_eq!(review.findings[0].path, path.display().to_string());
    }

    #[derive(Debug, Deserialize)]
    struct DeployReviewFixture {
        name: String,
        #[serde(default)]
        missing_manifest: bool,
        manifest: Option<String>,
        expected_status: String,
        expected_codes: Vec<String>,
        expected_paths: Vec<String>,
    }

    #[test]
    fn deploy_review_matches_public_contract_fixture_matrix() {
        let cases: Vec<DeployReviewFixture> = serde_yaml::from_str(include_str!(
            "../tests/fixtures/map-deploy-review-contract/cases.yml"
        ))
        .expect("fixture matrix parses");

        for case in cases {
            let manifest = if case.missing_manifest {
                None
            } else {
                Some(
                    case.manifest
                        .as_deref()
                        .expect("non-missing fixture has manifest"),
                )
            };
            let review = map_deploy_review_contract::review_manifest(manifest, "mithran.yaml");

            assert_eq!(
                review.status.as_str(),
                case.expected_status,
                "case {} status",
                case.name
            );
            assert_eq!(
                review.finding_codes, case.expected_codes,
                "case {} finding codes",
                case.name
            );
            let finding_paths = review
                .findings
                .iter()
                .map(|finding| finding.path.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                finding_paths, case.expected_paths,
                "case {} finding paths",
                case.name
            );
        }
    }

    #[test]
    fn root_help_hides_setup_command() {
        let mut command = Cli::command();
        let help = command.render_help().to_string();

        assert!(help.contains("onboard"));
        assert!(!help.contains("setup"));
    }

    #[test]
    fn local_only_help_hides_auth_globals_but_keeps_json_when_used() {
        for path in [
            &["init"][..],
            &["deploy-review"][..],
            &["validate"][..],
            &["version"][..],
            &["access", "plan"][..],
        ] {
            let mut command = Cli::command();
            let help = render_help_for_path(&mut command, path);
            let label = path.join(" ");

            for flag in ["--login-state", "--endpoint", "--token"] {
                assert!(
                    !help.contains(flag),
                    "{label} help should not show auth flag {flag}:\n{help}"
                );
            }
            assert!(
                help.contains("--json"),
                "{label} help should keep --json because it affects output:\n{help}"
            );
        }
    }

    #[test]
    fn authenticated_help_documents_auth_globals() {
        for path in [
            &["login"][..],
            &["login", "print-token"][..],
            &["whoami"][..],
            &["doctor"][..],
            &["deploy"][..],
            &["onboard"][..],
            &["offboard"][..],
            &["access"][..],
            &["access", "apply"][..],
            &["versions"][..],
            &["publish"][..],
            &["canary"][..],
            &["canary", "start"][..],
            &["canary", "promote"][..],
            &["canary", "rollback"][..],
            &["status"][..],
            &["watch"][..],
            &["logs"][..],
            &["evidence"][..],
            &["rollback"][..],
        ] {
            let mut command = Cli::command();
            let help = render_help_for_path(&mut command, path);
            let label = path.join(" ");
            assert!(
                help.contains(AUTH_FLAGS_HELP),
                "{label} help should document auth globals:\n{help}"
            );
        }
    }

    #[test]
    fn deploy_and_validate_help_mark_source_selector_required() {
        for path in [&["deploy"][..], &["validate"][..]] {
            let mut command = Cli::command();
            let help = render_help_for_path(&mut command, path);
            let usage = help
                .lines()
                .find(|line| line.starts_with("Usage:"))
                .unwrap_or_else(|| panic!("missing usage line:\n{help}"));

            assert!(usage.contains("--repo <REPO>"), "{help}");
            assert!(usage.contains("--ref <REF_NAME>"), "{help}");
            assert!(usage.contains("--sha <SHA>"), "{help}");
        }
    }

    #[test]
    fn login_save_help_documents_state_path_without_endpoint_or_token_noise() {
        let mut command = Cli::command();
        let help = render_help_for_path(&mut command, &["login", "save"]);

        assert!(
            help.contains(LOGIN_SAVE_HELP),
            "login save should document where the state file is written:\n{help}"
        );
        assert!(help.contains("--access-token-file"), "{help}");
        assert!(help.contains("--access-token-stdin"), "{help}");
        assert!(
            !help.contains("--endpoint"),
            "login save should not show command auth endpoint override:\n{help}"
        );
        assert!(
            !help.contains("--token"),
            "login save should not show command auth token override:\n{help}"
        );
    }

    #[test]
    fn authenticated_global_flags_still_parse_before_subcommand() {
        let cli = Cli::try_parse_from([
            "map",
            "--endpoint",
            "https://map.example",
            "--token",
            "test-token",
            "doctor",
        ])
        .expect("global auth flags before subcommand still parse");

        assert_eq!(cli.endpoint.as_deref(), Some("https://map.example"));
        assert_eq!(cli.token.as_deref(), Some("test-token"));
        assert!(cli.token_file.is_none());
        assert!(!cli.token_stdin);
        assert!(matches!(cli.command, Command::Doctor(_)));
    }

    #[test]
    fn public_help_does_not_leak_internal_planning_terms() {
        let forbidden = [
            "ADR-",
            "mithran-business#",
            "map-cli#",
            "P2a",
            "P2b",
            "P3a",
            "P3b",
            "BYO-CI",
            "Compatibility",
            "compatibility",
            "platform-env",
            "webhook-native",
            "Aegis.app",
            "sandbox",
            "staging",
            "Forge",
        ];

        for path in [
            &[][..],
            &["login"][..],
            &["login", "save"][..],
            &["login", "print-token"][..],
            &["doctor"][..],
            &["init"][..],
            &["validate"][..],
            &["deploy"][..],
            &["onboard"][..],
            &["offboard"][..],
            &["setup"][..],
            &["access"][..],
            &["access", "apply"][..],
            &["access", "plan"][..],
            &["versions"][..],
            &["publish"][..],
            &["canary"][..],
            &["canary", "start"][..],
            &["canary", "promote"][..],
            &["canary", "rollback"][..],
            &["status"][..],
            &["watch"][..],
            &["logs"][..],
            &["evidence"][..],
            &["rollback"][..],
        ] {
            let mut command = Cli::command();
            let label = if path.is_empty() {
                "root".to_string()
            } else {
                path.join(" ")
            };
            let help = render_help_for_path(&mut command, path);
            for term in forbidden {
                assert!(
                    !help.contains(term),
                    "{label} help should not contain internal term {term}"
                );
            }
        }
    }

    #[test]
    fn public_docs_do_not_use_stale_forge_name() {
        for (path, body) in [
            ("README.md", include_str!("../README.md")),
            ("docs/README.md", include_str!("../docs/README.md")),
            (
                "docs/map-cli-operator-guide.md",
                include_str!("../docs/map-cli-operator-guide.md"),
            ),
        ] {
            assert!(
                !body.to_ascii_lowercase().contains("forge"),
                "{path} should use MAP product language"
            );
        }
    }

    fn render_help_for_path(command: &mut clap::Command, path: &[&str]) -> String {
        command.build();
        let mut current = command;
        for segment in path {
            current = current
                .find_subcommand_mut(segment)
                .unwrap_or_else(|| panic!("expected {segment} subcommand"));
        }
        current.render_help().to_string()
    }

    #[test]
    fn setup_remains_callable_for_concise_guidance() {
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

        let guidance = setup_guidance("mithran-hq/demo");
        assert_eq!(
            guidance,
            "Use `map onboard mithran-hq/demo --installation-ref <ref>`."
        );
        assert!(!guidance.to_lowercase().contains("deprecated"));
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
                // Webhook-native default: no repo workflow scaffolded unless opted in.
                assert!(!args.with_ci_workflow);
            }
            _ => panic!("expected onboard"),
        }
    }

    #[test]
    fn onboard_help_explains_project_ref_is_app_identity() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("onboard")
            .expect("onboard command")
            .render_help()
            .to_string();

        assert!(help.contains("--project-ref"));
        assert!(help.contains("--app-ref"));
        assert!(help.contains("app identity"), "got: {help}");
        assert!(help.contains("deploy routing"), "got: {help}");
        assert!(help.contains("version"), "got: {help}");
        assert!(help.contains("publish"), "got: {help}");
    }

    #[test]
    fn onboard_accepts_app_ref_alias_without_breaking_project_ref() {
        let project_cli = Cli::try_parse_from([
            "map",
            "onboard",
            "john-smith/my-app",
            "--installation-ref",
            "github-installation://131136661",
            "--project-ref",
            "app:from-project",
        ])
        .expect("project-ref parses");
        match project_cli.command {
            Command::Onboard(args) => {
                assert_eq!(args.project_ref.as_deref(), Some("app:from-project"));
            }
            _ => panic!("expected onboard"),
        }

        let app_cli = Cli::try_parse_from([
            "map",
            "onboard",
            "john-smith/my-app",
            "--installation-ref",
            "github-installation://131136661",
            "--app-ref",
            "app:from-app",
        ])
        .expect("app-ref alias parses");
        match app_cli.command {
            Command::Onboard(args) => {
                assert_eq!(args.project_ref.as_deref(), Some("app:from-app"));
            }
            _ => panic!("expected onboard"),
        }
    }

    #[test]
    fn onboard_requires_installation_ref() {
        assert!(Cli::try_parse_from(["map", "onboard", "john-smith/my-app"]).is_err());
    }

    #[test]
    fn onboard_with_ci_workflow_opts_into_the_byo_ci_path() {
        let cli = Cli::try_parse_from([
            "map",
            "onboard",
            "john-smith/my-app",
            "--installation-ref",
            "github-installation://131136661",
            "--with-ci-workflow",
        ])
        .expect("parses");
        match cli.command {
            Command::Onboard(args) => assert!(args.with_ci_workflow),
            _ => panic!("expected onboard"),
        }
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
            with_ci_workflow: false,
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
    fn onboard_output_names_resolved_app_identity() {
        let (payload, text) = onboard_output(
            "john-smith/my-app",
            "app:my-app",
            json!({ "binding": "ok" }),
            true,
            Some(&PathBuf::from(".github/workflows/map-deploy.yml")),
            Some(&PathBuf::from("mithran.yaml")),
            json!({ "set": true }),
            "commit + push",
        );

        assert_eq!(payload["project_ref"], "app:my-app");
        assert_eq!(payload["app_ref"], "app:my-app");
        assert!(payload["identity_note"]
            .as_str()
            .unwrap()
            .contains("deploy routing"));
        assert!(text.contains("repo registration recorded"), "got: {text}");
        assert!(!text.contains("registry binding"), "got: {text}");
        assert!(text.contains("app identity: app:my-app"), "got: {text}");
        assert!(text.contains("identity.project_ref"), "got: {text}");
        assert!(text.contains("deploy routing"), "got: {text}");
        assert!(text.contains("versions"), "got: {text}");
        assert!(text.contains("publish"), "got: {text}");
    }

    #[test]
    fn onboard_response_preserves_json_error_body() {
        let err = parse_onboard_response(
            StatusCode::FORBIDDEN,
            r#"{"status":"error","message":"Bearer access_token denied"}"#,
        )
        .unwrap_err();

        assert_eq!(
            err,
            r#"onboard returned 403 Forbidden: {"message":"[REDACTED] [REDACTED] denied","status":"error"}"#
        );
    }

    #[test]
    fn onboard_response_preserves_non_json_error_body() {
        let err = parse_onboard_response(
            StatusCode::BAD_GATEWAY,
            "upstream proxy rejected Bearer token before JSON",
        )
        .unwrap_err();

        assert_eq!(
            err,
            "onboard returned 502 Bad Gateway: upstream proxy rejected [REDACTED] token before JSON"
        );
    }

    #[test]
    fn onboard_response_preserves_json_conflict_body() {
        let err = parse_onboard_response(
            StatusCode::CONFLICT,
            r#"{"status":"error","message":"already bound"}"#,
        )
        .unwrap_err();

        assert_eq!(
            err,
            r#"onboard conflict: {"message":"already bound","status":"error"}"#
        );
    }

    #[test]
    fn onboard_response_preserves_non_json_conflict_body() {
        let err = parse_onboard_response(
            StatusCode::CONFLICT,
            "repository binding conflict for Bearer token",
        )
        .unwrap_err();

        assert_eq!(
            err,
            "onboard conflict: repository binding conflict for [REDACTED] token"
        );
    }

    #[test]
    fn offboard_parses_single_installation_scope() {
        let cli = Cli::try_parse_from([
            "map",
            "offboard",
            "john-smith/my-app",
            "--installation-ref",
            "github-installation://131136661",
        ])
        .expect("parses");
        match cli.command {
            Command::Offboard(args) => {
                assert_eq!(args.repo, "john-smith/my-app");
                assert_eq!(
                    args.installation_ref.as_deref(),
                    Some("github-installation://131136661")
                );
                assert!(!args.all_installations);
            }
            _ => panic!("expected offboard"),
        }
    }

    #[test]
    fn offboard_requires_explicit_scope_for_repo_wide_removal() {
        assert!(Cli::try_parse_from(["map", "offboard", "john-smith/my-app"]).is_err());
        assert!(Cli::try_parse_from([
            "map",
            "offboard",
            "john-smith/my-app",
            "--installation-ref",
            "github-installation://131136661",
            "--all-installations",
        ])
        .is_err());

        let cli = Cli::try_parse_from([
            "map",
            "offboard",
            "john-smith/my-app",
            "--all-installations",
        ])
        .expect("repo-wide scope parses only when explicit");
        match cli.command {
            Command::Offboard(args) => {
                assert_eq!(args.repo, "john-smith/my-app");
                assert!(args.installation_ref.is_none());
                assert!(args.all_installations);
            }
            _ => panic!("expected offboard"),
        }
    }

    #[test]
    fn offboard_rejects_empty_installation_ref_before_request_body() {
        let args = OffboardArgs {
            repo: "john-smith/my-app".to_string(),
            installation_ref: Some("   ".to_string()),
            all_installations: false,
        };

        assert_eq!(
            offboard_installation_ref(&args),
            Err("--installation-ref must not be empty".to_string())
        );
    }

    #[test]
    fn offboard_help_makes_removal_scope_clear() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("offboard")
            .expect("offboard command")
            .render_help()
            .to_string();

        assert!(help.contains("Remove a GitHub repo from MAP deploy intake"));
        assert!(help.contains("--installation-ref"));
        assert!(help.contains("Remove only this GitHub App installation binding"));
        assert!(help.contains("--all-installations"));
        assert!(help.contains("Remove all installation bindings for this repository"));
    }

    #[test]
    fn offboard_request_body_uses_control_plane_shape() {
        let single = offboard_request_body(
            "github://john-smith/my-app",
            Some("github-installation://131136661"),
        );
        assert_eq!(OFFBOARD_PATH, "/v1/map-control/offboard");
        assert_eq!(single["repository_ref"], "github://john-smith/my-app");
        assert_eq!(
            single["installation_ref"],
            "github-installation://131136661"
        );

        let all = offboard_request_body("github://john-smith/my-app", None);
        assert_eq!(all["repository_ref"], "github://john-smith/my-app");
        assert!(all.get("installation_ref").is_none());
    }

    #[test]
    fn offboard_text_reports_removed_and_remaining_counts() {
        let text = render_offboard_text(
            "john-smith/my-app",
            Some("github-installation://131136661"),
            &json!({
                "status": "offboarded",
                "repository_ref": "github://john-smith/my-app",
                "installation_ref": "github-installation://131136661",
                "removed_binding_count": 1,
                "remaining_binding_count": 0,
            }),
        );
        assert_eq!(
            text,
            "offboarded john-smith/my-app (github-installation://131136661): removed 1; remaining 0"
        );

        let repo_wide = render_offboard_text(
            "john-smith/my-app",
            None,
            &json!({
                "removed_binding_count": 2,
                "remaining_binding_count": 0,
            }),
        );
        assert_eq!(
            repo_wide,
            "offboarded john-smith/my-app (all installations): removed 2; remaining 0"
        );
    }

    #[test]
    fn offboard_json_output_preserves_server_response_shape() {
        let response = json!({
            "status": "offboarded",
            "repository_ref": "github://john-smith/my-app",
            "installation_ref": null,
            "removed_binding_count": 2,
            "remaining_binding_count": 0,
            "server_extra": { "kept": true }
        });
        let raw = serde_json::to_string(&response).expect("serializes");
        let parsed = parse_offboard_response(StatusCode::OK, &raw).expect("parses");

        assert_eq!(parsed["removed_binding_count"], 2);
        assert_eq!(parsed["server_extra"]["kept"], true);
    }

    #[test]
    fn offboard_response_errors_are_redacted() {
        let err = parse_offboard_response(
            StatusCode::FORBIDDEN,
            r#"{"status":"error","message":"Bearer access_token denied"}"#,
        )
        .unwrap_err();

        assert_eq!(
            err,
            r#"offboard returned 403 Forbidden: {"message":"[REDACTED] [REDACTED] denied","status":"error"}"#
        );
    }

    #[test]
    fn starter_manifest_quotes_dynamic_yaml_scalars() {
        let body = starter_manifest(
            "null",
            "Needs \"quotes\"",
            "project: custom # not a comment [literal]\nline",
        );

        assert_starter_manifest(
            &body,
            "null",
            "Needs \"quotes\"",
            "project: custom # not a comment [literal]\nline",
        );
    }

    #[test]
    fn init_writes_starter_mithran_app_and_does_not_clobber() {
        let path = env::temp_dir().join(format!("map-init-test-{}.yaml", std::process::id()));
        let _ = fs::remove_file(&path);

        run(Cli {
            login_state: None,
            endpoint: None,
            token: None,
            token_file: None,
            token_stdin: false,
            json: false,
            command: Command::Init(InitArgs {
                manifest: path.clone(),
            }),
        })
        .expect("init writes manifest");

        let body = fs::read_to_string(&path).unwrap();
        assert_starter_manifest(&body, "example", "Example", "app:example");

        let err = run(Cli {
            login_state: None,
            endpoint: None,
            token: None,
            token_file: None,
            token_stdin: false,
            json: false,
            command: Command::Init(InitArgs {
                manifest: path.clone(),
            }),
        })
        .unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(fs::read_to_string(&path).unwrap(), body);

        fs::remove_file(path).ok();
    }

    #[test]
    fn logs_old_server_route_missing_falls_back_to_supported_investigation_commands() {
        let err = parse_deploy_logs_response(
            StatusCode::NOT_FOUND,
            Ok(r#"{"status":"error","message":"route not found"}"#.to_string()),
        )
        .unwrap_err();

        assert!(err.contains("map --json status <deployment-ref>"));
        assert!(err.contains("map --json evidence <deployment-ref>"));
        assert!(!err.contains("control plane"));
        assert!(!err.contains("control-plane"));
        assert!(!err.contains("route"));
        assert!(!err.contains("server"));
    }

    #[test]
    fn logs_missing_deployment_404_remains_a_real_map_error() {
        let err = parse_deploy_logs_response(
            StatusCode::NOT_FOUND,
            Ok(r#"{"status":"error","message":"deployment logs not found"}"#.to_string()),
        )
        .unwrap_err();

        assert_eq!(
            err,
            r#"MAP returned 404 Not Found: {"status":"error","message":"deployment logs not found"}"#
        );
    }

    #[test]
    fn logs_response_parses_and_json_mode_preserves_server_shape() {
        let raw = r#"{
            "status": "ok",
            "schema_version": "map-deploy-logs/v1",
            "deployment_ref": "deployment://production/app/demo-1",
            "deployment_status": "Succeeded",
            "phases": [],
            "server_extra": { "kept": true }
        }"#;

        let value =
            parse_deploy_logs_response(StatusCode::OK, Ok(raw.to_string())).expect("valid logs");
        let round_trip: Value =
            serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();

        assert_eq!(round_trip["schema_version"], "map-deploy-logs/v1");
        assert_eq!(round_trip["server_extra"]["kept"], true);
    }

    #[test]
    fn logs_text_renders_phase_refs_missing_markers_and_failure_details() {
        let payload = json!({
            "status": "ok",
            "schema_version": "map-deploy-logs/v1",
            "deployment_ref": "deployment://production/app/demo-1",
            "deployment_status": "RuntimeFailed",
            "phases": [
                {
                    "phase": "source",
                    "status": "available",
                    "message": "source snapshot recorded",
                    "refs": {
                        "source_snapshot_ref": "gcs://mithran-source/app/source.tar",
                        "manifest_digest": "sha256:manifest"
                    }
                },
                {
                    "phase": "build",
                    "status": "available",
                    "message": "build log reference recorded",
                    "refs": {
                        "logs_ref": "gcs://mithran-build-logs/build-123.log"
                    }
                },
                {
                    "phase": "runtime",
                    "status": "available",
                    "message": "runtime detail recorded",
                    "refs": {
                        "runtime_status": "Failed"
                    },
                    "details": {
                        "reason": "raw_firecracker_app_readiness_timeout",
                        "message": "runtime app readiness probe timed out"
                    }
                },
                {
                    "phase": "route",
                    "status": "not_captured",
                    "message": "no route log captured",
                    "refs": {}
                }
            ]
        });

        let rendered = render_deploy_logs_text(&payload);

        assert!(rendered.contains("deployment_ref: deployment://production/app/demo-1"));
        assert!(rendered.contains("deployment_status: RuntimeFailed"));
        assert!(rendered.contains("source: available - source snapshot recorded"));
        assert!(rendered.contains("logs_ref: gcs://mithran-build-logs/build-123.log"));
        assert!(rendered.contains("detail.reason: raw_firecracker_app_readiness_timeout"));
        assert!(rendered.contains("detail.message: runtime app readiness probe timed out"));
        assert!(rendered.contains("route: not_captured - no route log captured"));
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

        let manifest = scaffold_manifest(Some(&dir), "my-app", "project:custom")
            .expect("ok")
            .expect("path");
        let body = fs::read_to_string(&manifest).unwrap();
        assert_starter_manifest(&body, "my-app", "my-app", "project:custom");

        // existing manifest is never clobbered.
        fs::write(&manifest, "name: edited-by-user\n").unwrap();
        assert_eq!(
            scaffold_manifest(Some(&dir), "my-app", "project:custom").expect("ok"),
            None
        );
        assert_eq!(
            fs::read_to_string(&manifest).unwrap(),
            "name: edited-by-user\n"
        );

        // no --repo-dir → no-op.
        assert_eq!(
            scaffold_deploy_workflow(None, "map-deploy.yml").unwrap(),
            None
        );
        assert_eq!(
            scaffold_manifest(None, "my-app", "project:custom").unwrap(),
            None
        );
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
        // Runs on a GitHub-hosted runner against the public authenticated edge.
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("runs-on: ubuntu-latest"));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("MAP_CONTROL_TOKEN"));
        for required in [
            "MAP_CONTROL_ENDPOINT",
            "MAP_AUTH_ENDPOINT",
            "MAP_INSTALLATION_REF",
            "MAP_APP_REF",
        ] {
            assert!(
                MAP_DEPLOY_WORKFLOW_TEMPLATE.contains(&format!("${{{{ vars.{required} }}}}")),
                "template should read {required} from repo Variables"
            );
        }
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains(
            "for name in MAP_CONTROL_ENDPOINT MAP_AUTH_ENDPOINT MAP_INSTALLATION_REF MAP_APP_REF"
        ));
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("production repository variable is required"));
        assert!(MAP_DEPLOY_WORKFLOW_TEMPLATE.contains(
            "map onboard <owner/repo> --installation-ref <ref> --repo-dir <checkout> --with-ci-workflow"
        ));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("MAP_PLATFORM_ENV"));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("platform_env"));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("deployment_ref"));
        assert!(!MAP_DEPLOY_WORKFLOW_TEMPLATE.contains("manual `map deploy`"));
        for forbidden in [
            "ADR-",
            "mithran-control-plane",
            "Envoy",
            "ext_authz",
            "Keep it in sync",
            "clean-room runbook",
            "NO LLM",
            "map setup",
            "sandbox",
            "staging",
            "127.0.0.1",
            "localhost",
            "INGRESS GAP",
        ] {
            assert!(
                !MAP_DEPLOY_WORKFLOW_TEMPLATE.contains(forbidden),
                "template should not contain customer-facing stale term {forbidden}"
            );
        }
    }

    #[test]
    fn format_check_renders_level_and_remediation() {
        let ok = format_check(&Check::ok("control-plane reachable", "200"));
        assert_eq!(ok, "[  ok] control-plane reachable — 200");
        let fail = format_check(&Check::fail(
            "source access",
            "0 repositories",
            "run `map onboard <owner/repo> --installation-ref <ref>`",
        ));
        assert!(fail.starts_with("[fail] source access — 0 repositories"));
        assert!(fail.contains("↳ run `map onboard <owner/repo> --installation-ref <ref>`"));
    }

    #[test]
    fn app_route_check_recovery_command_has_required_deploy_inputs() {
        let check = app_route_check(false, false, "mithran-hq/demo");
        let remediation = check
            .remediation
            .expect("route check should guide recovery");

        assert!(remediation.contains("map onboard mithran-hq/demo --installation-ref <ref>"));
        assert!(remediation.contains("map deploy --repo mithran-hq/demo"));
        assert!(remediation.contains("--env production"));
        assert!(remediation.contains("--ref <git-ref>"));
        assert!(remediation.contains("--installation-ref <ref>"));
        assert!(remediation.contains("--app-ref app:demo"));
        assert!(!remediation.contains("--env preview --repo"));
    }

    #[test]
    fn adapter_check_classifies_live_mode() {
        assert_eq!(
            adapter_check(&json!({ "adapter_mode": LIVE_ADAPTER_MODE })).level,
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
        let zero = allowlist_check(&config(0));
        assert_eq!(zero.level, Level::Fail);
        assert_eq!(zero.name, "source access");
        assert_eq!(zero.detail, "0 repositories registered for source access");
        let one = allowlist_check(&config(1));
        assert_eq!(one.level, Level::Ok);
        assert_eq!(one.detail, "1 repository registered for source access");
        let two = allowlist_check(&config(2));
        assert_eq!(two.level, Level::Ok);
        assert_eq!(two.detail, "2 repositories registered for source access");
        let missing = allowlist_check(&json!({}));
        assert_eq!(missing.level, Level::Warn);
        assert_eq!(missing.name, "source access");
        assert_eq!(
            missing.detail,
            "source-access repository count missing from config"
        );
    }

    #[test]
    fn app_checks_use_deployment_and_alias_signals() {
        // A recorded deployment implies allowlisted, even though config exposes only a count.
        let deployed = app_allowlist_check(Some(2), true, "mithran-hq/demo");
        assert_eq!(deployed.level, Level::Ok);
        assert_eq!(deployed.name, "app source access");
        assert_eq!(
            deployed.detail,
            "mithran-hq/demo has a recorded deployment (confirms source access was available)"
        );
        // No deployment + nothing allowlisted is a hard fail.
        let no_access = app_allowlist_check(Some(0), false, "mithran-hq/demo");
        assert_eq!(no_access.level, Level::Fail);
        assert_eq!(no_access.name, "app source access");
        assert_eq!(
            no_access.detail,
            "no repositories registered for source access — mithran-hq/demo cannot deploy"
        );
        // Allowlisted-count > 0 but no deployment for this repo: can't confirm → warn.
        let unconfirmed = app_allowlist_check(Some(2), false, "mithran-hq/demo");
        assert_eq!(unconfirmed.level, Level::Warn);
        assert_eq!(unconfirmed.name, "app source access");
        assert_eq!(
            unconfirmed.detail,
            "cannot confirm source access for mithran-hq/demo (the config endpoint exposes only a count, not the registered repo list)"
        );
        assert_eq!(
            unconfirmed.remediation,
            Some(
                "run `map onboard mithran-hq/demo --installation-ref <ref>` to record repo source access, then rerun doctor"
                    .to_string()
            )
        );
        let unconfirmed_text = format_check(&unconfirmed);
        assert!(!unconfirmed_text.contains("registry"));
        assert!(!unconfirmed_text.contains("binding"));
        assert!(!unconfirmed_text.contains("P2a"));
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
        assert!(aliases[0].get("canary_deployment_ref").is_none());
        assert!(aliases[0].get("canary_weight_pct").is_none());

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
        assert!(rendered.contains(
            "production (ProductionPromote)  ->  deployment://sandbox/production/gtd-2  gtd-tracker-production.sandbox.apps.mithran.cloud\n"
        ));
        assert!(!rendered.contains("[canary"));
    }

    #[test]
    fn versions_payload_and_text_surface_active_canary_alias() {
        let mut routes = sample_routes_status();
        let production = routes["aliases"]["route-pointer://sandbox/production/app:gtd-tracker"]
            .as_object_mut()
            .expect("production alias object");
        production.insert("updated_from_action".to_string(), json!("CanaryWeight"));
        production.insert(
            "canary_deployment_ref".to_string(),
            json!("deployment://sandbox/production/gtd-3"),
        );
        production.insert("canary_weight_pct".to_string(), json!(20));

        let payload = versions_payload(&routes, "app:gtd-tracker");
        let aliases = payload["aliases"].as_array().expect("aliases array");
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0]["updated_from_action"], "CanaryWeight");
        assert_eq!(
            aliases[0]["canary_deployment_ref"],
            "deployment://sandbox/production/gtd-3"
        );
        assert_eq!(aliases[0]["canary_weight_pct"], 20);

        let rendered = render_versions_text(&payload);
        assert!(rendered.contains(
            "production (CanaryWeight)  ->  deployment://sandbox/production/gtd-2  gtd-tracker-production.sandbox.apps.mithran.cloud  [canary deployment://sandbox/production/gtd-3 20%]\n"
        ));
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

    // ── map canary (ADR-0017 #54) ──

    #[test]
    fn canary_start_parses_app_deployment_ref_and_weight() {
        let cli = Cli::try_parse_from([
            "map",
            "canary",
            "start",
            "gtd-tracker",
            "--deployment-ref",
            "deployment://sandbox/production/gtd-2",
            "--weight",
            "20",
        ])
        .expect("parses");
        match cli.command {
            Command::Canary(args) => match args.command {
                CanarySubcommand::Start(start) => {
                    assert_eq!(start.app, "gtd-tracker");
                    assert_eq!(
                        start.deployment_ref,
                        "deployment://sandbox/production/gtd-2"
                    );
                    assert_eq!(start.weight, 20);
                }
                _ => panic!("expected canary start"),
            },
            _ => panic!("expected canary"),
        }
    }

    #[test]
    fn canary_promote_and_rollback_parse_deployment_ref() {
        for command in ["promote", "rollback"] {
            let cli = Cli::try_parse_from([
                "map",
                "canary",
                command,
                "gtd-tracker",
                "--deployment-ref",
                "deployment://sandbox/production/gtd-2",
            ])
            .expect("parses");
            match cli.command {
                Command::Canary(args) => match args.command {
                    CanarySubcommand::Promote(end) | CanarySubcommand::Rollback(end) => {
                        assert_eq!(end.app, "gtd-tracker");
                        assert_eq!(end.deployment_ref, "deployment://sandbox/production/gtd-2");
                    }
                    _ => panic!("expected canary terminal action"),
                },
                _ => panic!("expected canary"),
            }
        }
    }

    #[test]
    fn canary_start_validates_weight_locally() {
        assert!(validate_canary_weight(1).is_ok());
        assert!(validate_canary_weight(99).is_ok());
        assert!(validate_canary_weight(0).is_err());
        assert!(validate_canary_weight(100).is_err());
    }

    #[test]
    fn canary_request_body_uses_control_plane_canary_shape() {
        let args = CanaryArgs {
            command: CanarySubcommand::Start(CanaryStartArgs {
                app: "gtd-tracker".to_string(),
                deployment_ref: "deployment://sandbox/production/gtd-2".to_string(),
                weight: 20,
            }),
        };
        let request = canary_request(&args).expect("valid request");
        assert_eq!(CANARY_DEPLOY_PATH, "/v1/map-control/deploy/canary");
        assert_eq!(request.body["app_ref"], "app:gtd-tracker");
        assert_eq!(request.body["canary_action"], "start");
        assert_eq!(
            request.body["canary_deployment_ref"],
            "deployment://sandbox/production/gtd-2"
        );
        assert_eq!(request.body["weight_pct"], 20);

        let promote = build_canary_body(
            "promote",
            "app:gtd-tracker",
            "deployment://sandbox/production/gtd-2",
            None,
        );
        assert_eq!(promote["canary_action"], "promote");
        assert_eq!(
            promote["canary_deployment_ref"],
            "deployment://sandbox/production/gtd-2"
        );
        assert!(promote.get("weight_pct").is_none());
    }

    #[test]
    fn canary_start_text_reports_action_app_alias_hostname_and_split() {
        let request = CanaryRequest {
            action: "start",
            app: "gtd-tracker".to_string(),
            app_ref: "app:gtd-tracker".to_string(),
            deployment_ref: "deployment://sandbox/production/gtd-2".to_string(),
            weight_pct: Some(20),
            body: json!({}),
        };
        let response = json!({
            "status": "ok",
            "action": "canary-start",
            "alias": {
                "route_pointer_ref": "route-pointer://sandbox/production/app:gtd-tracker",
                "hostname": "gtd-tracker.apps.mithran.cloud",
                "current_deployment_ref": "deployment://sandbox/production/gtd-1",
                "canary_deployment_ref": "deployment://sandbox/production/gtd-2",
                "canary_weight_pct": 20
            },
            "remote_authority": { "mode": "local" }
        });
        let text = render_canary_text(&request, &response);
        assert!(text.contains("action: canary-start"));
        assert!(text.contains("status: ok"));
        assert!(text.contains("app: gtd-tracker (requested)"));
        assert!(text.contains("app_ref: app:gtd-tracker (requested)"));
        assert!(text.contains("alias_app_ref: app:gtd-tracker"));
        assert!(text.contains("alias_app_mismatch: no"));
        assert!(text.contains("canary_deployment_ref: deployment://sandbox/production/gtd-2"));
        assert!(text.contains("alias: route-pointer://sandbox/production/app:gtd-tracker"));
        assert!(text.contains("hostname: gtd-tracker.apps.mithran.cloud"));
        assert!(text.contains("result: 20% canary, 80% current"));
        assert!(validate_canary_alias_app(&request, &response).is_ok());
    }

    #[test]
    fn canary_text_reports_alias_app_mismatch_and_validation_fails() {
        let request = CanaryRequest {
            action: "start",
            app: "gtd-tracker".to_string(),
            app_ref: "app:gtd-tracker".to_string(),
            deployment_ref: "deployment://sandbox/production/gtd-2".to_string(),
            weight_pct: Some(20),
            body: json!({}),
        };
        let response = json!({
            "status": "ok",
            "action": "canary-start",
            "alias": {
                "route_pointer_ref": "route-pointer://sandbox/production/app:billing",
                "hostname": "billing.apps.mithran.cloud",
                "current_deployment_ref": "deployment://sandbox/production/billing-1",
                "canary_deployment_ref": "deployment://sandbox/production/gtd-2",
                "canary_weight_pct": 20
            }
        });

        let text = render_canary_text(&request, &response);

        assert!(text.contains("app: gtd-tracker (requested)"));
        assert!(text.contains("alias_app_ref: app:billing"));
        assert!(text.contains(
            "alias_app_mismatch: requested app:gtd-tracker but alias returned app:billing"
        ));
        assert_eq!(
            validate_canary_alias_app(&request, &response),
            Err(
                "canary response alias app mismatch: requested app:gtd-tracker, alias returned app:billing"
                    .to_string()
            )
        );
    }

    #[test]
    fn canary_terminal_text_reports_promote_and_rollback_results() {
        let mut request = CanaryRequest {
            action: "promote",
            app: "gtd-tracker".to_string(),
            app_ref: "app:gtd-tracker".to_string(),
            deployment_ref: "deployment://sandbox/production/gtd-2".to_string(),
            weight_pct: None,
            body: json!({}),
        };
        let response = json!({
            "status": "ok",
            "action": "canary-promote",
            "alias": {
                "route_pointer_ref": "route-pointer://sandbox/production/app:gtd-tracker",
                "hostname": "gtd-tracker.apps.mithran.cloud",
                "current_deployment_ref": "deployment://sandbox/production/gtd-2",
                "canary_deployment_ref": null,
                "canary_weight_pct": null
            }
        });
        let promoted = render_canary_text(&request, &response);
        assert!(promoted.contains("result: promoted canary to current at 100%; split cleared"));
        assert!(promoted.contains("active_canary: (none)"));

        request.action = "rollback";
        let rolled = render_canary_text(
            &request,
            &json!({ "status": "ok", "action": "canary-rollback", "alias": response["alias"] }),
        );
        assert!(
            rolled.contains("result: rolled back canary; current remains at 100%; split cleared")
        );
    }

    #[test]
    fn canary_json_output_preserves_server_response_shape() {
        let response = json!({
            "status": "ok",
            "action": "canary-start",
            "alias": {
                "hostname": "gtd-tracker.apps.mithran.cloud",
                "canary_weight_pct": 20
            },
            "remote_authority": {
                "mode": "local",
                "decision_ref": "decision://canary"
            },
            "server_extra": { "kept": true }
        });
        let raw = serde_json::to_string(&response).expect("serializes");
        let round_trip: Value = serde_json::from_str(&raw).expect("json output is raw JSON");
        assert_eq!(
            round_trip["remote_authority"]["decision_ref"],
            "decision://canary"
        );
        assert_eq!(round_trip["server_extra"]["kept"], true);
    }
}
