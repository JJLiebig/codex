use super::*;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use codex_config::cache_deadline_compact_interval;

impl ChatWidget {
    pub(crate) fn open_cache_deadline_models(&mut self) {
        let mut models: Vec<String> = self
            .model_catalog
            .models
            .iter()
            .filter(|model| model.show_in_picker)
            .map(|model| model.model.clone())
            .collect();
        models.push(self.current_model().to_string());
        if let Some(configured) = self
            .config
            .config_layer_stack
            .effective_config()
            .get("cache_deadline_compact_seconds")
            .and_then(toml::Value::as_table)
        {
            models.extend(configured.keys().cloned());
        }
        let mut seen = std::collections::HashSet::new();
        models.retain(|model| seen.insert(model.clone()));
        let items = models
            .into_iter()
            .map(|model| {
                let interval =
                    cache_deadline_compact_interval(&self.config.config_layer_stack, &model);
                SelectionItem {
                    name: self.model_catalog.display_name(&model).to_string(),
                    description: Some(interval.map_or_else(
                        || "Off".to_string(),
                        |duration| format!("After {}", duration_label(duration.as_secs())),
                    )),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenCacheDeadlineInterval(model.clone()))
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        self.show_selection_view(SelectionViewParams {
            title: Some("Auto-compact before cache expiry".to_string()),
            subtitle: Some("Choose a model. GPT-6 and newer default to 55m.".to_string()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_cache_deadline_interval(&mut self, model: String) {
        let seconds = cache_deadline_compact_interval(&self.config.config_layer_stack, &model)
            .map(|interval| interval.as_secs())
            .unwrap_or(0);
        self.show_selection_view(interval_params(model, seconds));
    }

    pub(crate) fn open_cache_deadline_custom(&mut self, model: String) {
        let seconds = cache_deadline_compact_interval(&self.config.config_layer_stack, &model)
            .map(|interval| interval.as_secs())
            .unwrap_or(3300);
        let tx = self.app_event_tx.clone();
        self.bottom_pane.show_text_prompt(CustomPromptView::new(
            format!("Compaction interval · {model}"),
            "Enter a duration, e.g. 55m or 30s".to_string(),
            duration_label(seconds),
            Some("Compacts once when idle or before the next response.".to_string()),
            Box::new(move |text| match parse_duration(&text) {
                Some(seconds)
                    if codex_config::cache_deadline_known_lifetime(&model)
                        .is_some_and(|ttl| seconds >= ttl.as_secs()) =>
                {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event(
                            "Astra / Sol intervals must be under 60m. Try 55m.".to_string(),
                        ),
                    )))
                }
                Some(seconds) => tx.send(AppEvent::PersistCacheDeadline {
                    model: model.clone(),
                    seconds,
                }),
                None => tx.send(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_error_event(
                        "Enter a positive duration with s, m, or h, such as 55m.".to_string(),
                    ),
                ))),
            }),
        ));
    }
}

fn interval_params(model: String, seconds: u64) -> SelectionViewParams {
    let mut items: Vec<_> = [("Off", 0), ("55 minutes", 3300)]
        .into_iter()
        .map(|(name, value)| {
            let model = model.clone();
            SelectionItem {
                name: name.to_string(),
                is_current: seconds == value,
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::PersistCacheDeadline {
                        model: model.clone(),
                        seconds: value,
                    })
                })],
                dismiss_on_select: true,
                ..Default::default()
            }
        })
        .collect();
    let custom_model = model.clone();
    items.push(SelectionItem {
        name: "Custom interval…".to_string(),
        description: (seconds != 0 && seconds != 3300).then(|| duration_label(seconds)),
        is_current: seconds != 0 && seconds != 3300,
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenCacheDeadlineCustom(custom_model.clone()))
        })],
        dismiss_on_select: true,
        ..Default::default()
    });
    SelectionViewParams {
        title: Some(format!("Auto-compact · {model}")),
        subtitle: Some("GPT-6 and newer default: 55m. Compacts once when safe.".to_string()),
        items,
        ..Default::default()
    }
}

fn duration_label(seconds: u64) -> String {
    if seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn parse_duration(text: &str) -> Option<u64> {
    let text = text.trim();
    let multiplier = match text.chars().last()? {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return None,
    };
    text[..text.len() - 1]
        .trim()
        .parse::<u64>()
        .ok()?
        .checked_mul(multiplier)
        .filter(|seconds| *seconds > 0 && *seconds <= i64::MAX as u64)
}

#[cfg(test)]
#[path = "cache_deadline_tests.rs"]
mod tests;
