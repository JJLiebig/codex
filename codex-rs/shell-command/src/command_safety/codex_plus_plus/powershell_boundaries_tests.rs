use crate::is_dangerous_command::DangerousCommandMatch;
use crate::is_dangerous_command::DangerousCommandPlatform;
use crate::is_dangerous_command::dangerous_command_match_for_platform;
use pretty_assertions::assert_eq;

#[test]
fn inspection_force_does_not_apply_to_a_separate_delete() {
    for script in [
        "Get-ChildItem -Force\nRemove-Item test",
        "Get-ChildItem -Force\r\nRemove-Item test",
        "$target = 'C:\\Code\\.worktrees\\repo\\cleanup'\n\
         if ((Resolve-Path -LiteralPath $target).Path -ne $target) { throw 'Unexpected target' }\n\
         if (Get-ChildItem -LiteralPath $target -Force) { throw 'Directory not empty' }\n\
         Remove-Item -LiteralPath $target",
        "if ($true) {\nGet-ChildItem -Force\nRemove-Item test\n}",
    ] {
        for flag in ["-Command", "-c", "/Command", "-Command:"] {
            let mut command = vec!["pwsh.exe".to_string(), "-NoProfile".to_string()];
            if flag.ends_with(':') {
                command.push(format!("{flag}{script}"));
            } else {
                command.extend([flag.to_string(), script.to_string()]);
            }
            assert_eq!(
                dangerous_command_match_for_platform(&command, DangerousCommandPlatform::Windows),
                None,
                "{flag} {script}"
            );
        }
    }
}

#[test]
fn force_delete_survives_statement_boundaries_and_nested_execution() {
    for script in [
        "Get-ChildItem -Force\nRemove-Item test -Force",
        "Remove-Item -Force 'first\nsecond'",
        "if ($true) {\nRemove-Item test -Force\n}",
        "Remove-Item $(Write-Output test) -Force",
        "Remove-Item { Write-Output test } -Force",
        "Remove-Item -Force @'\nfirst\nsecond\n'@",
        "Remove-Item test -Force; if (",
        "Remove-Item test -Exclude \u{201c}unused\npattern\u{201d} -Force",
    ] {
        let command = vec![
            "pwsh.exe".to_string(),
            "-Command".to_string(),
            script.to_string(),
        ];
        assert_eq!(
            dangerous_command_match_for_platform(&command, DangerousCommandPlatform::Windows),
            Some(DangerousCommandMatch::Other),
            "{script}"
        );
    }
}
