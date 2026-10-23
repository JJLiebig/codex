use super::*;
use crate::ConfigLayerEntry;
use crate::ConfigLayerSource;
use pretty_assertions::assert_eq;

#[test]
fn cache_deadline_requires_an_exact_model_interval() {
    let config = ConfigLayerStack::default();
    for model in [
        "gpt-6-astra",
        "gpt-6.1-sol",
        "gpt-6-sol",
        "gpt-7",
        "claude-sonnet-4",
        "custom",
    ] {
        assert_eq!(
            cache_deadline_compact_interval(&config, model),
            None,
            "{model}"
        );
    }
    let config = ConfigLayerStack::new(
        vec![ConfigLayerEntry::new(
            ConfigLayerSource::SessionFlags,
            toml::from_str(
                "[cache_deadline_compact_seconds]\ngpt-6-astra = 0\n'gpt-6.1-sol' = 1800\ngpt-6-sol = 3600\ncustom = 20",
            )
            .unwrap(),
        )],
        Default::default(),
        Default::default(),
    )
    .unwrap();
    for (model, expected) in [
        ("gpt-6-astra", None),
        ("gpt-6.1-sol", Some(1800)),
        ("gpt-6-sol", None),
        ("custom", Some(20)),
        ("gpt-7", None),
    ] {
        assert_eq!(
            cache_deadline_compact_interval(&config, model),
            expected.map(Duration::from_secs),
            "{model}"
        );
    }
}
