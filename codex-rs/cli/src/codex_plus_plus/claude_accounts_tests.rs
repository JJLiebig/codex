use super::*;
use pretty_assertions::assert_eq;

fn fake_login(lines: &[&str], exit_code: u8) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("powershell");
        let script = lines
            .iter()
            .map(|line| format!("[Console]::WriteLine('{}')", line.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join("; ");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("{script}; [Console]::Error.WriteLine('secret-error'); exit {exit_code}"),
        ]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        let script = lines
            .iter()
            .map(|line| format!("printf '%s\\n' '{}'", line.replace('\'', "'\"'\"'")))
            .collect::<Vec<_>>()
            .join("; ");
        command.args([
            "-c",
            &format!("{script}; printf '%s\\n' 'secret-error' >&2; exit {exit_code}"),
        ]);
        command
    }
}

#[tokio::test]
async fn claude_login_requires_saved_marker_and_successful_exit() {
    for (lines, exit_code, expected) in [
        (
            vec!["Claude authentication successful", "secret-error"],
            0,
            false,
        ),
        (vec!["Claude authentication failed: secret-code"], 0, false),
        (vec!["Claude authentication successful!"], 1, false),
        (vec!["Claude authentication successful!"], 0, true),
    ] {
        let result = run_login(
            fake_login(&lines, exit_code),
            std::future::pending(),
            |_| panic!("unexpected URL"),
        )
        .await;
        assert_eq!(result.is_ok(), expected);
        assert!(
            !result
                .err()
                .is_some_and(|error| error.to_string().contains("secret"))
        );
    }
    let error = run_login(
        fake_login(&[], /*exit_code*/ 13),
        std::future::pending(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("54545"));
}

#[tokio::test]
async fn claude_login_cancellation_stops_the_child_without_claiming_success() {
    let home = tempfile::tempdir().unwrap();
    let pid_file = home.path().join("login.pid");
    #[cfg(windows)]
    let command = {
        let mut command = Command::new("powershell");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "[System.IO.File]::WriteAllText('{}', [string]$PID); Start-Sleep -Seconds 60",
                pid_file.display().to_string().replace('\'', "''")
            ),
        ]);
        command
    };
    #[cfg(not(windows))]
    let command = {
        let mut command = Command::new("sh");
        command
            .args(["-c", "echo $$ > \"$1\"; exec sleep 60", "fake-login"])
            .arg(&pid_file);
        command
    };
    let result = run_login(
        command,
        async {
            tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 5), async {
                while !std::fs::read_to_string(&pid_file)
                    .is_ok_and(|pid| pid.trim().parse::<u32>().is_ok())
                {
                    tokio::time::sleep(std::time::Duration::from_millis(/*millis*/ 20)).await;
                }
            })
            .await
            .expect("fake child started");
            Ok(())
        },
        |_| {},
    )
    .await;
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    #[cfg(windows)]
    {
        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}"),
            ])
            .status()
            .unwrap();
        assert!(status.success(), "cancelled login child is still running");
    }
    #[cfg(unix)]
    assert_eq!(
        unsafe {
            libc::kill(pid as libc::pid_t, /*sig*/ 0)
        },
        -1,
        "cancelled login child is still running"
    );
}

#[tokio::test]
async fn claude_login_only_discloses_pinned_authorization_link() {
    let link = "https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&scope=user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload&code_challenge=synthetic-challenge&code_challenge_method=S256&state=synthetic-state";
    let mut disclosed = Vec::new();
    let output = format!(
        "secret-error\nVisit the following URL to continue authentication:\n{link}\nClaude authentication successful!\n"
    );
    assert!(
        read_login_output(output.as_bytes(), |url| disclosed.push(url.to_owned()))
            .await
            .unwrap()
    );
    assert_eq!(disclosed, vec![link.to_owned()]);
    for unsafe_url in [
        "http://localhost:54545/callback?code=secret-code".to_owned(),
        link.replace("code=true", "code=secret-code"),
    ] {
        let output = format!("Visit the following URL to continue authentication:\n{unsafe_url}\n");
        assert!(
            read_login_output(output.as_bytes(), |_| panic!("unsafe URL disclosed"))
                .await
                .is_err()
        );
    }
    let oversized = vec![b'x'; LOGIN_OUTPUT_LIMIT as usize + 1];
    assert!(
        read_login_output(oversized.as_slice(), |_| panic!("unexpected URL"))
            .await
            .is_err()
    );
}

#[test]
fn claude_list_output_contains_only_safe_metadata_and_definitive_empty_states() {
    assert_eq!(
        format_accounts(Some(&[])),
        "accounts[0]{name,email,status}:\nhelp: Run codex account claude add to sign in.\n"
    );
    assert!(format_accounts(/*accounts*/ None).starts_with("status: not_running\n"));
    assert_eq!(
        format_accounts(Some(&[ClaudeAccount {
            name: "account,\nline.json".into(),
            email: Some("one@example.invalid".into()),
            disabled: false,
            unavailable: true
        }])),
        "accounts[1]{name,email,status}:\n  \"account,\\nline.json\",\"one@example.invalid\",unavailable\n"
    );
}
