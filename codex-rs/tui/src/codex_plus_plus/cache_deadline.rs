use crate::app::App;
use crate::app_command::AppCommand;
use crate::app_server_session::AppServerSession;

pub(crate) async fn persist(app: &mut App, server: &AppServerSession, model: String, seconds: u64) {
    let key = format!(
        "cache_deadline_compact_seconds.{}",
        serde_json::json!(model)
    );
    let edit = crate::config_update::replace_config_value(key, serde_json::json!(seconds));
    if let Err(error) =
        crate::config_update::write_config_batch(server.request_handle(), vec![edit]).await
    {
        app.chat_widget
            .add_error_message(format!("Failed to save compaction interval: {error}"));
        return;
    }
    let result = async {
        let response = crate::config_update::read_effective_config(
            server.request_handle(),
            app.config.cwd.display().to_string(),
        )
        .await?;
        let effective = response
            .config
            .additional
            .get("cache_deadline_compact_seconds")
            .and_then(|values| values.get(&model))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        app.refresh_in_memory_config_from_disk().await?;
        app.chat_widget
            .sync_codex_plus_plus_settings_config(&app.config);
        app.chat_widget.submit_op(AppCommand::reload_user_config());
        Ok::<_, color_eyre::eyre::Report>(effective)
    }
    .await;
    match result {
        Ok(effective) if effective == seconds => {
            app.chat_widget.add_info_message(format!("Compaction interval saved for {model}. Applies after the next foreground response."), /*hint*/ None);
        }
        Ok(_) => app.chat_widget.add_error_message(
            "This model's compaction interval is controlled by another configuration layer."
                .to_string(),
        ),
        Err(error) => app.chat_widget.add_error_message(format!(
            "Compaction interval saved, but verification failed: {error}"
        )),
    }
}
