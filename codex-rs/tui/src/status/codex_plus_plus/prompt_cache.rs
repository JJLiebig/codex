use ratatui::prelude::*;
use std::sync::Mutex;

/// A status card captures the percentage when `/status` is issued.
#[derive(Debug, Default)]
pub(crate) struct StatusCache(Mutex<Option<String>>);

impl StatusCache {
    pub(crate) fn set(&self, value: String) {
        #[expect(clippy::expect_used)]
        let mut stored = self.0.lock().expect("status cache state poisoned");
        *stored = Some(value);
    }

    pub(super) fn lines(
        &self,
        formatter: &super::FieldFormatter,
        value_width: usize,
    ) -> Vec<Line<'static>> {
        #[expect(clippy::expect_used)]
        let stored = self.0.lock().expect("status cache state poisoned");
        stored
            .as_ref()
            .map(|value| {
                let mut wrapped = crate::wrapping::word_wrap_lines(
                    [Line::from(value.clone())],
                    crate::wrapping::RtOptions::new(value_width.max(1)),
                )
                .into_iter();
                let mut lines = Vec::new();
                if let Some(first) = wrapped.next() {
                    lines.push(formatter.line("Cache", first.spans));
                    lines.extend(wrapped.map(|line| formatter.continuation(line.spans)));
                }
                lines
            })
            .unwrap_or_default()
    }
}
