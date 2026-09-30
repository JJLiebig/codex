use super::*;
use super::super::codex_plus_plus_settings_params;
use super::super::settings_list_keymap;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::codex_plus_plus::destructive_command_guard::DcgStatus;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::ConfigBuilder;
use codex_config::LoaderOverrides;
use codex_config::ModelCapacityRetryMode;
use codex_config::ToolActivityPresentation;
use codex_config::WeeklyUsageWindowAutoStart;
use codex_config::types::AutomaticAccountSelection;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

async fn local(home: &Path) -> anyhow::Result<LocalSettings> {
    let config = ConfigBuilder::default()
        .codex_home(home.to_path_buf())
        .loader_overrides(LoaderOverrides {
            ignore_project_config: true,
            ..LoaderOverrides::without_managed_config_for_tests()
        })
        .build()
        .await?;
    Ok(LocalSettings::from(&config))
}

fn menu(local: &LocalSettings, dcg: Option<DcgStatus>) -> SelectionViewParams {
    let keymap = settings_list_keymap(RuntimeKeymap::defaults().list);
    let mut params = codex_plus_plus_settings_params(
        AutomaticAccountSelection::Enabled,
        WeeklyUsageWindowAutoStart::Disabled,
        None,
        ModelCapacityRetryMode::Bounded,
        ToolActivityPresentation::Full,
        /*current_quiet_updates*/ true,
        /*weekly_supported*/ false,
        dcg,
        &keymap,
    );
    add_to_menu(&mut params, local);
    params
}

fn item<'a>(params: &'a SelectionViewParams, name: &str) -> &'a SelectionItem {
    params.items.iter().find(|item| item.name == name).expect("menu item")
}

fn disable(params: &SelectionViewParams, name: &str, tx: &AppEventSender) {
    let toggle = item(params, name).toggle.as_ref().expect("toggle");
    (toggle.action)(/*is_on*/ false, tx);
}

async fn next(rx: &mut UnboundedReceiver<AppEvent>) -> AppEvent {
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("settings action completed")
        .expect("settings event")
}

#[tokio::test]
async fn space_then_escape_does_not_write_or_submit_other_settings() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    let original = "# unchanged\n[tui]\nshow_tooltips = true\n";
    std::fs::write(&path, original)?;
    let local = local(home.path()).await?;
    let params = menu(&local, None);
    assert!(item(&params, "Show footer hints").toggle.as_ref().unwrap().is_on);
    assert!(item(&params, "Show tips").toggle.as_ref().unwrap().is_on);
    let down_to_footer = params.items.len() - 2;
    let (tx, mut rx) = unbounded_channel();
    let keymap = settings_list_keymap(RuntimeKeymap::defaults().list);
    let mut view = ListSelectionView::new(params, AppEventSender::new(tx), keymap);
    for _ in 0..down_to_footer {
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(rx.try_recv().is_err());
    assert_eq!(std::fs::read_to_string(path)?, original);
    Ok(())
}

#[tokio::test]
async fn any_ordinary_row_saves_both_display_values_before_server_settings() -> anyhow::Result<()> {
    for save_row in ["Automatic account selection", "Show footer hints", "Show tips"] {
        let home = tempfile::tempdir()?;
        let path = home.path().join("config.toml");
        std::fs::write(&path, "[tui]\nstatus_line = ['model-name']\nshow_server_version_notice = true\n")?;
        let local = local(home.path()).await?;
        let params = menu(&local, None);
        let (tx, mut rx) = unbounded_channel();
        let tx = AppEventSender::new(tx);
        disable(&params, "Show footer hints", &tx);
        disable(&params, "Show tips", &tx);
        (item(&params, save_row).actions[0])(&tx);
        assert!(matches!(next(&mut rx).await, AppEvent::InsertHistoryCell(_)));
        assert!(matches!(next(&mut rx).await, AppEvent::PersistCodexPlusPlusSettings { .. }));
        let saved: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
        assert_eq!(saved["tui"]["show_footer_hints"].as_bool(), Some(false));
        assert_eq!(saved["tui"]["show_tooltips"].as_bool(), Some(false));
        assert_eq!(saved["tui"]["show_server_version_notice"].as_bool(), Some(true));
        assert_eq!(saved["tui"]["status_line"][0].as_str(), Some("model-name"));
        assert!(saved["tui"].get("show_tips").is_none());
        assert!(rx.try_recv().is_err());
        let reloaded = self::local(home.path()).await?;
        let reopened = menu(&reloaded, None);
        assert!(!item(&reopened, "Show footer hints").toggle.as_ref().unwrap().is_on);
        assert!(!item(&reopened, "Show tips").toggle.as_ref().unwrap().is_on);
    }
    Ok(())
}

#[tokio::test]
async fn changing_only_footer_does_not_materialize_tooltip_default() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("custom-config.toml");
    std::fs::write(&path, "# preserve this comment\n[tui]\nstatus_line = []\n")?;
    let initial = DisplaySettings { show_footer_hints: true, show_tooltips: true };
    save(&path, initial, DisplaySettings { show_footer_hints: false, ..initial }).await?;
    let text = std::fs::read_to_string(&path)?;
    let saved: toml::Value = toml::from_str(&text)?;
    assert!(text.contains("# preserve this comment"));
    assert_eq!(saved["tui"]["show_footer_hints"].as_bool(), Some(false));
    assert!(saved["tui"].get("show_tooltips").is_none());
    assert!(!home.path().join("config.toml").exists());
    Ok(())
}

#[tokio::test]
async fn unchanged_display_values_forward_save_without_touching_local_file() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    let original = "# no explicit appearance overrides\n";
    std::fs::write(&path, original)?;
    let local = local(home.path()).await?;
    let params = menu(&local, None);
    let (tx, mut rx) = unbounded_channel();
    (item(&params, "Show tips").actions[0])(&AppEventSender::new(tx));
    assert!(matches!(rx.try_recv()?, AppEvent::PersistCodexPlusPlusSettings { .. }));
    assert_eq!(std::fs::read_to_string(path)?, original);
    Ok(())
}

#[tokio::test]
async fn dcg_management_retains_save_before_manage_order() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let local = local(home.path()).await?;
    let params = menu(&local, Some(DcgStatus::NotInstalled));
    let (tx, mut rx) = unbounded_channel();
    let tx = AppEventSender::new(tx);
    disable(&params, "Show tips", &tx);
    (params.items[0].actions[0])(&tx);
    assert!(matches!(next(&mut rx).await, AppEvent::InsertHistoryCell(_)));
    assert!(matches!(next(&mut rx).await, AppEvent::PersistCodexPlusPlusSettings { .. }));
    assert!(matches!(next(&mut rx).await, AppEvent::OpenDcgInstallConfirmation));
    Ok(())
}

#[tokio::test]
async fn failed_local_write_keeps_file_and_does_not_submit_server_or_dcg_actions() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let path = home.path().join("config.toml");
    let local = local(home.path()).await?;
    let params = menu(&local, Some(DcgStatus::NotInstalled));
    // A concurrent edit made after opening the menu must not be replaced with reconstructed TOML.
    let invalid = "[tui\n";
    std::fs::write(&path, invalid)?;
    let (tx, mut rx) = unbounded_channel();
    let tx = AppEventSender::new(tx);
    disable(&params, "Show footer hints", &tx);
    (params.items[0].actions[0])(&tx);
    assert!(matches!(next(&mut rx).await, AppEvent::InsertHistoryCell(_)));
    assert!(rx.try_recv().is_err());
    assert_eq!(std::fs::read_to_string(path)?, invalid);
    Ok(())
}
