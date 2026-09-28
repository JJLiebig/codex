//! Configuration regressions for the two independent client-owned hint controls.

use super::LocalSettings;
use crate::legacy_core::config::ConfigBuilder;
use codex_config::LoaderOverrides;
use codex_config::types::Tui;
use pretty_assertions::assert_eq;

#[test]
fn footer_preference_is_opt_out_and_independent_of_existing_tooltips() -> anyhow::Result<()> {
    let omitted: Tui = toml::from_str("")?;
    assert!(omitted.show_footer_hints.unwrap_or(true));
    assert!(omitted.show_tooltips);
    assert!(Tui::default().show_footer_hints.unwrap_or(true));
    for footer in [false, true] {
        for tips in [false, true] {
            let parsed: Tui = toml::from_str(&format!(
                "show_footer_hints = {footer}\nshow_tooltips = {tips}\n"
            ))?;
            assert_eq!(parsed.show_footer_hints, Some(footer));
            assert_eq!(parsed.show_tooltips, tips);
            assert_eq!(toml::from_str::<Tui>(&toml::to_string(&parsed)?)?, parsed);
        }
    }
    assert!(toml::from_str::<Tui>("show_footer_hints = 'false'").is_err());
    Ok(())
}

#[tokio::test]
async fn client_reload_resolves_hints_without_changing_transcript_mode() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    let mut previous = None::<LocalSettings>;
    for (footer, tips) in [(true, true), (false, true), (true, false), (false, false)] {
        std::fs::write(
            &path,
            format!(
                "[tui]\nshow_footer_hints = {footer}\nshow_tooltips = {tips}\nfullscreen_transcript = true\nstatus_line = ['model-name']\n"
            ),
        )?;
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .strict_config(true)
            .loader_overrides(LoaderOverrides {
                ignore_project_config: true,
                ..LoaderOverrides::without_managed_config_for_tests()
            })
            .build()
            .await?;
        let local = match previous.as_ref() {
            Some(previous) => previous.reloaded(&config),
            None => LocalSettings::from(&config),
        };
        assert_eq!(local.tui.show_footer_hints, Some(footer));
        assert_eq!(local.tui.show_tooltips, tips);
        assert!(local.tui.fullscreen_transcript);
        assert_eq!(local.tui.status_line, Some(vec!["model-name".to_string()]));
        if let Some(previous) = previous {
            assert_eq!(local.transcript_mode, previous.transcript_mode);
            assert_eq!(local.tui.alternate_screen, previous.tui.alternate_screen);
        }
        previous = Some(local);
    }
    Ok(())
}

#[tokio::test]
async fn launch_override_wins_over_saved_footer_preference() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    let original = "[tui]\nshow_footer_hints = false\nshow_tooltips = false\n";
    std::fs::write(&path, original)?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .strict_config(true)
        .loader_overrides(LoaderOverrides {
            ignore_project_config: true,
            ..LoaderOverrides::without_managed_config_for_tests()
        })
        .cli_overrides(vec![("tui.show_footer_hints".into(), true.into())])
        .build()
        .await?;
    let local = LocalSettings::from(&config);
    assert_eq!(local.tui.show_footer_hints, Some(true));
    assert!(!local.tui.show_tooltips);
    assert_eq!(std::fs::read_to_string(path)?, original);
    Ok(())
}
