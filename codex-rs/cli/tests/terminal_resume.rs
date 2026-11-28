//! Exercise OSC 7577 through the real CLI, embedded server, and a fresh PTY.

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::ensure;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_utils_pty::ChildFds;
use codex_utils_pty::SpawnedProcess;
use codex_utils_pty::TerminalSize;
use core_test_support::responses as mock;
use pretty_assertions::assert_eq;
use sha2::Digest as _;
use sha2::Sha256;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_string_contains;

const TOKEN: &str = "dummy-terminal-resume-token";
const NEXT_TOKEN: &str = "another-dummy-terminal-resume-token";

#[tokio::test]
async fn saved_primary_replaces_and_clears_only_its_own_resume_command() -> Result<()> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("codex home");
    let workspace = root.path().join("task workspace");
    fs::create_dir(&home)?;
    fs::create_dir(&workspace)?;
    let workspace = workspace.canonicalize()?;
    let home = home.canonicalize()?;
    let server = mock::start_mock_server().await;
    fs::write(
        home.join("config.toml"),
        format!(
            r#"model_provider = "fixture"
check_for_update_on_startup = false
cli_auth_credentials_store = "file"
chatgpt_base_url = "{}/backend-api"
analytics.enabled = false
tui.disable_paste_burst = true
tui.status_line = ["thread-name"]
windows.sandbox = "unelevated"
shell_environment_policy.ignore_default_excludes = true
[model_providers.fixture]
name = "Fixture"
base_url = "{}/v1"
wire_api = "responses"
supports_websockets = false
[projects.{}]
trust_level = "trusted"
"#,
            server.uri(),
            server.uri(),
            serde_json::to_string(&workspace)?,
        ),
    )?;
    fs::write(
        home.join("restore.config.toml"),
        "web_search = 'disabled'\nmodel = 'gpt-5.4'\nmodel_reasoning_effort = 'medium'\nservice_tier = 'fast'\n",
    )?;
    // Dotenv must not reintroduce a pane credential after native startup has taken it.
    fs::write(
        home.join(".env"),
        "OSC7577_TOKEN=dummy-dotenv-should-not-load\n",
    )?;
    Mock::given(body_string_contains(
        r#"\"thread_source\":\"thread_title\""#,
    ))
    .respond_with(
        ResponseTemplate::new(200)
            .set_body_raw(answer(r#"{"title":"Resume fixture"}"#), "text/event-stream"),
    )
    .mount(&server)
    .await;
    #[cfg(not(windows))]
    let probe = r#"if [ "${OSC7577_TOKEN+x}" = x ]; then echo exposed; else echo private; fi"#;
    #[cfg(windows)]
    let probe = "if (Test-Path Env:OSC7577_TOKEN) { 'exposed' } else { 'private' }";
    let _tool = mock::mount_sse_once_match(
        &server,
        body_string_contains("private launch prompt"),
        mock::sse(vec![
            mock::ev_response_created("tool"),
            mock::ev_exec_command_call("probe-env", probe),
            mock::ev_completed("tool"),
        ]),
    )
    .await;
    let tool_result = mock::mount_sse_once_match(
        &server,
        body_string_contains(r#""type":"function_call_output""#),
        answer("ORIGINAL_PRIMARY"),
    )
    .await;
    let _side = mock::mount_sse_once_match(
        &server,
        body_string_contains("side turn prompt"),
        answer("SIDE_RESPONSE"),
    )
    .await;
    let _new = mock::mount_sse_once_match(
        &server,
        body_string_contains("new primary prompt"),
        answer("NEW_PRIMARY"),
    )
    .await;
    let resumed_turn = mock::mount_sse_once_match(
        &server,
        body_string_contains("after recovery prompt"),
        answer("RESUMED_PRIMARY"),
    )
    .await;
    let program = codex_utils_cargo_bin::cargo_bin("codex")?.canonicalize()?;
    let argv = [
        "--no-daemon",
        "--no-alt-screen",
        "--profile",
        "restore",
        "--model",
        "gpt-5.4-mini",
        "--sandbox",
        "read-only",
        "--ask-for-approval",
        "never",
        "-c",
        r#"model_reasoning_effort="low""#,
        "-c",
        r#"service_tier="flex""#,
        "-c",
        "features.shell_snapshot=false",
        "-c",
        r#"automatic_account_selection="disabled""#,
        "--cd",
        workspace.to_str().context("workspace path")?,
        "private launch prompt",
    ]
    .map(str::to_owned);
    let mut terminal = Terminal::start(&program, &argv, root.path(), &home, TOKEN).await?;
    terminal
        .wait_for(|out, _| out.contains("ORIGINAL_PRIMARY") && out.contains("state=done:app=codex"))
        .await?;
    let [(recover_argv, cwd)] = terminal
        .records(TOKEN)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected one resume report for the saved primary"))?;
    let id = ThreadId::from_string(&recover_argv[4])?;
    let expected = [
        program.display().to_string(),
        "--codex-resume-home".to_owned(),
        home.display().to_string(),
        "resume".to_owned(),
        id.to_string(),
        "--no-daemon".to_owned(),
        "--no-alt-screen".to_owned(),
        "--profile".to_owned(),
        "restore".to_owned(),
        "--config".to_owned(),
        "features.shell_snapshot=false".to_owned(),
        "--config".to_owned(),
        r#"automatic_account_selection="disabled""#.to_owned(),
    ];
    let mut normalized_argv = recover_argv.clone();
    for index in [0, 2] {
        let path = Path::new(&recover_argv[index]);
        ensure!(path.is_absolute());
        normalized_argv[index] = path.canonicalize()?.display().to_string();
    }
    assert_eq!(normalized_argv, expected);
    assert_eq!(Path::new(&cwd).canonicalize()?, workspace);
    ensure!(terminal.output.contains(&format!(":session={id}\x1b\\")));
    let shell_output = tool_result
        .single_request()
        .function_call_output("probe-env");
    ensure!(shell_output.to_string().contains("private"));
    ensure!(!shell_output.to_string().contains("exposed"));
    ensure!(!terminal.output.contains(TOKEN));
    #[cfg(target_os = "linux")]
    if fs::metadata("/proc/self")?.uid() != 0 {
        let pid = terminal
            .output
            .split_once("\x1b]7577;pid=")
            .and_then(|(_, report)| report.split(':').next())
            .context("resume process ID")?
            .parse::<u32>()?;
        ensure!(
            fs::File::open(format!("/proc/{pid}/environ"))
                .is_err_and(|err| err.kind() == std::io::ErrorKind::PermissionDenied),
            "the pane credential must be inaccessible through the reporter's initial environment"
        );
    }
    let rollout = codex_rollout::find_thread_path_by_id_str(
        &home,
        &id.to_string(),
        /*state_db_ctx*/ None,
    )
    .await?
    .context("resume report must refer to durable history")?;

    terminal.submit("/side").await?;
    terminal
        .wait_for(|_, screen| screen.contains("Side from main thread"))
        .await?;
    terminal.submit("side turn prompt").await?;
    terminal
        .wait_for(|_, screen| screen.contains("SIDE_RESPONSE"))
        .await?;
    terminal.send(b"\x03").await?;
    terminal
        .wait_for(|_, screen| screen.contains("ORIGINAL_PRIMARY"))
        .await?;
    assert_eq!(terminal.records(TOKEN)?.len(), 1);
    terminal.submit("/new").await?;
    terminal
        .wait_for(|out, screen| {
            out.contains(":clear=1:mac=")
                && out.split(":session=").skip(1).any(|tail| {
                    tail.get(..36).is_some_and(|candidate| {
                        ThreadId::from_string(candidate).is_ok_and(|next| next != id)
                    })
                })
                && !screen.contains("ORIGINAL_PRIMARY")
        })
        .await?;
    assert_eq!(terminal.records(TOKEN)?.len(), 1);
    terminal.submit("/rename Saved draft").await?;
    terminal
        .wait_for(|_, screen| screen.contains("Saved draft") && !screen.contains("/rename"))
        .await?;
    assert_eq!(terminal.records(TOKEN)?.len(), 1);
    terminal.submit("new primary prompt").await?;
    terminal
        .wait_for(|out, screen| {
            out.matches("\x1b]7577;").count() >= 3 && screen.contains("NEW_PRIMARY")
        })
        .await?;
    let records = terminal.records(TOKEN)?;
    assert_eq!(records.len(), 2);
    ensure!(records[1].0[4] != id.to_string());
    terminal.quit().await?;

    let mut recovered = Terminal::start(
        &program,
        &recover_argv[1..],
        Path::new(&cwd),
        root.path(),
        NEXT_TOKEN,
    )
    .await?;
    recovered
        .wait_for(|out, screen| out.contains(":argv=") && screen.contains("ORIGINAL_PRIMARY"))
        .await?;
    ensure!(recovered.output.contains(&format!(":session={id}\x1b\\")));
    recovered.submit("after recovery prompt").await?;
    recovered
        .wait_for(|out, _| {
            out.contains("RESUMED_PRIMARY") && out.matches("state=done:app=codex").count() >= 1
        })
        .await?;
    let request = resumed_turn.single_request().body_json();
    assert_eq!(request["model"], "gpt-5.4-mini");
    assert_eq!(request["reasoning"]["effort"], "low");
    assert_eq!(request["service_tier"], "flex");
    recovered.quit().await?;
    assert_eq!(recovered.records(NEXT_TOKEN)?, [(recover_argv, cwd)]);
    ensure!(!recovered.output.contains(NEXT_TOKEN));
    let history = fs::read_to_string(rollout)?;
    ensure!(!history.contains(TOKEN) && !history.contains(NEXT_TOKEN));
    let context = history
        .lines()
        .rev()
        .find_map(
            |line| match codex_rollout::parse_rollout_line(line).ok()?.item {
                codex_rollout::RolloutItem::TurnContext(context) => Some(context),
                _ => None,
            },
        )
        .context("persisted settings after native recovery")?;
    assert_eq!(context.approval_policy, AskForApproval::Never);
    assert_eq!(context.permission_profile, PermissionProfile::read_only());
    assert_eq!(context.cwd.as_path().canonicalize()?, workspace);
    Ok(())
}

fn answer(marker: &str) -> String {
    mock::sse(vec![
        mock::ev_response_created("reply"),
        mock::ev_assistant_message("reply-message", marker),
        mock::ev_completed("reply"),
    ])
}

struct Terminal {
    pty: SpawnedProcess,
    output: String,
    screen: vt100::Parser,
    queries: Vec<(&'static str, &'static [u8])>,
}

impl Terminal {
    async fn start(
        program: &Path,
        argv: &[String],
        cwd: &Path,
        home: &Path,
        token: &str,
    ) -> Result<Self> {
        let mut env: HashMap<String, String> = [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SystemRoot",
            "ComSpec",
            "PATHEXT",
            "TEMP",
            "TMP",
        ]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect();
        env.extend([
            ("CODEX_HOME".into(), home.display().to_string()),
            ("TERM".into(), "xterm-256color".into()),
            ("HC_TERMINAL_ID".into(), "terminal-resume-test".into()),
            ("HC_TERMINAL_SOCKET".into(), "terminal-resume-test".into()),
            ("OSC7577_TOKEN".into(), token.into()),
        ]);
        let pty = codex_utils_pty::spawn_pty_process(
            program,
            argv,
            cwd,
            &env,
            /*arg0*/ &None,
            TerminalSize {
                rows: 45,
                cols: 120,
            },
            ChildFds::Inherited(&[]),
        )
        .await?;
        Ok(Self {
            pty,
            output: String::new(),
            screen: vt100::Parser::new(
                /*rows*/ 45, /*cols*/ 120, /*scrollback_len*/ 0,
            ),
            queries: vec![
                ("\x1b[6n", b"\x1b[1;1R"),
                ("\x1b[?u", b"\x1b[?0u"),
                ("\x1b[c", b"\x1b[?1;2c"),
                ("\x1b]10;?", b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
                ("\x1b]11;?", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
            ],
        })
    }

    async fn send(&self, input: &[u8]) -> Result<()> {
        Ok(self
            .pty
            .session
            .writer_sender()
            .send(input.to_vec())
            .await?)
    }

    async fn submit(&mut self, input: &str) -> Result<()> {
        self.send(format!("\x1b[200~{input}\x1b[201~").as_bytes())
            .await?;
        self.wait_for(|_, screen| {
            screen.contains(input) && !screen.contains("tab to queue message")
        })
        .await?;
        self.send(b"\r").await
    }

    async fn wait_for(&mut self, ready: impl Fn(&str, &str) -> bool) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(40), async {
            while !ready(&self.output, &self.screen.screen().contents()) {
                let bytes = self.pty.stdout_rx.recv().await.context("terminal closed")?;
                self.output.push_str(&String::from_utf8_lossy(&bytes));
                self.screen.process(&bytes);
                let writer = self.pty.session.writer_sender();
                for (query, reply) in &mut self.queries {
                    if !query.is_empty() && self.output.contains(*query) {
                        writer.send(reply.to_vec()).await?;
                        *query = "";
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "terminal did not reach the expected state:\n{}",
                self.screen.screen().contents()
            )
        })?
    }

    async fn quit(&mut self) -> Result<()> {
        let clears = self.output.matches(":clear=1:mac=").count();
        self.submit("/quit").await?;
        self.wait_for(|out, _| out.matches(":clear=1:mac=").count() > clears)
            .await?;
        let exit_code =
            tokio::time::timeout(Duration::from_secs(15), &mut self.pty.exit_rx).await??;
        assert_eq!(exit_code, 0);
        while let Ok(bytes) = self.pty.stdout_rx.try_recv() {
            self.output.push_str(&String::from_utf8_lossy(&bytes));
        }
        Ok(())
    }

    fn records(&self, token: &str) -> Result<Vec<(Vec<String>, String)>> {
        let mut records = Vec::new();
        for frame in self.output.split("\x1b]7577;").skip(1) {
            let (body, _) = frame
                .split_once("\x1b\\")
                .context("unterminated resume report")?;
            let (body, mac) = body.rsplit_once(":mac=").context("missing resume MAC")?;
            ensure!(mac == format!("{:x}", Sha256::digest(format!("{body}:{token}"))));
            if body.ends_with(":clear=1") {
                continue;
            }
            let (_, args) = body.split_once(":argv=").context("missing resume argv")?;
            let (args, cwd) = args.split_once(":cwd=").context("missing resume cwd")?;
            let decode =
                |value| Ok::<_, anyhow::Error>(String::from_utf8(STANDARD.decode(value)?)?);
            records.push((
                args.split(',').map(decode).collect::<Result<_>>()?,
                decode(cwd)?,
            ));
        }
        Ok(records)
    }
}
