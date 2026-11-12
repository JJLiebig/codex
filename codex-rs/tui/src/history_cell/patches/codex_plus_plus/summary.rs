use super::PatchHistoryCell;
use crate::terminal_hyperlinks::HyperlinkLine;

impl PatchHistoryCell {
    pub(crate) fn summary_hyperlink_lines(&self) -> Vec<HyperlinkLine> {
        crate::diff_render::create_diff_preview_with_links(
            &self.changes,
            &self.cwd,
            /*wrap_cols*/ 0,
            /*preview_lines*/ 0,
        )
    }
}
