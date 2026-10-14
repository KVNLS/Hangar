//! Segmented control (`Split | Inline`, `By status | By project`).

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::ActiveTokens as _;

type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    on_select: Option<SelectHandler>,
}

impl Segmented {
    pub fn new(
        id: impl Into<ElementId>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            on_select: None,
        }
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        let on_select = self.on_select;
        div()
            .id(self.id)
            .flex()
            .flex_none()
            .p(px(2.))
            .rounded(px(6.))
            .bg(t.hover)
            .border_1()
            .border_color(t.line)
            .children(self.options.into_iter().enumerate().map(|(i, label)| {
                let on = i == self.selected;
                div()
                    .id(i)
                    .flex()
                    .items_center()
                    .h(px(22.))
                    .px(px(10.))
                    .rounded(px(4.))
                    .border_1()
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .map(|this| {
                        if on {
                            this.bg(t.elev).border_color(t.line2).text_color(t.tx)
                        } else {
                            this.border_color(transparent_black()).text_color(t.tx2)
                        }
                    })
                    .when_some(on_select.clone().filter(|_| !on), |this, handler| {
                        this.cursor_pointer()
                            .on_click(move |_, window, cx| handler(&i, window, cx))
                    })
                    .child(label)
            }))
    }
}
