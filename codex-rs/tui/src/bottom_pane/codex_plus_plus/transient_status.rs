use super::*;

impl BottomPane {
    pub(crate) fn as_renderable_with_status_override<'a>(
        &'a self,
        options: ComposerRenderOptions<'a>,
        status_override: Option<String>,
    ) -> RenderableItem<'a> {
        if status_override.is_none() {
            return self.backdrop_with_options(options);
        }
        let views = if self.centered_dialog().is_some() {
            &self.view_stack[..self.view_stack.len() - 1]
        } else {
            &self.view_stack
        };
        self.renderable_for_views(options, views, status_override)
    }
}
